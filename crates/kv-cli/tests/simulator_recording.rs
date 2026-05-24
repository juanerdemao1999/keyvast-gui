use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use kv_cli::{
    CliCommand, CliError, SimulatorRecordingOptions, parse_args, run_directory_name_utc,
    run_simulator_recording,
};
use kv_types::{DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET};

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
    assert_eq!(
        fs::metadata(output_dir.join("recording.kvraw"))
            .expect("raw file should exist")
            .len(),
        expected_bytes
    );

    let metadata = fs::read_to_string(output_dir.join("recording.json"))
        .expect("metadata file should be readable");
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
    assert!(events.contains("0,,started,,"));
    assert!(events.contains("0,,stopped,,"));
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

    let CliCommand::SimulatorRecord(options) = command;
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
