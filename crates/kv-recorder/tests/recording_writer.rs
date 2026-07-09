use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use kv_recorder::{
    BenchmarkSummary, KVRAW_DATA_OFFSET, RecorderError, StreamingRecorder, write_benchmark_summary,
    write_integrity_summary, write_log_file, write_recording,
};
use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::{
    AcquisitionEvent, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET, IntegritySummary,
    SampleBlock,
};

#[test]
fn writes_simulator_blocks_to_kvraw_with_expected_byte_count() {
    let output_dir = unique_output_dir("byte-count");
    let blocks = next_simulator_blocks(3);

    let summary = write_recording(&output_dir, &blocks).expect("recording should write");

    assert_eq!(summary.block_count, 3);
    assert_eq!(summary.written_samples, 3 * samples_per_block());
    assert_eq!(summary.byte_count, 3 * samples_per_block() * 2);
    assert_eq!(
        fs::metadata(output_dir.join("recording.kvraw"))
            .expect("raw file metadata")
            .len(),
        3 * samples_per_block() * 2
    );

    cleanup_dir(&output_dir);
}

#[test]
fn writes_i16_samples_as_little_endian_interleaved_bytes() {
    let output_dir = unique_output_dir("little-endian");
    let block = sample_block(0, 0, 2, 2, vec![0x0102, -2, 0x0304, -32768]);

    write_recording(&output_dir, &[block]).expect("recording should write");

    let raw = fs::read(output_dir.join("recording.kvraw")).expect("raw bytes");
    assert_eq!(raw, vec![0x02, 0x01, 0xfe, 0xff, 0x04, 0x03, 0x00, 0x80]);

    cleanup_dir(&output_dir);
}

#[test]
fn writes_minimal_recording_metadata() {
    let output_dir = unique_output_dir("metadata");
    let blocks = next_simulator_blocks(2);

    write_recording(&output_dir, &blocks).expect("recording should write");

    let metadata = fs::read_to_string(output_dir.join("recording.json")).expect("metadata json");
    assert!(metadata.contains("\"format\": \"kvraw\""));
    assert!(metadata.contains("\"format_version\": 1"));
    assert!(metadata.contains("\"device_id\": \"simulator-0\""));
    assert!(metadata.contains("\"backend\": \"simulator\""));
    assert!(metadata.contains("\"sample_rate\": 30000.0"));
    assert!(metadata.contains("\"channel_count\": 64"));
    assert!(metadata.contains("\"samples_per_packet\": 64"));
    assert!(metadata.contains("\"sample_type\": \"i16\""));
    assert!(metadata.contains("\"endianness\": \"little\""));
    assert!(metadata.contains("\"layout\": \"interleaved_by_sample\""));
    assert!(metadata.contains("\"first_packet_id\": 0"));
    assert!(metadata.contains("\"last_packet_id\": 1"));
    assert!(metadata.contains("\"written_samples\": 8192"));
    assert!(metadata.contains("\"clean_stop\": true"));

    cleanup_dir(&output_dir);
}

#[test]
fn writes_machine_readable_integrity_summary() {
    let output_dir = unique_output_dir("integrity-summary");
    let summary = IntegritySummary {
        expected_packets: 4,
        observed_packets: 3,
        missing_packets: 1,
        crc_errors: 0,
        timestamp_discontinuities: 1,
        buffer_overflows: 2,
        expected_samples: 16_384,
        written_samples: 12_288,
    };

    let integrity_path =
        write_integrity_summary(&output_dir, &summary).expect("integrity summary should write");

    assert_eq!(integrity_path, output_dir.join("integrity.json"));
    let integrity_json = fs::read_to_string(integrity_path).expect("integrity json");
    assert!(integrity_json.contains("\"expected_packets\": 4"));
    assert!(integrity_json.contains("\"observed_packets\": 3"));
    assert!(integrity_json.contains("\"missing_packets\": 1"));
    assert!(integrity_json.contains("\"crc_errors\": 0"));
    assert!(integrity_json.contains("\"timestamp_discontinuities\": 1"));
    assert!(integrity_json.contains("\"buffer_overflows\": 2"));
    assert!(integrity_json.contains("\"expected_samples\": 16384"));
    assert!(integrity_json.contains("\"written_samples\": 12288"));

    cleanup_dir(&output_dir);
}

