//! Live acquisition pipeline: one acquisition backend → FanoutBlockBuffer →
//! recorder thread (disk) + preview channel (GUI).
//!
//! Replaces the old `start_preview()` path in Device mode.  Both recording
//! and display now consume the same data source, so what you see is what
//! gets saved.  The producer's backend is selected by `PipelineSource`
//! (simulator or real RHD hardware) — everything downstream is identical.
//!
//! Thread layout:
//!   producer thread  — PipelineSource backend → preview_tx (mpsc) + shared FanoutBuffer
//!   recorder thread  — pops from FanoutBuffer recorder consumer → StreamingRecorder
//!   GUI main thread  — drains preview_rx each frame → ingest_block()

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use std::{sync::mpsc, thread};

use kv_buffer::{BufferConsumerId, FanoutBlockBuffer};
use kv_recorder::StreamingRecorder;
use kv_rhd::{RhdHardwareBackend, RhdHardwareOptions};
use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::SampleBlock;

use crate::channel_select;

/// Recorder buffer: ~5 s at 64ch × 30 kHz / 64 spp = 2344 blocks/s.
const RECORDER_CAPACITY: usize = 12_000;

// ── Data source selector ────────────────────────────────────────────

/// Which backend the producer thread pulls blocks from.
///
/// Everything downstream of the producer (fanout buffer, recorder thread,
/// preview channel, GUI ingest) is identical regardless of source, so the
/// hardware-independent boundary holds: upper layers only ever see
/// `SampleBlock`.
pub enum PipelineSource {
    /// Synthetic data — the default, so the GUI runs without hardware.
    Simulator(SimulatorConfig),
    /// Real Intan RHD / Opal Kelly acquisition via the `kv-rhd` backend.
    /// Boxed because `RhdHardwareOptions` is much larger than the simulator
    /// config (avoids a lopsided enum / `clippy::large_enum_variant`).
    Rhd(Box<RhdHardwareOptions>),
}

// ── Commands GUI → recorder thread ──────────────────────────────────

