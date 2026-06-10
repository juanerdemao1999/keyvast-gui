use kv_core::{AcquisitionRunError, run_fixed_blocks};
use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::{
    AcquisitionState, DEFAULT_CHANNEL_COUNT, DEFAULT_DEVICE_ID, DEFAULT_SAMPLE_RATE,
    DEFAULT_SAMPLES_PER_PACKET, DeviceBackendKind, DeviceConfig, SampleBlock,
};

#[test]
fn simulator_backed_run_returns_requested_block_count() {
    let simulator_config = SimulatorConfig::default();
    let device_config = simulator_config.device.clone();
    let mut simulator = SimulatorBackend::new(simulator_config).expect("valid simulator config");

    let run = run_fixed_blocks(&device_config, 3, &mut || simulator.next_block())
        .expect("fixed simulator run should succeed");

    assert_eq!(run.blocks.len(), 3);
    assert_eq!(run.summary.requested_blocks, 3);
    assert_eq!(run.summary.acquired_blocks, 3);
    assert_eq!(run.summary.state, AcquisitionState::Stopped);
    assert_eq!(
        run.summary.state_history,
        vec![
            AcquisitionState::DeviceConnected,
            AcquisitionState::Configured,
            AcquisitionState::Acquiring,
            AcquisitionState::Stopping,
            AcquisitionState::Stopped,
        ]
    );
    assert_eq!(run.summary.status.last_packet_id, Some(2));
    assert!(run.integrity.packet_gaps.is_empty());
}

#[test]
fn summary_includes_device_status_and_sample_counters() {
    let simulator_config = SimulatorConfig::default();
    let device_config = simulator_config.device.clone();
    let mut simulator = SimulatorBackend::new(simulator_config).expect("valid simulator config");

    let run = run_fixed_blocks(&device_config, 2, &mut || simulator.next_block())
        .expect("fixed simulator run should succeed");

    assert_eq!(
        run.summary.sample_values,
        (2 * DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64
    );
    assert_eq!(run.summary.status.device_id, DEFAULT_DEVICE_ID);
    assert_eq!(run.summary.status.backend, DeviceBackendKind::Simulator);
    assert!(run.summary.status.connected);
    assert!(run.summary.status.configured);
    assert!(!run.summary.status.acquiring);
    assert_eq!(run.summary.status.sample_rate, DEFAULT_SAMPLE_RATE);
    assert_eq!(run.summary.status.channel_count, DEFAULT_CHANNEL_COUNT);
    assert_eq!(
        run.summary.status.packet_rate_hz,
        DEFAULT_SAMPLE_RATE / DEFAULT_SAMPLES_PER_PACKET as f64
    );
    assert_eq!(run.summary.status.last_error, None);
    assert_eq!(run.integrity.summary.observed_packets, 2);
    assert_eq!(
        run.integrity.summary.written_samples,
        run.summary.sample_values
    );
}

#[test]
fn backend_errors_move_run_into_explicit_error_path() {
    let device_config = DeviceConfig::simulator_default();
    let mut simulator = SimulatorBackend::default();
    let mut emitted_blocks = 0;
    let mut backend = || {
        if emitted_blocks == 0 {
            emitted_blocks += 1;
            Ok(simulator
                .next_block()
                .expect("default simulator should emit a block"))
        } else {
            Err("synthetic read failure")
        }
    };

    let error =
        run_fixed_blocks(&device_config, 2, &mut backend).expect_err("second read should fail");

    let AcquisitionRunError::BackendRead { summary, message } = error else {
        panic!("expected backend read error");
    };

    assert_eq!(message, "synthetic read failure");
    assert_eq!(summary.state, AcquisitionState::Error);
    assert_eq!(
        summary.state_history,
        vec![
            AcquisitionState::DeviceConnected,
            AcquisitionState::Configured,
            AcquisitionState::Acquiring,
            AcquisitionState::Error,
        ]
    );
    assert_eq!(summary.requested_blocks, 2);
    assert_eq!(summary.acquired_blocks, 1);
    assert_eq!(summary.status.last_packet_id, Some(0));
    assert_eq!(
        summary.status.last_error.as_deref(),
        Some("synthetic read failure")
    );
}

#[test]
fn invalid_config_is_rejected_before_reading_backend() {
    let mut device_config = DeviceConfig::simulator_default();
    device_config.samples_per_packet = 0;
    let mut read_called = false;
    let mut backend = || -> Result<SampleBlock, &'static str> {
        read_called = true;
        Err("should not read")
    };

    let error = run_fixed_blocks(&device_config, 1, &mut backend)
        .expect_err("invalid config should fail before acquisition");

    let AcquisitionRunError::InvalidConfig { summary, reason } = error else {
        panic!("expected invalid config error");
    };

    assert!(!read_called);
    assert_eq!(
        reason.to_string(),
        "samples per packet must be greater than zero"
    );
    assert_eq!(summary.state, AcquisitionState::Error);
    assert_eq!(
        summary.state_history,
        vec![AcquisitionState::DeviceConnected, AcquisitionState::Error]
    );
    assert_eq!(summary.acquired_blocks, 0);
    assert_eq!(
        summary.status.last_error.as_deref(),
        Some("samples per packet must be greater than zero")
    );
}
