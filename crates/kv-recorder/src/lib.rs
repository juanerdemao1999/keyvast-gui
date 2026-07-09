//! Raw recorder for Keyvast sample blocks.

pub mod export_formats;

use std::{
    fmt,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

// ── KVRAW v2 embedded-header format constants ────────────────────────────────
//
// File layout:
//   [0..8]    magic b"KEYVAST\n"
//   [8..12]   json_len: u32 LE   (bytes of valid JSON in the block below)
//   [12..524] json_block: 512 B  (UTF-8 JSON, zero-padded to 512 bytes)
//   [524..]   raw i16 samples    (channel-interleaved, little-endian)
//
// `new()` writes a zeroed placeholder header; `finish()` seeks back to
// byte 8 and overwrites with the final metadata.
const KVRAW_MAGIC: &[u8; 8] = b"KEYVAST\n";
const KVRAW_JSON_RESERVED: usize = 512;
/// Byte offset where sample data begins (8 magic + 4 len + 512 json = 524).
pub const KVRAW_DATA_OFFSET: u64 = 8 + 4 + KVRAW_JSON_RESERVED as u64;

/// Maximum number of write-latency samples kept in memory (reservoir sampler).
const LATENCY_RESERVOIR_CAP: usize = 65_536;

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
    pub p50_write_latency_ms: Option<f64>,
    pub p95_write_latency_ms: Option<f64>,
    pub p99_write_latency_ms: Option<f64>,
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
    /// A legacy (v1) `.kvraw` file had neither an embedded header nor a
    /// companion `.json`, so its channel count and sample rate are unknown.
    MissingMetadata {
        path: PathBuf,
    },
    /// A frame range was so large that computing its byte offset/length
    /// overflowed the addressable range — the file or request is corrupt.
    OffsetOverflow {
        context: &'static str,
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
            Self::MissingMetadata { path } => {
                write!(
                    formatter,
                    "legacy kvraw file {} has no embedded header or companion .json; \
                     channel count and sample rate are unknown",
                    path.display()
                )
            }
            Self::OffsetOverflow { context } => {
                write!(formatter, "kvraw {context} computation overflowed")
            }
        }
    }
}

impl std::error::Error for RecorderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidBlock { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::InconsistentBlockConfig { .. }
            | Self::MissingMetadata { .. }
            | Self::OffsetOverflow { .. } => None,
        }
    }
}

pub fn write_recording(
    output_dir: impl AsRef<Path>,
    blocks: &[SampleBlock],
) -> Result<RecordingSummary, RecorderError> {
    write_recording_with_backend(output_dir, blocks, "simulator")
}

