//! Threaded fan-out acquisition pipeline.
//!
//! Wires a block producer (any `AcquisitionSource`) through a
//! `FanoutBlockBuffer` so that recorder and preview consumers receive
//! independent copies without blocking each other.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use kv_buffer::{BufferError, ConsumerBufferStatus, FanoutBlockBuffer};
use kv_integrity::{IncrementalIntegrity, IntegrityError, IntegrityReport, check_blocks};
use kv_recorder::{
    LatencyDistribution, RecorderError, RecordingConfig, RecordingSummary, StreamingRecorder,
};
use kv_types::{DeviceConfig, SampleBlock};

use crate::AcquisitionSource;

/// Configuration for a threaded fan-out acquisition pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineConfig {
    pub device: DeviceConfig,
    pub requested_blocks: usize,
    pub recorder_capacity_blocks: usize,
    pub preview_capacity_blocks: usize,
}

/// Wall-clock timing measurements from a pipeline run.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineTiming {
    pub wall_clock_seconds: f64,
    pub first_block_latency_seconds: Option<f64>,
}

/// Result of a completed threaded pipeline run.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineResult {
    pub recorded_blocks: Vec<SampleBlock>,
    pub integrity: IntegrityReport,
    pub timing: PipelineTiming,
    pub recorder_status: ConsumerBufferStatus,
    pub preview_status: ConsumerBufferStatus,
}

/// Configuration for a streaming fan-out pipeline that writes to disk
/// incrementally instead of collecting all blocks in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamingPipelineConfig {
    pub device: DeviceConfig,
    pub requested_blocks: usize,
    pub output_dir: PathBuf,
    pub recorder_capacity_blocks: usize,
    pub preview_capacity_blocks: usize,
}

/// Result of a completed streaming pipeline run.
#[derive(Debug)]
pub struct StreamingPipelineResult {
    pub recording: RecordingSummary,
    pub integrity: IntegrityReport,
    pub timing: PipelineTiming,
    pub recorder_status: ConsumerBufferStatus,
    pub preview_status: ConsumerBufferStatus,
    pub max_write_latency_us: Option<u64>,
    pub latency_distribution: Option<LatencyDistribution>,
}

/// Errors from the threaded pipeline.
#[derive(Debug)]
pub enum PipelineError {
    BufferSetup(BufferError),
    ProducerFailed {
        message: String,
        /// Number of blocks successfully acquired before the failure, so
        /// callers can tell "failed after 0 blocks" from "failed after 999".
        blocks_acquired: u64,
    },
    ProducerPanicked,
    IntegrityCheck(IntegrityError),
    Recorder(RecorderError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferSetup(error) => write!(formatter, "buffer setup failed: {error}"),
            Self::ProducerFailed {
                message,
                blocks_acquired,
            } => write!(
                formatter,
                "producer failed after {blocks_acquired} blocks: {message}"
            ),
            Self::ProducerPanicked => write!(formatter, "producer thread panicked"),
            Self::IntegrityCheck(error) => write!(formatter, "integrity check failed: {error}"),
            Self::Recorder(error) => write!(formatter, "recorder failed: {error}"),
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BufferSetup(error) => Some(error),
            Self::IntegrityCheck(error) => Some(error),
            Self::Recorder(error) => Some(error),
            Self::ProducerFailed { .. } | Self::ProducerPanicked => None,
        }
    }
}

impl From<BufferError> for PipelineError {
    fn from(error: BufferError) -> Self {
        Self::BufferSetup(error)
    }
}

impl From<IntegrityError> for PipelineError {
    fn from(error: IntegrityError) -> Self {
        Self::IntegrityCheck(error)
    }
}

impl From<RecorderError> for PipelineError {
    fn from(error: RecorderError) -> Self {
        Self::Recorder(error)
    }
}

struct SharedState {
    buffer: FanoutBlockBuffer,
    producer_done: bool,
    producer_error: Option<String>,
}