pub enum RecorderCmd {
    /// Open a new recording at the given directory. When `channels` is
    /// `Some`, only those channel indices are written (selective save).
    Start {
        path: PathBuf,
        channels: Option<Vec<usize>>,
    },
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
    /// Periodic progress report while recording (sent ~5/s).
    Progress { blocks: u64, bytes: u64 },
    /// Error opening or writing the recording.
    Error(String),
    /// Periodic buffer health report (sent ~5/s while running).
    /// `occupancy` is 0.0..=1.0 (buffered / capacity).
    BufferStatus { occupancy: f64 },
    /// The acquisition source (simulator or hardware) failed to open or to
    /// produce a block. Carries a human-readable message for the GUI banner.
    /// The producer thread has stopped by the time this is sent.
    SourceError(String),
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
    /// Packet-ID based drop detection: expected next packet_id.
    pub expected_next_packet_id: Option<u64>,
    /// Cumulative count of detected dropped blocks (packet-ID gaps).
    pub dropped_blocks: u64,
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

/// Shared state: fanout buffer + condvar so the recorder wakes on new data.
type SharedBuffer = Arc<(Mutex<FanoutBlockBuffer>, Condvar)>;

/// Start the live pipeline and return a handle for the GUI to use.
pub fn start_live_pipeline(source: PipelineSource) -> LivePipelineHandle {
    // Shared fanout buffer — recorder consumer pops from here
    let shared: SharedBuffer = Arc::new((Mutex::new(FanoutBlockBuffer::new()), Condvar::new()));
    let recorder_id = {
        let mut buf = shared.0.lock().expect("buffer lock poisoned");
        buf.add_consumer("recorder", RECORDER_CAPACITY)
            .expect("failed to add recorder consumer")
    };

    // Bounded preview channel: at 30 kHz / 64 spp ≈ 469 blocks/s, 1024 slots
    // gives ~2 s of headroom before dropping.  If the GUI can't keep up, the
    // producer will block briefly rather than accumulating unbounded memory.
    let (preview_tx, preview_rx) = mpsc::sync_channel::<SampleBlock>(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel::<RecorderCmd>();
    let (event_tx, event_rx) = mpsc::channel::<RecorderEvent>();
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Producer thread — reports source open/read failures via its own event sender.
    let shared_prod = Arc::clone(&shared);
    let stop_prod = Arc::clone(&stop_flag);
    let event_tx_prod = event_tx.clone();
    let producer_thread = thread::spawn(move || {
        producer_loop(source, shared_prod, preview_tx, event_tx_prod, stop_prod);
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
        expected_next_packet_id: None,
        dropped_blocks: 0,
        start_time: Instant::now(),
        stop_flag,
        producer_thread: Some(producer_thread),
        recorder_thread: Some(recorder_thread),
    }
}

// ── Producer thread ──────────────────────────────────────────────────

/// An opened acquisition backend. Unifies the simulator and the RHD hardware
/// behind a single `read_block`, keeping `producer_loop` source-agnostic.
enum ActiveSource {
    Simulator(SimulatorBackend),
    Rhd(RhdHardwareBackend),
}

impl ActiveSource {
    /// Read the next block, normalising the backend-specific error to a string
    /// the GUI can display.
    fn read_block(&mut self) -> Result<SampleBlock, String> {
        match self {
            ActiveSource::Simulator(s) => s.next_block().map_err(|e| e.to_string()),
            ActiveSource::Rhd(r) => r.read_block().map_err(|e| e.to_string()),
        }
    }
}

fn producer_loop(
    source: PipelineSource,
    shared: SharedBuffer,
    preview_tx: mpsc::SyncSender<SampleBlock>,
    event_tx: mpsc::Sender<RecorderEvent>,
    stop_flag: Arc<AtomicBool>,
) {
    // Open the backend. The simulator runs faster than real time, so it is
    // paced with a sleep; real hardware blocks inside read_block() until a
    // full USB block is available, so it needs no artificial pacing.
    let (mut active, sleep_dur) = match source {
        PipelineSource::Simulator(config) => {
            let sample_rate = config.device.sample_rate;
            let spp = config.device.samples_per_packet;
            let sleep_dur = Duration::from_secs_f64(if sample_rate > 0.0 && spp > 0 {
                spp as f64 / sample_rate
            } else {
                0.001
            });
            match SimulatorBackend::new(config) {
                Ok(sim) => (ActiveSource::Simulator(sim), sleep_dur),
                Err(e) => {
                    let _ = event_tx
                        .send(RecorderEvent::SourceError(format!("simulator init failed: {e}")));
                    return;
                }
            }
        }
        PipelineSource::Rhd(options) => match RhdHardwareBackend::open(*options) {
            Ok(backend) => (ActiveSource::Rhd(backend), Duration::ZERO),
            Err(e) => {
                let _ = event_tx
                    .send(RecorderEvent::SourceError(format!("RHD device open failed: {e}")));
                return;
            }
        },
    };

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        match active.read_block() {
            Ok(block) => {
                // Send preview copy to GUI.  try_send avoids blocking the
                // producer when the GUI falls behind — dropped preview frames
                // are acceptable, but stalling the acquisition is not.
                // If the receiver is disconnected, stop the producer.
                match preview_tx.try_send(block.clone()) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full(_)) => {
                        // GUI too slow — skip this preview frame
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                }
                // Push original into shared fanout (recorder gets its slot)
                // and notify the recorder thread via condvar.
                {
                    shared.0.lock().expect("buffer lock poisoned").push(block);
                    shared.1.notify_one();
                }
            }
            Err(message) => {
                // Surface the failure to the GUI, unless the user already asked
                // to stop (in which case the error is just the teardown race).
                if !stop_flag.load(Ordering::Relaxed) {
                    let _ = event_tx.send(RecorderEvent::SourceError(message));
                }
                break;
            }
        }

        if !sleep_dur.is_zero() {
            thread::sleep(sleep_dur);
        }
    }
}

// ── Recorder thread ──────────────────────────────────────────────────

fn recorder_loop(
    shared: SharedBuffer,
    recorder_id: BufferConsumerId,
    cmd_rx: mpsc::Receiver<RecorderCmd>,
    event_tx: mpsc::Sender<RecorderEvent>,
) {
    let mut recorder: Option<StreamingRecorder> = None;
    // Channel subset for selective save — captured at recording start so a
    // selection change mid-recording cannot change the file's channel count.
    let mut record_channels: Option<Vec<usize>> = None;
    // Rate-limit BufferStatus events to ~5 per second.
    let mut last_status_report = Instant::now();
    const STATUS_INTERVAL: Duration = Duration::from_millis(200);
    // Condvar timeout — wakes periodically to check commands even if idle.
    const CONDVAR_TIMEOUT: Duration = Duration::from_millis(50);

    loop {
        // Handle any pending commands.
        match cmd_rx.try_recv() {
            Ok(RecorderCmd::Start { path, channels }) => match StreamingRecorder::new(&path) {
                Ok(rec) => {
                    recorder = Some(rec);
                    record_channels = channels;
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
        let mut drained_any = false;
        loop {
            let block = {
                let mut buf = shared.0.lock().expect("buffer lock poisoned");
                buf.pop(recorder_id).ok().flatten()
            }; // lock released before write
            let Some(block) = block else { break };
            drained_any = true;

            if let Some(ref mut rec) = recorder {
                let result = match record_channels {
                    Some(ref indices) => {
                        let filtered = channel_select::filter_block_channels(&block, indices);
                        rec.write_block(&filtered)
                    }
                    None => rec.write_block(&block),
                };
                if let Err(e) = result {
                    // A failed write means the file can no longer be trusted —
                    // stop and finalize immediately rather than keep writing
                    // into a possibly corrupt recording.
                    let _ = event_tx.send(RecorderEvent::Error(format!(
                        "write failed: {e} — recording stopped; file may be incomplete"
                    )));
                    log::error!("recording write failed, stopping: {e}");
                    if let Some(rec) = recorder.take() {
                        finish_recording(rec, &event_tx);
                    }
                    record_channels = None;
                }
            }
            // If not recording, block is silently discarded.
        }

        // Send buffer occupancy to GUI at ~5 Hz.
        if last_status_report.elapsed() >= STATUS_INTERVAL {
            let occupancy = {
                let buf = shared.0.lock().expect("buffer lock poisoned");
                buf.consumer_status(recorder_id)
                    .map(|s| {
                        if s.capacity_blocks > 0 {
                            s.buffered_blocks as f64 / s.capacity_blocks as f64
                        } else {
                            0.0
                        }
                    })
                    .unwrap_or(0.0)
            };
            let _ = event_tx.send(RecorderEvent::BufferStatus { occupancy });
            if let Some(ref rec) = recorder {
                let _ = event_tx.send(RecorderEvent::Progress {
                    blocks: rec.block_count(),
                    bytes: rec.byte_count(),
                });
            }
            last_status_report = Instant::now();
        }

        // Wait for new data via condvar instead of busy-polling.
        if !drained_any {
            let buf = shared.0.lock().expect("buffer lock poisoned");
            let _ = shared.1.wait_timeout(buf, CONDVAR_TIMEOUT);
        }
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
