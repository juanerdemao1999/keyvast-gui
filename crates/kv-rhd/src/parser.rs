use std::fmt;

use kv_types::{SampleBlock, SampleBlockError};

use crate::protocol::{
    CHANNELS_PER_STREAM, RHYTHM_HEADER_MAGIC, RhythmConfigError, RhythmDataConfig,
    bytes_per_block, raw_word_to_signed_count,
};

pub fn parse_rhythm_data_block(
    packet_id: u64,
    raw: &[u8],
    config: &RhythmDataConfig,
) -> Result<SampleBlock, RhythmParseError> {
    config.validate().map_err(RhythmParseError::InvalidConfig)?;

    let expected_len = bytes_per_block(config.enabled_streams, config.samples_per_block)
        .map_err(RhythmParseError::InvalidConfig)?;
    if raw.len() != expected_len {
        return Err(RhythmParseError::LengthMismatch {
            expected: expected_len,
            observed: raw.len(),
        });
    }

    let channel_count = config.channel_count();
    let mut data = Vec::with_capacity(channel_count.saturating_mul(config.samples_per_block));
    let mut offset = 0_usize;
    let mut timestamp_start = None;
    let mut ttl_bits = None;

    for sample_index in 0..config.samples_per_block {
        let frame_offset = offset;
        let header = read_u64_le(raw, &mut offset);
        if header != RHYTHM_HEADER_MAGIC {
            return Err(RhythmParseError::BadMagic {
                sample_index,
                offset: frame_offset,
                observed: header,
            });
        }

        let timestamp = read_u32_le(raw, &mut offset);
        let first_timestamp = *timestamp_start.get_or_insert(timestamp);
        let expected_timestamp = first_timestamp.wrapping_add(sample_index as u32);
        if timestamp != expected_timestamp {
            return Err(RhythmParseError::TimestampDiscontinuity {
                sample_index,
                expected: expected_timestamp,
                observed: timestamp,
            });
        }

        offset = offset.saturating_add(3 * config.enabled_streams * 2);

        let mut frame_samples = vec![0_i16; channel_count];
        for channel in 0..CHANNELS_PER_STREAM {
            for stream in 0..config.enabled_streams {
                let word = read_u16_le(raw, &mut offset);
                frame_samples[stream * CHANNELS_PER_STREAM + channel] =
                    raw_word_to_signed_count(word);
            }
        }
        data.extend_from_slice(&frame_samples);

        offset = offset.saturating_add((config.enabled_streams % 4) * 2);
        offset = offset.saturating_add(8 * 2);
        let frame_ttl_bits = read_u16_le(raw, &mut offset) as u32;
        ttl_bits.get_or_insert(frame_ttl_bits);
        offset = offset.saturating_add(2);
    }

    let block = SampleBlock {
        device_id: config.device_id.clone(),
        stream_id: config.stream_id,
        packet_id,
        timestamp_start: timestamp_start.unwrap_or_default() as u64,
        sample_rate: config.sample_rate,
        channel_count,
        samples_per_channel: config.samples_per_block,
        ttl_bits: ttl_bits.unwrap_or_default(),
        data,
    };

    block
        .validate_against_ttl_lines(16)
        .map_err(RhythmParseError::InvalidSampleBlock)?;

    Ok(block)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RhythmParseError {
    InvalidConfig(RhythmConfigError),
    LengthMismatch {
        expected: usize,
        observed: usize,
    },
    BadMagic {
        sample_index: usize,
        offset: usize,
        observed: u64,
    },
    TimestampDiscontinuity {
        sample_index: usize,
        expected: u32,
        observed: u32,
    },
    InvalidSampleBlock(SampleBlockError),
}

impl fmt::Display for RhythmParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(error) => write!(formatter, "{error}"),
            Self::LengthMismatch { expected, observed } => write!(
                formatter,
                "raw Rhythm block length mismatch: expected {expected} bytes, observed {observed}"
            ),
            Self::BadMagic {
                sample_index,
                offset,
                observed,
            } => write!(
                formatter,
                "bad Rhythm frame magic at sample {sample_index}, byte offset {offset}: observed {observed:#018x}"
            ),
            Self::TimestampDiscontinuity {
                sample_index,
                expected,
                observed,
            } => write!(
                formatter,
                "Rhythm timestamp discontinuity at sample {sample_index}: expected {expected}, observed {observed}"
            ),
            Self::InvalidSampleBlock(error) => {
                write!(formatter, "parsed RHD sample block is invalid: {error}")
            }
        }
    }
}

impl std::error::Error for RhythmParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig(error) => Some(error),
            Self::InvalidSampleBlock(error) => Some(error),
            Self::LengthMismatch { .. }
            | Self::BadMagic { .. }
            | Self::TimestampDiscontinuity { .. } => None,
        }
    }
}

fn read_u16_le(raw: &[u8], offset: &mut usize) -> u16 {
    let value = u16::from_le_bytes([raw[*offset], raw[*offset + 1]]);
    *offset += 2;
    value
}

fn read_u32_le(raw: &[u8], offset: &mut usize) -> u32 {
    let value = u32::from_le_bytes([
        raw[*offset],
        raw[*offset + 1],
        raw[*offset + 2],
        raw[*offset + 3],
    ]);
    *offset += 4;
    value
}

fn read_u64_le(raw: &[u8], offset: &mut usize) -> u64 {
    let value = u64::from_le_bytes([
        raw[*offset],
        raw[*offset + 1],
        raw[*offset + 2],
        raw[*offset + 3],
        raw[*offset + 4],
        raw[*offset + 5],
        raw[*offset + 6],
        raw[*offset + 7],
    ]);
    *offset += 8;
    value
}
