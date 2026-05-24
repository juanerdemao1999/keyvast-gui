use kv_integrity::{IntegrityError, PacketGap, TimestampDiscontinuity, check_blocks};
use kv_simulator::{SimulatorBackend, SimulatorConfig};
use kv_types::{DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET, SampleBlock};

#[test]
fn consecutive_simulator_blocks_have_no_integrity_issues() {
    let blocks = next_simulator_blocks(SimulatorConfig::default(), 3);

    let report = check_blocks(&blocks).expect("valid simulator blocks should report cleanly");

    assert_eq!(report.summary.observed_packets, 3);
    assert_eq!(report.summary.expected_packets, 3);
    assert_eq!(report.summary.missing_packets, 0);
    assert_eq!(report.summary.timestamp_discontinuities, 0);
    assert_eq!(report.summary.expected_samples, 3 * samples_per_block());
    assert_eq!(report.summary.written_samples, 3 * samples_per_block());
    assert!(report.packet_gaps.is_empty());
    assert!(report.timestamp_discontinuities.is_empty());
}

#[test]
fn packet_id_gap_reports_missing_packet_count_and_detail() {
    let blocks = next_simulator_blocks(
        SimulatorConfig {
            drop_packet_ids: vec![2],
            ..SimulatorConfig::default()
        },
        4,
    );

    let report = check_blocks(&blocks).expect("packet gap should be reported, not fatal");

    assert_eq!(report.summary.observed_packets, 4);
    assert_eq!(report.summary.expected_packets, 5);
    assert_eq!(report.summary.missing_packets, 1);
    assert_eq!(report.summary.expected_samples, 5 * samples_per_block());
    assert_eq!(report.summary.written_samples, 4 * samples_per_block());
    assert_eq!(
        report.packet_gaps,
        vec![PacketGap {
            expected_packet_id: 2,
            observed_packet_id: 3,
            missing_count: 1,
        }]
    );
}

#[test]
fn simulator_timestamp_jump_reports_discontinuity_detail() {
    let mut blocks = next_simulator_blocks(SimulatorConfig::default(), 2);
    blocks[1].timestamp_start += 10;

    let report = check_blocks(&blocks).expect("timestamp gap should be reported, not fatal");

    assert_eq!(report.summary.timestamp_discontinuities, 1);
    assert_eq!(
        report.timestamp_discontinuities,
        vec![TimestampDiscontinuity {
            packet_id: 1,
            expected_timestamp_start: DEFAULT_SAMPLES_PER_PACKET as u64,
            observed_timestamp_start: DEFAULT_SAMPLES_PER_PACKET as u64 + 10,
        }]
    );
}

#[test]
fn sample_counts_are_summarized_from_blocks() {
    let blocks = vec![
        sample_block(0, 0, 2, 3),
        sample_block(1, 3, 2, 3),
        sample_block(2, 6, 2, 3),
    ];

    let report = check_blocks(&blocks).expect("valid small blocks should report cleanly");

    assert_eq!(report.summary.expected_samples, 18);
    assert_eq!(report.summary.written_samples, 18);
}

#[test]
fn empty_input_produces_zero_summary() {
    let report = check_blocks(&[]).expect("empty input should be a valid zero report");

    assert_eq!(report.summary.observed_packets, 0);
    assert_eq!(report.summary.expected_packets, 0);
    assert_eq!(report.summary.missing_packets, 0);
    assert_eq!(report.summary.expected_samples, 0);
    assert_eq!(report.summary.written_samples, 0);
    assert!(report.packet_gaps.is_empty());
    assert!(report.timestamp_discontinuities.is_empty());
}

#[test]
fn invalid_block_is_rejected_before_reporting() {
    let mut block = sample_block(0, 0, 2, 3);
    block.data.pop();

    let error = check_blocks(&[block]).expect_err("invalid block should stop the report");

    assert!(matches!(
        error,
        IntegrityError::InvalidBlock { packet_id: 0, .. }
    ));
}

fn next_simulator_blocks(config: SimulatorConfig, count: usize) -> Vec<SampleBlock> {
    let mut simulator = SimulatorBackend::new(config).expect("valid simulator config");

    (0..count)
        .map(|_| simulator.next_block().expect("simulator should emit block"))
        .collect()
}

fn sample_block(
    packet_id: u64,
    timestamp_start: u64,
    channel_count: usize,
    samples_per_channel: usize,
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
        data: vec![0; channel_count * samples_per_channel],
    }
}

fn samples_per_block() -> u64 {
    (DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64
}
