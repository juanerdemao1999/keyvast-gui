use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use kv_cli::{
    BenchmarkOptions, CliCommand, CliError, RhdSmokeOptions, SimulatorPipelineOptions,
    SimulatorRecordingOptions, blocks_for_duration, parse_args, run_benchmark,
    run_directory_name_utc, run_rhd_smoke, run_simulator_pipeline, run_simulator_recording,
    run_simulator_stream,
};
use kv_types::{DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLE_RATE, DEFAULT_SAMPLES_PER_PACKET};

#[test]
fn simulator_recording_writes_raw_metadata_and_integrity_summary() {
    let output_dir = unique_output_dir("clean-recording");
    let result = run_simulator_recording(SimulatorRecordingOptions {
        output_dir: output_dir.clone(),
        blocks: 3,
        drop_packet_ids: Vec::new(),
    })
    .expect("simulator recording should succeed");

    let expected_samples = (3 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64;
    let expected_bytes = expected_samples * 2;

    assert_eq!(result.acquisition.acquired_blocks, 3);
    assert_eq!(result.recording.byte_count, expected_bytes);
    assert_eq!(result.integrity.summary.observed_packets, 3);
    assert_eq!(result.integrity.summary.missing_packets, 0);
    assert_eq!(result.integrity.summary.written_samples, expected_samples);
    // Batch recorder (write_recording) still uses separate JSON + raw files
    assert_eq!(
        fs::metadata(output_dir.join("recording.kvraw"))
            .expect("raw file should exist")
            .len(),
        expected_bytes
    );

    let metadata = fs::read_to_string(output_dir.join("recording.json"))
        .expect("metadata file should be readable");
    assert!(metadata.contains("\"backend\": \"simulator\""));
    assert!(metadata.contains("\"first_packet_id\": 0"));
    assert!(metadata.contains("\"last_packet_id\": 2"));
    assert!(metadata.contains(&format!("\"written_samples\": {expected_samples}")));

    let integrity = fs::read_to_string(output_dir.join("integrity.json"))
        .expect("integrity summary should be readable");
    assert!(integrity.contains("\"expected_packets\": 3"));
    assert!(integrity.contains("\"observed_packets\": 3"));
    assert!(integrity.contains("\"missing_packets\": 0"));
    assert!(integrity.contains(&format!("\"expected_samples\": {expected_samples}")));
    assert!(integrity.contains(&format!("\"written_samples\": {expected_samples}")));

    let log = fs::read_to_string(output_dir.join("log.txt")).expect("log file should be readable");
    assert!(log.contains("[INFO] acquisition started"));
    assert!(log.contains("[INFO] recorder flushed"));
    assert!(log.contains("[INFO] acquisition stopped cleanly"));

    let events =
        fs::read_to_string(output_dir.join("events.csv")).expect("events csv should be readable");
    assert!(events.contains("host_time_ms,timestamp_start,event_type,value,message"));
    assert!(events.contains(",,started,,"));
    assert!(events.contains(",,stopped,,"));
    assert!(!events.contains("packet_missing"));

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark json should be readable");
    assert!(benchmark.contains("\"measurement_kind\": \"simulator_estimate\""));
    assert!(benchmark.contains("\"channel_count\": 64"));
    assert!(benchmark.contains("\"sample_rate\": 30000.0"));
    assert!(benchmark.contains(&format!("\"written_samples\": {expected_samples}")));
    assert!(benchmark.contains(&format!("\"byte_count\": {expected_bytes}")));

    cleanup_dir(&output_dir);
}

#[test]
fn simulator_recording_reports_injected_packet_loss() {
    let output_dir = unique_output_dir("packet-loss");
    let result = run_simulator_recording(SimulatorRecordingOptions {
        output_dir: output_dir.clone(),
        blocks: 3,
        drop_packet_ids: vec![1],
    })
    .expect("simulator recording with injected packet loss should succeed");

    assert_eq!(result.acquisition.acquired_blocks, 3);
    assert_eq!(result.acquisition.status.last_packet_id, Some(3));
    assert_eq!(result.integrity.summary.observed_packets, 3);
    assert_eq!(result.integrity.summary.missing_packets, 1);
    assert_eq!(result.integrity.packet_gaps.len(), 1);
    assert_eq!(result.integrity.packet_gaps[0].expected_packet_id, 1);
    assert_eq!(result.integrity.packet_gaps[0].observed_packet_id, 2);

    let samples_per_block = (DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64;
    let integrity = fs::read_to_string(output_dir.join("integrity.json"))
        .expect("integrity summary should be readable");
    assert!(integrity.contains("\"expected_packets\": 4"));
    assert!(integrity.contains("\"observed_packets\": 3"));
    assert!(integrity.contains("\"missing_packets\": 1"));
    assert!(integrity.contains(&format!("\"expected_samples\": {}", 4 * samples_per_block)));
    assert!(integrity.contains(&format!("\"written_samples\": {}", 3 * samples_per_block)));

    let log = fs::read_to_string(output_dir.join("log.txt")).expect("log file should be readable");
    assert!(log.contains("[WARN] missing packet expected=1 observed=2 missing=1"));

    let events =
        fs::read_to_string(output_dir.join("events.csv")).expect("events csv should be readable");
    assert!(events.contains(",,packet_missing,1,expected_packet_id=1 observed_packet_id=2"));

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark json should be readable");
    assert!(benchmark.contains("\"expected_samples\": 16384"));
    assert!(benchmark.contains("\"written_samples\": 12288"));
    assert!(benchmark.contains("\"missing_packets\": 1"));
    assert!(benchmark.contains("\"average_write_mb_s\": 3.840000"));

    cleanup_dir(&output_dir);
}

#[test]
fn zero_block_recording_is_rejected_before_creating_output() {
    let output_dir = unique_output_dir("zero-blocks");
    let error = run_simulator_recording(SimulatorRecordingOptions {
        output_dir: output_dir.clone(),
        blocks: 0,
        drop_packet_ids: Vec::new(),
    })
    .expect_err("zero block recording should be rejected");

    assert!(matches!(error, CliError::InvalidBlockCount { blocks: 0 }));
    assert!(!output_dir.exists());
}

#[test]
fn run_directory_name_uses_documented_timestamp_format() {
    let name = run_directory_name_utc(UNIX_EPOCH).expect("unix epoch should format");

    assert_eq!(name, "run-19700101-000000");
}

#[test]
fn simulator_record_parse_uses_default_run_directory_when_output_is_omitted() {
    let command = parse_args(["simulator-record", "--blocks", "2"]).expect("args should parse");

    let CliCommand::SimulatorRecord(options) = command else {
        panic!("expected SimulatorRecord command");
    };
    assert_eq!(options.blocks, 2);

    let dir_name = options
        .output_dir
        .file_name()
        .expect("default output dir should have a final component")
        .to_string_lossy();

    assert!(dir_name.starts_with("run-"));
    assert_eq!(dir_name.len(), "run-YYYYMMDD-HHMMSS".len());
    assert!(
        dir_name[4..12]
            .chars()
            .all(|character| character.is_ascii_digit())
    );
    assert_eq!(&dir_name[12..13], "-");
    assert!(
        dir_name[13..19]
            .chars()
            .all(|character| character.is_ascii_digit())
    );
}

#[test]
fn kv_acq_binary_runs_simulator_record_command() {
    let output_dir = unique_output_dir("binary-recording");
    let binary = env!("CARGO_BIN_EXE_kv-acq");

    let output = Command::new(binary)
        .arg("simulator-record")
        .arg("--blocks")
        .arg("2")
        .arg("--output")
        .arg(&output_dir)
        .output()
        .expect("kv-acq should run");

    assert!(
        output.status.success(),
        "kv-acq failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_dir.join("recording.kvraw").exists());
    assert!(output_dir.join("integrity.json").exists());
    assert!(output_dir.join("log.txt").exists());
    assert!(output_dir.join("events.csv").exists());
    assert!(output_dir.join("benchmark.json").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("acquired_blocks=2"));

    cleanup_dir(&output_dir);
}

#[test]
fn simulator_pipeline_writes_all_output_files_with_measured_timing() {
    let output_dir = unique_output_dir("pipeline-clean");
    let result = run_simulator_pipeline(SimulatorPipelineOptions {
        output_dir: output_dir.clone(),
        blocks: 4,
        drop_packet_ids: Vec::new(),
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
    })
    .expect("simulator pipeline should succeed");

    let expected_samples = (4 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64;
    let expected_bytes = expected_samples * 2;

    assert_eq!(result.recording.byte_count, expected_bytes);
    assert_eq!(result.integrity.summary.observed_packets, 4);
    assert_eq!(result.integrity.summary.missing_packets, 0);
    assert!(result.timing.wall_clock_seconds > 0.0);
    assert_eq!(result.recorder_dropped_blocks, 0);

    assert!(output_dir.join("recording.kvraw").exists());
    assert!(output_dir.join("recording.json").exists());
    assert!(output_dir.join("integrity.json").exists());
    assert!(output_dir.join("log.txt").exists());
    assert!(output_dir.join("events.csv").exists());

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark json should be readable");
    assert!(benchmark.contains("\"measurement_kind\": \"measured\""));
    assert!(benchmark.contains("\"channel_count\": 64"));
    assert!(benchmark.contains(&format!("\"written_samples\": {expected_samples}")));

    cleanup_dir(&output_dir);
}

#[test]
fn simulator_pipeline_detects_packet_loss_with_measured_timing() {
    let output_dir = unique_output_dir("pipeline-loss");
    let result = run_simulator_pipeline(SimulatorPipelineOptions {
        output_dir: output_dir.clone(),
        blocks: 4,
        drop_packet_ids: vec![1],
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
    })
    .expect("simulator pipeline with packet loss should succeed");

    assert_eq!(result.integrity.summary.missing_packets, 1);
    assert!(result.timing.wall_clock_seconds > 0.0);

    let integrity = fs::read_to_string(output_dir.join("integrity.json"))
        .expect("integrity summary should be readable");
    assert!(integrity.contains("\"missing_packets\": 1"));

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark json should be readable");
    assert!(benchmark.contains("\"measurement_kind\": \"measured\""));

    cleanup_dir(&output_dir);
}

#[test]
fn kv_acq_binary_runs_simulator_pipeline_command() {
    let output_dir = unique_output_dir("binary-pipeline");
    let binary = env!("CARGO_BIN_EXE_kv-acq");

    let output = Command::new(binary)
        .arg("simulator-pipeline")
        .arg("--blocks")
        .arg("3")
        .arg("--output")
        .arg(&output_dir)
        .output()
        .expect("kv-acq should run");

    assert!(
        output.status.success(),
        "kv-acq simulator-pipeline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("measurement_kind=measured"));
    assert!(stdout.contains("wall_clock_seconds="));
    assert!(output_dir.join("recording.kvraw").exists());
    assert!(output_dir.join("benchmark.json").exists());

    cleanup_dir(&output_dir);
}

#[test]
fn simulator_stream_writes_all_output_files_with_streaming_benchmark() {
    let output_dir = unique_output_dir("stream-clean");
    let result = run_simulator_stream(SimulatorPipelineOptions {
        output_dir: output_dir.clone(),
        blocks: 4,
        drop_packet_ids: Vec::new(),
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
    })
    .expect("simulator stream should succeed");

    let expected_samples = (4 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64;
    let expected_bytes = expected_samples * 2;

    assert_eq!(result.recording.byte_count, expected_bytes);
    assert_eq!(result.integrity.summary.observed_packets, 4);
    assert_eq!(result.integrity.summary.missing_packets, 0);
    assert!(result.timing.wall_clock_seconds > 0.0);
    assert_eq!(result.recorder_dropped_blocks, 0);

    assert!(output_dir.join("recording.kvraw").exists());
    // run_simulator_stream uses StreamingRecorder (KVRAW v2): metadata embedded, no JSON file
    assert!(!output_dir.join("recording.json").exists());
    assert!(output_dir.join("integrity.json").exists());
    assert!(output_dir.join("log.txt").exists());
    assert!(output_dir.join("events.csv").exists());

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark json should be readable");
    assert!(benchmark.contains("\"measurement_kind\": \"measured_streaming\""));
    assert!(benchmark.contains("\"channel_count\": 64"));
    assert!(benchmark.contains(&format!("\"written_samples\": {expected_samples}")));

    cleanup_dir(&output_dir);
}

#[test]
fn simulator_stream_detects_packet_loss() {
    let output_dir = unique_output_dir("stream-loss");
    let result = run_simulator_stream(SimulatorPipelineOptions {
        output_dir: output_dir.clone(),
        blocks: 4,
        drop_packet_ids: vec![1],
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
    })
    .expect("simulator stream with packet loss should succeed");

    assert_eq!(result.integrity.summary.missing_packets, 1);
    assert!(result.timing.wall_clock_seconds > 0.0);

    let integrity = fs::read_to_string(output_dir.join("integrity.json"))
        .expect("integrity summary should be readable");
    assert!(integrity.contains("\"missing_packets\": 1"));

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark json should be readable");
    assert!(benchmark.contains("\"measurement_kind\": \"measured_streaming\""));

    cleanup_dir(&output_dir);
}

#[test]
fn kv_acq_binary_runs_simulator_stream_command() {
    let output_dir = unique_output_dir("binary-stream");
    let binary = env!("CARGO_BIN_EXE_kv-acq");

    let output = Command::new(binary)
        .arg("simulator-stream")
        .arg("--blocks")
        .arg("3")
        .arg("--output")
        .arg(&output_dir)
        .output()
        .expect("kv-acq should run");

    assert!(
        output.status.success(),
        "kv-acq simulator-stream failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("measurement_kind=measured_streaming"));
    assert!(stdout.contains("wall_clock_seconds="));
    assert!(output_dir.join("recording.kvraw").exists());
    assert!(output_dir.join("benchmark.json").exists());

    cleanup_dir(&output_dir);
}

#[test]
fn blocks_for_duration_computes_correct_block_count() {
    // 10 seconds at 30 kHz with 64 samples per packet → 10 / (64/30000) = 4687.5 → ceil = 4688
    let blocks = blocks_for_duration(10.0, 30_000.0, 64);
    assert_eq!(blocks, 4688);

    // Edge case: zero duration
    assert_eq!(blocks_for_duration(0.0, 30_000.0, 64), 0);

    // Exact multiple: 1 second at 1000 Hz with 100 samples per packet → 10 blocks
    assert_eq!(blocks_for_duration(1.0, 1000.0, 100), 10);
}

#[test]
fn benchmark_smoke_preset_runs_short_duration() {
    let output_dir = unique_output_dir("bench-smoke");
    // Use a very short custom duration (0.01s) to keep the test fast
    let result = run_benchmark(BenchmarkOptions {
        output_dir: output_dir.clone(),
        duration_seconds: 0.01,
        channel_count: DEFAULT_CHANNEL_COUNT,
        sample_rate: DEFAULT_SAMPLE_RATE,
        samples_per_packet: DEFAULT_SAMPLES_PER_PACKET,
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
        drop_packet_ids: Vec::new(),
    })
    .expect("benchmark should succeed");

    assert!(result.computed_block_count > 0);
    assert_eq!(result.requested_duration_seconds, 0.01);
    assert!(result.timing.wall_clock_seconds > 0.0);
    assert_eq!(result.integrity.summary.missing_packets, 0);
    assert_eq!(result.recorder_dropped_blocks, 0);

    assert!(output_dir.join("recording.kvraw").exists());
    assert!(output_dir.join("integrity.json").exists());
    assert!(output_dir.join("benchmark.json").exists());
    assert!(output_dir.join("log.txt").exists());
    assert!(output_dir.join("events.csv").exists());

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark json should be readable");
    assert!(benchmark.contains("\"measurement_kind\": \"measured_streaming\""));

    cleanup_dir(&output_dir);
}

#[test]
fn benchmark_detects_injected_packet_loss() {
    let output_dir = unique_output_dir("bench-loss");
    let result = run_benchmark(BenchmarkOptions {
        output_dir: output_dir.clone(),
        duration_seconds: 0.01,
        channel_count: DEFAULT_CHANNEL_COUNT,
        sample_rate: DEFAULT_SAMPLE_RATE,
        samples_per_packet: DEFAULT_SAMPLES_PER_PACKET,
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
        drop_packet_ids: vec![2],
    })
    .expect("benchmark with packet loss should succeed");

    assert_eq!(result.integrity.summary.missing_packets, 1);

    let integrity = fs::read_to_string(output_dir.join("integrity.json"))
        .expect("integrity summary should be readable");
    assert!(integrity.contains("\"missing_packets\": 1"));

    cleanup_dir(&output_dir);
}

#[test]
fn benchmark_parse_preset_smoke() {
    let command = parse_args(["benchmark", "--preset", "smoke", "--output", "test-dir"])
        .expect("args should parse");

    let CliCommand::Benchmark(options) = command else {
        panic!("expected Benchmark command");
    };
    assert_eq!(options.duration_seconds, 10.0);
    assert_eq!(options.channel_count, DEFAULT_CHANNEL_COUNT);
}

#[test]
fn benchmark_parse_preset_stress_128_overrides_channels() {
    let command = parse_args([
        "benchmark",
        "--preset",
        "stress-128",
        "--output",
        "test-dir",
    ])
    .expect("args should parse");

    let CliCommand::Benchmark(options) = command else {
        panic!("expected Benchmark command");
    };
    assert_eq!(options.duration_seconds, 600.0);
    assert_eq!(options.channel_count, 128);
}

#[test]
fn benchmark_parse_custom_duration_and_channels() {
    let command = parse_args([
        "benchmark",
        "--duration",
        "30",
        "--channels",
        "256",
        "--output",
        "test-dir",
    ])
    .expect("args should parse");

    let CliCommand::Benchmark(options) = command else {
        panic!("expected Benchmark command");
    };
    assert_eq!(options.duration_seconds, 30.0);
    assert_eq!(options.channel_count, 256);
}

#[test]
fn kv_acq_binary_runs_benchmark_command() {
    let output_dir = unique_output_dir("binary-benchmark");
    let binary = env!("CARGO_BIN_EXE_kv-acq");

    let output = Command::new(binary)
        .arg("benchmark")
        .arg("--duration")
        .arg("0.01")
        .arg("--output")
        .arg(&output_dir)
        .output()
        .expect("kv-acq should run");

    assert!(
        output.status.success(),
        "kv-acq benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("measurement_kind=measured_streaming"));
    assert!(stdout.contains("wall_clock_seconds="));
    assert!(stdout.contains("computed_block_count="));
    assert!(output_dir.join("recording.kvraw").exists());
    assert!(output_dir.join("benchmark.json").exists());

    cleanup_dir(&output_dir);
}

#[test]
fn rhd_smoke_parse_accepts_raw_input_stream_count_and_default_bitfile() {
    let command = parse_args([
        "rhd-smoke",
        "--blocks",
        "2",
        "--streams",
        "1",
        "--raw-input",
        "capture.bin",
        "--output",
        "rhd-out",
    ])
    .expect("args should parse");

    let CliCommand::RhdSmoke(options) = command else {
        panic!("expected RhdSmoke command");
    };

    assert_eq!(options.blocks, 2);
    assert_eq!(options.enabled_streams, 1);
    assert_eq!(options.raw_input, Some(PathBuf::from("capture.bin")));
    assert_eq!(options.output_dir, PathBuf::from("rhd-out"));
    assert!(
        options
            .bitfile_path
            .ends_with("keyvast_260607_with_UART.bit")
    );
}

#[test]
fn rhd_smoke_raw_input_writes_rhd_backend_metadata() {
    let output_dir = unique_output_dir("rhd-raw-smoke");
    fs::create_dir_all(&output_dir).expect("test output dir should be creatable");
    let raw_path = output_dir.join("capture.bin");
    fs::write(&raw_path, build_rhd_raw_blocks(1, 1)).expect("raw capture should be writable");

    let result = run_rhd_smoke(RhdSmokeOptions {
        output_dir: output_dir.clone(),
        blocks: 1,
        enabled_streams: 1,
        raw_input: Some(raw_path),
        bitfile_path: PathBuf::from("unused.bit"),
        frontpanel_dll_path: None,
        serial: None,
    })
    .expect("raw RHD smoke should parse");

    assert!(!result.hardware);
    assert_eq!(result.acquisition.acquired_blocks, 1);
    assert_eq!(result.recording.written_samples, 32 * 256);
    assert_eq!(result.integrity.summary.missing_packets, 0);

    let metadata = fs::read_to_string(output_dir.join("recording.json"))
        .expect("metadata file should be readable");
    assert!(metadata.contains("\"device_id\": \"rhd-xem7310\""));
    assert!(metadata.contains("\"backend\": \"rhd-frontpanel\""));
    assert!(metadata.contains("\"channel_count\": 32"));

    let benchmark = fs::read_to_string(output_dir.join("benchmark.json"))
        .expect("benchmark file should be readable");
    assert!(benchmark.contains("\"measurement_kind\": \"rhd_raw_input\""));

    cleanup_dir(&output_dir);
}

fn unique_output_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-output")
        .join(format!("{name}-{}-{nanos}", std::process::id()))
}

fn cleanup_dir(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("test output directory should be removable");
    }
}

fn build_rhd_raw_blocks(block_count: usize, enabled_streams: usize) -> Vec<u8> {
    const SAMPLES_PER_BLOCK: usize = 256;
    const CHANNELS_PER_STREAM: usize = 32;
    const HEADER_MAGIC: u64 = 0xd7a2_2aaa_3813_2a53;

    let filler = (4 - enabled_streams % 4) % 4;
    let words_per_frame = 4 + 2 + enabled_streams * (CHANNELS_PER_STREAM + 3) + filler + 8 + 2;
    let mut raw = Vec::with_capacity(block_count * SAMPLES_PER_BLOCK * words_per_frame * 2);

    for block in 0..block_count {
        for sample in 0..SAMPLES_PER_BLOCK {
            raw.extend_from_slice(&HEADER_MAGIC.to_le_bytes());
            let timestamp = (block * SAMPLES_PER_BLOCK + sample) as u32;
            raw.extend_from_slice(&timestamp.to_le_bytes());

            for _ in 0..3 {
                for stream in 0..enabled_streams {
                    raw.extend_from_slice(&(0x0100_u16 + stream as u16).to_le_bytes());
                }
            }

            for channel in 0..CHANNELS_PER_STREAM {
                for stream in 0..enabled_streams {
                    let signed = (stream * 1000 + channel) as i32;
                    raw.extend_from_slice(&((signed + 32_768) as u16).to_le_bytes());
                }
            }

            for _ in 0..((4 - enabled_streams % 4) % 4) {
                raw.extend_from_slice(&0_u16.to_le_bytes());
            }
            for _ in 0..8 {
                raw.extend_from_slice(&0_u16.to_le_bytes());
            }
            raw.extend_from_slice(&0x0001_u16.to_le_bytes());
            raw.extend_from_slice(&0_u16.to_le_bytes());
        }
    }

    raw
}
