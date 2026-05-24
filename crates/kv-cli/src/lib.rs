//! Developer command helpers for exercising the Keyvast acquisition stack.

use std::{
    fmt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use kv_core::pipeline::{
    PipelineConfig, PipelineError, PipelineResult, PipelineTiming, StreamingPipelineConfig,
    StreamingPipelineResult, run_streaming_pipeline,
};
use kv_core::process_metrics::{ProcessMetrics, ProcessMetricsCollector};
use kv_core::{AcquisitionRunError, AcquisitionRunSummary, run_fixed_blocks};
use kv_integrity::IntegrityReport;
use kv_recorder::{
    BenchmarkSummary, RecorderError, RecordingSummary, write_benchmark_summary, write_events_csv,
    write_integrity_summary, write_log_file, write_recording,
};
use kv_simulator::{SimulatorBackend, SimulatorConfig, SimulatorConfigError, SimulatorError};
use kv_types::{
    AcquisitionEvent, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLE_RATE, DEFAULT_SAMPLES_PER_PACKET,
    DeviceConfig,
};

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

#[derive(Debug)]
pub enum CommandResult {
    Record(SimulatorRecordingResult),
    Pipeline(SimulatorPipelineResult),
    Stream(SimulatorStreamResult),
    Benchmark(BenchmarkResult),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CliCommand {
    SimulatorRecord(SimulatorRecordingOptions),
    SimulatorPipeline(SimulatorPipelineOptions),
    SimulatorStream(SimulatorPipelineOptions),
    Benchmark(BenchmarkOptions),
}

#[derive(Debug)]
pub enum CliError {
    MissingCommand,
    UnknownCommand { command: String },
    MissingArgumentValue { flag: &'static str },
    UnknownArgument { argument: String },
    MissingOutputDir,
    InvalidBlockCount { blocks: usize },
    InvalidNumber { flag: &'static str, value: String },
    InvalidDuration { seconds: f64 },
    UnknownPreset { name: String },
    SystemTimeBeforeUnixEpoch,
    SimulatorConfig(SimulatorConfigError),
    Acquisition(Box<AcquisitionRunError>),
    Recording(RecorderError),
    Pipeline(PipelineError),
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
            Self::Acquisition(error) => write!(formatter, "{error}"),
            Self::Recording(error) => write!(formatter, "{error}"),
            Self::Pipeline(error) => write!(formatter, "pipeline failed: {error}"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SimulatorConfig(error) => Some(error),
            Self::Acquisition(error) => Some(error.as_ref()),
            Self::Recording(error) => Some(error),
            Self::Pipeline(error) => Some(error),
            Self::MissingCommand
            | Self::UnknownCommand { .. }
            | Self::MissingArgumentValue { .. }
            | Self::UnknownArgument { .. }
            | Self::MissingOutputDir
            | Self::InvalidBlockCount { .. }
            | Self::InvalidNumber { .. }
            | Self::InvalidDuration { .. }
            | Self::UnknownPreset { .. }
            | Self::SystemTimeBeforeUnixEpoch => None,
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

pub fn run_command(command: CliCommand) -> Result<CommandResult, CliError> {
    match command {
        CliCommand::SimulatorRecord(options) => {
            run_simulator_recording(options).map(CommandResult::Record)
        }
        CliCommand::SimulatorPipeline(options) => {
            run_simulator_pipeline(options).map(CommandResult::Pipeline)
        }
        CliCommand::SimulatorStream(options) => {
            run_simulator_stream(options).map(CommandResult::Stream)
        }
        CliCommand::Benchmark(options) => run_benchmark(options).map(CommandResult::Benchmark),
    }
}

pub fn run_simulator_recording(
    options: SimulatorRecordingOptions,
) -> Result<SimulatorRecordingResult, CliError> {
    if options.blocks == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let simulator_config = SimulatorConfig {
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let device_config = simulator_config.device.clone();
    let mut simulator = SimulatorBackend::new(simulator_config)?;

    let acquisition = run_fixed_blocks(&device_config, options.blocks, &mut || {
        simulator.next_block().map_err(SimulatorReadError)
    })?;
    let recording = write_recording(&options.output_dir, &acquisition.blocks)?;
    write_integrity_summary(&options.output_dir, &acquisition.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&acquisition.integrity),
    )?;
    let events = simulator_recording_events(&acquisition.integrity);
    write_events_csv(&options.output_dir, &events)?;
    let benchmark =
        simulator_benchmark_summary(&acquisition.summary, &recording, &acquisition.integrity);
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(SimulatorRecordingResult {
        acquisition: acquisition.summary,
        recording,
        integrity: acquisition.integrity,
    })
}

pub fn run_simulator_pipeline(
    options: SimulatorPipelineOptions,
) -> Result<SimulatorPipelineResult, CliError> {
    use kv_core::pipeline::run_threaded_pipeline;

    if options.blocks == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let simulator_config = SimulatorConfig {
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let device_config = simulator_config.device.clone();
    let simulator = SimulatorBackend::new(simulator_config)?;

    let pipeline_config = PipelineConfig {
        device: device_config,
        requested_blocks: options.blocks,
        recorder_capacity_blocks: options.recorder_capacity_blocks,
        preview_capacity_blocks: options.preview_capacity_blocks,
    };

    let source = {
        let mut sim = simulator;
        move || sim.next_block().map_err(|e| e.to_string())
    };

    let pipeline_result = run_threaded_pipeline(&pipeline_config, source)?;

    let recording = write_recording(&options.output_dir, &pipeline_result.recorded_blocks)?;
    write_integrity_summary(&options.output_dir, &pipeline_result.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&pipeline_result.integrity),
    )?;
    let events = simulator_recording_events(&pipeline_result.integrity);
    write_events_csv(&options.output_dir, &events)?;

    let benchmark = pipeline_benchmark_summary(&pipeline_result, &recording);
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(SimulatorPipelineResult {
        recording,
        integrity: pipeline_result.integrity,
        timing: pipeline_result.timing,
        recorder_dropped_blocks: pipeline_result.recorder_status.dropped_blocks,
        preview_dropped_blocks: pipeline_result.preview_status.dropped_blocks,
    })
}

pub fn run_simulator_stream(
    options: SimulatorPipelineOptions,
) -> Result<SimulatorStreamResult, CliError> {
    if options.blocks == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let simulator_config = SimulatorConfig {
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let device_config = simulator_config.device.clone();
    let simulator = SimulatorBackend::new(simulator_config)?;

    let streaming_config = StreamingPipelineConfig {
        device: device_config,
        requested_blocks: options.blocks,
        output_dir: options.output_dir.clone(),
        recorder_capacity_blocks: options.recorder_capacity_blocks,
        preview_capacity_blocks: options.preview_capacity_blocks,
    };

    let source = {
        let mut sim = simulator;
        move || sim.next_block().map_err(|e| e.to_string())
    };

    let result = run_streaming_pipeline(&streaming_config, source)?;

    write_integrity_summary(&options.output_dir, &result.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&result.integrity),
    )?;
    let events = simulator_recording_events(&result.integrity);
    write_events_csv(&options.output_dir, &events)?;

    let benchmark = streaming_benchmark_summary(&result, &streaming_config.device, None);
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(SimulatorStreamResult {
        recording: result.recording,
        integrity: result.integrity,
        timing: result.timing,
        recorder_dropped_blocks: result.recorder_status.dropped_blocks,
        preview_dropped_blocks: result.preview_status.dropped_blocks,
        max_write_latency_us: result.max_write_latency_us,
    })
}

/// Computes the number of blocks needed to cover `duration_seconds` given
/// `sample_rate` and `samples_per_packet`.
pub fn blocks_for_duration(
    duration_seconds: f64,
    sample_rate: f64,
    samples_per_packet: usize,
) -> usize {
    if samples_per_packet == 0 || sample_rate <= 0.0 || duration_seconds <= 0.0 {
        return 0;
    }
    let seconds_per_block = samples_per_packet as f64 / sample_rate;
    (duration_seconds / seconds_per_block).ceil() as usize
}

pub fn run_benchmark(options: BenchmarkOptions) -> Result<BenchmarkResult, CliError> {
    if !options.duration_seconds.is_finite() || options.duration_seconds <= 0.0 {
        return Err(CliError::InvalidDuration {
            seconds: options.duration_seconds,
        });
    }

    let block_count = blocks_for_duration(
        options.duration_seconds,
        options.sample_rate,
        options.samples_per_packet,
    );
    if block_count == 0 {
        return Err(CliError::InvalidBlockCount { blocks: 0 });
    }

    let mut device = DeviceConfig::simulator_default();
    device.channel_count = options.channel_count;
    device.enabled_channels = (0..options.channel_count).collect();
    device.sample_rate = options.sample_rate;
    device.samples_per_packet = options.samples_per_packet;

    let simulator_config = SimulatorConfig {
        device: device.clone(),
        drop_packet_ids: options.drop_packet_ids,
        ..SimulatorConfig::default()
    };
    let simulator = SimulatorBackend::new(simulator_config)?;

    let streaming_config = StreamingPipelineConfig {
        device: device.clone(),
        requested_blocks: block_count,
        output_dir: options.output_dir.clone(),
        recorder_capacity_blocks: options.recorder_capacity_blocks,
        preview_capacity_blocks: options.preview_capacity_blocks,
    };

    let source = {
        let mut sim = simulator;
        move || sim.next_block().map_err(|e| e.to_string())
    };

    let metrics_collector = ProcessMetricsCollector::start();
    let result = run_streaming_pipeline(&streaming_config, source)?;
    let process_metrics = metrics_collector.finish(result.timing.wall_clock_seconds);

    write_integrity_summary(&options.output_dir, &result.integrity.summary)?;
    write_log_file(
        &options.output_dir,
        &simulator_recording_log_lines(&result.integrity),
    )?;
    let events = simulator_recording_events(&result.integrity);
    write_events_csv(&options.output_dir, &events)?;

    let benchmark = streaming_benchmark_summary(&result, &device, process_metrics.as_ref());
    write_benchmark_summary(&options.output_dir, &benchmark)?;

    Ok(BenchmarkResult {
        recording: result.recording,
        integrity: result.integrity,
        timing: result.timing,
        recorder_dropped_blocks: result.recorder_status.dropped_blocks,
        preview_dropped_blocks: result.preview_status.dropped_blocks,
        max_write_latency_us: result.max_write_latency_us,
        requested_duration_seconds: options.duration_seconds,
        computed_block_count: block_count,
        process_metrics,
    })
}

fn streaming_benchmark_summary(
    result: &StreamingPipelineResult,
    device: &kv_types::DeviceConfig,
    process_metrics: Option<&ProcessMetrics>,
) -> BenchmarkSummary {
    BenchmarkSummary {
        measurement_kind: "measured_streaming".to_string(),
        duration_seconds: result.timing.wall_clock_seconds,
        channel_count: device.channel_count,
        sample_rate: device.sample_rate,
        expected_samples: result.integrity.summary.expected_samples,
        written_samples: result.integrity.summary.written_samples,
        missing_packets: result.integrity.summary.missing_packets,
        crc_errors: result.integrity.summary.crc_errors,
        timestamp_discontinuities: result.integrity.summary.timestamp_discontinuities,
        byte_count: result.recording.byte_count,
        average_write_mb_s: average_write_mb_s(
            result.recording.byte_count,
            result.timing.wall_clock_seconds,
        ),
        max_write_latency_ms: result.max_write_latency_us.map(|us| us as f64 / 1_000.0),
        p50_write_latency_ms: result
            .latency_distribution
            .as_ref()
            .map(|d| d.p50_us as f64 / 1_000.0),
        p95_write_latency_ms: result
            .latency_distribution
            .as_ref()
            .map(|d| d.p95_us as f64 / 1_000.0),
        p99_write_latency_ms: result
            .latency_distribution
            .as_ref()
            .map(|d| d.p99_us as f64 / 1_000.0),
        max_buffer_occupancy: Some(
            result
                .recorder_status
                .occupancy
                .max(result.preview_status.occupancy),
        ),
        cpu_percent_avg: process_metrics.map(|m| m.cpu_percent_avg),
        memory_mb_max: process_metrics.map(|m| m.memory_mb_max),
    }
}

fn pipeline_benchmark_summary(
    pipeline: &PipelineResult,
    recording: &RecordingSummary,
) -> BenchmarkSummary {
    let first_block = pipeline.recorded_blocks.first();
    let channel_count = first_block.map_or(0, |b| b.channel_count);
    let sample_rate = first_block.map_or(0.0, |b| b.sample_rate);

    BenchmarkSummary {
        measurement_kind: "measured".to_string(),
        duration_seconds: pipeline.timing.wall_clock_seconds,
        channel_count,
        sample_rate,
        expected_samples: pipeline.integrity.summary.expected_samples,
        written_samples: pipeline.integrity.summary.written_samples,
        missing_packets: pipeline.integrity.summary.missing_packets,
        crc_errors: pipeline.integrity.summary.crc_errors,
        timestamp_discontinuities: pipeline.integrity.summary.timestamp_discontinuities,
        byte_count: recording.byte_count,
        average_write_mb_s: average_write_mb_s(
            recording.byte_count,
            pipeline.timing.wall_clock_seconds,
        ),
        max_write_latency_ms: None,
        p50_write_latency_ms: None,
        p95_write_latency_ms: None,
        p99_write_latency_ms: None,
        max_buffer_occupancy: Some(
            pipeline
                .recorder_status
                .occupancy
                .max(pipeline.preview_status.occupancy),
        ),
        cpu_percent_avg: None,
        memory_mb_max: None,
    }
}

fn simulator_recording_log_lines(integrity: &IntegrityReport) -> Vec<String> {
    let mut lines = vec![
        "[INFO] acquisition started".to_string(),
        format!(
            "[INFO] acquired_blocks={}",
            integrity.summary.observed_packets
        ),
    ];

    for gap in &integrity.packet_gaps {
        lines.push(format!(
            "[WARN] missing packet expected={} observed={} missing={}",
            gap.expected_packet_id, gap.observed_packet_id, gap.missing_count
        ));
    }

    for discontinuity in &integrity.timestamp_discontinuities {
        lines.push(format!(
            "[WARN] timestamp discontinuity packet={} expected={} observed={}",
            discontinuity.packet_id,
            discontinuity.expected_timestamp_start,
            discontinuity.observed_timestamp_start
        ));
    }

    lines.push("[INFO] recorder flushed".to_string());
    lines.push("[INFO] acquisition stopped cleanly".to_string());
    lines
}

fn simulator_recording_events(integrity: &IntegrityReport) -> Vec<AcquisitionEvent> {
    let mut events = vec![AcquisitionEvent::Started {
        timestamp_host_ms: 0,
    }];

    for gap in &integrity.packet_gaps {
        events.push(AcquisitionEvent::PacketMissing {
            expected_packet_id: gap.expected_packet_id,
            observed_packet_id: gap.observed_packet_id,
            missing_count: gap.missing_count,
        });
    }

    events.push(AcquisitionEvent::Stopped {
        timestamp_host_ms: 0,
    });
    events
}

fn simulator_benchmark_summary(
    acquisition: &AcquisitionRunSummary,
    recording: &RecordingSummary,
    integrity: &IntegrityReport,
) -> BenchmarkSummary {
    let duration_seconds = recorded_duration_seconds(
        integrity.summary.written_samples,
        acquisition.status.channel_count,
        acquisition.status.sample_rate,
    );

    BenchmarkSummary {
        measurement_kind: "simulator_estimate".to_string(),
        duration_seconds,
        channel_count: acquisition.status.channel_count,
        sample_rate: acquisition.status.sample_rate,
        expected_samples: integrity.summary.expected_samples,
        written_samples: integrity.summary.written_samples,
        missing_packets: integrity.summary.missing_packets,
        crc_errors: integrity.summary.crc_errors,
        timestamp_discontinuities: integrity.summary.timestamp_discontinuities,
        byte_count: recording.byte_count,
        average_write_mb_s: average_write_mb_s(recording.byte_count, duration_seconds),
        max_write_latency_ms: None,
        p50_write_latency_ms: None,
        p95_write_latency_ms: None,
        p99_write_latency_ms: None,
        max_buffer_occupancy: None,
        cpu_percent_avg: None,
        memory_mb_max: None,
    }
}

fn recorded_duration_seconds(written_samples: u64, channel_count: usize, sample_rate: f64) -> f64 {
    if channel_count == 0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 0.0;
    }

    written_samples as f64 / channel_count as f64 / sample_rate
}

fn average_write_mb_s(byte_count: u64, duration_seconds: f64) -> f64 {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return 0.0;
    }

    byte_count as f64 / duration_seconds / 1_000_000.0
}

pub fn parse_args<I, S>(args: I) -> Result<CliCommand, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let Some(command) = args.next() else {
        return Err(CliError::MissingCommand);
    };

    match command.as_str() {
        "simulator-record" => parse_simulator_record_args(args),
        "simulator-pipeline" => parse_simulator_pipeline_args(args),
        "simulator-stream" => parse_simulator_stream_args(args),
        "benchmark" => parse_benchmark_args(args),
        _ => Err(CliError::UnknownCommand { command }),
    }
}

fn parse_simulator_record_args(args: impl Iterator<Item = String>) -> Result<CliCommand, CliError> {
    let mut blocks = 1_usize;
    let mut output_dir: Option<PathBuf> = None;
    let mut drop_packet_ids = Vec::new();
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--blocks" => {
                let value = next_value(&mut args, "--blocks")?;
                blocks = parse_usize("--blocks", &value)?;
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::SimulatorRecord(SimulatorRecordingOptions {
        output_dir,
        blocks,
        drop_packet_ids,
    }))
}

const DEFAULT_RECORDER_CAPACITY: usize = 2048;
const DEFAULT_PREVIEW_CAPACITY: usize = 32;

fn parse_simulator_pipeline_args(
    args: impl Iterator<Item = String>,
) -> Result<CliCommand, CliError> {
    let mut blocks = 1_usize;
    let mut output_dir: Option<PathBuf> = None;
    let mut drop_packet_ids = Vec::new();
    let mut recorder_capacity = DEFAULT_RECORDER_CAPACITY;
    let mut preview_capacity = DEFAULT_PREVIEW_CAPACITY;
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--blocks" => {
                let value = next_value(&mut args, "--blocks")?;
                blocks = parse_usize("--blocks", &value)?;
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            "--recorder-capacity" => {
                let value = next_value(&mut args, "--recorder-capacity")?;
                recorder_capacity = parse_usize("--recorder-capacity", &value)?;
            }
            "--preview-capacity" => {
                let value = next_value(&mut args, "--preview-capacity")?;
                preview_capacity = parse_usize("--preview-capacity", &value)?;
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::SimulatorPipeline(SimulatorPipelineOptions {
        output_dir,
        blocks,
        drop_packet_ids,
        recorder_capacity_blocks: recorder_capacity,
        preview_capacity_blocks: preview_capacity,
    }))
}

fn parse_simulator_stream_args(args: impl Iterator<Item = String>) -> Result<CliCommand, CliError> {
    let mut blocks = 1_usize;
    let mut output_dir: Option<PathBuf> = None;
    let mut drop_packet_ids = Vec::new();
    let mut recorder_capacity = DEFAULT_RECORDER_CAPACITY;
    let mut preview_capacity = DEFAULT_PREVIEW_CAPACITY;
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--blocks" => {
                let value = next_value(&mut args, "--blocks")?;
                blocks = parse_usize("--blocks", &value)?;
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            "--recorder-capacity" => {
                let value = next_value(&mut args, "--recorder-capacity")?;
                recorder_capacity = parse_usize("--recorder-capacity", &value)?;
            }
            "--preview-capacity" => {
                let value = next_value(&mut args, "--preview-capacity")?;
                preview_capacity = parse_usize("--preview-capacity", &value)?;
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::SimulatorStream(SimulatorPipelineOptions {
        output_dir,
        blocks,
        drop_packet_ids,
        recorder_capacity_blocks: recorder_capacity,
        preview_capacity_blocks: preview_capacity,
    }))
}

fn parse_benchmark_args(args: impl Iterator<Item = String>) -> Result<CliCommand, CliError> {
    let mut output_dir: Option<PathBuf> = None;
    let mut duration: Option<f64> = None;
    let mut channel_count: Option<usize> = None;
    let mut sample_rate: Option<f64> = None;
    let mut samples_per_packet: Option<usize> = None;
    let mut preset: Option<BenchmarkPreset> = None;
    let mut drop_packet_ids = Vec::new();
    let mut recorder_capacity = DEFAULT_RECORDER_CAPACITY;
    let mut preview_capacity = DEFAULT_PREVIEW_CAPACITY;
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--preset" => {
                let value = next_value(&mut args, "--preset")?;
                preset = Some(parse_benchmark_preset(&value)?);
            }
            "--duration" => {
                let value = next_value(&mut args, "--duration")?;
                duration = Some(parse_f64("--duration", &value)?);
            }
            "--channels" => {
                let value = next_value(&mut args, "--channels")?;
                channel_count = Some(parse_usize("--channels", &value)?);
            }
            "--sample-rate" => {
                let value = next_value(&mut args, "--sample-rate")?;
                sample_rate = Some(parse_f64("--sample-rate", &value)?);
            }
            "--samples-per-packet" => {
                let value = next_value(&mut args, "--samples-per-packet")?;
                samples_per_packet = Some(parse_usize("--samples-per-packet", &value)?);
            }
            "--output" => {
                let value = next_value(&mut args, "--output")?;
                output_dir = Some(PathBuf::from(value));
            }
            "--drop-packet" => {
                let value = next_value(&mut args, "--drop-packet")?;
                drop_packet_ids.push(parse_u64("--drop-packet", &value)?);
            }
            "--recorder-capacity" => {
                let value = next_value(&mut args, "--recorder-capacity")?;
                recorder_capacity = parse_usize("--recorder-capacity", &value)?;
            }
            "--preview-capacity" => {
                let value = next_value(&mut args, "--preview-capacity")?;
                preview_capacity = parse_usize("--preview-capacity", &value)?;
            }
            _ => return Err(CliError::UnknownArgument { argument }),
        }
    }

    let duration_seconds = match (&preset, duration) {
        (Some(p), None) => p.duration_seconds(),
        (None, Some(d)) => d,
        (Some(p), Some(_)) => p.duration_seconds(),
        (None, None) => 10.0,
    };

    let channel_count = match (&preset, channel_count) {
        (_, Some(c)) => c,
        (Some(p), None) => p.channel_count().unwrap_or(DEFAULT_CHANNEL_COUNT),
        (None, None) => DEFAULT_CHANNEL_COUNT,
    };

    let output_dir = match output_dir {
        Some(output_dir) => output_dir,
        None => default_recording_output_dir()?,
    };

    Ok(CliCommand::Benchmark(BenchmarkOptions {
        output_dir,
        duration_seconds,
        channel_count,
        sample_rate: sample_rate.unwrap_or(DEFAULT_SAMPLE_RATE),
        samples_per_packet: samples_per_packet.unwrap_or(DEFAULT_SAMPLES_PER_PACKET),
        recorder_capacity_blocks: recorder_capacity,
        preview_capacity_blocks: preview_capacity,
        drop_packet_ids,
    }))
}

