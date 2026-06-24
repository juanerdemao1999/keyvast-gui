//! Integrity checks for Keyvast sample blocks.

use std::collections::HashMap;
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

    // Build O(1) lookup for samples-per-block by packet_id.
    let samples_by_id: HashMap<u64, u64> = blocks
        .iter()
        .map(|b| (b.packet_id, b.expected_sample_values() as u64))
        .collect();
    report.summary.expected_samples = report.summary.written_samples.saturating_add(
        report
            .packet_gaps
            .iter()
            .map(|gap| {
                let preceding_id = gap.expected_packet_id.saturating_sub(1);
                samples_by_id
                    .get(&preceding_id)
                    .copied()
                    .unwrap_or_default()
                    .saturating_mul(gap.missing_count)
            })
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

/// Incremental integrity checker that processes blocks one at a time.
///
/// Tracks packet continuity, timestamp continuity, and sample counts without
/// holding all blocks in memory.
#[derive(Debug, Clone)]
pub struct IncrementalIntegrity {
    report: IntegrityReport,
    previous_packet_id: Option<u64>,
    previous_timestamp_after_block: Option<u64>,
    previous_samples_per_block: Option<u64>,
}

impl IncrementalIntegrity {
    pub fn new() -> Self {
        Self {
            report: IntegrityReport {
                summary: IntegritySummary::default(),
                packet_gaps: Vec::new(),
                timestamp_discontinuities: Vec::new(),
            },
            previous_packet_id: None,
            previous_timestamp_after_block: None,
            previous_samples_per_block: None,
        }
    }

    /// Feed one block into the checker. Returns an error only for fatal
    /// conditions (invalid block data, backwards packet IDs).
    pub fn push(&mut self, block: &SampleBlock) -> Result<(), IntegrityError> {
        block
            .validate()
            .map_err(|source| IntegrityError::InvalidBlock {
                packet_id: block.packet_id,
                source,
            })?;

        self.report.summary.observed_packets =
            self.report.summary.observed_packets.saturating_add(1);
        self.report.summary.written_samples = self
            .report
            .summary
            .written_samples
            .saturating_add(block.data.len() as u64);

        if let Some(previous_id) = self.previous_packet_id {
            let expected_packet_id = previous_id.saturating_add(1);

            if block.packet_id < expected_packet_id {
                return Err(IntegrityError::PacketIdWentBackwards {
                    previous_packet_id: previous_id,
                    observed_packet_id: block.packet_id,
                });
            }

            if block.packet_id > expected_packet_id {
                let missing_count = block.packet_id.saturating_sub(expected_packet_id);
                self.report.summary.missing_packets = self
                    .report
                    .summary
                    .missing_packets
                    .saturating_add(missing_count);

                let missing_samples = self
                    .previous_samples_per_block
                    .unwrap_or_default()
                    .saturating_mul(missing_count);
                self.report.summary.expected_samples = self
                    .report
                    .summary
                    .expected_samples
                    .saturating_add(missing_samples);

                self.report.packet_gaps.push(PacketGap {
                    expected_packet_id,
                    observed_packet_id: block.packet_id,
                    missing_count,
                });
            }
        }

        if let Some(expected_timestamp) = self.previous_timestamp_after_block
            && block.timestamp_start != expected_timestamp
        {
            self.report.summary.timestamp_discontinuities = self
                .report
                .summary
                .timestamp_discontinuities
                .saturating_add(1);
            self.report
                .timestamp_discontinuities
                .push(TimestampDiscontinuity {
                    packet_id: block.packet_id,
                    expected_timestamp_start: expected_timestamp,
                    observed_timestamp_start: block.timestamp_start,
                });
        }

        self.previous_packet_id = Some(block.packet_id);
        self.previous_timestamp_after_block = Some(block.timestamp_after_block());
        self.previous_samples_per_block = Some(block.expected_sample_values() as u64);

        Ok(())
    }

    /// Finalize and return the integrity report.
    pub fn finish(mut self) -> IntegrityReport {
        self.report.summary.expected_packets = self
            .report
            .summary
            .observed_packets
            .saturating_add(self.report.summary.missing_packets);
        self.report.summary.expected_samples = self
            .report
            .summary
            .expected_samples
            .saturating_add(self.report.summary.written_samples);
        self.report
    }
}

impl Default for IncrementalIntegrity {
    fn default() -> Self {
        Self::new()
    }
}