#[test]
fn writes_human_readable_log_file() {
    let output_dir = unique_output_dir("log-file");
    let lines = vec![
        "[INFO] acquisition started".to_string(),
        "[WARN] missing packet expected=1 observed=2 missing=1".to_string(),
        "[INFO] acquisition stopped cleanly".to_string(),
    ];

    let log_path = write_log_file(&output_dir, &lines).expect("log file should write");

    assert_eq!(log_path, output_dir.join("log.txt"));
    let log = fs::read_to_string(log_path).expect("log file");
    assert_eq!(
        log,
        "[INFO] acquisition started\n[WARN] missing packet expected=1 observed=2 missing=1\n[INFO] acquisition stopped cleanly\n"
    );

    cleanup_dir(&output_dir);
}

#[test]
fn writes_events_csv_file() {
    let output_dir = unique_output_dir("events-csv");
    let events = vec![
        AcquisitionEvent::Started {
            timestamp_host_ms: 0,
        },
        AcquisitionEvent::PacketMissing {
            expected_packet_id: 1,
            observed_packet_id: 2,
            missing_count: 1,
        },
        AcquisitionEvent::Stopped {
            timestamp_host_ms: 10,
        },
    ];

    let events_path =
        kv_recorder::write_events_csv(&output_dir, &events).expect("events csv should write");

    assert_eq!(events_path, output_dir.join("events.csv"));
    let events_csv = fs::read_to_string(events_path).expect("events csv");
    assert_eq!(
        events_csv,
        concat!(
            "host_time_ms,timestamp_start,event_type,value,message\n",
            "0,,started,,\n",
            ",,packet_missing,1,expected_packet_id=1 observed_packet_id=2\n",
            "10,,stopped,,\n"
        )
    );

    cleanup_dir(&output_dir);
}

#[test]
fn writes_benchmark_summary_json() {
    let output_dir = unique_output_dir("benchmark-json");
    let summary = BenchmarkSummary {
        measurement_kind: "simulator_estimate".to_string(),
        duration_seconds: 0.0064,
        channel_count: 64,
        sample_rate: 30_000.0,
        expected_samples: 16_384,
        written_samples: 12_288,
        missing_packets: 1,
        crc_errors: 0,
        timestamp_discontinuities: 1,
        byte_count: 24_576,
        average_write_mb_s: 3.84,
        max_write_latency_ms: None,
        p50_write_latency_ms: None,
        p95_write_latency_ms: None,
        p99_write_latency_ms: None,
        max_buffer_occupancy: None,
        cpu_percent_avg: None,
        memory_mb_max: None,
    };

    let benchmark_path =
        write_benchmark_summary(&output_dir, &summary).expect("benchmark summary should write");

    assert_eq!(benchmark_path, output_dir.join("benchmark.json"));
    let benchmark = fs::read_to_string(benchmark_path).expect("benchmark json");
    assert!(benchmark.contains("\"measurement_kind\": \"simulator_estimate\""));
    assert!(benchmark.contains("\"duration_seconds\": 0.006400"));
    assert!(benchmark.contains("\"channel_count\": 64"));
    assert!(benchmark.contains("\"sample_rate\": 30000.0"));
    assert!(benchmark.contains("\"expected_samples\": 16384"));
    assert!(benchmark.contains("\"written_samples\": 12288"));
    assert!(benchmark.contains("\"missing_packets\": 1"));
    assert!(benchmark.contains("\"byte_count\": 24576"));
    assert!(benchmark.contains("\"average_write_mb_s\": 3.840000"));
    assert!(benchmark.contains("\"max_write_latency_ms\": null"));

    cleanup_dir(&output_dir);
}

#[test]
fn invalid_block_is_rejected_before_files_are_written() {
    let output_dir = unique_output_dir("invalid");
    let mut block = sample_block(0, 0, 2, 2, vec![1, 2, 3, 4]);
    block.data.pop();

    let error = write_recording(&output_dir, &[block]).expect_err("invalid block should fail");

    assert!(matches!(
        error,
        RecorderError::InvalidBlock { packet_id: 0, .. }
    ));
    assert!(!output_dir.join("recording.kvraw").exists());
    assert!(!output_dir.join("recording.json").exists());

    cleanup_dir(&output_dir);
}

#[test]
fn output_path_errors_are_returned_to_the_caller() {
    let output_path = unique_output_dir("path-error");
    fs::create_dir_all(output_path.parent().expect("parent")).expect("parent dir");
    fs::write(&output_path, b"not a directory").expect("marker file");

    let error = write_recording(&output_path, &next_simulator_blocks(1))
        .expect_err("file path should not be usable as output directory");

    assert!(matches!(error, RecorderError::Io { .. }));
    fs::remove_file(&output_path).expect("cleanup marker file");
}

