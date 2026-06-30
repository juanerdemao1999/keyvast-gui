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
//   [0..8]     magic b"KEYVAST\n"
//   [8..12]    json_len: u32 LE   (bytes of valid JSON in the block below)
//   [12..1036] json_block: 1024 B (UTF-8 JSON, zero-padded to 1024 bytes)
//   [1036..]   raw i16 samples    (channel-interleaved, little-endian)
//
// `new()` writes a zeroed placeholder header; `finish()` seeks back to
// byte 8 and overwrites with the final metadata.
//
// The reserved block is 1024 B (was 512 B): the DA16 clock-domain fields
// (`fpga_timestamp_first/last`, `host_clock_first/last_ns`) push a fully
// populated header past 512 B, which `finish()` would otherwise truncate into
// invalid JSON. Readers consume exactly `data_offset_bytes` (recorded in the
// JSON) before the samples, so older 512-B files still parse via their own
// stored offset.
const KVRAW_MAGIC: &[u8; 8] = b"KEYVAST\n";
const KVRAW_JSON_RESERVED: usize = 1024;
/// Byte offset where sample data begins (8 magic + 4 len + 1024 json = 1036).
pub const KVRAW_DATA_OFFSET: u64 = 8 + 4 + KVRAW_JSON_RESERVED as u64;

// ── KVAUX side-channel sidecar format constants ──────────────────────────────
//
// The `.kvraw` file holds only interleaved amplifier samples.  Per-sample TTL
// in/out words, board-ADC channels and auxiliary-command channels — all parsed
// off the wire but previously discarded — are persisted alongside it in a
// companion `recording.kvaux` file, together with the channel→electrode mapping
// metadata that the bare `.kvraw` header lacks.
//
// File layout (mirrors the `.kvraw` embedded-header convention):
//   [0..8]      magic b"KVAUX1\0\0"
//   [8..12]     json_len: u32 LE
//   [12..8204]  json_block: 8192 B  (UTF-8 JSON, zero-padded)
//   [8204..]    per-block side-channel payload (see `SideChannelLayout`)
//
// The sidecar is always written so the channel mapping is recoverable even when
// a recording carries no side-channel streams.
const KVAUX_MAGIC: &[u8; 8] = b"KVAUX1\0\0";
const KVAUX_JSON_RESERVED: usize = 8192;
/// Byte offset where side-channel payload begins (8 magic + 4 len + 8192 json).
pub const KVAUX_DATA_OFFSET: u64 = 8 + 4 + KVAUX_JSON_RESERVED as u64;

/// Maximum number of write-latency samples kept in memory (reservoir sampler).
const LATENCY_RESERVOIR_CAP: usize = 65_536;

use kv_types::{AcquisitionEvent, IntegritySummary, SampleBlock, SampleBlockError};

/// Channel-mapping and acquisition metadata captured when a recording starts.
///
/// The bare `.kvraw` header records only a `channel_count`; this carries the
/// selective-save column→electrode mapping and TTL line width so a recording is
/// self-describing.  When `enabled_channels` is empty the recording captured
/// every channel in natural order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordingConfig {
    /// Original device channel indices, in the order they appear in `.kvraw`
    /// columns.  Empty means "all channels, natural order".
    pub enabled_channels: Vec<usize>,
    /// Number of TTL lines the device exposes (0 when TTL is disabled).
    pub ttl_line_count: usize,
}

/// Per-block layout of the side-channel streams written to the `.kvaux` sidecar.
///
/// Established from the first block and enforced for every subsequent block so
/// the payload is a fixed-stride sequence the reader can index without a table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SideChannelLayout {
    /// Samples per block for the TTL-input word stream, or `None` when absent.
    pub ttl_in_samples: Option<usize>,
    /// Samples per block for the TTL-output word stream, or `None` when absent.
    pub ttl_out_samples: Option<usize>,
    /// Number of board-ADC channels (0 when absent).
    pub board_adc_channels: usize,
    /// Samples per block, per board-ADC channel.
    pub board_adc_samples: usize,
    /// Number of auxiliary streams (0 when absent).
    pub aux_streams: usize,
    /// Auxiliary channels per stream (typically 3).
    pub aux_channels_per_stream: usize,
    /// Samples per block, per auxiliary channel.
    pub aux_samples: usize,
}

