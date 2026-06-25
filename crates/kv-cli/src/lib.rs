//! Developer command helpers for exercising the Keyvast acquisition stack.

use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) use std::fmt;
pub(crate) use std::fs;
pub(crate) use std::path::PathBuf;
pub(crate) use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) use kv_core::pipeline::{
    PipelineConfig, PipelineError, PipelineResult, PipelineTiming, StreamingPipelineConfig,
    StreamingPipelineResult, run_streaming_pipeline,
};
pub(crate) use kv_core::process_metrics::{ProcessMetrics, ProcessMetricsCollector};
pub(crate) use kv_core::{AcquisitionRunError, AcquisitionRunSummary, run_fixed_blocks};
pub(crate) use kv_integrity::IntegrityReport;
pub(crate) use kv_recorder::{
    BenchmarkSummary, RecorderError, RecordingSummary, write_benchmark_summary, write_events_csv,
    write_integrity_summary, write_log_file, write_recording, write_recording_with_backend,
};
pub(crate) use kv_rhd::{
    DEFAULT_CABLE_LENGTH_METERS, DEFAULT_RHD_DEVICE_ID, DEFAULT_RHD_SAMPLE_RATE,
    RhdHardwareBackend, RhdHardwareOptions, RhdReadError, RhythmDataConfig, SAMPLES_PER_USB_BLOCK,
    bytes_per_block, parse_rhythm_data_block,
};
pub(crate) use kv_simulator::{SimulatorBackend, SimulatorConfig, SimulatorConfigError};
pub(crate) use kv_types::{
    AcquisitionEvent, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLE_RATE, DEFAULT_SAMPLES_PER_PACKET,
    DeviceConfig,
};

mod args;
mod commands;
mod report;

pub use args::{parse_args, run_directory_name_utc};
pub use commands::{
    blocks_for_duration, run_benchmark, run_command, run_rhd_smoke, run_simulator_pipeline,
    run_simulator_recording, run_simulator_stream,
};

/// Global cancellation flag set by the Ctrl-C handler.
static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Install a Ctrl-C handler that sets the global cancellation flag.
/// Call once at program start; subsequent Ctrl-C presses are no-ops.
pub fn install_ctrlc_handler() {
    ctrlc::set_handler(move || {
        if CANCELLED.swap(true, Ordering::SeqCst) {
            // Second Ctrl-C: force exit immediately.
            std::process::exit(130);
        }
        eprintln!("\nCtrl-C received — stopping acquisition gracefully…");
    })
    .expect("failed to install Ctrl-C handler");
}

/// Returns `true` if Ctrl-C has been received.
pub fn is_cancelled() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkPreset {
    Smoke,
    Recorder,
    Stress128,
    Stress256,
    Endurance,
}

impl BenchmarkPreset {
    fn duration_seconds(self) -> f64 {
        match self {
            Self::Smoke => 10.0,
            Self::Recorder => 600.0,
            Self::Stress128 => 600.0,
            Self::Stress256 => 600.0,
            Self::Endurance => 7200.0,
        }
    }

