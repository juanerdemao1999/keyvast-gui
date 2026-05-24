//! Raw recorder for Keyvast sample blocks.

use std::{
    fmt,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use kv_types::{AcquisitionEvent, IntegritySummary, SampleBlock, SampleBlockError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSummary {
    pub output_dir: PathBuf,
    pub raw_path: PathBuf,
    pub metadata_path: PathBuf,
    pub block_count: u64,
    pub written_samples: u64,
    pub byte_count: u64,
    pub first_packet_id: Option<u64>,
    pub last_packet_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkSummary {
    pub measurement_kind: String,
    pub duration_seconds: f64,
    pub channel_count: usize,
    pub sample_rate: f64,
    pub expected_samples: u64,
    pub written_samples: u64,
    pub missing_packets: u64,
    pub crc_errors: u64,
    pub timestamp_discontinuities: u64,
    pub byte_count: u64,
    pub average_write_mb_s: f64,
    pub max_write_latency_ms: Option<f64>,
    pub max_buffer_occupancy: Option<f64>,
    pub cpu_percent_avg: Option<f64>,
    pub memory_mb_max: Option<f64>,
}

#[derive(Debug)]
pub enum RecorderError {
    InvalidBlock {
        packet_id: u64,
        source: SampleBlockError,
    },
    InconsistentBlockConfig {
        packet_id: u64,
        field: &'static str,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for RecorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBlock { packet_id, source } => {
                write!(
                    formatter,
                    "packet {packet_id} has an invalid sample block: {source}"
                )
            }
            Self::InconsistentBlockConfig { packet_id, field } => {
                write!(
                    formatter,
                    "packet {packet_id} has inconsistent recording field {field}"
                )
            }
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "recorder I/O failed for {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for RecorderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidBlock { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::InconsistentBlockConfig { .. } => None,
        }
    }
}

pub fn write_recording(
    output_dir: impl AsRef<Path>,
    blocks: &[SampleBlock],
) -> Result<RecordingSummary, RecorderError> {
    validate_blocks(blocks)?;

    let output_dir = output_dir.as_ref().to_path_buf();
    fs::create_dir_all(&output_dir).map_err(|source| RecorderError::Io {
        path: output_dir.clone(),
        source,
    })?;

    let raw_path = output_dir.join("recording.kvraw");
    let metadata_path = output_dir.join("recording.json");

    write_raw_file(&raw_path, blocks)?;
    write_metadata_file(&metadata_path, blocks)?;

    Ok(RecordingSummary {
        output_dir,
        raw_path,
        metadata_path,
        block_count: blocks.len() as u64,
        written_samples: written_samples(blocks),
        byte_count: written_samples(blocks).saturating_mul(2),
        first_packet_id: blocks.first().map(|block| block.packet_id),
        last_packet_id: blocks.last().map(|block| block.packet_id),
    })
}

pub fn write_integrity_summary(
    output_dir: impl AsRef<Path>,
    summary: &IntegritySummary,
) -> Result<PathBuf, RecorderError> {
    let output_dir = output_dir.as_ref().to_path_buf();
    fs::create_dir_all(&output_dir).map_err(|source| RecorderError::Io {
        path: output_dir.clone(),
        source,
    })?;

    let integrity_path = output_dir.join("integrity.json");
    fs::write(&integrity_path, integrity_summary_json(summary)).map_err(|source| {
        RecorderError::Io {
            path: integrity_path.clone(),
            source,
        }
    })?;

    Ok(integrity_path)
}

pub fn write_log_file<S>(
    output_dir: impl AsRef<Path>,
    lines: &[S],
) -> Result<PathBuf, RecorderError>
where
    S: AsRef<str>,
{
    let output_dir = output_dir.as_ref().to_path_buf();
    fs::create_dir_all(&output_dir).map_err(|source| RecorderError::Io {
        path: output_dir.clone(),
        source,
    })?;

    let log_path = output_dir.join("log.txt");
    fs::write(&log_path, log_text(lines)).map_err(|source| RecorderError::Io {
        path: log_path.clone(),
        source,
    })?;

    Ok(log_path)
}

pub fn write_events_csv(
    output_dir: impl AsRef<Path>,
    events: &[AcquisitionEvent],
) -> Result<PathBuf, RecorderError> {
    let output_dir = output_dir.as_ref().to_path_buf();
    fs::create_dir_all(&output_dir).map_err(|source| RecorderError::Io {
        path: output_dir.clone(),
        source,
    })?;

    let events_path = output_dir.join("events.csv");
    fs::write(&events_path, events_csv(events)).map_err(|source| RecorderError::Io {
        path: events_path.clone(),
        source,
    })?;

    Ok(events_path)
}

pub fn write_benchmark_summary(
    output_dir: impl AsRef<Path>,
    summary: &BenchmarkSummary,
) -> Result<PathBuf, RecorderError> {
    let output_dir = output_dir.as_ref().to_path_buf();
    fs::create_dir_all(&output_dir).map_err(|source| RecorderError::Io {
        path: output_dir.clone(),
        source,
    })?;

    let benchmark_path = output_dir.join("benchmark.json");
    fs::write(&benchmark_path, benchmark_summary_json(summary)).map_err(|source| {
        RecorderError::Io {
            path: benchmark_path.clone(),
            source,
        }
    })?;

    Ok(benchmark_path)
}

fn validate_blocks(blocks: &[SampleBlock]) -> Result<(), RecorderError> {
    for block in blocks {
        block
            .validate()
            .map_err(|source| RecorderError::InvalidBlock {
                packet_id: block.packet_id,
                source,
            })?;
    }

    if let Some(first) = blocks.first() {
        for block in &blocks[1..] {
            if block.device_id != first.device_id {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "device_id",
                });
            }

            if block.sample_rate != first.sample_rate {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "sample_rate",
                });
            }

            if block.channel_count != first.channel_count {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "channel_count",
                });
            }

            if block.samples_per_channel != first.samples_per_channel {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "samples_per_channel",
                });
            }
        }
    }

    Ok(())
}

