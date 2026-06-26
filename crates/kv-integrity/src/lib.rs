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
    check_blocks_with_expected_start(None, blocks)
}

/// Like [`check_blocks`], but `expected_first_packet_id` declares the packet id
/// the session was expected to begin at. Packets lost *before* the first
/// observed block (DA43) are then counted as missing instead of being silently
/// skipped because accounting anchored on whichever block happened to arrive
/// first. Acquisition numbers packets from 0, so the pipeline passes `Some(0)`;
/// pass `None` to anchor on the first observed block as before.
pub fn check_blocks_with_expected_start(
    expected_first_packet_id: Option<u64>,
    blocks: &[SampleBlock],
) -> Result<IntegrityReport, IntegrityError> {
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

        match previous_block {
            Some(previous) => {
                let had_gap = check_packet_continuity(previous, block, &mut report)?;
                // Only check timestamp continuity when there is no packet gap;
                // a gap naturally causes a timestamp jump that is not a clock
                // discontinuity.
                if !had_gap {
                    check_timestamp_continuity(previous, block, &mut report);
                }
            }
            None => {
                if let Some(base) = expected_first_packet_id {
                    account_missing_before_first_block(base, block, &mut report);
                }
            }
        }

        previous_block = Some(block);
    }

    report.summary.expected_packets = report
        .summary
        .observed_packets
        .saturating_add(report.summary.missing_packets);
    // `expected_samples` accumulated the missing-sample estimate for each gap
    // as it was found (O(n) total); add the samples actually written.
    report.summary.expected_samples = report
        .summary
        .expected_samples
        .saturating_add(report.summary.written_samples);

    Ok(report)
}

/// Account for packets lost before the first observed block (DA43). Counting
/// starts at `expected_first_packet_id`; if the first block we actually saw is
/// forward of it, the difference was lost ahead of the recording and is folded
/// into the missing tallies (sample estimate uses the first block's geometry).
fn account_missing_before_first_block(
    expected_first_packet_id: u64,
    first_block: &SampleBlock,
    report: &mut IntegrityReport,
) {
    let forward_gap = first_block.packet_id.wrapping_sub(expected_first_packet_id);
    // `0` means the stream started exactly where expected; a huge wrapping
    // distance means the first id is *behind* the baseline, which is not
    // pre-stream loss, so leave both cases alone.
    if forward_gap == 0 || forward_gap > u64::MAX / 2 {
        return;
    }

    report.summary.missing_packets = report.summary.missing_packets.saturating_add(forward_gap);
    report.summary.expected_samples = report.summary.expected_samples.saturating_add(
        (first_block.expected_sample_values() as u64).saturating_mul(forward_gap),
    );
    report.packet_gaps.push(PacketGap {
        expected_packet_id: expected_first_packet_id,
        observed_packet_id: first_block.packet_id,
        missing_count: forward_gap,
    });
}

/// Returns `Ok(true)` if a packet gap was detected, `Ok(false)` if
/// the packet ID is continuous.
fn check_packet_continuity(
    previous: &SampleBlock,
    current: &SampleBlock,
    report: &mut IntegrityReport,
) -> Result<bool, IntegrityError> {
    let expected_packet_id = previous.packet_id.wrapping_add(1);

    if current.packet_id == expected_packet_id {
        return Ok(false);
    }

    // Use wrapping subtraction to compute the forward gap. A large
    // forward distance (> half the u64 space) is treated as a backwards
    // jump.
    let forward_gap = current.packet_id.wrapping_sub(expected_packet_id);
    if forward_gap > u64::MAX / 2 {
        return Err(IntegrityError::PacketIdWentBackwards {
            previous_packet_id: previous.packet_id,
            observed_packet_id: current.packet_id,
        });
    }

    report.summary.missing_packets = report.summary.missing_packets.saturating_add(forward_gap);
    // The block right before the gap is `previous`, so the missing-sample
    // estimate can be computed here instead of re-scanning all blocks per gap.
    report.summary.expected_samples = report
        .summary
        .expected_samples
        .saturating_add((previous.expected_sample_values() as u64).saturating_mul(forward_gap));
    report.packet_gaps.push(PacketGap {
        expected_packet_id,
        observed_packet_id: current.packet_id,
        missing_count: forward_gap,
    });

    Ok(true)
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
        report.summary.expected_samples = report
            .summary
            .expected_samples
            .saturating_add(hardware_missing_sample_values(
                expected_timestamp_start,
                current,
            ));
    }
}

