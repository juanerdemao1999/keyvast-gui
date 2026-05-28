//! Live acquisition pipeline: one SimulatorBackend → FanoutBlockBuffer →
//! recorder thread (disk) + preview channel (GUI).
//!
//! Replaces the old `start_preview()` path in Device mode.  Both recording
//! and display now consume the same data source, so what you see is what
//! gets saved.
//!
//! Thread layout:
//!   producer thread  — SimulatorBackend → preview_tx (mpsc) + shared FanoutBuffer
//!   recorder thread  — pops from FanoutBuffer recorder consumer → StreamingRecorder
//!   GUI main thread  — drains preview_rx each frame → ingest_block()

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{sync::mpsc, thread};

use kv_buffer::{BufferConsumerId, FanoutBlockBuffer};
use kv_recorder::StreamingRecorder;
use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::SampleBlock;

/// Recorder buffer: ~5 s at 64ch × 30 kHz / 64 spp = 2344 blocks/s.
const RECORDER_CAPACITY: usize = 12_000;

// ── Commands GUI → recorder thread ──────────────────────────────────

pub enum RecorderCmd {
    /// Open a new recording at the given directory.
    Start(PathBuf),
    /// Finalize and close the current recording.
    Stop,
    /// Stop recording (if active) and terminate the thread.
    Terminate,
}

// ── Events recorder thread → GUI ────────────────────────────────────

#[derive(Debug)]
pub enum RecorderEvent {
    /// Recording file opened successfully.
    Started,
    /// Recording finalized — carries final block and byte counts.
    Stopped { blocks: u64, bytes: u64 },
    /// Error opening or writing the recording.
    Error(String),
}

// ── Public handle ────────────────────────────────────────────────────

pub struct LivePipelineHandle {
    /// GUI receives one SampleBlock per real-time packet via this channel.
    pub preview_rx: mpsc::Receiver<SampleBlock>,
    /// Events sent from the recorder thread back to the GUI.
    pub event_rx: mpsc::Receiver<RecorderEvent>,
    /// Commands GUI sends to the recorder thread.
    pub recorder_cmd_tx: mpsc::Sender<RecorderCmd>,
    /// Cumulative preview blocks received (for BlockStats computation).
    pub total_blocks: u64,
    /// Wall-clock start time (for elapsed-seconds in BlockStats).
    pub start_time: Instant,
    stop_flag: Arc<AtomicBool>,
    producer_thread: Option<thread::JoinHandle<()>>,
    recorder_thread: Option<thread::JoinHandle<()>>,
}

impl LivePipelineHandle {
    /// Stop both background threads and join them.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        // Ask recorder to terminate (finishes any active recording cleanly)
        let _ = self.recorder_cmd_tx.send(RecorderCmd::Terminate);
        if let Some(t) = self.producer_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.recorder_thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for LivePipelineHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Constructor ──────────────────────────────────────────────────────

/// Start the live pipeline and return a handle for the GUI to use.
pub fn start_live_pipeline(config: SimulatorConfig) -> LivePipelineHandle {
    // Shared fanout buffer — recorder consumer pops from here
    let shared = Arc::new(Mutex::new(FanoutBlockBuffer::new()));
    let recorder_id = {
        let mut buf = shared.lock().expect("buffer lock poisoned");
        buf.add_consumer("recorder", RECORDER_CAPACITY)
            .expect("failed to add recorder consumer")
    };

    let (preview_tx, preview_rx) = mpsc::channel::<SampleBlock>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<RecorderCmd>();
    let (event_tx, event_rx) = mpsc::channel::<RecorderEvent>();
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Producer thread
    let shared_prod = Arc::clone(&shared);
    let stop_prod = Arc::clone(&stop_flag);
    let producer_thread = thread::spawn(move || {
        producer_loop(config, shared_prod, preview_tx, stop_prod);
    });

    // Recorder thread
    let shared_rec = Arc::clone(&shared);
    let recorder_thread = thread::spawn(move || {
        recorder_loop(shared_rec, recorder_id, cmd_rx, event_tx);
    });

    LivePipelineHandle {
        preview_rx,
        event_rx,
        recorder_cmd_tx: cmd_tx,
        total_blocks: 0,
        start_time: Instant::now(),
        stop_flag,
        producer_thread: Some(producer_thread),
        recorder_thread: Some(recorder_thread),
    }
}

// ── Producer thread ──────────────────────────────────────────────────

fn producer_loop(
    config: SimulatorConfig,
    shared: Arc<Mutex<FanoutBlockBuffer>>,
    preview_tx: mpsc::Sender<SampleBlock>,
    stop_flag: Arc<AtomicBool>,
) {
    let sample_rate = config.device.sample_rate;
    let spp = config.device.samples_per_packet;
    let sleep_dur = Duration::from_secs_f64(if sample_rate > 0.0 && spp > 0 {
        spp as f64 / sample_rate
    } else {
        0.001
    });

    let Ok(mut sim) = SimulatorBackend::new(config) else {
        return;
    };

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        match sim.next_block() {
            Ok(block) => {
                // Send preview copy to GUI — if receiver is gone, stop.
                if preview_tx.send(block.clone()).is_err() {
                    break;
                }
                // Push original into shared fanout (recorder gets its slot).
                shared.lock().expect("buffer lock poisoned").push(block);
            }
            Err(_) => break,
        }

        thread::sleep(sleep_dur);
    }
}

// ── Recorder thread ──────────────────────────────────────────────────

fn recorder_loop(
    shared: Arc<Mutex<FanoutBlockBuffer>>,
    recorder_id: BufferConsumerId,
    cmd_rx: mpsc::Receiver<RecorderCmd>,
    event_tx: mpsc::Sender<RecorderEvent>,
) {
    let mut recorder: Option<StreamingRecorder> = None;

    loop {
        // Handle any pending commands.
        match cmd_rx.try_recv() {
            Ok(RecorderCmd::Start(path)) => match StreamingRecorder::new(&path) {
                Ok(rec) => {
                    recorder = Some(rec);
                    let _ = event_tx.send(RecorderEvent::Started);
                }
                Err(e) => {
                    let _ = event_tx.send(RecorderEvent::Error(e.to_string()));
                }
            },
            Ok(RecorderCmd::Stop) => {
                if let Some(rec) = recorder.take() {
                    finish_recording(rec, &event_tx);
                }
            }
            Ok(RecorderCmd::Terminate) | Err(mpsc::TryRecvError::Disconnected) => {
                // Finalize any active recording then exit.
                if let Some(rec) = recorder.take() {
                    let _ = rec.finish();
                }
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        // Drain recorder consumer — write if recording, discard otherwise.
        loop {
            let block = {
                let mut buf = shared.lock().expect("buffer lock poisoned");
                buf.pop(recorder_id).ok().flatten()
            }; // lock released before write
            let Some(block) = block else { break };

            if let Some(ref mut rec) = recorder {
                if let Err(e) = rec.write_block(&block) {
                    eprintln!("[recorder] write_block error: {e}");
                }
            }
            // If not recording, block is silently discarded.
        }

        thread::sleep(Duration::from_millis(1));
    }
}

/// Finalize a `StreamingRecorder` and send the result as a `RecorderEvent`.
fn finish_recording(rec: StreamingRecorder, event_tx: &mpsc::Sender<RecorderEvent>) {
    match rec.finish() {
        Ok(summary) => {
            let _ = event_tx.send(RecorderEvent::Stopped {
                blocks: summary.recording.block_count,
                bytes: summary.recording.byte_count,
            });
        }
        Err(e) => {
            let _ = event_tx.send(RecorderEvent::Error(e.to_string()));
        }
    }
}