fn write_raw_file(path: &Path, blocks: &[SampleBlock]) -> Result<(), RecorderError> {
    let file = File::create(path).map_err(|source| RecorderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut writer = BufWriter::new(file);

    for block in blocks {
        for sample in &block.data {
            writer
                .write_all(&sample.to_le_bytes())
                .map_err(|source| RecorderError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
    }

    writer.flush().map_err(|source| RecorderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_metadata_file(path: &Path, blocks: &[SampleBlock]) -> Result<(), RecorderError> {
    let metadata = recording_metadata_json(blocks);

    fs::write(path, metadata).map_err(|source| RecorderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn recording_metadata_json(blocks: &[SampleBlock]) -> String {
    let Some(first) = blocks.first() else {
        return empty_recording_metadata_json();
    };

    let last = blocks
        .last()
        .expect("non-empty block slice should have last block");

    format!(
        concat!(
            "{{\n",
            "  \"format\": \"kvraw\",\n",
            "  \"format_version\": 1,\n",
            "  \"device_id\": \"{}\",\n",
            "  \"backend\": \"simulator\",\n",
            "  \"sample_rate\": {},\n",
            "  \"channel_count\": {},\n",
            "  \"samples_per_packet\": {},\n",
            "  \"sample_type\": \"i16\",\n",
            "  \"endianness\": \"little\",\n",
            "  \"layout\": \"interleaved_by_sample\",\n",
            "  \"first_packet_id\": {},\n",
            "  \"last_packet_id\": {},\n",
            "  \"written_samples\": {},\n",
            "  \"clean_stop\": true\n",
            "}}\n"
        ),
        escape_json_string(&first.device_id),
        format_sample_rate(first.sample_rate),
        first.channel_count,
        first.samples_per_channel,
        first.packet_id,
        last.packet_id,
        written_samples(blocks)
    )
}

fn empty_recording_metadata_json() -> String {
    concat!(
        "{\n",
        "  \"format\": \"kvraw\",\n",
        "  \"format_version\": 1,\n",
        "  \"backend\": \"simulator\",\n",
        "  \"sample_type\": \"i16\",\n",
        "  \"endianness\": \"little\",\n",
        "  \"layout\": \"interleaved_by_sample\",\n",
        "  \"first_packet_id\": null,\n",
        "  \"last_packet_id\": null,\n",
        "  \"written_samples\": 0,\n",
        "  \"clean_stop\": true\n",
        "}\n"
    )
    .to_string()
}

fn integrity_summary_json(summary: &IntegritySummary) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"expected_packets\": {},\n",
            "  \"observed_packets\": {},\n",
            "  \"missing_packets\": {},\n",
            "  \"crc_errors\": {},\n",
            "  \"timestamp_discontinuities\": {},\n",
            "  \"buffer_overflows\": {},\n",
            "  \"expected_samples\": {},\n",
            "  \"written_samples\": {}\n",
            "}}\n"
        ),
        summary.expected_packets,
        summary.observed_packets,
        summary.missing_packets,
        summary.crc_errors,
        summary.timestamp_discontinuities,
        summary.buffer_overflows,
        summary.expected_samples,
        summary.written_samples
    )
}

fn log_text<S>(lines: &[S]) -> String
where
    S: AsRef<str>,
{
    let mut text = String::new();

    for line in lines {
        text.push_str(line.as_ref());
        text.push('\n');
    }

    text
}

fn events_csv(events: &[AcquisitionEvent]) -> String {
    let mut csv = String::from("host_time_ms,timestamp_start,event_type,value,message\n");

    for event in events {
        csv.push_str(&event_csv_row(event));
    }

    csv
}

fn benchmark_summary_json(summary: &BenchmarkSummary) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"measurement_kind\": \"{}\",\n",
            "  \"duration_seconds\": {},\n",
            "  \"channel_count\": {},\n",
            "  \"sample_rate\": {},\n",
            "  \"expected_samples\": {},\n",
            "  \"written_samples\": {},\n",
            "  \"missing_packets\": {},\n",
            "  \"crc_errors\": {},\n",
            "  \"timestamp_discontinuities\": {},\n",
            "  \"byte_count\": {},\n",
            "  \"average_write_mb_s\": {},\n",
            "  \"max_write_latency_ms\": {},\n",
            "  \"max_buffer_occupancy\": {},\n",
            "  \"cpu_percent_avg\": {},\n",
            "  \"memory_mb_max\": {}\n",
            "}}\n"
        ),
        escape_json_string(&summary.measurement_kind),
        format_metric(summary.duration_seconds),
        summary.channel_count,
        format_sample_rate(summary.sample_rate),
        summary.expected_samples,
        summary.written_samples,
        summary.missing_packets,
        summary.crc_errors,
        summary.timestamp_discontinuities,
        summary.byte_count,
        format_metric(summary.average_write_mb_s),
        format_optional_metric(summary.max_write_latency_ms),
        format_optional_metric(summary.max_buffer_occupancy),
        format_optional_metric(summary.cpu_percent_avg),
        format_optional_metric(summary.memory_mb_max)
    )
}

fn event_csv_row(event: &AcquisitionEvent) -> String {
    match event {
        AcquisitionEvent::Started { timestamp_host_ms } => csv_row(
            Some(*timestamp_host_ms),
            None,
            "started",
            None,
            String::new(),
        ),
        AcquisitionEvent::Stopped { timestamp_host_ms } => csv_row(
            Some(*timestamp_host_ms),
            None,
            "stopped",
            None,
            String::new(),
        ),
        AcquisitionEvent::TtlChanged {
            timestamp_start,
            ttl_bits,
        } => csv_row(
            None,
            Some(*timestamp_start),
            "ttl_changed",
            Some(ttl_bits.to_string()),
            String::new(),
        ),
        AcquisitionEvent::PacketMissing {
            expected_packet_id,
            observed_packet_id,
            missing_count,
        } => csv_row(
            None,
            None,
            "packet_missing",
            Some(missing_count.to_string()),
            format!(
                "expected_packet_id={expected_packet_id} observed_packet_id={observed_packet_id}"
            ),
        ),
        AcquisitionEvent::BufferOverflow {
            dropped_blocks,
            buffer_occupancy,
        } => csv_row(
            None,
            None,
            "buffer_overflow",
            Some(dropped_blocks.to_string()),
            format!("buffer_occupancy={buffer_occupancy:.3}"),
        ),
        AcquisitionEvent::RecorderError { message } => {
            csv_row(None, None, "recorder_error", None, message.clone())
        }
    }
}

fn csv_row(
    host_time_ms: Option<u64>,
    timestamp_start: Option<u64>,
    event_type: &str,
    value: Option<String>,
    message: String,
) -> String {
    format!(
        "{},{},{},{},{}\n",
        host_time_ms
            .map(|timestamp| timestamp.to_string())
            .unwrap_or_default(),
        timestamp_start
            .map(|timestamp| timestamp.to_string())
            .unwrap_or_default(),
        event_type,
        value.unwrap_or_default(),
        escape_csv_field(&message)
    )
}

fn escape_csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn written_samples(blocks: &[SampleBlock]) -> u64 {
    blocks
        .iter()
        .map(|block| block.data.len() as u64)
        .sum::<u64>()
}

fn format_sample_rate(sample_rate: f64) -> String {
    if sample_rate.fract() == 0.0 {
        format!("{sample_rate:.1}")
    } else {
        sample_rate.to_string()
    }
}

fn format_metric(value: f64) -> String {
    format!("{value:.6}")
}

fn format_optional_metric(value: Option<f64>) -> String {
    value
        .map(format_metric)
        .unwrap_or_else(|| "null".to_string())
}

fn escape_json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(character),
        }
    }

    escaped
}
