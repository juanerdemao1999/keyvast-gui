use std::fmt;

use kv_types::{DeviceBackendKind, DeviceConfig};

pub const RHYTHM_HEADER_MAGIC: u64 = 0xd7a2_2aaa_3813_2a53;
pub const RHYTHM_BOARD_ID: u32 = 700;
pub const CHANNELS_PER_STREAM: usize = 32;
pub const SAMPLES_PER_USB_BLOCK: usize = 256;
pub const MAX_SUPPORTED_STREAMS: usize = 2;
pub const DEFAULT_RHD_SAMPLE_RATE: f64 = 30_000.0;
pub const USB3_BLOCK_SIZE_BYTES: usize = 1024;
pub const RHD_AMPLIFIER_MICROVOLTS_PER_COUNT: f32 = 0.195;

#[derive(Debug, Clone, PartialEq)]
pub struct RhythmDataConfig {
    pub device_id: String,
    pub stream_id: u32,
    pub enabled_streams: usize,
    pub sample_rate: f64,
    pub samples_per_block: usize,
}

impl RhythmDataConfig {
    pub fn two_headstages(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            stream_id: 0,
            enabled_streams: 2,
            sample_rate: DEFAULT_RHD_SAMPLE_RATE,
            samples_per_block: SAMPLES_PER_USB_BLOCK,
        }
    }

    pub fn channel_count(&self) -> usize {
        self.enabled_streams.saturating_mul(CHANNELS_PER_STREAM)
    }

    pub fn device_config(&self) -> Result<DeviceConfig, RhythmConfigError> {
        validate_stream_count(self.enabled_streams)?;
        validate_samples_per_block(self.samples_per_block)?;
        validate_sample_rate(self.sample_rate)?;

        let channel_count = self.channel_count();
        Ok(DeviceConfig {
            device_id: self.device_id.clone(),
            backend: DeviceBackendKind::Usb,
            sample_rate: self.sample_rate,
            channel_count,
            samples_per_packet: self.samples_per_block,
            enabled_channels: (0..channel_count).collect(),
            ttl_enabled: true,
            ttl_line_count: 16,
        })
    }

    pub fn validate(&self) -> Result<(), RhythmConfigError> {
        self.device_config().map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RhythmConfigError {
    InvalidStreamCount { enabled_streams: usize },
    InvalidSamplesPerBlock { samples_per_block: usize },
    InvalidSampleRate,
}

impl fmt::Display for RhythmConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStreamCount { enabled_streams } => write!(
                formatter,
                "enabled RHD stream count must be 1..={MAX_SUPPORTED_STREAMS}, got {enabled_streams}"
            ),
            Self::InvalidSamplesPerBlock { samples_per_block } => write!(
                formatter,
                "RHD samples per USB block must be greater than zero, got {samples_per_block}"
            ),
            Self::InvalidSampleRate => write!(
                formatter,
                "RHD sample rate must be finite and greater than zero"
            ),
        }
    }
}

impl std::error::Error for RhythmConfigError {}

pub fn validate_stream_count(enabled_streams: usize) -> Result<(), RhythmConfigError> {
    if enabled_streams == 0 || enabled_streams > MAX_SUPPORTED_STREAMS {
        return Err(RhythmConfigError::InvalidStreamCount { enabled_streams });
    }

    Ok(())
}

pub fn validate_samples_per_block(samples_per_block: usize) -> Result<(), RhythmConfigError> {
    if samples_per_block == 0 {
        return Err(RhythmConfigError::InvalidSamplesPerBlock { samples_per_block });
    }

    Ok(())
}

pub fn validate_sample_rate(sample_rate: f64) -> Result<(), RhythmConfigError> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(RhythmConfigError::InvalidSampleRate);
    }

    Ok(())
}

pub fn words_per_frame(enabled_streams: usize) -> Result<usize, RhythmConfigError> {
    validate_stream_count(enabled_streams)?;

    Ok(
        4 + 2
            + enabled_streams * (CHANNELS_PER_STREAM + 3)
            + (enabled_streams % 4)
            + 8
            + 2,
    )
}

pub fn bytes_per_block(
    enabled_streams: usize,
    samples_per_block: usize,
) -> Result<usize, RhythmConfigError> {
    validate_samples_per_block(samples_per_block)?;

    Ok(words_per_frame(enabled_streams)?
        .saturating_mul(samples_per_block)
        .saturating_mul(2))
}

pub fn raw_word_to_signed_count(word: u16) -> i16 {
    (word as i32 - 32_768) as i16
}

pub fn signed_count_to_microvolts(count: i16) -> f32 {
    count as f32 * RHD_AMPLIFIER_MICROVOLTS_PER_COUNT
}