fn next_simulator_blocks(count: usize) -> Vec<SampleBlock> {
    let mut simulator =
        SimulatorBackend::new(SimulatorConfig::default()).expect("valid simulator config");

    (0..count)
        .map(|_| simulator.next_block().expect("simulator block"))
        .collect()
}

fn sample_block(
    packet_id: u64,
    timestamp_start: u64,
    channel_count: usize,
    samples_per_channel: usize,
    data: Vec<i16>,
) -> SampleBlock {
    SampleBlock {
        device_id: "simulator-0".to_string(),
        stream_id: 0,
        packet_id,
        timestamp_start,
        sample_rate: 30_000.0,
        channel_count,
        samples_per_channel,
        ttl_bits: 0,
        data,
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
    }
}

fn samples_per_block() -> u64 {
    (DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64
}

fn unique_output_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();

    Path::new("target")
        .join("test-runs")
        .join("kv-recorder")
        .join(format!("{name}-{nonce}"))
}

fn cleanup_dir(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("cleanup test directory");
    }
}

// --- StreamingRecorder tests ---

#[test]
fn streaming_recorder_writes_blocks_incrementally() {
    let output_dir = unique_output_dir("streaming-basic");
    let blocks = next_simulator_blocks(3);

    let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder should open");
    for block in &blocks {
        recorder
            .write_block(block)
            .expect("block write should succeed");
    }
    let summary = recorder.finish().expect("finish should succeed");

    let expected_samples = (3 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64;
    let expected_bytes = expected_samples * 2;

    assert_eq!(summary.recording.block_count, 3);
    assert_eq!(summary.recording.written_samples, expected_samples);
    assert_eq!(summary.recording.byte_count, expected_bytes);
    assert_eq!(summary.recording.first_packet_id, Some(0));
    assert_eq!(summary.recording.last_packet_id, Some(2));

    // KVRAW v2: file size = header (KVRAW_DATA_OFFSET) + raw sample bytes
    assert_eq!(
        fs::metadata(output_dir.join("recording.kvraw"))
            .expect("raw file should exist")
            .len(),
        KVRAW_DATA_OFFSET + expected_bytes
    );

    // Metadata is embedded in the kvraw file header; no separate JSON file
    assert!(
        !output_dir.join("recording.json").exists(),
        "separate JSON file should not exist in v2 format"
    );
    // Read the embedded JSON from the header (bytes 12..524 of the kvraw file)
    let kvraw_bytes = fs::read(output_dir.join("recording.kvraw")).expect("kvraw readable");
    let json_len = u32::from_le_bytes(kvraw_bytes[8..12].try_into().unwrap()) as usize;
    let metadata =
        std::str::from_utf8(&kvraw_bytes[12..12 + json_len]).expect("header JSON is valid UTF-8");
    assert!(metadata.contains("\"first_packet_id\": 0"));
    assert!(metadata.contains("\"last_packet_id\": 2"));
    assert!(metadata.contains(&format!("\"written_samples\": {expected_samples}")));

    cleanup_dir(&output_dir);
}

#[test]
fn streaming_recorder_matches_batch_recorder_output() {
    let blocks = next_simulator_blocks(4);

    let batch_dir = unique_output_dir("streaming-vs-batch-batch");
    let batch_summary = write_recording(&batch_dir, &blocks).expect("batch recording");

    let stream_dir = unique_output_dir("streaming-vs-batch-stream");
    let mut recorder = StreamingRecorder::new(&stream_dir).expect("streaming recorder");
    for block in &blocks {
        recorder.write_block(block).expect("block write");
    }
    let stream_summary = recorder.finish().expect("finish");

    assert_eq!(
        stream_summary.recording.block_count,
        batch_summary.block_count
    );
    assert_eq!(
        stream_summary.recording.written_samples,
        batch_summary.written_samples
    );
    assert_eq!(
        stream_summary.recording.byte_count,
        batch_summary.byte_count
    );
    assert_eq!(
        stream_summary.recording.first_packet_id,
        batch_summary.first_packet_id
    );
    assert_eq!(
        stream_summary.recording.last_packet_id,
        batch_summary.last_packet_id
    );

    // Batch recorder: old format (no header), just raw i16 samples.
    // Streaming recorder: KVRAW v2 (524-byte header + raw i16 samples).
    // The sample data portions should be byte-identical.
    let batch_raw = fs::read(batch_dir.join("recording.kvraw")).expect("batch raw");
    let stream_raw = fs::read(stream_dir.join("recording.kvraw")).expect("stream raw");
    let stream_samples = &stream_raw[KVRAW_DATA_OFFSET as usize..];
    assert_eq!(
        batch_raw, stream_samples,
        "sample data should be byte-identical"
    );

    cleanup_dir(&batch_dir);
    cleanup_dir(&stream_dir);
}

#[test]
fn streaming_recorder_tracks_write_latency() {
    let output_dir = unique_output_dir("streaming-latency");
    let blocks = next_simulator_blocks(5);

    let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder");
    for block in &blocks {
        recorder.write_block(block).expect("write");
    }
    let summary = recorder.finish().expect("finish");

    assert!(summary.max_write_latency_us.is_some());
    assert!(summary.latency_distribution.is_some());
    let dist = summary.latency_distribution.unwrap();
    assert_eq!(dist.count, 5);
    assert!(dist.min_us <= dist.p50_us);
    assert!(dist.p50_us <= dist.p95_us);
    assert!(dist.p95_us <= dist.p99_us);
    assert!(dist.p99_us <= dist.max_us);

    cleanup_dir(&output_dir);
}

#[test]
fn latency_distribution_computes_correct_percentiles() {
    use kv_recorder::LatencyDistribution;

    let samples: Vec<u64> = (1..=100).collect();
    let dist = LatencyDistribution::from_samples(&samples).expect("distribution should exist");

    assert_eq!(dist.count, 100);
    assert_eq!(dist.min_us, 1);
    assert_eq!(dist.max_us, 100);
    assert_eq!(dist.mean_us, 50);
    assert_eq!(dist.p50_us, 50);
    assert_eq!(dist.p95_us, 95);
    assert_eq!(dist.p99_us, 99);
}

#[test]
fn streaming_recorder_rejects_inconsistent_device_id() {
    let output_dir = unique_output_dir("streaming-inconsistent");
    let mut blocks = next_simulator_blocks(2);
    blocks[1].device_id = "wrong-device".to_string();

    let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder");
    recorder.write_block(&blocks[0]).expect("first block");
    let error = recorder
        .write_block(&blocks[1])
        .expect_err("inconsistent device should fail");

    assert!(matches!(
        error,
        RecorderError::InconsistentBlockConfig {
            field: "device_id",
            ..
        }
    ));

    cleanup_dir(&output_dir);
}

#[test]
fn kvraw_reader_round_trips_streaming_data() {
    use kv_recorder::KvrawReader;

    let output_dir = unique_output_dir("reader-round-trip");
    let blocks = next_simulator_blocks(5);
    let ch = blocks[0].channel_count;
    let sr = blocks[0].sample_rate;

    let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder");
    for block in &blocks {
        recorder.write_block(block).expect("write");
    }
    recorder.finish().expect("finish");

    let raw_path = output_dir.join("recording.kvraw");
    let mut reader = KvrawReader::open(&raw_path).expect("open kvraw");

    let meta = reader.metadata();
    assert_eq!(meta.channel_count, ch);
    assert!((meta.sample_rate - sr).abs() < 0.1);
    assert!(meta.total_frames() > 0);

    // Read first 256 frames and verify shape.
    let frames_to_read = 256.min(meta.total_frames() as usize);
    let data = reader.read_frames(0, frames_to_read).expect("read frames");
    assert_eq!(data.len(), ch * frames_to_read);

    // Read as per-channel vectors.
    let channels = reader
        .read_channels(0, frames_to_read)
        .expect("read channels");
    assert_eq!(channels.len(), ch);
    for c in &channels {
        assert_eq!(c.len(), frames_to_read);
    }

    // Verify interleaved and per-channel agree.
    for frame in 0..frames_to_read {
        for c in 0..ch {
            assert_eq!(data[frame * ch + c], channels[c][frame]);
        }
    }

    cleanup_dir(&output_dir);
}

#[test]
fn dropping_without_finish_still_leaves_a_valid_readable_file() {
    // C1 safety-net: if the recorder is dropped without an explicit finish()
    // (app quit / thread unwind), the Drop impl must rewrite the embedded header
    // so the file is a valid, readable v2 kvraw rather than the zeroed
    // placeholder that would make channel_count/offset garbage.
    use kv_recorder::KvrawReader;

    let output_dir = unique_output_dir("drop-finalize");
    let blocks = next_simulator_blocks(3);
    let ch = blocks[0].channel_count;
    let sr = blocks[0].sample_rate;

    {
        let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder");
        for block in &blocks {
            recorder.write_block(block).expect("write");
        }
        // Intentionally NO finish() — recorder is dropped here at end of scope.
    }

    let raw_path = output_dir.join("recording.kvraw");
    let mut reader = KvrawReader::open(&raw_path).expect("drop-finalized file should open");
    let meta = reader.metadata();
    assert_eq!(
        meta.format_version, 2,
        "header must be finalized, not placeholder"
    );
    assert_eq!(meta.channel_count, ch);
    assert!((meta.sample_rate - sr).abs() < 0.1);
    assert_eq!(meta.block_count, blocks.len() as u64);
    assert!(meta.total_frames() > 0);

    // The data is actually readable end-to-end.
    let frames = reader.read_frames(0, 1).expect("read first frame");
    assert_eq!(frames.len(), ch);

    cleanup_dir(&output_dir);
}

#[test]
fn kvraw_reader_errors_on_legacy_file_without_metadata() {
    use kv_recorder::KvrawReader;

    // A non-v2 file (no KEYVAST magic) with no companion .json: the channel
    // count and sample rate are unknown, so the reader must refuse rather than
    // fabricate 64ch/30kHz defaults.
    let output_dir = unique_output_dir("v1-no-meta");
    fs::create_dir_all(&output_dir).expect("mkdir");
    let raw_path = output_dir.join("legacy.kvraw");
    fs::write(&raw_path, vec![0u8; 4096]).expect("write raw");

    let error = KvrawReader::open(&raw_path).expect_err("missing metadata should be an error");
    assert!(matches!(error, RecorderError::MissingMetadata { .. }));

    cleanup_dir(&output_dir);
}

#[test]
fn kvraw_reader_errors_on_truncated_v2_header() {
    use kv_recorder::KvrawReader;

    // KEYVAST magic followed by fewer than the 4 json-length bytes: the header
    // is truncated and the reader should surface an I/O error, not panic.
    let output_dir = unique_output_dir("v2-trunc");
    fs::create_dir_all(&output_dir).expect("mkdir");
    let raw_path = output_dir.join("trunc.kvraw");
    let mut bytes = b"KEYVAST\n".to_vec();
    bytes.extend_from_slice(&[0u8; 2]);
    fs::write(&raw_path, bytes).expect("write raw");

    let error = KvrawReader::open(&raw_path).expect_err("truncated header should be an error");
    assert!(matches!(error, RecorderError::Io { .. }));

    cleanup_dir(&output_dir);
}

// ---------- M34: StreamingRecorder consistency tests ----------

#[test]
fn streaming_recorder_rejects_inconsistent_sample_rate() {
    let output_dir = unique_output_dir("streaming-bad-sr");
    let mut blocks = next_simulator_blocks(2);
    blocks[1].sample_rate = 15_000.0;

    let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder");
    recorder.write_block(&blocks[0]).expect("first block");
    let error = recorder
        .write_block(&blocks[1])
        .expect_err("inconsistent sample_rate should fail");

    assert!(matches!(
        error,
        RecorderError::InconsistentBlockConfig {
            field: "sample_rate",
            ..
        }
    ));

    cleanup_dir(&output_dir);
}

#[test]
fn streaming_recorder_rejects_inconsistent_channel_count() {
    let output_dir = unique_output_dir("streaming-bad-ch");
    let mut blocks = next_simulator_blocks(2);
    blocks[1].channel_count = 32;
    blocks[1].data = vec![0; 32 * blocks[1].samples_per_channel];

    let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder");
    recorder.write_block(&blocks[0]).expect("first block");
    let error = recorder
        .write_block(&blocks[1])
        .expect_err("inconsistent channel_count should fail");

    assert!(matches!(
        error,
        RecorderError::InconsistentBlockConfig {
            field: "channel_count",
            ..
        }
    ));

    cleanup_dir(&output_dir);
}

#[test]
fn streaming_recorder_rejects_inconsistent_samples_per_channel() {
    let output_dir = unique_output_dir("streaming-bad-spc");
    let mut blocks = next_simulator_blocks(2);
    let orig_ch = blocks[1].channel_count;
    blocks[1].samples_per_channel *= 2;
    blocks[1].data = vec![0; orig_ch * blocks[1].samples_per_channel];

    let mut recorder = StreamingRecorder::new(&output_dir).expect("recorder");
    recorder.write_block(&blocks[0]).expect("first block");
    let error = recorder
        .write_block(&blocks[1])
        .expect_err("inconsistent samples_per_channel should fail");

    assert!(matches!(
        error,
        RecorderError::InconsistentBlockConfig {
            field: "samples_per_channel",
            ..
        }
    ));

    cleanup_dir(&output_dir);
}
