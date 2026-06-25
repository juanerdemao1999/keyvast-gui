use std::fmt;

use kv_types::{SampleBlock, SampleBlockError};

use crate::protocol::{
    AUX_CHANNELS_PER_STREAM, BOARD_ADC_CHANNELS, CHANNELS_PER_STREAM, FrameLayout,
    RHYTHM_HEADER_MAGIC, RHYTHM_TTL_LINE_COUNT, RhythmConfigError, RhythmDataConfig,
    bytes_per_block, raw_word_to_signed_count,
};

/// Per-block tally of recoverable framing anomalies encountered while parsing.
///
/// A clean block reports all-zero counts.  Non-zero counts mean the parser
/// recovered from a transient framing fault (a corrupt frame magic or a
/// timestamp that did not increment by one) rather than aborting acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockParseReport {
    /// Frames whose header word did not match `RHYTHM_HEADER_MAGIC`.
    pub bad_magic_frames: u32,
    /// Samples whose timestamp was not exactly one greater than the previous.
    pub timestamp_discontinuities: u32,
}

impl BlockParseReport {
    pub fn is_clean(&self) -> bool {
        self.bad_magic_frames == 0 && self.timestamp_discontinuities == 0
    }
}

/// A parsed sample block paired with its recoverable-anomaly report.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRhythmBlock {
    pub block: SampleBlock,
    pub report: BlockParseReport,
}

/// Parse a raw Rhythm USB block, discarding the recoverable-anomaly report.
///
/// Equivalent to [`parse_rhythm_data_block_reporting`] followed by taking the
/// `block` field.  Use the reporting variant when callers need to observe or
/// surface recovered framing faults.
pub fn parse_rhythm_data_block(
    packet_id: u64,
    raw: &[u8],
    config: &RhythmDataConfig,
) -> Result<SampleBlock, RhythmParseError> {
    parse_rhythm_data_block_reporting(packet_id, raw, config).map(|parsed| parsed.block)
}

// Index-based loops mirror the Rhythm wire layout (stream-major word order).
#[allow(clippy::needless_range_loop)]
pub fn parse_rhythm_data_block_reporting(
    packet_id: u64,
    raw: &[u8],
    config: &RhythmDataConfig,
) -> Result<ParsedRhythmBlock, RhythmParseError> {
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
    let layout = FrameLayout::new(streams);
    let mut data = Vec::with_capacity(channel_count.saturating_mul(samples));
    let mut offset = 0_usize;
    let mut timestamp_start = None;
    let mut prev_timestamp: Option<u32> = None;
    let mut report = BlockParseReport::default();

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
        let header = read_u64_le(raw, &mut offset)?;
        if header != RHYTHM_HEADER_MAGIC {
            // A corrupt frame magic is a transient framing fault, not a reason
            // to tear down acquisition.  The block is fixed-length and each
            // frame is self-positioned, so decode this frame in place and tally
            // the anomaly for the caller to surface (DA2).
            report.bad_magic_frames = report.bad_magic_frames.saturating_add(1);
            log::warn!(
                "bad Rhythm frame magic in packet {packet_id} at sample {sample_index}, byte offset {frame_offset}: observed {header:#018x}; decoding frame in place"
            );
        }

        let timestamp = read_u32_le(raw, &mut offset)?;
        timestamp_start.get_or_insert(timestamp);
        if let Some(previous) = prev_timestamp {
            let expected_timestamp = previous.wrapping_add(1);
            if timestamp != expected_timestamp {
                // Accept the observed timestamp and resync the baseline so a
                // single jump is reported once instead of cascading (DA2).
                report.timestamp_discontinuities =
                    report.timestamp_discontinuities.saturating_add(1);
                log::warn!(
                    "Rhythm timestamp discontinuity in packet {packet_id} at sample {sample_index}: expected {expected_timestamp}, observed {timestamp}"
                );
            }
        }
        prev_timestamp = Some(timestamp);

        // Parse auxiliary command results (3 words per stream).
        for aux_ch in 0..AUX_CHANNELS_PER_STREAM {
            for stream in 0..streams {
                let word = read_u16_le(raw, &mut offset)?;
                aux_data[stream][aux_ch].push(word);
            }
        }

        // Parse amplifier data (32 channels × N streams).
        let mut frame_samples = vec![0_i16; channel_count];
        for channel in 0..CHANNELS_PER_STREAM {
            for stream in 0..streams {
                let word = read_u16_le(raw, &mut offset)?;
                frame_samples[stream * CHANNELS_PER_STREAM + channel] =
                    raw_word_to_signed_count(word);
            }
        }
        data.extend_from_slice(&frame_samples);

        // Skip filler word(s) that pad the active stream count up to a multiple
        // of 4 (see `FrameLayout::filler_words`).
        offset = offset.saturating_add(layout.filler_words() * 2);

        // Parse 8 board ADC channels.
        for adc_ch in 0..BOARD_ADC_CHANNELS {
            let word = read_u16_le(raw, &mut offset)?;
            board_adc[adc_ch].push(word);
        }

        // Parse TTL input and TTL output words.
        let ttl_in = read_u16_le(raw, &mut offset)? as u32;
        let ttl_out = read_u16_le(raw, &mut offset)? as u32;
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
        .validate_against_ttl_lines(RHYTHM_TTL_LINE_COUNT)
        .map_err(RhythmParseError::InvalidSampleBlock)?;

    Ok(ParsedRhythmBlock { block, report })
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
    BufferTruncated {
        offset: usize,
    },
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
            Self::BufferTruncated { offset } => {
                write!(formatter, "buffer truncated at byte offset {offset}")
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
            | Self::TimestampDiscontinuity { .. }
            | Self::BufferTruncated { .. } => None,
        }
    }
}

fn read_u16_le(raw: &[u8], offset: &mut usize) -> Result<u16, RhythmParseError> {
    let end = offset
        .checked_add(2)
        .ok_or(RhythmParseError::BufferTruncated { offset: *offset })?;
    let slice = raw
        .get(*offset..end)
        .ok_or(RhythmParseError::BufferTruncated { offset: *offset })?;
    let value = u16::from_le_bytes([slice[0], slice[1]]);
    *offset = end;
    Ok(value)
}

fn read_u32_le(raw: &[u8], offset: &mut usize) -> Result<u32, RhythmParseError> {
    let end = offset
        .checked_add(4)
        .ok_or(RhythmParseError::BufferTruncated { offset: *offset })?;
    let slice = raw
        .get(*offset..end)
        .ok_or(RhythmParseError::BufferTruncated { offset: *offset })?;
    let value = u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]);
    *offset = end;
    Ok(value)
}

fn read_u64_le(raw: &[u8], offset: &mut usize) -> Result<u64, RhythmParseError> {
    let end = offset
        .checked_add(8)
        .ok_or(RhythmParseError::BufferTruncated { offset: *offset })?;
    let slice = raw
        .get(*offset..end)
        .ok_or(RhythmParseError::BufferTruncated { offset: *offset })?;
    let value = u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]);
    *offset = end;
    Ok(value)
}
