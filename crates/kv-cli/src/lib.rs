//! Developer command helpers for exercising the Keyvast acquisition stack.

use std::{
    fmt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use kv_core::{AcquisitionRunError, AcquisitionRunSummary, run_fixed_blocks};
use kv_integrity::IntegrityReport;
use kv_recorder::{
    BenchmarkSummary, RecorderError, RecordingSummary, write_benchmark_summary, write_events_csv,
    write_integrity_summary, write_log_file, write_recording,
};
use kv_simulator::{SimulatorBackend, SimulatorConfig, SimulatorConfigError, SimulatorError};
use kv_types::AcquisitionEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatorRecordingOptions {
    pub output_dir: PathBuf,
    pub blocks: usize,
    pub drop_packet_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimulatorRecordingResult {
    pub acquisition: AcquisitionRunSummary,
    pub recording: RecordingSummary,
    pub integrity: IntegrityReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    SimulatorRecord(SimulatorRecordingOptions),
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
    SystemTimeBeforeUnixEpoch,
    SimulatorConfig(SimulatorConfigError),
    Acquisition(Box<AcquisitionRunError>),
    Recording(RecorderError),
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
            Self::SystemTimeBeforeUnixEpoch => {
                write!(formatter, "system time is before the unix epoch")
            }
            Self::SimulatorConfig(error) => {
                write!(formatter, "simulator config is invalid: {error}")
            }
            Self::Acquisition(error) => write!(formatter, "{error}"),
            Self::Recording(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SimulatorConfig(error) => Some(error),
            Self::Acquisition(error) => Some(error.as_ref()),
            Self::Recording(error) => Some(error),
            Self::MissingCommand
            | Self::UnknownCommand { .. }
            | Self::MissingArgumentValue { .. }
            | Self::UnknownArgument { .. }
            | Self::MissingOutputDir
            | Self::InvalidBlockCount { .. }
            | Self::InvalidNumber { .. }
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

pub fn run_command(command: CliCommand) -> Result<SimulatorRecordingResult, CliError> {
    match command {
        CliCommand::SimulatorRecord(options) => run_simulator_recording(options),
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
