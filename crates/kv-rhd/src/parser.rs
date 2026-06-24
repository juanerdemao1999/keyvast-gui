use std::fmt;

use kv_types::{SampleBlock, SampleBlockError};

use crate::protocol::{
    AUX_CHANNELS_PER_STREAM, BOARD_ADC_CHANNELS, CHANNELS_PER_STREAM, RHYTHM_HEADER_MAGIC,
    RhythmConfigError, RhythmDataConfig, bytes_per_block, raw_word_to_signed_count,
};

// Index-based loops mirror the Rhythm wire layout (stream-major word order).
#[allow(clippy::needless_range_loop)]
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
    let samples = config.samples_per_block;
    let streams = config.enabled_streams;
    let mut data = Vec::with_capacity(channel_count.saturating_mul(samples));
    let mut offset = 0_usize;
    let mut timestamp_start = None;

    // Auxiliary data: [stream][aux_ch][sample]
    let mut aux_data: Vec<Vec<Vec<u16>>> = (0..streams)
        .map(|_| {
            (0..AUX_CHANNELS_PER_STREAM)
                .map(|_| Vec::with_capacity(samples))
                .collect()
        })
        .collect();

    // Board ADC: [adc_ch][sample]
    let mut board_adc: Vec<Vec<u16>> = (0..BOARD_ADC_CHANNELS)
        .map(|_| Vec::with_capacity(samples))
        .collect();

    let mut ttl_in_vec: Vec<u32> = Vec::with_capacity(samples);
    let mut ttl_out_vec: Vec<u32> = Vec::with_capacity(samples);

    for sample_index in 0..samples {
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

        // Parse auxiliary command results (3 words per stream).
        for aux_ch in 0..AUX_CHANNELS_PER_STREAM {
            for stream in 0..streams {
                let word = read_u16_le(raw, &mut offset);
                aux_data[stream][aux_ch].push(word);
            }
        }

        // Parse amplifier data (32 channels × N streams).
        let mut frame_samples = vec![0_i16; channel_count];
        for channel in 0..CHANNELS_PER_STREAM {
            for stream in 0..streams {
                let word = read_u16_le(raw, &mut offset);
                frame_samples[stream * CHANNELS_PER_STREAM + channel] =
                    raw_word_to_signed_count(word);
            }
        }
        data.extend_from_slice(&frame_samples);

        // Skip filler word(s) that align each frame to a 4-stream boundary.
        offset = offset.saturating_add(((4 - streams % 4) % 4) * 2);

        // Parse 8 board ADC channels.
        for adc_ch in 0..BOARD_ADC_CHANNELS {
            let word = read_u16_le(raw, &mut offset);
            board_adc[adc_ch].push(word);
        }

        // Parse TTL input and TTL output words.
        let ttl_in = read_u16_le(raw, &mut offset) as u32;
        let ttl_out = read_u16_le(raw, &mut offset) as u32;
        ttl_in_vec.push(ttl_in);
        ttl_out_vec.push(ttl_out);
    }

    let last_ttl = ttl_in_vec.last().copied().unwrap_or_default();

    let block = SampleBlock {
        device_id: config.device_id.clone(),
        stream_id: config.stream_id,
        packet_id,
        timestamp_start: timestamp_start.unwrap_or_default() as u64,
        sample_rate: config.sample_rate,
        channel_count,
        samples_per_channel: samples,
        ttl_bits: last_ttl,
        data,
        aux_data: Some(aux_data),
        board_adc_data: Some(board_adc),
        ttl_in_per_sample: Some(ttl_in_vec),
        ttl_out_per_sample: Some(ttl_out_vec),
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