impl SideChannelLayout {
    /// Derives the layout from a block's optional side-channel vectors.
    fn from_block(block: &SampleBlock) -> Self {
        let board_adc_channels = block
            .board_adc_data
            .as_ref()
            .map(|adc| adc.len())
            .unwrap_or(0);
        let board_adc_samples = block
            .board_adc_data
            .as_ref()
            .and_then(|adc| adc.first())
            .map(|ch| ch.len())
            .unwrap_or(0);
        let aux_streams = block.aux_data.as_ref().map(|aux| aux.len()).unwrap_or(0);
        let aux_channels_per_stream = block
            .aux_data
            .as_ref()
            .and_then(|aux| aux.first())
            .map(|stream| stream.len())
            .unwrap_or(0);
        let aux_samples = block
            .aux_data
            .as_ref()
            .and_then(|aux| aux.first())
            .and_then(|stream| stream.first())
            .map(|ch| ch.len())
            .unwrap_or(0);
        Self {
            ttl_in_samples: block.ttl_in_per_sample.as_ref().map(|v| v.len()),
            ttl_out_samples: block.ttl_out_per_sample.as_ref().map(|v| v.len()),
            board_adc_channels,
            board_adc_samples,
            aux_streams,
            aux_channels_per_stream,
            aux_samples,
        }
    }

    /// True when no side-channel stream is present.
    fn is_empty(&self) -> bool {
        self.ttl_in_samples.is_none()
            && self.ttl_out_samples.is_none()
            && self.board_adc_channels == 0
            && self.aux_streams == 0
    }
}

/// Derives `AcquisitionEvent::TtlChanged` events from the per-sample TTL-input
/// words across a sequence of recorded blocks.
///
/// A block without `ttl_in_per_sample` contributes nothing.  An event is
/// emitted for the first observed TTL word and for every subsequent sample
/// whose word differs from its predecessor; `timestamp_start` carries the
/// absolute sample index of the transition.
pub fn ttl_change_events(blocks: &[SampleBlock]) -> Vec<AcquisitionEvent> {
    let mut tracker = TtlChangeTracker::default();
    let mut events = Vec::new();
    for block in blocks {
        tracker.observe(block, &mut events);
    }
    events
}

/// Tracks the most recently observed TTL-input word so transitions can be
/// turned into `AcquisitionEvent::TtlChanged` records as blocks arrive.
#[derive(Debug, Default)]
struct TtlChangeTracker {
    last_ttl: Option<u32>,
}

impl TtlChangeTracker {
    /// Appends a `TtlChanged` event for every per-sample TTL transition in
    /// `block`, using the sample's absolute index as `timestamp_start`.
    fn observe(&mut self, block: &SampleBlock, events: &mut Vec<AcquisitionEvent>) {
        let Some(words) = block.ttl_in_per_sample.as_ref() else {
            return;
        };
        for (offset, &word) in words.iter().enumerate() {
            if self.last_ttl != Some(word) {
                events.push(AcquisitionEvent::TtlChanged {
                    timestamp_start: block.timestamp_start.saturating_add(offset as u64),
                    ttl_bits: word,
                });
                self.last_ttl = Some(word);
            }
        }
    }
}

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
    /// Length of acquired signal in seconds (`written_samples / channels /
    /// sample_rate`), i.e. how much data was captured — *not* how long the
    /// run took on the wall clock.
    pub duration_seconds: f64,
    /// Wall-clock time the run took to compute, when measured. Distinct from
    /// `duration_seconds`; `None` for estimate-only summaries.
    pub wall_clock_seconds: Option<f64>,
    /// Signal duration the caller requested (e.g. `benchmark --duration`),
    /// when applicable. `None` when the run is sized by block count instead.
    pub requested_duration_seconds: Option<f64>,
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