/// Estimate the sample values the FPGA dropped across a hardware-timestamp jump
/// (DA35). The host packet id increments on every read, so FPGA FIFO loss never
/// shows up as a packet gap; the only evidence is the timestamp advancing
/// further than the previous block covered. A forward jump of `n` sample ticks
/// means `n * channel_count` sample values went missing. A backwards jump
/// (timestamp reset/overlap) is not loss, so it contributes nothing.
fn hardware_missing_sample_values(expected_timestamp_start: u64, current: &SampleBlock) -> u64 {
    let forward = current.timestamp_start.wrapping_sub(expected_timestamp_start);
    if forward == 0 || forward > u64::MAX / 2 {
        return 0;
    }
    forward.saturating_mul(current.channel_count as u64)
}

/// Incremental integrity checker that processes blocks one at a time.
///
/// Tracks packet continuity, timestamp continuity, and sample counts without
/// holding all blocks in memory.
#[derive(Debug, Clone)]
pub struct IncrementalIntegrity {
    report: IntegrityReport,
    expected_first_packet_id: Option<u64>,
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
            expected_first_packet_id: None,
            previous_packet_id: None,
            previous_timestamp_after_block: None,
            previous_samples_per_block: None,
        }
    }

    /// Like [`IncrementalIntegrity::new`], but declares the packet id the
    /// session was expected to begin at so packets lost before the first
    /// observed block (DA43) are counted as missing. Acquisition numbers
    /// packets from 0, so the streaming pipeline passes `0`.
    pub fn with_expected_first_packet_id(expected_first_packet_id: u64) -> Self {
        Self {
            expected_first_packet_id: Some(expected_first_packet_id),
            ..Self::new()
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

        if self.previous_packet_id.is_none()
            && let Some(base) = self.expected_first_packet_id
        {
            account_missing_before_first_block(base, block, &mut self.report);
        }

        let mut had_gap = false;
        if let Some(previous_id) = self.previous_packet_id {
            let expected_packet_id = previous_id.wrapping_add(1);

            if block.packet_id != expected_packet_id {
                let forward_gap = block.packet_id.wrapping_sub(expected_packet_id);
                if forward_gap > u64::MAX / 2 {
                    return Err(IntegrityError::PacketIdWentBackwards {
                        previous_packet_id: previous_id,
                        observed_packet_id: block.packet_id,
                    });
                }

                had_gap = true;
                self.report.summary.missing_packets = self
                    .report
                    .summary
                    .missing_packets
                    .saturating_add(forward_gap);

                let missing_samples = self
                    .previous_samples_per_block
                    .unwrap_or_default()
                    .saturating_mul(forward_gap);
                self.report.summary.expected_samples = self
                    .report
                    .summary
                    .expected_samples
                    .saturating_add(missing_samples);

                self.report.packet_gaps.push(PacketGap {
                    expected_packet_id,
                    observed_packet_id: block.packet_id,
                    missing_count: forward_gap,
                });
            }
        }

        // Only check timestamp continuity when there is no packet gap.
        if !had_gap
            && let Some(expected_timestamp) = self.previous_timestamp_after_block
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
            self.report.summary.expected_samples = self
                .report
                .summary
                .expected_samples
                .saturating_add(hardware_missing_sample_values(expected_timestamp, block));
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