/// Run a threaded fan-out acquisition pipeline.
///
/// The producer reads blocks from `source` in a dedicated thread and pushes
/// each block into a `FanoutBlockBuffer`.  The main thread drains the
/// recorder consumer and discards preview blocks.  After the producer
/// finishes, the pipeline runs an integrity check on the recorded blocks
/// and returns timing, buffer status, and integrity results.
pub fn run_threaded_pipeline<S>(
    config: &PipelineConfig,
    source: S,
) -> Result<PipelineResult, PipelineError>
where
    S: AcquisitionSource + Send + 'static,
    S::Error: Send + 'static,
{
    let start = Instant::now();

    let mut fanout = FanoutBlockBuffer::new();
    let recorder_id = fanout.add_consumer("recorder", config.recorder_capacity_blocks)?;
    let preview_id = fanout.add_consumer("preview", config.preview_capacity_blocks)?;

    let shared = Arc::new((
        Mutex::new(SharedState {
            buffer: fanout,
            producer_done: false,
            producer_error: None,
        }),
        Condvar::new(),
    ));

    let requested = config.requested_blocks;
    let shared_producer = Arc::clone(&shared);

    let producer_handle = thread::spawn(move || {
        producer_loop(source, requested, &shared_producer);
    });

    let mut recorded_blocks = Vec::with_capacity(requested);
    let mut first_block_time: Option<Instant> = None;

    loop {
        let (lock, cvar) = &*shared;
        let mut state = lock.lock().expect("shared state lock poisoned");

        while !state.producer_done {
            if let Ok(status) = state.buffer.consumer_status(recorder_id)
                && status.buffered_blocks > 0
            {
                break;
            }
            state = cvar.wait(state).expect("condvar wait failed");
        }

        // Collect Arc pointers inside the lock, then release before cloning.
        let drained = collect_consumer(&mut state.buffer, recorder_id);
        let done = state.producer_done;
        let error = if done {
            state.producer_error.clone()
        } else {
            None
        };
        collect_preview(&mut state.buffer, preview_id);

        // Additional drain when producer is done.
        let drained_final = if done {
            let d = collect_consumer(&mut state.buffer, recorder_id);
            collect_preview(&mut state.buffer, preview_id);
            d
        } else {
            Vec::new()
        };

        // Release the lock before any cloning / heavy work.
        drop(state);

        // Clone block data outside the critical section.
        for block in drained {
            recorded_blocks.push((*block).clone());
        }
        if first_block_time.is_none() && !recorded_blocks.is_empty() {
            first_block_time = Some(Instant::now());
        }
        for block in drained_final {
            recorded_blocks.push((*block).clone());
        }

        if done {
            if let Some(error) = error {
                return Err(PipelineError::ProducerFailed {
                    message: error,
                    blocks_acquired: recorded_blocks.len() as u64,
                });
            }
            break;
        }
    }

    producer_handle
        .join()
        .map_err(|_| PipelineError::ProducerPanicked)?;

    let (lock, _) = &*shared;
    let state = lock.lock().expect("shared state lock poisoned");
    let recorder_status = state.buffer.consumer_status(recorder_id)?;
    let preview_status = state.buffer.consumer_status(preview_id)?;
    drop(state);

    let wall_clock = start.elapsed();
    let integrity = check_blocks(&recorded_blocks)?;

    let timing = PipelineTiming {
        wall_clock_seconds: wall_clock.as_secs_f64(),
        first_block_latency_seconds: first_block_time
            .map(|t| t.duration_since(start).as_secs_f64()),
    };

    Ok(PipelineResult {
        recorded_blocks,
        integrity,
        timing,
        recorder_status,
        preview_status,
    })
}

fn producer_loop<S>(mut source: S, requested: usize, shared: &Arc<(Mutex<SharedState>, Condvar)>)
where
    S: AcquisitionSource,
{
    let (lock, cvar) = &**shared;

    for _ in 0..requested {
        match source.read_block() {
            Ok(block) => {
                let mut state = lock.lock().expect("shared state lock poisoned");
                if let Some(overflow) = state.buffer.push(block) {
                    log::warn!(
                        "buffer overflow: dropped_blocks={}, occupancy={:.1}%",
                        overflow.dropped_blocks,
                        overflow.buffer_occupancy * 100.0
                    );
                }
                cvar.notify_all();
            }
            Err(error) => {
                let mut state = lock.lock().expect("shared state lock poisoned");
                state.producer_error = Some(error.to_string());
                state.producer_done = true;
                cvar.notify_all();
                return;
            }
        }
    }

    let mut state = lock.lock().expect("shared state lock poisoned");
    state.producer_done = true;
    cvar.notify_all();
}

/// Collect Arc pointers from a consumer queue without cloning the data.
/// The caller is responsible for cloning or processing the blocks outside
/// the critical section.
fn collect_consumer(
    buffer: &mut FanoutBlockBuffer,
    consumer_id: kv_buffer::BufferConsumerId,
) -> Vec<Arc<SampleBlock>> {
    let mut collected = Vec::new();
    while let Ok(Some(block)) = buffer.pop(consumer_id) {
        collected.push(block);
    }
    collected
}