pub fn write_recording_with_backend(
    output_dir: impl AsRef<Path>,
    blocks: &[SampleBlock],
    backend: &str,
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
    write_metadata_file(&metadata_path, blocks, backend)?;

    let total_samples = written_samples(blocks);
    Ok(RecordingSummary {
        output_dir,
        raw_path,
        metadata_path,
        block_count: blocks.len() as u64,
        written_samples: total_samples,
        byte_count: total_samples.saturating_mul(2),
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
    let mut scratch = Vec::new();

    for block in blocks {
        write_samples_le(&mut writer, &block.data, &mut scratch, path)?;
    }

    writer.flush().map_err(|source| RecorderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Serialize an i16 sample slice into little-endian bytes through a reusable
/// scratch buffer and emit them in a single `write_all`, instead of one
/// `write_all` call per sample.
fn write_samples_le(
    writer: &mut BufWriter<File>,
    samples: &[i16],
    scratch: &mut Vec<u8>,
    path: &Path,
) -> Result<(), RecorderError> {
    scratch.clear();
    scratch.reserve(samples.len() * 2);
    for &sample in samples {
        scratch.extend_from_slice(&sample.to_le_bytes());
    }
    writer
        .write_all(scratch)
        .map_err(|source| RecorderError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn write_metadata_file(
    path: &Path,
    blocks: &[SampleBlock],
    backend: &str,
) -> Result<(), RecorderError> {
    let metadata = recording_metadata_json(blocks, backend);

    fs::write(path, metadata).map_err(|source| RecorderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn recording_metadata_json(blocks: &[SampleBlock], backend: &str) -> String {
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
            "  \"backend\": \"{}\",\n",
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
        escape_json_string(backend),
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
            "  \"p50_write_latency_ms\": {},\n",
            "  \"p95_write_latency_ms\": {},\n",
            "  \"p99_write_latency_ms\": {},\n",
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
        format_optional_metric(summary.p50_write_latency_ms),
        format_optional_metric(summary.p95_write_latency_ms),
        format_optional_metric(summary.p99_write_latency_ms),
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

/// Incremental recorder that writes sample blocks to disk one at a time.
///
/// Opens the raw data file on creation and appends each block as it arrives.
/// Call `finish()` to flush, write metadata, and get the final summary.
pub struct StreamingRecorder {
    output_dir: PathBuf,
    raw_path: PathBuf,
    writer: BufWriter<File>,
    block_count: u64,
    written_samples: u64,
    byte_count: u64,
    first_packet_id: Option<u64>,
    last_packet_id: Option<u64>,
    device_id: Option<String>,
    sample_rate: Option<f64>,
    channel_count: Option<usize>,
    samples_per_packet: Option<usize>,
    write_latencies_us: Vec<u64>,
    latency_sample_count: u64,
    sample_scratch: Vec<u8>,
    /// Set once the embedded header has been written back, so an explicit
    /// `finish()` and the `Drop` safety-net never finalize twice.
    finalized: bool,
}

impl StreamingRecorder {
    pub fn new(output_dir: impl AsRef<Path>) -> Result<Self, RecorderError> {
        let output_dir = output_dir.as_ref().to_path_buf();
        fs::create_dir_all(&output_dir).map_err(|source| RecorderError::Io {
            path: output_dir.clone(),
            source,
        })?;

        let raw_path = output_dir.join("recording.kvraw");
        let file = File::create(&raw_path).map_err(|source| RecorderError::Io {
            path: raw_path.clone(),
            source,
        })?;

        let mut writer = BufWriter::new(file);

        // Write placeholder header (overwritten with real metadata on finish())
        writer
            .write_all(KVRAW_MAGIC)
            .map_err(|source| RecorderError::Io {
                path: raw_path.clone(),
                source,
            })?;
        // json_len placeholder (4 bytes) + json_block placeholder (512 bytes)
        let placeholder = [0u8; 4 + KVRAW_JSON_RESERVED];
        writer
            .write_all(&placeholder)
            .map_err(|source| RecorderError::Io {
                path: raw_path.clone(),
                source,
            })?;

        Ok(Self {
            output_dir,
            raw_path,
            writer,
            block_count: 0,
            written_samples: 0,
            byte_count: 0,
            first_packet_id: None,
            last_packet_id: None,
            device_id: None,
            sample_rate: None,
            channel_count: None,
            samples_per_packet: None,
            write_latencies_us: Vec::with_capacity(LATENCY_RESERVOIR_CAP),
            latency_sample_count: 0,
            sample_scratch: Vec::new(),
            finalized: false,
        })
    }

    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    pub fn byte_count(&self) -> u64 {
        self.byte_count
    }

    /// Write one block, validating consistency and appending raw samples.
    pub fn write_block(&mut self, block: &SampleBlock) -> Result<(), RecorderError> {
        block
            .validate()
            .map_err(|source| RecorderError::InvalidBlock {
                packet_id: block.packet_id,
                source,
            })?;

        self.check_consistency(block)?;

        let start = std::time::Instant::now();

        write_samples_le(
            &mut self.writer,
            &block.data,
            &mut self.sample_scratch,
            &self.raw_path,
        )?;

        let elapsed_us = start.elapsed().as_micros() as u64;
        self.latency_sample_count += 1;
        if self.write_latencies_us.len() < LATENCY_RESERVOIR_CAP {
            self.write_latencies_us.push(elapsed_us);
        } else {
            // Reservoir sampling (Algorithm R): replace a random element
            // with probability LATENCY_RESERVOIR_CAP / latency_sample_count.
            let idx = cheap_rng(self.latency_sample_count) % self.latency_sample_count;
            if (idx as usize) < LATENCY_RESERVOIR_CAP {
                self.write_latencies_us[idx as usize] = elapsed_us;
            }
        }

        let sample_values = block.data.len() as u64;
        self.block_count = self.block_count.saturating_add(1);
        self.written_samples = self.written_samples.saturating_add(sample_values);
        self.byte_count = self
            .byte_count
            .saturating_add(sample_values.saturating_mul(2));

        if self.first_packet_id.is_none() {
            self.first_packet_id = Some(block.packet_id);
        }
        self.last_packet_id = Some(block.packet_id);

        Ok(())
    }

    /// Flush pending samples and overwrite the placeholder header in place with
    /// the final embedded JSON metadata. Idempotent via the `finalized` flag so
    /// both `finish()` and the `Drop` safety-net can call it.
    ///
    /// Uses `BufWriter::get_mut()` (a seekable `&mut File`) rather than
    /// `into_inner()` so it works from `&mut self` — the header is written after
    /// the buffered samples are flushed, so the internal writer position is not
    /// used again.
    fn finalize_header(&mut self) -> Result<(), RecorderError> {
        if self.finalized {
            return Ok(());
        }
        self.finalized = true;

        // 1. Flush all pending sample data (appended after the 524-byte header).
        self.writer.flush().map_err(|source| RecorderError::Io {
            path: self.raw_path.clone(),
            source,
        })?;

        // 2. Build the final JSON header.
        let metadata = self.streaming_metadata_json();
        let path = self.raw_path.clone();
        let file = self.writer.get_mut();

        // 3. Seek back to byte 8 (right after the 8-byte magic).
        file.seek(SeekFrom::Start(8))
            .map_err(|source| RecorderError::Io {
                path: path.clone(),
                source,
            })?;

        // 4. Write json_len + padded json_block.
        let json_bytes = metadata.as_bytes();
        let json_len = json_bytes.len().min(KVRAW_JSON_RESERVED);
        file.write_all(&(json_len as u32).to_le_bytes())
            .map_err(|source| RecorderError::Io {
                path: path.clone(),
                source,
            })?;
        let mut json_block = [0u8; KVRAW_JSON_RESERVED];
        json_block[..json_len].copy_from_slice(&json_bytes[..json_len]);
        file.write_all(&json_block)
            .map_err(|source| RecorderError::Io {
                path: path.clone(),
                source,
            })?;

        file.flush()
            .map_err(|source| RecorderError::Io { path, source })?;
        Ok(())
    }

    /// Flush raw data, write embedded JSON header, return summary.
    ///
    /// Seeks back to byte 8 (after the magic) to overwrite the placeholder
    /// header with the final metadata.  No separate `.json` file is created —
    /// all information is self-contained in `recording.kvraw`.
    pub fn finish(mut self) -> Result<StreamingRecordingSummary, RecorderError> {
        self.finalize_header()?;

        let path = self.raw_path.clone();
        let max_write_latency_us = self.write_latencies_us.iter().copied().max();
        let latency_distribution = LatencyDistribution::from_samples(&self.write_latencies_us);

        Ok(StreamingRecordingSummary {
            recording: RecordingSummary {
                output_dir: self.output_dir.clone(),
                // Metadata is embedded in the kvraw file; metadata_path == raw_path
                metadata_path: path.clone(),
                raw_path: path,
                block_count: self.block_count,
                written_samples: self.written_samples,
                byte_count: self.byte_count,
                first_packet_id: self.first_packet_id,
                last_packet_id: self.last_packet_id,
            },
            max_write_latency_us,
            latency_distribution,
        })
    }

    fn check_consistency(&mut self, block: &SampleBlock) -> Result<(), RecorderError> {
        if let Some(ref device_id) = self.device_id {
            if block.device_id != *device_id {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "device_id",
                });
            }
        } else {
            self.device_id = Some(block.device_id.clone());
        }

        if let Some(sample_rate) = self.sample_rate {
            if block.sample_rate != sample_rate {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "sample_rate",
                });
            }
        } else {
            self.sample_rate = Some(block.sample_rate);
        }

        if let Some(channel_count) = self.channel_count {
            if block.channel_count != channel_count {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "channel_count",
                });
            }
        } else {
            self.channel_count = Some(block.channel_count);
        }

        if let Some(samples_per_packet) = self.samples_per_packet {
            if block.samples_per_channel != samples_per_packet {
                return Err(RecorderError::InconsistentBlockConfig {
                    packet_id: block.packet_id,
                    field: "samples_per_channel",
                });
            }
        } else {
            self.samples_per_packet = Some(block.samples_per_channel);
        }

        Ok(())
    }

    fn streaming_metadata_json(&self) -> String {
        let Some(ref device_id) = self.device_id else {
            return empty_recording_metadata_json();
        };

        // Infer backend from device_id: "demo-*" → demo, "rhd-*" → rhd-hardware,
        // "simulator-*" → simulator.  Falls back to the raw device_id prefix.
        let backend = if device_id.starts_with("demo") {
            "demo"
        } else if device_id.starts_with("rhd") {
            "rhd-hardware"
        } else if device_id.starts_with("simulator") {
            "simulator"
        } else {
            device_id.split('-').next().unwrap_or("unknown")
        };

        format!(
            concat!(
                "{{\n",
                "  \"format\": \"kvraw\",\n",
                "  \"format_version\": 2,\n",
                "  \"data_offset_bytes\": {},\n",
                "  \"device_id\": \"{}\",\n",
                "  \"backend\": \"{}\",\n",
                "  \"sample_rate\": {},\n",
                "  \"channel_count\": {},\n",
                "  \"samples_per_channel\": {},\n",
                "  \"sample_type\": \"i16\",\n",
                "  \"endianness\": \"little\",\n",
                "  \"layout\": \"interleaved_by_sample\",\n",
                "  \"block_count\": {},\n",
                "  \"first_packet_id\": {},\n",
                "  \"last_packet_id\": {},\n",
                "  \"written_samples\": {},\n",
                "  \"clean_stop\": true\n",
                "}}\n"
            ),
            KVRAW_DATA_OFFSET,
            escape_json_string(device_id),
            escape_json_string(backend),
            format_sample_rate(self.sample_rate.unwrap_or(0.0)),
            self.channel_count.unwrap_or(0),
            self.samples_per_packet.unwrap_or(0),
            self.block_count,
            self.first_packet_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "null".to_string()),
            self.last_packet_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "null".to_string()),
            self.written_samples
        )
    }
}

impl Drop for StreamingRecorder {
    /// Safety-net: if the recorder is dropped without an explicit `finish()`
    /// (e.g. the app quits or a thread unwinds mid-recording), rewrite the
    /// embedded header on a best-effort basis so the `.kvraw` is left as a valid
    /// v2 file with `json_len > 0` instead of the zeroed placeholder that would
    /// make it unreadable. Errors can only be logged from `drop`.
    fn drop(&mut self) {
        if self.finalized {
            return;
        }
        match self.finalize_header() {
            Ok(()) => eprintln!(
                "warning: StreamingRecorder dropped without finish(); header finalized on drop ({} blocks) at {}",
                self.block_count,
                self.raw_path.display()
            ),
            Err(e) => eprintln!(
                "error: StreamingRecorder dropped without finish() and best-effort finalize failed: {e}"
            ),
        }
    }
}

impl fmt::Debug for StreamingRecorder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamingRecorder")
            .field("output_dir", &self.output_dir)
            .field("block_count", &self.block_count)
            .field("written_samples", &self.written_samples)
            .field("byte_count", &self.byte_count)
            .finish()
    }
}