    fn channel_count(self) -> Option<usize> {
        match self {
            Self::Stress128 => Some(128),
            Self::Stress256 => Some(256),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkOptions {
    pub output_dir: PathBuf,
    pub duration_seconds: f64,
    pub channel_count: usize,
    pub sample_rate: f64,
    pub samples_per_packet: usize,
    pub recorder_capacity_blocks: usize,
    pub preview_capacity_blocks: usize,
    pub drop_packet_ids: Vec<u64>,
}

#[derive(Debug)]
pub struct BenchmarkResult {
    pub recording: RecordingSummary,
    pub integrity: IntegrityReport,
    pub timing: PipelineTiming,
    pub recorder_dropped_blocks: u64,
    pub preview_dropped_blocks: u64,
    pub max_write_latency_us: Option<u64>,
    pub requested_duration_seconds: f64,
    pub computed_block_count: usize,
    pub process_metrics: Option<ProcessMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatorRecordingOptions {
    pub output_dir: PathBuf,
    pub blocks: usize,
    pub drop_packet_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatorPipelineOptions {
    pub output_dir: PathBuf,
    pub blocks: usize,
    pub drop_packet_ids: Vec<u64>,
    pub recorder_capacity_blocks: usize,
    pub preview_capacity_blocks: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimulatorRecordingResult {
    pub acquisition: AcquisitionRunSummary,
    pub recording: RecordingSummary,
    pub integrity: IntegrityReport,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimulatorPipelineResult {
    pub recording: RecordingSummary,
    pub integrity: IntegrityReport,
    pub timing: PipelineTiming,
    pub recorder_dropped_blocks: u64,
    pub preview_dropped_blocks: u64,
}

#[derive(Debug)]
pub struct SimulatorStreamResult {
    pub recording: RecordingSummary,
    pub integrity: IntegrityReport,
    pub timing: PipelineTiming,
    pub recorder_dropped_blocks: u64,
    pub preview_dropped_blocks: u64,
    pub max_write_latency_us: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RhdSmokeOptions {
    pub output_dir: PathBuf,
    pub blocks: usize,
    pub enabled_streams: usize,
    pub raw_input: Option<PathBuf>,
    pub bitfile_path: PathBuf,
    pub frontpanel_dll_path: Option<PathBuf>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RhdSmokeResult {
    pub acquisition: AcquisitionRunSummary,
    pub recording: RecordingSummary,
    pub integrity: IntegrityReport,
    pub hardware: bool,
}

#[derive(Debug)]
pub enum CommandResult {
    Record(SimulatorRecordingResult),
    Pipeline(SimulatorPipelineResult),
    Stream(SimulatorStreamResult),
    Benchmark(BenchmarkResult),
    RhdSmoke(RhdSmokeResult),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CliCommand {
    SimulatorRecord(SimulatorRecordingOptions),
    SimulatorPipeline(SimulatorPipelineOptions),
    SimulatorStream(SimulatorPipelineOptions),
    Benchmark(BenchmarkOptions),
    RhdSmoke(RhdSmokeOptions),
}

#[derive(Debug)]
pub enum CliError {
    MissingCommand,
    UnknownCommand {
        command: String,
    },
    MissingArgumentValue {
        flag: &'static str,
    },
    UnknownArgument {
        argument: String,
    },
    MissingOutputDir,
    InvalidBlockCount {
        blocks: usize,
    },
    InvalidNumber {
        flag: &'static str,
        value: String,
    },
    InvalidDuration {
        seconds: f64,
    },
    UnknownPreset {
        name: String,
    },
    SystemTimeBeforeUnixEpoch,
    SimulatorConfig(SimulatorConfigError),
    Rhd(RhdReadError),
    Acquisition(Box<AcquisitionRunError>),
    Recording(RecorderError),
    Pipeline(PipelineError),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    RawInputTooShort {
        path: PathBuf,
        expected_bytes: usize,
        observed_bytes: usize,
    },
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCommand => write!(formatter, "missing command"),
            Self::UnknownCommand { command } => write!(formatter, "unknown command {command}"),
            Self::MissingArgumentValue { flag } => {
                write!(formatter, "missing value for argument {flag}")
            }
            Self::UnknownArgument { argument } => write!(formatter, "unknown argument {argument}"),
            Self::MissingOutputDir => write!(formatter, "missing required --output directory"),
            Self::InvalidBlockCount { blocks } => {
                write!(
                    formatter,
                    "block count must be greater than zero, got {blocks}"
                )
            }
            Self::InvalidNumber { flag, value } => {
                write!(formatter, "invalid numeric value for {flag}: {value}")
            }
            Self::InvalidDuration { seconds } => {
                write!(
                    formatter,
                    "duration must be a positive finite number, got {seconds}"
                )
            }
            Self::UnknownPreset { name } => {
                write!(
                    formatter,
                    "unknown benchmark preset '{name}'; valid presets: smoke, recorder, stress-128, stress-256, endurance"
                )
            }
            Self::SystemTimeBeforeUnixEpoch => {
                write!(formatter, "system time is before the unix epoch")
            }
            Self::SimulatorConfig(error) => {
                write!(formatter, "simulator config is invalid: {error}")
            }
            Self::Rhd(error) => write!(formatter, "RHD hardware read failed: {error}"),
            Self::Acquisition(error) => write!(formatter, "{error}"),
            Self::Recording(error) => write!(formatter, "{error}"),
            Self::Pipeline(error) => write!(formatter, "pipeline failed: {error}"),
            Self::Io { path, source } => {
                write!(formatter, "I/O error at {}: {source}", path.display())
            }
            Self::RawInputTooShort {
                path,
                expected_bytes,
                observed_bytes,
            } => write!(
                formatter,
                "raw RHD input {} is too short: expected at least {expected_bytes} bytes, observed {observed_bytes}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SimulatorConfig(error) => Some(error),
            Self::Rhd(error) => Some(error),
            Self::Acquisition(error) => Some(error.as_ref()),
            Self::Recording(error) => Some(error),
            Self::Pipeline(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::MissingCommand
            | Self::UnknownCommand { .. }
            | Self::MissingArgumentValue { .. }
            | Self::UnknownArgument { .. }
            | Self::MissingOutputDir
            | Self::InvalidBlockCount { .. }
            | Self::InvalidNumber { .. }
            | Self::InvalidDuration { .. }
            | Self::UnknownPreset { .. }
            | Self::SystemTimeBeforeUnixEpoch
            | Self::RawInputTooShort { .. } => None,
        }
    }
}

impl From<RecorderError> for CliError {
    fn from(error: RecorderError) -> Self {
        Self::Recording(error)
    }
}

impl From<AcquisitionRunError> for CliError {
    fn from(error: AcquisitionRunError) -> Self {
        Self::Acquisition(Box::new(error))
    }
}

impl From<SimulatorConfigError> for CliError {
    fn from(error: SimulatorConfigError) -> Self {
        Self::SimulatorConfig(error)
    }
}

impl From<PipelineError> for CliError {
    fn from(error: PipelineError) -> Self {
        Self::Pipeline(error)
    }
}

impl From<RhdReadError> for CliError {
    fn from(error: RhdReadError) -> Self {
        Self::Rhd(error)
    }
}
