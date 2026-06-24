//! Shared internal contracts for the Keyvast acquisition stack.

use std::fmt;

pub const DEFAULT_DEVICE_ID: &str = "simulator-0";
pub const DEFAULT_SAMPLE_RATE: f64 = 30_000.0;
pub const DEFAULT_CHANNEL_COUNT: usize = 64;
pub const DEFAULT_SAMPLES_PER_PACKET: usize = 64;
pub const DEFAULT_TTL_LINE_COUNT: usize = 16;

/// Hardware backend type for an acquisition device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceBackendKind {
    Simulator,
    Usb,
    Ethernet,
    Pcie,
}

/// Static configuration describing a connected acquisition device.
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

/// A single contiguous block of multi-channel sample data produced by an
/// acquisition source.  All amplifier samples are interleaved as
/// `data[sample * channel_count + channel]`.
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
        if self.sample_rate <= 0.0 {
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

/// Validation errors for [`SampleBlock`] invariants.
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
        }
    }
}

impl std::error::Error for SampleBlockError {}

/// State machine for the overall acquisition pipeline lifecycle.
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

/// Events emitted by the pipeline for monitoring, diagnostics, and GUI updates.
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

/// Snapshot of a device's current operational state, periodically polled by
/// the GUI for live status display.
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

/// Cumulative data-integrity statistics for a completed or in-progress acquisition.
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