fn collect_preview(buffer: &mut FanoutBlockBuffer, consumer_id: kv_buffer::BufferConsumerId) {
    while let Ok(Some(_)) = buffer.pop(consumer_id) {}
}

/// Run a streaming fan-out pipeline that writes blocks to disk as they arrive.
///
/// Like `run_threaded_pipeline`, but the recorder consumer writes each block
/// through a `StreamingRecorder` and checks integrity via
/// `IncrementalIntegrity`, so memory usage stays bounded regardless of
/// acquisition length.
pub fn run_streaming_pipeline<S>(
    config: &StreamingPipelineConfig,
    source: S,
) -> Result<StreamingPipelineResult, PipelineError>
where
    S: AcquisitionSource + Send + 'static,
    S::Error: Send + 'static,
{
    let start = Instant::now();

    let mut fanout = FanoutBlockBuffer::new();
    let recorder_id = fanout.add_consumer("recorder", config.recorder_capacity_blocks)?;
    let preview_id = fanout.add_consumer("preview", config.preview_capacity_blocks)?;

    let shared = Arc::new((
        Mutex::new(SharedState {
            buffer: fanout,
            producer_done: false,
            producer_error: None,
        }),
        Condvar::new(),
    ));

    let requested = config.requested_blocks;
    let shared_producer = Arc::clone(&shared);

    let producer_handle = thread::spawn(move || {
        producer_loop(source, requested, &shared_producer);
    });

    let recording_config = RecordingConfig {
        enabled_channels: config.device.enabled_channels.clone(),
        ttl_line_count: if config.device.ttl_enabled {
            config.device.ttl_line_count
        } else {
            0
        },
    };
    let mut recorder = StreamingRecorder::with_config(&config.output_dir, recording_config)?;
    let mut integrity = IncrementalIntegrity::new();
    let mut first_block_time: Option<Instant> = None;

    loop {
        let (lock, cvar) = &*shared;
        let mut state = lock.lock().expect("shared state lock poisoned");

        while !state.producer_done {
            if let Ok(status) = state.buffer.consumer_status(recorder_id)
                && status.buffered_blocks > 0
            {
                break;
            }
            state = cvar.wait(state).expect("condvar wait failed");
        }

        // Collect Arc pointers inside the lock, then release before I/O.
        let drained = collect_consumer(&mut state.buffer, recorder_id);
        let done = state.producer_done;
        let error = if done {
            state.producer_error.clone()
        } else {
            None
        };
        collect_preview(&mut state.buffer, preview_id);

        let drained_final = if done {
            let d = collect_consumer(&mut state.buffer, recorder_id);
            collect_preview(&mut state.buffer, preview_id);
            d
        } else {
            Vec::new()
        };

        // Release the lock before performing disk I/O.
        drop(state);

        // Write blocks to disk and check integrity outside the critical section.
        for block in &drained {
            integrity.push(block)?;
            recorder.write_block(block)?;
        }
        if first_block_time.is_none() && recorder.block_count() > 0 {
            first_block_time = Some(Instant::now());
        }
        for block in &drained_final {
            integrity.push(block)?;
            recorder.write_block(block)?;
        }

        if done {
            if let Some(error) = error {
                let blocks_acquired = recorder.block_count();
                // Finalize the file (flush + rewrite the header) even on the
                // error path so a partial .kvraw is not left silently truncated.
                let _ = recorder.finish();
                return Err(PipelineError::ProducerFailed {
                    message: error,
                    blocks_acquired,
                });
            }
            break;
        }
    }

    producer_handle
        .join()
        .map_err(|_| PipelineError::ProducerPanicked)?;

    let (lock, _) = &*shared;
    let state = lock.lock().expect("shared state lock poisoned");
    let recorder_status = state.buffer.consumer_status(recorder_id)?;
    let preview_status = state.buffer.consumer_status(preview_id)?;
    drop(state);

    let wall_clock = start.elapsed();
    let streaming_summary = recorder.finish()?;
    let integrity_report = integrity.finish();

    let timing = PipelineTiming {
        wall_clock_seconds: wall_clock.as_secs_f64(),
        first_block_latency_seconds: first_block_time
            .map(|t| t.duration_since(start).as_secs_f64()),
    };

    Ok(StreamingPipelineResult {
        recording: streaming_summary.recording,
        integrity: integrity_report,
        timing,
        recorder_status,
        preview_status,
        max_write_latency_us: streaming_summary.max_write_latency_us,
        latency_distribution: streaming_summary.latency_distribution,
    })
}
