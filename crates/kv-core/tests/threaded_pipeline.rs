use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kv_core::pipeline::{
    PipelineConfig, PipelineError, StreamingPipelineConfig, run_streaming_pipeline,
    run_threaded_pipeline,
};
use kv_recorder::KVRAW_DATA_OFFSET;
use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::{DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET, DeviceConfig, SampleBlock};

fn default_pipeline_config(requested_blocks: usize) -> PipelineConfig {
    PipelineConfig {
        device: DeviceConfig::simulator_default(),
        requested_blocks,
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
    }
}

#[test]
fn pipeline_records_all_requested_blocks() {
    let config = default_pipeline_config(10);
    let simulator = SimulatorBackend::default();
    let source = make_source(simulator);

    let result =
        run_threaded_pipeline(&config, source).expect("pipeline should complete successfully");

    assert_eq!(result.recorded_blocks.len(), 10);
    assert_eq!(result.integrity.summary.observed_packets, 10);
    assert_eq!(result.integrity.summary.missing_packets, 0);
    assert!(result.integrity.packet_gaps.is_empty());
    assert_eq!(
        result.integrity.summary.written_samples,
        (10 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64
    );
}

#[test]
fn pipeline_reports_wall_clock_timing() {
    let config = default_pipeline_config(4);
    let simulator = SimulatorBackend::default();
    let source = make_source(simulator);

    let result =
        run_threaded_pipeline(&config, source).expect("pipeline should complete successfully");

    assert!(result.timing.wall_clock_seconds > 0.0);
    assert!(result.timing.wall_clock_seconds < 10.0);
}

#[test]
fn pipeline_preview_drops_without_affecting_recorder() {
    let config = PipelineConfig {
        device: DeviceConfig::simulator_default(),
        requested_blocks: 32,
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 2,
    };
    let simulator = SimulatorBackend::default();
    let source = make_source(simulator);

    let result =
        run_threaded_pipeline(&config, source).expect("pipeline should complete successfully");

    assert_eq!(result.recorded_blocks.len(), 32);
    assert_eq!(result.recorder_status.dropped_blocks, 0);
    assert_eq!(result.recorder_status.name, "recorder");
    assert_eq!(result.preview_status.name, "preview");
}

#[test]
fn pipeline_recorder_and_preview_statuses_are_independent() {
    let config = default_pipeline_config(5);
    let simulator = SimulatorBackend::default();
    let source = make_source(simulator);

    let result =
        run_threaded_pipeline(&config, source).expect("pipeline should complete successfully");

    assert_eq!(result.recorder_status.pushed_blocks, 5);
    assert_eq!(result.preview_status.pushed_blocks, 5);
    assert_eq!(result.recorder_status.popped_blocks, 5);
}

#[test]
fn pipeline_propagates_producer_error() {
    let config = default_pipeline_config(4);
    let simulator = SimulatorBackend::default();
    let mut emitted = 0_usize;
    let source = move || -> Result<SampleBlock, String> {
        if emitted >= 2 {
            return Err("synthetic producer failure".to_string());
        }
        emitted += 1;
        let mut sim = simulator.clone();
        for _ in 0..emitted {
            let _ = sim.next_block();
        }
        // Use a fresh default simulator for deterministic output per block.
        SimulatorBackend::default()
            .next_block()
            .map_err(|e| e.to_string())
    };

    let error = run_threaded_pipeline(&config, source).expect_err("pipeline should fail");
    match error {
        PipelineError::ProducerFailed {
            message,
            blocks_acquired,
        } => {
            assert_eq!(message, "synthetic producer failure");
            // The synthetic producer emits a few blocks before failing, so the
            // count must be recorded rather than lost.
            assert!(blocks_acquired > 0);
        }
        other => panic!("expected ProducerFailed, got: {other}"),
    }
}

#[test]
fn pipeline_detects_packet_gaps_from_dropped_packets() {
    let simulator_config = SimulatorConfig {
        drop_packet_ids: vec![2],
        ..SimulatorConfig::default()
    };
    let config = default_pipeline_config(4);
    let simulator = SimulatorBackend::new(simulator_config).expect("valid simulator config");
    let source = make_source(simulator);

    let result =
        run_threaded_pipeline(&config, source).expect("pipeline should complete successfully");

    assert_eq!(result.recorded_blocks.len(), 4);
    assert_eq!(result.integrity.summary.missing_packets, 1);
    assert_eq!(result.integrity.packet_gaps.len(), 1);
    assert_eq!(result.integrity.packet_gaps[0].expected_packet_id, 2);
    assert_eq!(result.integrity.packet_gaps[0].observed_packet_id, 3);
}

fn make_source(
    mut simulator: SimulatorBackend,
) -> impl FnMut() -> Result<SampleBlock, String> + Send + 'static {
    move || simulator.next_block().map_err(|e| e.to_string())
}