fn parse_benchmark_preset(name: &str) -> Result<BenchmarkPreset, CliError> {
    match name {
        "smoke" => Ok(BenchmarkPreset::Smoke),
        "recorder" => Ok(BenchmarkPreset::Recorder),
        "stress-128" => Ok(BenchmarkPreset::Stress128),
        "stress-256" => Ok(BenchmarkPreset::Stress256),
        "endurance" => Ok(BenchmarkPreset::Endurance),
        _ => Err(CliError::UnknownPreset {
            name: name.to_string(),
        }),
    }
}

fn default_recording_output_dir() -> Result<PathBuf, CliError> {
    Ok(PathBuf::from(run_directory_name_utc(SystemTime::now())?))
}

pub fn run_directory_name_utc(timestamp: SystemTime) -> Result<String, CliError> {
    let duration = timestamp
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::SystemTimeBeforeUnixEpoch)?;
    let total_seconds = duration.as_secs();
    let days = total_seconds / 86_400;
    let seconds_in_day = total_seconds % 86_400;
    let (year, month, day) = civil_date_from_unix_days(days as i64);
    let hour = seconds_in_day / 3_600;
    let minute = (seconds_in_day % 3_600) / 60;
    let second = seconds_in_day % 60;

    Ok(format!(
        "run-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}"
    ))
}

fn civil_date_from_unix_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };

    (year as i32, month as u32, day as u32)
}

fn next_value(
    args: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> Result<String, CliError> {
    args.next().ok_or(CliError::MissingArgumentValue { flag })
}

fn parse_usize(flag: &'static str, value: &str) -> Result<usize, CliError> {
    value.parse().map_err(|_| CliError::InvalidNumber {
        flag,
        value: value.to_string(),
    })
}

fn parse_f64(flag: &'static str, value: &str) -> Result<f64, CliError> {
    value.parse().map_err(|_| CliError::InvalidNumber {
        flag,
        value: value.to_string(),
    })
}

fn parse_u64(flag: &'static str, value: &str) -> Result<u64, CliError> {
    value.parse().map_err(|_| CliError::InvalidNumber {
        flag,
        value: value.to_string(),
    })
}

#[derive(Debug)]
struct SimulatorReadError(SimulatorError);

impl fmt::Display for SimulatorReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}