fn append_u32_le(buf: &mut Vec<u8>, words: &[u32]) {
    buf.reserve(words.len() * 4);
    for &word in words {
        buf.extend_from_slice(&word.to_le_bytes());
    }
}

fn append_u16_le(buf: &mut Vec<u8>, values: &[u16]) {
    buf.reserve(values.len() * 2);
    for &value in values {
        buf.extend_from_slice(&value.to_le_bytes());
    }
}

/// Create the `.kvaux` sidecar and write its zeroed placeholder header.
fn create_kvaux_placeholder(path: &Path) -> Result<BufWriter<File>, RecorderError> {
    let file = File::create(path).map_err(|source| RecorderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(KVAUX_MAGIC)
        .map_err(|source| RecorderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let placeholder = [0u8; 4 + KVAUX_JSON_RESERVED];
    writer
        .write_all(&placeholder)
        .map_err(|source| RecorderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(writer)
}

/// Infer the backend label from a device id: `demo-*` → demo, `rhd-*` →
/// rhd-hardware, `simulator-*` → simulator; otherwise the id's first segment.
fn infer_backend(device_id: &str) -> String {
    if device_id.starts_with("demo") {
        "demo".to_string()
    } else if device_id.starts_with("rhd") {
        "rhd-hardware".to_string()
    } else if device_id.starts_with("simulator") {
        "simulator".to_string()
    } else {
        device_id.split('-').next().unwrap_or("unknown").to_string()
    }
}

fn format_usize_array(values: &[usize]) -> String {
    let mut out = String::from("[");
    for (idx, value) in values.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        out.push_str(&value.to_string());
    }
    out.push(']');
    out
}

fn format_optional_usize(value: Option<usize>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string())
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
            "  \"fpga_timestamp_first\": {},\n",
            "  \"fpga_timestamp_last\": {},\n",
            "  \"host_clock_first_ns\": {},\n",
            "  \"host_clock_last_ns\": {},\n",
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
        first.timestamp_start,
        last.timestamp_start,
        json_opt_i64(first.host_time_ns),
        json_opt_i64(last.host_time_ns),
        written_samples(blocks)
    )
}

/// Render an optional host wall-clock timestamp (DA16) as a JSON number or
/// `null`. Pairing `host_clock_*_ns` with `fpga_timestamp_*` lets offline tools
/// align the FPGA sample counter to wall-clock time and estimate clock drift.
fn json_opt_i64(value: Option<i64>) -> String {
    match value {
        Some(ns) => ns.to_string(),
        None => "null".to_string(),
    }
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
            "  \"wall_clock_seconds\": {},\n",
            "  \"requested_duration_seconds\": {},\n",
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
        format_optional_metric(summary.wall_clock_seconds),
        format_optional_metric(summary.requested_duration_seconds),
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
    first_timestamp_start: Option<u64>,
    last_timestamp_start: Option<u64>,
    first_host_time_ns: Option<i64>,
    last_host_time_ns: Option<i64>,
    device_id: Option<String>,
    sample_rate: Option<f64>,
    channel_count: Option<usize>,
    samples_per_packet: Option<usize>,
    write_latencies_us: Vec<u64>,
    latency_sample_count: u64,
    sample_scratch: Vec<u8>,
    config: RecordingConfig,
    aux_path: PathBuf,
    aux_writer: Option<BufWriter<File>>,
    side_layout: Option<SideChannelLayout>,
    aux_scratch: Vec<u8>,
}

impl StreamingRecorder {
    pub fn new(output_dir: impl AsRef<Path>) -> Result<Self, RecorderError> {
        Self::with_config(output_dir, RecordingConfig::default())
    }