// --- Streaming pipeline tests ---

fn streaming_config(requested_blocks: usize, output_dir: PathBuf) -> StreamingPipelineConfig {
    StreamingPipelineConfig {
        device: DeviceConfig::simulator_default(),
        requested_blocks,
        output_dir,
        recorder_capacity_blocks: 128,
        preview_capacity_blocks: 16,
    }
}

fn unique_output_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-output")
        .join(format!("{name}-{}-{nanos}", std::process::id()))
}

fn cleanup_dir(path: &Path) {
    if path.exists() {
        std::fs::remove_dir_all(path).expect("cleanup");
    }
}

#[test]
fn streaming_pipeline_writes_blocks_to_disk() {
    let output_dir = unique_output_dir("stream-basic");
    let config = streaming_config(8, output_dir.clone());
    let source = make_source(SimulatorBackend::default());

    let result =
        run_streaming_pipeline(&config, source).expect("streaming pipeline should succeed");

    assert_eq!(result.recording.block_count, 8);
    assert_eq!(result.integrity.summary.observed_packets, 8);
    assert_eq!(result.integrity.summary.missing_packets, 0);
    assert!(result.timing.wall_clock_seconds > 0.0);

    let expected_samples = (8 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64;
    assert_eq!(result.recording.written_samples, expected_samples);

    // KVRAW v2: file size = header (KVRAW_DATA_OFFSET) + raw sample bytes
    let raw_size = std::fs::metadata(output_dir.join("recording.kvraw"))
        .expect("raw file")
        .len();
    assert_eq!(raw_size, KVRAW_DATA_OFFSET + expected_samples * 2);

    // Metadata is embedded in kvraw header; no separate JSON file in v2 format
    assert!(!output_dir.join("recording.json").exists());

    cleanup_dir(&output_dir);
}

#[test]
fn streaming_pipeline_detects_packet_gaps() {
    let output_dir = unique_output_dir("stream-gap");
    let config = streaming_config(4, output_dir.clone());
    let simulator_config = SimulatorConfig {
        drop_packet_ids: vec![2],
        ..SimulatorConfig::default()
    };
    let simulator = SimulatorBackend::new(simulator_config).expect("valid config");
    let source = make_source(simulator);

    let result = run_streaming_pipeline(&config, source).expect("pipeline should succeed");

    assert_eq!(result.integrity.summary.missing_packets, 1);
    assert_eq!(result.integrity.packet_gaps.len(), 1);
    assert_eq!(result.integrity.packet_gaps[0].expected_packet_id, 2);

    cleanup_dir(&output_dir);
}

#[test]
fn streaming_pipeline_reports_write_latency() {
    let output_dir = unique_output_dir("stream-latency");
    let config = streaming_config(5, output_dir.clone());
    let source = make_source(SimulatorBackend::default());

    let result = run_streaming_pipeline(&config, source).expect("pipeline should succeed");

    assert!(result.max_write_latency_us.is_some());

    cleanup_dir(&output_dir);
}

#[test]
fn streaming_pipeline_recorder_has_zero_drops_with_large_buffer() {
    let output_dir = unique_output_dir("stream-no-drop");
    let config = streaming_config(10, output_dir.clone());
    let source = make_source(SimulatorBackend::default());

    let result = run_streaming_pipeline(&config, source).expect("pipeline should succeed");

    assert_eq!(result.recorder_status.dropped_blocks, 0);
    assert_eq!(result.recorder_status.pushed_blocks, 10);

    cleanup_dir(&output_dir);
}