/// Per-block write latency distribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatencyDistribution {
    pub count: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub mean_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

impl LatencyDistribution {
    /// Compute a latency distribution from a slice of microsecond samples.
    /// Returns `None` if the slice is empty.
    pub fn from_samples(samples: &[u64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let count = sorted.len() as u64;
        let sum: u64 = sorted.iter().sum();
        Some(Self {
            count,
            min_us: sorted[0],
            max_us: sorted[sorted.len() - 1],
            mean_us: sum / count,
            p50_us: percentile(&sorted, 50),
            p95_us: percentile(&sorted, 95),
            p99_us: percentile(&sorted, 99),
        })
    }
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (pct * (sorted.len() - 1)) / 100;
    sorted[idx]
}

/// Summary returned by `StreamingRecorder::finish()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingRecordingSummary {
    pub recording: RecordingSummary,
    pub max_write_latency_us: Option<u64>,
    pub latency_distribution: Option<LatencyDistribution>,
}

pub(crate) fn escape_json_string(value: &str) -> String {
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

/// Cheap deterministic hash for reservoir sampling (splitmix64-style).
fn cheap_rng(x: u64) -> u64 {
    let mut z = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

// ── KVRAW v2 Reader ──────────────────────────────────────────────────────────

/// Metadata parsed from a KVRAW v2 file header.
#[derive(Debug, Clone, PartialEq)]
pub struct KvrawMetadata {
    pub format_version: u32,
    pub data_offset_bytes: u64,
    pub device_id: String,
    pub backend: String,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub samples_per_channel: usize,
    pub written_samples: u64,
    pub block_count: u64,
    pub first_packet_id: Option<u64>,
    pub last_packet_id: Option<u64>,
    pub clean_stop: bool,
}

impl KvrawMetadata {
    /// Total number of complete multi-channel samples in the file.
    /// Each "sample" contains `channel_count` i16 values.
    pub fn total_frames(&self) -> u64 {
        if self.channel_count == 0 {
            return 0;
        }
        self.written_samples / self.channel_count as u64
    }

    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate <= 0.0 {
            return 0.0;
        }
        self.total_frames() as f64 / self.sample_rate
    }
}

/// Reader for KVRAW v2 files (embedded JSON header + raw i16 samples).
pub struct KvrawReader {
    reader: BufReader<File>,
    metadata: KvrawMetadata,
    data_start: u64,
    file_size: u64,
}

impl KvrawReader {
    /// Open a .kvraw file and parse its embedded header.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RecorderError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| RecorderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file_size = file
            .metadata()
            .map_err(|source| RecorderError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .len();

        let mut reader = BufReader::new(file);

        // Read magic
        let mut magic = [0u8; 8];
        reader
            .read_exact(&mut magic)
            .map_err(|source| RecorderError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        // Check for KVRAW v2 (embedded header) vs v1 (external JSON)
        let (metadata, data_start) = if &magic == KVRAW_MAGIC {
            // v2: embedded JSON header
            let mut json_len_bytes = [0u8; 4];
            reader
                .read_exact(&mut json_len_bytes)
                .map_err(|source| RecorderError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
            let json_len = u32::from_le_bytes(json_len_bytes) as usize;
            let json_len = json_len.min(KVRAW_JSON_RESERVED);

            let mut json_block = vec![0u8; KVRAW_JSON_RESERVED];
            reader
                .read_exact(&mut json_block)
                .map_err(|source| RecorderError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;

            let json_str = std::str::from_utf8(&json_block[..json_len]).unwrap_or("{}");
            let meta = parse_kvraw_json(json_str);
            let data_offset = meta.data_offset_bytes;
            (meta, data_offset)
        } else {
            // v1: no embedded header — try loading companion .json file
            let json_path = path.with_extension("json");
            let meta = if json_path.exists() {
                let json_str =
                    fs::read_to_string(&json_path).map_err(|source| RecorderError::Io {
                        path: json_path,
                        source,
                    })?;
                parse_kvraw_json(&json_str)
            } else {
                // No embedded header and no companion .json: the channel count
                // and sample rate are genuinely unknown. Fabricating defaults
                // (64 ch / 30 kHz) would silently mis-interpret every frame, so
                // surface the missing metadata instead.
                return Err(RecorderError::MissingMetadata {
                    path: path.to_path_buf(),
                });
            };
            // v1 files have raw data from offset 0
            (meta, 0)
        };

        // If written_samples is 0, compute from file size.
        let mut metadata = metadata;
        if metadata.written_samples == 0 && metadata.channel_count > 0 {
            let data_bytes = file_size.saturating_sub(data_start);
            metadata.written_samples = data_bytes / 2; // i16 = 2 bytes
        }

        Ok(Self {
            reader,
            metadata,
            data_start,
            file_size,
        })
    }

    /// Get the parsed metadata.
    pub fn metadata(&self) -> &KvrawMetadata {
        &self.metadata
    }

    /// Total number of time-frames in the file.
    pub fn total_frames(&self) -> u64 {
        self.metadata.total_frames()
    }

    /// Read a range of frames as interleaved i16 samples.
    /// Returns `channel_count * num_frames` i16 values.
    pub fn read_frames(
        &mut self,
        start_frame: u64,
        num_frames: usize,
    ) -> Result<Vec<i16>, RecorderError> {
        let ch = self.metadata.channel_count;
        if ch == 0 {
            return Ok(Vec::new());
        }

        let max_frames = self.total_frames();
        let start_frame = start_frame.min(max_frames);
        let available = (max_frames - start_frame) as usize;
        let num_frames = num_frames.min(available);
        if num_frames == 0 {
            return Ok(Vec::new());
        }

        let overflow = || RecorderError::OffsetOverflow {
            context: "frame byte-offset",
        };
        let sample_offset = start_frame.checked_mul(ch as u64).ok_or_else(overflow)?;
        let byte_offset = sample_offset
            .checked_mul(2)
            .and_then(|bytes| self.data_start.checked_add(bytes))
            .ok_or_else(overflow)?;
        let total_samples = ch.checked_mul(num_frames).ok_or_else(overflow)?;
        let byte_count = total_samples.checked_mul(2).ok_or_else(overflow)?;

        self.reader
            .seek(SeekFrom::Start(byte_offset))
            .map_err(|source| RecorderError::Io {
                path: PathBuf::from("<kvraw>"),
                source,
            })?;

        let mut bytes = vec![0u8; byte_count];
        self.reader
            .read_exact(&mut bytes)
            .map_err(|source| RecorderError::Io {
                path: PathBuf::from("<kvraw>"),
                source,
            })?;

        let samples: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();

        Ok(samples)
    }

    /// Read a range of frames and return per-channel vectors.
    /// Returns `channel_count` vectors, each with `num_frames` samples.
    pub fn read_channels(
        &mut self,
        start_frame: u64,
        num_frames: usize,
    ) -> Result<Vec<Vec<i16>>, RecorderError> {
        let ch = self.metadata.channel_count;
        let interleaved = self.read_frames(start_frame, num_frames)?;
        let actual_frames = interleaved.len() / ch.max(1);

        let mut channels: Vec<Vec<i16>> =
            (0..ch).map(|_| Vec::with_capacity(actual_frames)).collect();

        for frame in interleaved.chunks_exact(ch) {
            for (c, &sample) in frame.iter().enumerate() {
                channels[c].push(sample);
            }
        }

        Ok(channels)
    }
}

impl fmt::Debug for KvrawReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KvrawReader")
            .field("metadata", &self.metadata)
            .field("data_start", &self.data_start)
            .field("file_size", &self.file_size)
            .finish()
    }
}

/// Minimal JSON parser for KVRAW metadata.  Avoids adding serde as a
/// dependency — the JSON is always machine-generated with a known schema.
fn parse_kvraw_json(json: &str) -> KvrawMetadata {
    // Return the text immediately following `"key":` but only when the quoted
    // key is in key position (directly followed by optional whitespace and a
    // colon).  This prevents a *value* that happens to equal a key name (e.g.
    // `"backend": "channel_count"`) from being picked up as that key.
    let value_after = |key: &str| -> Option<&str> {
        let needle = format!("\"{key}\"");
        let mut search_from = 0;
        while let Some(rel) = json[search_from..].find(&needle) {
            let pos = search_from + rel;
            let after = &json[pos + needle.len()..];
            if let Some(rest) = after.trim_start().strip_prefix(':') {
                return Some(rest.trim_start());
            }
            search_from = pos + needle.len();
        }
        None
    };

    let get_str = |key: &str| -> String {
        value_after(key)
            .and_then(|rest| {
                let value = rest.strip_prefix('"')?;
                let end = value.find('"')?;
                Some(value[..end].to_string())
            })
            .unwrap_or_default()
    };

    let get_u64 = |key: &str| -> u64 {
        value_after(key)
            .and_then(|rest| {
                // Extract numeric chars
                let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                num_str.parse().ok()
            })
            .unwrap_or(0)
    };

    let get_f64 = |key: &str| -> f64 {
        value_after(key)
            .and_then(|rest| {
                let num_str: String = rest
                    .chars()
                    .take_while(|c| {
                        c.is_ascii_digit()
                            || *c == '.'
                            || *c == '-'
                            || *c == 'e'
                            || *c == 'E'
                            || *c == '+'
                    })
                    .collect();
                num_str.parse().ok()
            })
            .unwrap_or(0.0)
    };

    let get_optional_u64 = |key: &str| -> Option<u64> {
        let rest = value_after(key)?;
        if rest.starts_with("null") {
            return None;
        }
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    };

    let get_bool = |key: &str| -> bool {
        value_after(key)
            .map(|rest| rest.starts_with("true"))
            .unwrap_or(false)
    };

    let format_version = get_u64("format_version") as u32;
    let data_offset = if format_version >= 2 {
        get_u64("data_offset_bytes")
    } else {
        0
    };

    KvrawMetadata {
        format_version,
        data_offset_bytes: if data_offset > 0 {
            data_offset
        } else {
            KVRAW_DATA_OFFSET
        },
        device_id: get_str("device_id"),
        backend: get_str("backend"),
        sample_rate: get_f64("sample_rate"),
        channel_count: get_u64("channel_count") as usize,
        samples_per_channel: get_u64("samples_per_channel") as usize,
        written_samples: get_u64("written_samples"),
        block_count: get_u64("block_count"),
        first_packet_id: get_optional_u64("first_packet_id"),
        last_packet_id: get_optional_u64("last_packet_id"),
        clean_stop: get_bool("clean_stop"),
    }
}

#[cfg(test)]
mod json_parse_tests {
    use super::parse_kvraw_json;

    #[test]
    fn parses_a_well_formed_metadata_header() {
        let json = concat!(
            "{\n",
            "  \"format_version\": 2,\n",
            "  \"data_offset_bytes\": 524,\n",
            "  \"device_id\": \"dev-1\",\n",
            "  \"backend\": \"simulator\",\n",
            "  \"sample_rate\": 30000.0,\n",
            "  \"channel_count\": 64,\n",
            "  \"clean_stop\": true\n",
            "}\n"
        );
        let meta = parse_kvraw_json(json);
        assert_eq!(meta.format_version, 2);
        assert_eq!(meta.data_offset_bytes, 524);
        assert_eq!(meta.device_id, "dev-1");
        assert_eq!(meta.backend, "simulator");
        assert_eq!(meta.sample_rate, 30000.0);
        assert_eq!(meta.channel_count, 64);
        assert!(meta.clean_stop);
    }

    #[test]
    fn key_lookup_ignores_a_value_that_matches_a_key_name() {
        // The backend value is literally the string "channel_count", so it
        // appears as the quoted substring `"channel_count"` *before* the real
        // key.  A naive `find("\"channel_count\"")` would latch onto the value
        // and then grab the next colon it sees; key-position matching must skip
        // the value and read the genuine channel_count number.
        let json = concat!(
            "{\n",
            "  \"backend\": \"channel_count\",\n",
            "  \"channel_count\": 32\n",
            "}\n"
        );
        let meta = parse_kvraw_json(json);
        assert_eq!(meta.backend, "channel_count");
        assert_eq!(meta.channel_count, 32);
    }
}
