use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::{DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET, DeviceConfig};

#[test]
fn default_simulator_emits_a_valid_sample_block() {
    let mut simulator = SimulatorBackend::default();

    let block = simulator
        .next_block()
        .expect("default simulator should emit one block");

    block
        .validate_against_ttl_lines(16)
        .expect("simulator block should satisfy the shared data contract");
    assert_eq!(block.device_id, "simulator-0");
    assert_eq!(block.stream_id, 0);
    assert_eq!(block.packet_id, 0);
    assert_eq!(block.timestamp_start, 0);
    assert_eq!(block.sample_rate, 30_000.0);
    assert_eq!(block.channel_count, DEFAULT_CHANNEL_COUNT);
    assert_eq!(block.samples_per_channel, DEFAULT_SAMPLES_PER_PACKET);
    assert_eq!(
        block.data.len(),
        DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET
    );
    assert!(
        block.data.iter().any(|sample| *sample != 0),
        "synthetic data should be observable rather than all zeros"
    );
}

#[test]
fn packet_ids_and_timestamps_advance_by_one_block() {
    let mut simulator = SimulatorBackend::default();

    let first = simulator.next_block().expect("packet 0");
    let second = simulator.next_block().expect("packet 1");
    let third = simulator.next_block().expect("packet 2");

    assert_eq!(first.packet_id, 0);
    assert_eq!(second.packet_id, 1);
    assert_eq!(third.packet_id, 2);
    assert_eq!(first.timestamp_start, 0);
    assert_eq!(second.timestamp_start, DEFAULT_SAMPLES_PER_PACKET as u64);
    assert_eq!(
        third.timestamp_start,
        (DEFAULT_SAMPLES_PER_PACKET * 2) as u64
    );
}

#[test]
fn same_seed_and_config_produce_repeatable_blocks() {
    let config = SimulatorConfig {
        seed: 1234,
        ..SimulatorConfig::default()
    };
    let mut first = SimulatorBackend::new(config.clone()).expect("valid simulator config");
    let mut second = SimulatorBackend::new(config).expect("valid simulator config");

    let first_blocks = [
        first.next_block().expect("first packet"),
        first.next_block().expect("second packet"),
        first.next_block().expect("third packet"),
    ];
    let second_blocks = [
        second.next_block().expect("first packet"),
        second.next_block().expect("second packet"),
        second.next_block().expect("third packet"),
    ];

    assert_eq!(first_blocks, second_blocks);
}

#[test]
fn deterministic_packet_drop_is_represented_as_a_packet_id_gap() {
    let config = SimulatorConfig {
        drop_packet_ids: vec![2],
        ..SimulatorConfig::default()
    };
    let mut simulator = SimulatorBackend::new(config).expect("valid simulator config");

    let observed = [
        simulator.next_block().expect("packet 0"),
        simulator.next_block().expect("packet 1"),
        simulator
            .next_block()
            .expect("packet 3 after dropped packet 2"),
        simulator.next_block().expect("packet 4"),
    ];

    let packet_ids = observed
        .iter()
        .map(|block| block.packet_id)
        .collect::<Vec<_>>();
    let timestamps = observed
        .iter()
        .map(|block| block.timestamp_start)
        .collect::<Vec<_>>();

    assert_eq!(packet_ids, vec![0_u64, 1_u64, 3_u64, 4_u64]);
    assert_eq!(
        timestamps,
        vec![
            0_u64,
            DEFAULT_SAMPLES_PER_PACKET as u64,
            (DEFAULT_SAMPLES_PER_PACKET * 3) as u64,
            (DEFAULT_SAMPLES_PER_PACKET * 4) as u64,
        ]
    );
}

#[test]
fn invalid_device_config_is_rejected_before_acquisition() {
    let mut device_config = DeviceConfig::simulator_default();
    device_config.channel_count = 0;

    let config = SimulatorConfig {
        device: device_config,
        ..SimulatorConfig::default()
    };

    assert!(SimulatorBackend::new(config).is_err());
}
