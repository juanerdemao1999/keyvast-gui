//! Shared internal contracts for the Keyvast acquisition stack.

use std::fmt;

pub const DEFAULT_DEVICE_ID: &str = "simulator-0";
pub const DEFAULT_SAMPLE_RATE: f64 = 30_000.0;
pub const DEFAULT_CHANNEL_COUNT: usize = 64;
pub const DEFAULT_SAMPLES_PER_PACKET: usize = 64;
pub const DEFAULT_TTL_LINE_COUNT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceBackendKind {
    Simulator,
    Usb,
    Ethernet,
    Pcie,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceConfig {
    pub device_id: String,
    pub backend: DeviceBackendKind,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub samples_per_packet: usize,
    pub enabled_channels: Vec<usize>,
    pub ttl_enabled: bool,
    pub ttl_line_count: usize,
}

impl DeviceConfig {
    pub fn simulator_default() -> Self {
        Self {
            device_id: DEFAULT_DEVICE_ID.to_string(),
            backend: DeviceBackendKind::Simulator,
            sample_rate: DEFAULT_SAMPLE_RATE,
            channel_count: DEFAULT_CHANNEL_COUNT,
            samples_per_packet: DEFAULT_SAMPLES_PER_PACKET,
            enabled_channels: (0..DEFAULT_CHANNEL_COUNT).collect(),
            ttl_enabled: true,
            ttl_line_count: DEFAULT_TTL_LINE_COUNT,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SampleBlock {
    pub device_id: String,
    pub stream_id: u32,
    pub packet_id: u64,
    pub timestamp_start: u64,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub samples_per_channel: usize,
    pub ttl_bits: u32,
    pub data: Vec<i16>,

    /// Raw auxiliary command results: `[stream][aux_ch][sample]`.
    /// 3 aux channels per stream, one u16 per sample.
    /// `None` when the parser does not extract auxiliary data.
    pub aux_data: Option<Vec<Vec<Vec<u16>>>>,

    /// Board ADC channels: `[adc_ch][sample]`, 8 channels of u16.
    /// `None` when not extracted.
    pub board_adc_data: Option<Vec<Vec<u16>>>,

    /// Per-sample TTL input words.  When present the length equals
    /// `samples_per_channel`.  The legacy `ttl_bits` field still holds the
    /// last sample's TTL word for backward compatibility.
    pub ttl_in_per_sample: Option<Vec<u32>>,

    /// Per-sample TTL output words.
    pub ttl_out_per_sample: Option<Vec<u32>>,
}

impl SampleBlock {
    pub fn expected_sample_values(&self) -> usize {
        self.channel_count.saturating_mul(self.samples_per_channel)
    }

    pub fn timestamp_after_block(&self) -> u64 {
        self.timestamp_start
            .saturating_add(self.samples_per_channel as u64)
    }

    pub fn validate(&self) -> Result<(), SampleBlockError> {
        if !(self.sample_rate > 0.0 && self.sample_rate.is_finite()) {
            return Err(SampleBlockError::InvalidSampleRate);
        }

        if self.channel_count == 0 {
            return Err(SampleBlockError::EmptyChannelSet);
        }

        if self.samples_per_channel == 0 {
            return Err(SampleBlockError::EmptyBlock);
        }

        let expected = self.expected_sample_values();
        let observed = self.data.len();
        if observed != expected {
            return Err(SampleBlockError::DataLengthMismatch { expected, observed });
        }

        self.validate_side_channels()?;

        Ok(())
    }

    /// Verify that every populated side-channel vector (per-sample TTL,
    /// board ADC, auxiliary) carries exactly `samples_per_channel` samples,
    /// so malformed/partial blocks cannot pass the integrity gate and later
    /// panic in unchecked export/render paths.
    fn validate_side_channels(&self) -> Result<(), SampleBlockError> {
        let spc = self.samples_per_channel;

        for (channel, len) in [
            (
                "ttl_in_per_sample",
                self.ttl_in_per_sample.as_ref().map(Vec::len),
            ),
            (
                "ttl_out_per_sample",
                self.ttl_out_per_sample.as_ref().map(Vec::len),
            ),
        ] {
            if let Some(observed) = len
                && observed != spc
            {
                return Err(SampleBlockError::SideChannelLengthMismatch {
                    channel,
                    expected: spc,
                    observed,
                });
            }
        }

        if let Some(board_adc) = &self.board_adc_data {
            for chan in board_adc {
                if chan.len() != spc {
                    return Err(SampleBlockError::SideChannelLengthMismatch {
                        channel: "board_adc_data",
                        expected: spc,
                        observed: chan.len(),
                    });
                }
            }
        }

        if let Some(aux) = &self.aux_data {
            for stream in aux {
                for chan in stream {
                    if chan.len() != spc {
                        return Err(SampleBlockError::SideChannelLengthMismatch {
                            channel: "aux_data",
                            expected: spc,
                            observed: chan.len(),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    pub fn validate_against_ttl_lines(
        &self,
        ttl_line_count: usize,
    ) -> Result<(), SampleBlockError> {
        self.validate()?;

        if ttl_line_count > u32::BITS as usize {
            return Err(SampleBlockError::TtlLineCountOutOfRange { ttl_line_count });
        }

        let allowed_mask = if ttl_line_count == u32::BITS as usize {
            u32::MAX
        } else if ttl_line_count == 0 {
            0
        } else {
            (1_u32 << ttl_line_count) - 1
        };

        if self.ttl_bits & !allowed_mask != 0 {
            return Err(SampleBlockError::TtlBitsOutOfRange {
                ttl_bits: self.ttl_bits,
                ttl_line_count,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleBlockError {
    EmptyBlock,
    EmptyChannelSet,
    InvalidSampleRate,
    DataLengthMismatch {
        expected: usize,
        observed: usize,
    },
    TtlBitsOutOfRange {
        ttl_bits: u32,
        ttl_line_count: usize,
    },
    TtlLineCountOutOfRange {
        ttl_line_count: usize,
    },
    SideChannelLengthMismatch {
        channel: &'static str,
        expected: usize,
        observed: usize,
    },
}

impl fmt::Display for SampleBlockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBlock => write!(formatter, "sample block has no samples"),
            Self::EmptyChannelSet => write!(formatter, "sample block has no channels"),
            Self::InvalidSampleRate => write!(formatter, "sample rate must be positive"),
            Self::DataLengthMismatch { expected, observed } => write!(
                formatter,
                "sample block data length mismatch: expected {expected}, observed {observed}"
            ),
            Self::TtlBitsOutOfRange {
                ttl_bits,
                ttl_line_count,
            } => write!(
                formatter,
                "ttl bits {ttl_bits:#034b} exceed configured ttl line count {ttl_line_count}"
            ),
            Self::TtlLineCountOutOfRange { ttl_line_count } => write!(
                formatter,
                "ttl line count {ttl_line_count} exceeds u32 ttl storage width"
            ),
            Self::SideChannelLengthMismatch {
                channel,
                expected,
                observed,
            } => write!(
                formatter,
                "side-channel {channel} length mismatch: expected {expected}, observed {observed}"
            ),
        }
    }
}

impl std::error::Error for SampleBlockError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionState {
    Idle,
    DeviceConnected,
    Configured,
    Acquiring,
    Stopping,
    Stopped,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AcquisitionEvent {
    Started {
        timestamp_host_ms: u64,
    },
    Stopped {
        timestamp_host_ms: u64,
    },
    TtlChanged {
        timestamp_start: u64,
        ttl_bits: u32,
    },
    PacketMissing {
        expected_packet_id: u64,
        observed_packet_id: u64,
        missing_count: u64,
    },
    BufferOverflow {
        dropped_blocks: u64,
        buffer_occupancy: f64,
    },
    RecorderError {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStatus {
    pub device_id: String,
    pub backend: DeviceBackendKind,
    pub connected: bool,
    pub configured: bool,
    pub acquiring: bool,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub packet_rate_hz: f64,
    pub last_packet_id: Option<u64>,
    pub ttl_bits: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntegritySummary {
    pub expected_packets: u64,
    pub observed_packets: u64,
    pub missing_packets: u64,
    pub crc_errors: u64,
    pub timestamp_discontinuities: u64,
    pub buffer_overflows: u64,
    pub expected_samples: u64,
    pub written_samples: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_block() -> SampleBlock {
        SampleBlock {
            device_id: "test".to_string(),
            stream_id: 0,
            packet_id: 0,
            timestamp_start: 0,
            sample_rate: 30_000.0,
            channel_count: 2,
            samples_per_channel: 3,
            ttl_bits: 0,
            data: vec![0; 6],
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        }
    }

    #[test]
    fn validate_accepts_a_consistent_block() {
        assert_eq!(valid_block().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_non_finite_or_non_positive_sample_rate() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            let mut block = valid_block();
            block.sample_rate = bad;
            assert_eq!(
                block.validate(),
                Err(SampleBlockError::InvalidSampleRate),
                "sample_rate {bad} should be rejected"
            );
        }
    }

    #[test]
    fn validate_rejects_empty_channels_and_samples() {
        let mut no_channels = valid_block();
        no_channels.channel_count = 0;
        no_channels.data.clear();
        assert_eq!(
            no_channels.validate(),
            Err(SampleBlockError::EmptyChannelSet)
        );

        let mut no_samples = valid_block();
        no_samples.samples_per_channel = 0;
        no_samples.data.clear();
        assert_eq!(no_samples.validate(), Err(SampleBlockError::EmptyBlock));
    }

    #[test]
    fn validate_rejects_data_length_mismatch() {
        let mut block = valid_block();
        block.data.push(0);
        assert_eq!(
            block.validate(),
            Err(SampleBlockError::DataLengthMismatch {
                expected: 6,
                observed: 7,
            })
        );
    }

    #[test]
    fn validate_accepts_correctly_sized_side_channels() {
        let mut block = valid_block();
        block.ttl_in_per_sample = Some(vec![0; 3]);
        block.ttl_out_per_sample = Some(vec![0; 3]);
        block.board_adc_data = Some(vec![vec![0; 3], vec![0; 3]]);
        block.aux_data = Some(vec![vec![vec![0; 3]]]);
        assert_eq!(block.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_short_per_sample_ttl() {
        let mut block = valid_block();
        block.ttl_in_per_sample = Some(vec![0; 2]);
        assert_eq!(
            block.validate(),
            Err(SampleBlockError::SideChannelLengthMismatch {
                channel: "ttl_in_per_sample",
                expected: 3,
                observed: 2,
            })
        );
    }

    #[test]
    fn validate_rejects_short_board_adc_and_aux() {
        let mut adc = valid_block();
        adc.board_adc_data = Some(vec![vec![0; 3], vec![0; 1]]);
        assert_eq!(
            adc.validate(),
            Err(SampleBlockError::SideChannelLengthMismatch {
                channel: "board_adc_data",
                expected: 3,
                observed: 1,
            })
        );

        let mut aux = valid_block();
        aux.aux_data = Some(vec![vec![vec![0; 3], vec![0; 0]]]);
        assert_eq!(
            aux.validate(),
            Err(SampleBlockError::SideChannelLengthMismatch {
                channel: "aux_data",
                expected: 3,
                observed: 0,
            })
        );
    }

    #[test]
    fn expected_sample_values_saturates_instead_of_overflowing() {
        let mut block = valid_block();
        block.channel_count = usize::MAX;
        block.samples_per_channel = 2;
        assert_eq!(block.expected_sample_values(), usize::MAX);
    }

    #[test]
    fn validate_against_ttl_lines_enforces_the_line_mask() {
        let mut block = valid_block();
        block.ttl_bits = 0b1010;
        assert_eq!(block.validate_against_ttl_lines(4), Ok(()));
        // Bit 4 is set but only 4 lines (bits 0..=3) are allowed.
        block.ttl_bits = 0b1_0000;
        assert_eq!(
            block.validate_against_ttl_lines(4),
            Err(SampleBlockError::TtlBitsOutOfRange {
                ttl_bits: 0b1_0000,
                ttl_line_count: 4,
            })
        );
        // Full 32-bit mask accepts any pattern.
        block.ttl_bits = u32::MAX;
        assert_eq!(block.validate_against_ttl_lines(32), Ok(()));
        // Asking for more lines than u32 can hold is rejected.
        assert_eq!(
            block.validate_against_ttl_lines(33),
            Err(SampleBlockError::TtlLineCountOutOfRange { ttl_line_count: 33 })
        );
    }
}
