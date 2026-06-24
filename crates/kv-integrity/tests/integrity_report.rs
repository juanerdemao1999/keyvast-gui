use kv_integrity::{
    IncrementalIntegrity, IntegrityError, PacketGap, TimestampDiscontinuity, check_blocks,
};
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
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
    }
}

fn samples_per_block() -> u64 {
    (DEFAULT_CHANNEL_COUNT * DEFAULT_SAMPLES_PER_PACKET) as u64
}

// --- IncrementalIntegrity tests ---

#[test]
fn incremental_consecutive_blocks_match_batch_report() {
    let blocks = next_simulator_blocks(SimulatorConfig::default(), 5);

    let batch_report = check_blocks(&blocks).expect("batch should succeed");

    let mut incremental = IncrementalIntegrity::new();
    for block in &blocks {
        incremental
            .push(block)
            .expect("incremental push should succeed");
    }
    let inc_report = incremental.finish();

    assert_eq!(
        inc_report.summary.observed_packets,
        batch_report.summary.observed_packets
    );
    assert_eq!(
        inc_report.summary.expected_packets,
        batch_report.summary.expected_packets
    );
    assert_eq!(
        inc_report.summary.missing_packets,
        batch_report.summary.missing_packets
    );
    assert_eq!(
        inc_report.summary.written_samples,
        batch_report.summary.written_samples
    );
    assert_eq!(
        inc_report.summary.expected_samples,
        batch_report.summary.expected_samples
    );
    assert!(inc_report.packet_gaps.is_empty());
    assert!(inc_report.timestamp_discontinuities.is_empty());
}

#[test]
fn incremental_detects_packet_gap() {
    let blocks = next_simulator_blocks(
        SimulatorConfig {
            drop_packet_ids: vec![2],
            ..SimulatorConfig::default()
        },
        4,
    );

    let mut incremental = IncrementalIntegrity::new();
    for block in &blocks {
        incremental
            .push(block)
            .expect("incremental push should succeed");
    }
    let report = incremental.finish();

    assert_eq!(report.summary.observed_packets, 4);
    assert_eq!(report.summary.expected_packets, 5);
    assert_eq!(report.summary.missing_packets, 1);
    assert_eq!(report.packet_gaps.len(), 1);
    assert_eq!(report.packet_gaps[0].expected_packet_id, 2);
    assert_eq!(report.packet_gaps[0].observed_packet_id, 3);
}

#[test]
fn incremental_detects_timestamp_discontinuity() {
    let mut blocks = next_simulator_blocks(SimulatorConfig::default(), 2);
    blocks[1].timestamp_start += 10;

    let mut incremental = IncrementalIntegrity::new();
    for block in &blocks {
        incremental
            .push(block)
            .expect("incremental push should succeed");
    }
    let report = incremental.finish();

    assert_eq!(report.summary.timestamp_discontinuities, 1);
    assert_eq!(report.timestamp_discontinuities.len(), 1);
    assert_eq!(
        report.timestamp_discontinuities[0],
        TimestampDiscontinuity {
            packet_id: 1,
            expected_timestamp_start: DEFAULT_SAMPLES_PER_PACKET as u64,
            observed_timestamp_start: DEFAULT_SAMPLES_PER_PACKET as u64 + 10,
        }
    );
}

#[test]
fn incremental_empty_produces_zero_report() {
    let report = IncrementalIntegrity::new().finish();

    assert_eq!(report.summary.observed_packets, 0);
    assert_eq!(report.summary.expected_packets, 0);
    assert_eq!(report.summary.missing_packets, 0);
    assert_eq!(report.summary.expected_samples, 0);
    assert_eq!(report.summary.written_samples, 0);
}

// ---------- M33: PacketIdWentBackwards tests ----------

#[test]
fn check_blocks_detects_backwards_packet_id() {
    let blocks = vec![
        sample_block(5, 0, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET),
        sample_block(
            0,
            DEFAULT_SAMPLES_PER_PACKET as u64,
            DEFAULT_CHANNEL_COUNT,
            DEFAULT_SAMPLES_PER_PACKET,
        ),
    ];

    let error = check_blocks(&blocks).expect_err("backwards packet_id should be an error");
    assert!(matches!(
        error,
        IntegrityError::PacketIdWentBackwards {
            previous_packet_id: 5,
            observed_packet_id: 0,
        }
    ));
}

#[test]
fn incremental_detects_backwards_packet_id() {
    let blocks = [
        sample_block(5, 0, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET),
        sample_block(
            0,
            DEFAULT_SAMPLES_PER_PACKET as u64,
            DEFAULT_CHANNEL_COUNT,
            DEFAULT_SAMPLES_PER_PACKET,
        ),
    ];

    let mut incremental = IncrementalIntegrity::new();
    incremental.push(&blocks[0]).expect("first push ok");
    let error = incremental
        .push(&blocks[1])
        .expect_err("backwards packet_id should be an error");
    assert!(matches!(
        error,
        IntegrityError::PacketIdWentBackwards {
            previous_packet_id: 5,
            observed_packet_id: 0,
        }
    ));
}

// ---------- M18: Wraparound and multi-gap tests ----------

#[test]
fn packet_id_wraparound_at_u64_max_is_treated_as_forward_gap() {
    let blocks = vec![
        sample_block(
            u64::MAX - 1,
            0,
            DEFAULT_CHANNEL_COUNT,
            DEFAULT_SAMPLES_PER_PACKET,
        ),
        sample_block(
            u64::MAX,
            DEFAULT_SAMPLES_PER_PACKET as u64,
            DEFAULT_CHANNEL_COUNT,
            DEFAULT_SAMPLES_PER_PACKET,
        ),
    ];

    let report = check_blocks(&blocks).expect("consecutive u64::MAX-1 → u64::MAX should be ok");
    assert_eq!(report.summary.missing_packets, 0);
}

#[test]
fn multiple_consecutive_gaps_counted_correctly() {
    // blocks: 0, 2, 5 → gaps at 1 (1 missing) and 3-4 (2 missing) = 3 total missing
    let spp = DEFAULT_SAMPLES_PER_PACKET as u64;
    let blocks = vec![
        sample_block(0, 0, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET),
        sample_block(2, spp, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET),
        sample_block(
            5,
            2 * spp,
            DEFAULT_CHANNEL_COUNT,
            DEFAULT_SAMPLES_PER_PACKET,
        ),
    ];

    let report = check_blocks(&blocks).expect("gaps should produce report, not error");
    assert_eq!(report.summary.observed_packets, 3);
    assert_eq!(report.summary.missing_packets, 3);
    assert_eq!(report.packet_gaps.len(), 2);
}

#[test]
fn incremental_equivalence_with_gaps() {
    let spp = DEFAULT_SAMPLES_PER_PACKET as u64;
    let blocks = vec![
        sample_block(0, 0, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET),
        sample_block(3, spp, DEFAULT_CHANNEL_COUNT, DEFAULT_SAMPLES_PER_PACKET),
    ];

    let batch = check_blocks(&blocks).expect("batch ok");
    let mut incremental = IncrementalIntegrity::new();
    for b in &blocks {
        incremental.push(b).expect("push ok");
    }
    let inc = incremental.finish();

    assert_eq!(batch.summary.missing_packets, inc.summary.missing_packets);
    assert_eq!(batch.summary.expected_packets, inc.summary.expected_packets);
    assert_eq!(batch.packet_gaps, inc.packet_gaps);
}