    /// Create a recorder that records the given channel-mapping/TTL metadata
    /// into the `.kvaux` sidecar alongside the raw amplifier stream.
    pub fn with_config(
        output_dir: impl AsRef<Path>,
        config: RecordingConfig,
    ) -> Result<Self, RecorderError> {
        let output_dir = output_dir.as_ref().to_path_buf();
        fs::create_dir_all(&output_dir).map_err(|source| RecorderError::Io {
            path: output_dir.clone(),
            source,
        })?;

        let raw_path = output_dir.join("recording.kvraw");
        let aux_path = output_dir.join("recording.kvaux");
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
        // json_len placeholder (4 bytes) + json_block placeholder (KVRAW_JSON_RESERVED bytes)
        let placeholder = [0u8; 4 + KVRAW_JSON_RESERVED];
        writer
            .write_all(&placeholder)
            .map_err(|source| RecorderError::Io {
                path: raw_path.clone(),
                source,
            })?;

        // Always create the `.kvaux` sidecar so the channel mapping is recorded
        // even for recordings without side-channel streams.  Side-channel
        // payload is appended later by `write_side_channels`.
        let aux_writer = create_kvaux_placeholder(&aux_path)?;

        Ok(Self {
            output_dir,
            raw_path,
            writer,
            block_count: 0,
            written_samples: 0,
            byte_count: 0,
            first_packet_id: None,
            last_packet_id: None,
            first_timestamp_start: None,
            last_timestamp_start: None,
            first_host_time_ns: None,
            last_host_time_ns: None,
            device_id: None,
            sample_rate: None,
            channel_count: None,
            samples_per_packet: None,
            write_latencies_us: Vec::with_capacity(LATENCY_RESERVOIR_CAP),
            latency_sample_count: 0,
            sample_scratch: Vec::new(),
            config,
            aux_path,
            aux_writer: Some(aux_writer),
            side_layout: None,
            aux_scratch: Vec::new(),
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

        self.write_side_channels(block)?;

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
            self.first_timestamp_start = Some(block.timestamp_start);
            self.first_host_time_ns = block.host_time_ns;
        }
        self.last_packet_id = Some(block.packet_id);
        self.last_timestamp_start = Some(block.timestamp_start);
        self.last_host_time_ns = block.host_time_ns;

        Ok(())
    }

    /// Flush raw data, write embedded JSON header, return summary.
    ///
    /// Seeks back to byte 8 (after the magic) to overwrite the placeholder
    /// header with the final metadata.  No separate `.json` file is created —
    /// all information is self-contained in `recording.kvraw`.
    pub fn finish(mut self) -> Result<StreamingRecordingSummary, RecorderError> {
        // 0. Finalize the side-channel sidecar (header + flush).
        self.finish_aux()?;

        // 1. Flush all pending sample data to disk
        self.writer.flush().map_err(|source| RecorderError::Io {
            path: self.raw_path.clone(),
            source,
        })?;

        // 2. Build the final JSON BEFORE moving out of self.writer.
        //    (into_inner() partially moves self, so we can't borrow self after.)
        let metadata = self.streaming_metadata_json();
        let path = self.raw_path.clone();

        // 3. Take the underlying File out of the BufWriter so we can seek.
        let mut file = self.writer.into_inner().map_err(|e| RecorderError::Io {
            path: path.clone(),
            source: e.into_error(),
        })?;

        // 4. Seek back to byte 8 (right after the 8-byte magic)
        file.seek(SeekFrom::Start(8))
            .map_err(|source| RecorderError::Io {
                path: path.clone(),
                source,
            })?;

        // 5. Write json_len + padded json_block
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

        file.flush().map_err(|source| RecorderError::Io {
            path: path.clone(),
            source,
        })?;

        let max_write_latency_us = self.write_latencies_us.iter().copied().max();
        let latency_distribution = LatencyDistribution::from_samples(&self.write_latencies_us);

        Ok(StreamingRecordingSummary {
            recording: RecordingSummary {
                output_dir: self.output_dir,
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

    /// Append this block's side-channel streams (TTL in/out, board ADC, aux) to
    /// the `.kvaux` sidecar.  The layout is fixed by the first block; a later
    /// block whose side-channel shape differs is rejected so the payload stays a
    /// constant-stride sequence.
    fn write_side_channels(&mut self, block: &SampleBlock) -> Result<(), RecorderError> {
        let layout = SideChannelLayout::from_block(block);

        match self.side_layout {
            Some(existing) => {
                if existing != layout {
                    return Err(RecorderError::InconsistentBlockConfig {
                        packet_id: block.packet_id,
                        field: "side_channel_layout",
                    });
                }
            }
            None => {
                self.side_layout = Some(layout);
            }
        }

        if layout.is_empty() {
            return Ok(());
        }

        self.aux_scratch.clear();
        if let Some(ref words) = block.ttl_in_per_sample {
            append_u32_le(&mut self.aux_scratch, words);
        }
        if let Some(ref words) = block.ttl_out_per_sample {
            append_u32_le(&mut self.aux_scratch, words);
        }
        if let Some(ref adc) = block.board_adc_data {
            for channel in adc {
                append_u16_le(&mut self.aux_scratch, channel);
            }
        }
        if let Some(ref aux) = block.aux_data {
            for stream in aux {
                for channel in stream {
                    append_u16_le(&mut self.aux_scratch, channel);
                }
            }
        }

        let Some(writer) = self.aux_writer.as_mut() else {
            return Ok(());
        };
        writer
            .write_all(&self.aux_scratch)
            .map_err(|source| RecorderError::Io {
                path: self.aux_path.clone(),
                source,
            })
    }

    /// Flush the `.kvaux` sidecar and overwrite its placeholder header with the
    /// final channel-mapping/layout metadata.
    fn finish_aux(&mut self) -> Result<(), RecorderError> {
        let Some(mut writer) = self.aux_writer.take() else {
            return Ok(());
        };
        writer.flush().map_err(|source| RecorderError::Io {
            path: self.aux_path.clone(),
            source,
        })?;
        let json = self.kvaux_metadata_json();
        let mut file = writer.into_inner().map_err(|e| RecorderError::Io {
            path: self.aux_path.clone(),
            source: e.into_error(),
        })?;
        file.seek(SeekFrom::Start(8))
            .map_err(|source| RecorderError::Io {
                path: self.aux_path.clone(),
                source,
            })?;
        let json_bytes = json.as_bytes();
        let json_len = json_bytes.len().min(KVAUX_JSON_RESERVED);
        file.write_all(&(json_len as u32).to_le_bytes())
            .map_err(|source| RecorderError::Io {
                path: self.aux_path.clone(),
                source,
            })?;
        let mut json_block = vec![0u8; KVAUX_JSON_RESERVED];
        json_block[..json_len].copy_from_slice(&json_bytes[..json_len]);
        file.write_all(&json_block)
            .map_err(|source| RecorderError::Io {
                path: self.aux_path.clone(),
                source,
            })?;
        file.flush().map_err(|source| RecorderError::Io {
            path: self.aux_path.clone(),
            source,
        })
    }

    /// Build the `.kvaux` JSON header (channel mapping + side-channel layout).
    fn kvaux_metadata_json(&self) -> String {
        let device_id = self.device_id.as_deref().unwrap_or("");
        let backend = infer_backend(device_id);
        let layout = self.side_layout.unwrap_or_default();
        format!(
            concat!(
                "{{\n",
                "  \"format\": \"kvaux\",\n",
                "  \"format_version\": 1,\n",
                "  \"data_offset_bytes\": {},\n",
                "  \"device_id\": \"{}\",\n",
                "  \"backend\": \"{}\",\n",
                "  \"sample_rate\": {},\n",
                "  \"channel_count\": {},\n",
                "  \"samples_per_block\": {},\n",
                "  \"block_count\": {},\n",
                "  \"ttl_line_count\": {},\n",
                "  \"enabled_channels\": {},\n",
                "  \"endianness\": \"little\",\n",
                "  \"layout\": \"per_block_streams\",\n",
                "  \"side_channels\": {{\n",
                "    \"ttl_in_samples\": {},\n",
                "    \"ttl_out_samples\": {},\n",
                "    \"board_adc_channels\": {},\n",
                "    \"board_adc_samples\": {},\n",
                "    \"aux_streams\": {},\n",
                "    \"aux_channels_per_stream\": {},\n",
                "    \"aux_samples\": {}\n",
                "  }}\n",
                "}}\n"
            ),
            KVAUX_DATA_OFFSET,
            escape_json_string(device_id),
            escape_json_string(&backend),
            format_sample_rate(self.sample_rate.unwrap_or(0.0)),
            self.channel_count.unwrap_or(0),
            self.samples_per_packet.unwrap_or(0),
            self.block_count,
            self.config.ttl_line_count,
            format_usize_array(&self.config.enabled_channels),
            format_optional_usize(layout.ttl_in_samples),
            format_optional_usize(layout.ttl_out_samples),
            layout.board_adc_channels,
            layout.board_adc_samples,
            layout.aux_streams,
            layout.aux_channels_per_stream,
            layout.aux_samples,
        )
    }

    fn streaming_metadata_json(&self) -> String {
        let Some(ref device_id) = self.device_id else {
            return empty_recording_metadata_json();
        };

        let backend = infer_backend(device_id);

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
                "  \"fpga_timestamp_first\": {},\n",
                "  \"fpga_timestamp_last\": {},\n",
                "  \"host_clock_first_ns\": {},\n",
                "  \"host_clock_last_ns\": {},\n",
                "  \"written_samples\": {},\n",
                "  \"clean_stop\": true\n",
                "}}\n"
            ),
            KVRAW_DATA_OFFSET,
            escape_json_string(device_id),
            escape_json_string(&backend),
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
            self.first_timestamp_start
                .map(|ts| ts.to_string())
                .unwrap_or_else(|| "null".to_string()),
            self.last_timestamp_start
                .map(|ts| ts.to_string())
                .unwrap_or_else(|| "null".to_string()),
            json_opt_i64(self.first_host_time_ns),
            json_opt_i64(self.last_host_time_ns),
            self.written_samples
        )
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

/// Parsed header of a `.kvaux` side-channel sidecar: the channel mapping
/// (DA17) plus the per-block side-channel layout (DA1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KvauxMetadata {
    pub format_version: u32,
    pub data_offset_bytes: u64,
    pub device_id: String,
    pub backend: String,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub samples_per_block: usize,
    pub block_count: u64,
    pub ttl_line_count: usize,
    pub enabled_channels: Vec<usize>,
    pub layout: SideChannelLayout,
}

impl KvauxMetadata {
    /// Number of side-channel payload bytes one block occupies, given the
    /// fixed per-block layout.  Zero when no side channels were recorded.
    pub fn block_payload_bytes(&self) -> usize {
        let l = &self.layout;
        let ttl_in = l.ttl_in_samples.unwrap_or(0) * 4;
        let ttl_out = l.ttl_out_samples.unwrap_or(0) * 4;
        let board_adc = l.board_adc_channels * l.board_adc_samples * 2;
        let aux = l.aux_streams * l.aux_channels_per_stream * l.aux_samples * 2;
        ttl_in + ttl_out + board_adc + aux
    }
}

/// Reader for `.kvaux` side-channel sidecars (embedded JSON header + raw
/// little-endian per-block payload).
pub struct KvauxReader {
    metadata: KvauxMetadata,
    payload: Vec<u8>,
}

impl KvauxReader {
    /// Open a `.kvaux` file and parse its embedded header and payload.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RecorderError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| RecorderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if bytes.len() < KVAUX_DATA_OFFSET as usize || &bytes[..8] != KVAUX_MAGIC {
            return Err(RecorderError::MissingMetadata {
                path: path.to_path_buf(),
            });
        }
        let json_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        let json_len = json_len.min(KVAUX_JSON_RESERVED);
        let json_start = 12;
        let json_str =
            std::str::from_utf8(&bytes[json_start..json_start + json_len]).unwrap_or("{}");
        let metadata = parse_kvaux_json(json_str);
        let payload = bytes[KVAUX_DATA_OFFSET as usize..].to_vec();
        Ok(Self { metadata, payload })
    }

    pub fn metadata(&self) -> &KvauxMetadata {
        &self.metadata
    }

    /// Raw little-endian side-channel payload (concatenated per-block records).
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

impl fmt::Debug for KvauxReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KvauxReader")
            .field("metadata", &self.metadata)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

/// Minimal JSON parser for `.kvaux` headers — mirrors `parse_kvraw_json` and
/// additionally decodes the `enabled_channels` array and nested layout block.
fn parse_kvaux_json(json: &str) -> KvauxMetadata {
    let value_after = |key: &str| -> Option<usize> {
        let needle = format!("\"{key}\"");
        let mut search_from = 0;
        while let Some(rel) = json[search_from..].find(&needle) {
            let pos = search_from + rel;
            let after = &json[pos + needle.len()..];
            if let Some(rest) = after.trim_start().strip_prefix(':') {
                return Some(json.len() - rest.trim_start().len());
            }
            search_from = pos + needle.len();
        }
        None
    };

    let get_str = |key: &str| -> String {
        value_after(key)
            .and_then(|start| {
                let rest = &json[start..];
                let value = rest.strip_prefix('"')?;
                let end = value.find('"')?;
                Some(value[..end].to_string())
            })
            .unwrap_or_default()
    };

    let get_u64 = |key: &str| -> u64 {
        value_after(key)
            .and_then(|start| {
                let num: String = json[start..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                num.parse().ok()
            })
            .unwrap_or(0)
    };

    let get_f64 = |key: &str| -> f64 {
        value_after(key)
            .and_then(|start| {
                let num: String = json[start..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || matches!(*c, '.' | '-' | 'e' | 'E' | '+'))
                    .collect();
                num.parse().ok()
            })
            .unwrap_or(0.0)
    };

    let get_optional_usize = |key: &str| -> Option<usize> {
        let start = value_after(key)?;
        let rest = &json[start..];
        if rest.starts_with("null") {
            return None;
        }
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num.parse().ok()
    };

    let get_usize_array = |key: &str| -> Vec<usize> {
        value_after(key)
            .and_then(|start| {
                let rest = json[start..].trim_start();
                let inner = rest.strip_prefix('[')?;
                let end = inner.find(']')?;
                Some(
                    inner[..end]
                        .split(',')
                        .filter_map(|tok| tok.trim().parse().ok())
                        .collect(),
                )
            })
            .unwrap_or_default()
    };

    KvauxMetadata {
        format_version: get_u64("format_version") as u32,
        data_offset_bytes: {
            let off = get_u64("data_offset_bytes");
            if off > 0 { off } else { KVAUX_DATA_OFFSET }
        },
        device_id: get_str("device_id"),
        backend: get_str("backend"),
        sample_rate: get_f64("sample_rate"),
        channel_count: get_u64("channel_count") as usize,
        samples_per_block: get_u64("samples_per_block") as usize,
        block_count: get_u64("block_count"),
        ttl_line_count: get_u64("ttl_line_count") as usize,
        enabled_channels: get_usize_array("enabled_channels"),
        layout: SideChannelLayout {
            ttl_in_samples: get_optional_usize("ttl_in_samples"),
            ttl_out_samples: get_optional_usize("ttl_out_samples"),
            board_adc_channels: get_u64("board_adc_channels") as usize,
            board_adc_samples: get_u64("board_adc_samples") as usize,
            aux_streams: get_u64("aux_streams") as usize,
            aux_channels_per_stream: get_u64("aux_channels_per_stream") as usize,
            aux_samples: get_u64("aux_samples") as usize,
        },
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
