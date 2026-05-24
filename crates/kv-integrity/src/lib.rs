//! Integrity checks for Keyvast sample blocks.

use std::fmt;

use kv_types::{IntegritySummary, SampleBlock, SampleBlockError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    pub summary: IntegritySummary,
    pub packet_gaps: Vec<PacketGap>,
    pub timestamp_discontinuities: Vec<TimestampDiscontinuity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketGap {
    pub expected_packet_id: u64,
    pub observed_packet_id: u64,
    pub missing_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampDiscontinuity {
    pub packet_id: u64,
    pub expected_timestamp_start: u64,
    pub observed_timestamp_start: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityError {
    InvalidBlock {
        packet_id: u64,
        source: SampleBlockError,
    },
    PacketIdWentBackwards {
        previous_packet_id: u64,
        observed_packet_id: u64,
    },
}

impl fmt::Display for IntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBlock { packet_id, source } => {
                write!(
                    formatter,
                    "packet {packet_id} has an invalid sample block: {source}"
                )
            }
            Self::PacketIdWentBackwards {
                previous_packet_id,
                observed_packet_id,
            } => write!(
                formatter,
                "packet id went backwards: previous {previous_packet_id}, observed {observed_packet_id}"
            ),
        }
    }
}

impl std::error::Error for IntegrityError {}

pub fn check_blocks(blocks: &[SampleBlock]) -> Result<IntegrityReport, IntegrityError> {
    let mut report = IntegrityReport {
        summary: IntegritySummary::default(),
        packet_gaps: Vec::new(),
        timestamp_discontinuities: Vec::new(),
    };

    let mut previous_block: Option<&SampleBlock> = None;

    for block in blocks {
        block
            .validate()
            .map_err(|source| IntegrityError::InvalidBlock {
                packet_id: block.packet_id,
                source,
            })?;

        report.summary.observed_packets = report.summary.observed_packets.saturating_add(1);
        report.summary.written_samples = report
            .summary
            .written_samples
            .saturating_add(block.data.len() as u64);

        if let Some(previous) = previous_block {
            check_packet_continuity(previous, block, &mut report)?;
            check_timestamp_continuity(previous, block, &mut report);
        }

        previous_block = Some(block);
    }

    report.summary.expected_packets = report
        .summary
        .observed_packets
        .saturating_add(report.summary.missing_packets);
    report.summary.expected_samples = report.summary.written_samples.saturating_add(
        report
            .packet_gaps
            .iter()
            .map(|gap| expected_missing_samples(blocks, gap))
            .sum::<u64>(),
    );

    Ok(report)
}

fn check_packet_continuity(
    previous: &SampleBlock,
    current: &SampleBlock,
    report: &mut IntegrityReport,
) -> Result<(), IntegrityError> {
    let expected_packet_id = previous.packet_id.saturating_add(1);

    if current.packet_id < expected_packet_id {
        return Err(IntegrityError::PacketIdWentBackwards {
            previous_packet_id: previous.packet_id,
            observed_packet_id: current.packet_id,
        });
    }

    if current.packet_id > expected_packet_id {
        let missing_count = current.packet_id.saturating_sub(expected_packet_id);
        report.summary.missing_packets =
            report.summary.missing_packets.saturating_add(missing_count);
        report.packet_gaps.push(PacketGap {
            expected_packet_id,
            observed_packet_id: current.packet_id,
            missing_count,
        });
    }

    Ok(())
}

fn check_timestamp_continuity(
    previous: &SampleBlock,
    current: &SampleBlock,
    report: &mut IntegrityReport,
) {
    let expected_timestamp_start = previous.timestamp_after_block();

    if current.timestamp_start != expected_timestamp_start {
        report.summary.timestamp_discontinuities =
            report.summary.timestamp_discontinuities.saturating_add(1);
        report
            .timestamp_discontinuities
            .push(TimestampDiscontinuity {
                packet_id: current.packet_id,
                expected_timestamp_start,
                observed_timestamp_start: current.timestamp_start,
            });
    }
}

fn expected_missing_samples(blocks: &[SampleBlock], gap: &PacketGap) -> u64 {
    blocks
        .iter()
        .find(|block| block.packet_id.saturating_add(1) == gap.expected_packet_id)
        .map(|block| block.expected_sample_values() as u64)
        .unwrap_or_default()
        .saturating_mul(gap.missing_count)
}
