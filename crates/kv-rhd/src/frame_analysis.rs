//! Pure Rhythm frame-layout analysis helpers used during MISO bring-up and
//! impedance measurement. They operate on raw little-endian USB block bytes
//! and have no hardware dependency, which keeps them unit-testable.

use crate::commands::AuxCommandSlot;
use crate::protocol::CHANNELS_PER_STREAM;

pub(crate) fn aux_command_trigger_bit(slot: AuxCommandSlot) -> i32 {
    match slot {
        AuxCommandSlot::AuxCmd1 => 1,
        AuxCommandSlot::AuxCmd2 => 2,
        AuxCommandSlot::AuxCmd3 => 3,
    }
}

/// Compact label for the contiguous data-stream range a port probe enables,
/// e.g. "stream 4" (one 32-channel stream) or "streams 4-5" (64-channel pair).
pub(crate) fn stream_range_label(first_stream: u32, enabled_streams: usize) -> String {
    if enabled_streams <= 1 {
        format!("stream {first_stream}")
    } else {
        format!(
            "streams {first_stream}-{}",
            first_stream + enabled_streams as u32 - 1
        )
    }
}

/// Verify that the AuxCmd3 results in a probe block contain the RHD chip
/// identification signature "INTAN" (registers 63, 62, 61, 60, 59). This is a
/// strong validation — it proves SPI communication is correctly aligned and a
/// real RHD2000 chip is responding. Returns true if the pattern is found on any
/// stream.
///
/// The register config command list (`create_command_list_register_config`) reads
/// these identification registers after writing config registers. The MISO
/// response for each command arrives one SPI cycle later, so the 'I','N','T','A','N'
/// bytes appear in consecutive AuxCmd3 result words. Rather than relying on exact
/// command-list indices, we scan all samples for the pattern.
pub(crate) fn verify_chip_id_in_probe(raw: &[u8], enabled_streams: usize, samples: usize) -> bool {
    if enabled_streams == 0 || samples < 5 {
        return false;
    }

    // Frame layout per sample (in bytes):
    //   8 (magic) + 4 (timestamp) + 3*enabled_streams*2 (aux results)
    //   + CHANNELS_PER_STREAM*enabled_streams*2 (amplifier)
    //   + (enabled_streams%4)*2 (pad) + 8*2 (board ADC) + 2 (TTL in) + 2 (TTL out)
    let frame_bytes =
        (4 + 2 + enabled_streams * (CHANNELS_PER_STREAM + 3) + (enabled_streams % 4) + 8 + 2) * 2;

    // AuxCmd3 results start after magic(8) + timestamp(4) + AuxCmd1(streams*2) + AuxCmd2(streams*2)
    let auxcmd3_base = 12 + 2 * enabled_streams * 2;

    let pattern: [u8; 5] = [b'I', b'N', b'T', b'A', b'N'];

    for stream in 0..enabled_streams {
        let word_offset_in_frame = auxcmd3_base + stream * 2;
        let mut aux_bytes: Vec<u8> = Vec::with_capacity(samples);
        for s in 0..samples {
            let off = s * frame_bytes + word_offset_in_frame;
            if off + 2 > raw.len() {
                return false;
            }
            let word = u16::from_le_bytes([raw[off], raw[off + 1]]);
            aux_bytes.push((word & 0xFF) as u8);
        }
        if aux_bytes.windows(5).any(|w| w == pattern) {
            return true;
        }
    }
    false
}

/// Fraction (0.0..=1.0) of amplifier words that are railed/invalid (0xFFFF or
/// 0x0000) on the best-communicating stream, evaluated over the second half of
/// the block (after the in-run register config + ADC calibration has settled).
/// A correctly-delayed, responding chip reports ~0%; a wrong delay samples the
/// idle MISO line and reports ~100%. The byte walk mirrors `parse_rhythm_data_block`.
pub(crate) fn min_stream_railed_fraction(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
) -> f64 {
    if enabled_streams == 0 || samples == 0 {
        return 1.0;
    }
    let mut railed = vec![0_usize; enabled_streams];
    let mut total = vec![0_usize; enabled_streams];
    let evaluate_from = samples / 2;
    let mut offset = 0_usize;

    for sample_index in 0..samples {
        offset += 8; // frame magic
        offset += 4; // timestamp
        offset += 3 * enabled_streams * 2; // aux command results
        for _channel in 0..CHANNELS_PER_STREAM {
            for stream in 0..enabled_streams {
                if offset + 2 > raw.len() {
                    return 1.0;
                }
                let word = u16::from_le_bytes([raw[offset], raw[offset + 1]]);
                offset += 2;
                if sample_index >= evaluate_from {
                    if word == 0xffff || word == 0x0000 {
                        railed[stream] += 1;
                    }
                    total[stream] += 1;
                }
            }
        }
        offset += (enabled_streams % 4) * 2; // alignment padding
        offset += 8 * 2; // auxiliary ADC slots
        offset += 2; // TTL in
        offset += 2; // TTL out
    }

    (0..enabled_streams)
        .map(|stream| {
            if total[stream] == 0 {
                1.0
            } else {
                railed[stream] as f64 / total[stream] as f64
            }
        })
        .fold(f64::INFINITY, f64::min)
}

/// Extract raw i16 amplifier samples for a single logical channel from a raw
/// data block.  Channel `ch` maps to stream `ch / CHANNELS_PER_STREAM`, intra-
/// stream channel `ch % CHANNELS_PER_STREAM`.  Layout mirrors
/// `parse_rhythm_data_block`.
pub(crate) fn extract_channel_from_raw(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
    ch: usize,
) -> Vec<i16> {
    use crate::protocol::raw_word_to_signed_count;

    let stream = ch / CHANNELS_PER_STREAM;
    let intra_ch = ch % CHANNELS_PER_STREAM;

    // Frame layout (in u16 words): 4 (magic) + 2 (timestamp)
    // + 3*enabled_streams (aux) + CHANNELS_PER_STREAM*enabled_streams (amp)
    // + (enabled_streams%4) (pad) + 8 (board ADC) + 1 (TTL in) + 1 (TTL out)
    let frame_words =
        4 + 2 + enabled_streams * (CHANNELS_PER_STREAM + 3) + (enabled_streams % 4) + 8 + 2;
    let frame_bytes = frame_words * 2;

    // Amplifier data starts after magic(4w) + timestamp(2w) + aux(3*streams w).
    let amp_base_words = 4 + 2 + 3 * enabled_streams;

    let mut out = Vec::with_capacity(samples);
    for s in 0..samples {
        // Within the amplifier section: data is channel-major, stream-minor.
        // Word index = amp_base + (intra_ch * enabled_streams + stream)
        let word_idx = amp_base_words + intra_ch * enabled_streams + stream;
        let byte_off = s * frame_bytes + word_idx * 2;
        if byte_off + 2 > raw.len() {
            break;
        }
        let word = u16::from_le_bytes([raw[byte_off], raw[byte_off + 1]]);
        out.push(raw_word_to_signed_count(word));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Word count of one frame for `streams` enabled streams. Mirrors
    /// `protocol::words_per_frame`.
    fn frame_words(streams: usize) -> usize {
        4 + 2 + streams * (CHANNELS_PER_STREAM + 3) + (streams % 4) + 8 + 2
    }

    /// Word offset (within a frame) of the amplifier sample for a given
    /// intra-stream channel and stream. Channel-major, stream-minor.
    fn amp_word(streams: usize, intra_ch: usize, stream: usize) -> usize {
        (4 + 2 + 3 * streams) + intra_ch * streams + stream
    }

    /// Word offset (within a frame) of the AuxCmd3 result for a stream.
    fn aux3_word(streams: usize, stream: usize) -> usize {
        6 + 2 * streams + stream
    }

    fn to_bytes(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// Allocate a block of `samples` frames, every word initialised to 0x8000
    /// (signed count 0 — neither railed nor an INTAN byte).
    fn blank_block(streams: usize, samples: usize) -> Vec<u16> {
        vec![0x8000; frame_words(streams) * samples]
    }

    fn set_word(
        block: &mut [u16],
        streams: usize,
        sample: usize,
        word_in_frame: usize,
        value: u16,
    ) {
        block[sample * frame_words(streams) + word_in_frame] = value;
    }

    #[test]
    fn aux_command_trigger_bit_maps_each_slot() {
        assert_eq!(aux_command_trigger_bit(AuxCommandSlot::AuxCmd1), 1);
        assert_eq!(aux_command_trigger_bit(AuxCommandSlot::AuxCmd2), 2);
        assert_eq!(aux_command_trigger_bit(AuxCommandSlot::AuxCmd3), 3);
    }

    #[test]
    fn stream_range_label_single_and_pair() {
        assert_eq!(stream_range_label(4, 1), "stream 4");
        assert_eq!(stream_range_label(4, 2), "streams 4-5");
        assert_eq!(stream_range_label(0, 4), "streams 0-3");
    }

    #[test]
    fn extract_channel_reads_correct_stream_and_count() {
        let streams = 2;
        let samples = 3;
        let mut block = blank_block(streams, samples);
        // Channel 33 -> stream 1, intra-stream channel 1.
        for s in 0..samples {
            set_word(
                &mut block,
                streams,
                s,
                amp_word(streams, 1, 1),
                0x8000 + (s as u16 + 1),
            );
        }
        let raw = to_bytes(&block);
        assert_eq!(
            extract_channel_from_raw(&raw, streams, samples, 33),
            vec![1, 2, 3]
        );
        // Channel 0 (stream 0, intra 0) was left at mid-scale -> count 0.
        assert_eq!(
            extract_channel_from_raw(&raw, streams, samples, 0),
            vec![0, 0, 0]
        );
    }

    #[test]
    fn verify_chip_id_detects_intan_signature() {
        let streams = 1;
        let samples = 6;
        let mut block = blank_block(streams, samples);
        for (s, b) in [b'I', b'N', b'T', b'A', b'N'].into_iter().enumerate() {
            // Low byte carries the AuxCmd3 result byte; high byte irrelevant.
            set_word(
                &mut block,
                streams,
                s,
                aux3_word(streams, 0),
                0x4D00 | b as u16,
            );
        }
        let raw = to_bytes(&block);
        assert!(verify_chip_id_in_probe(&raw, streams, samples));
    }

    #[test]
    fn verify_chip_id_absent_returns_false() {
        let streams = 1;
        let samples = 6;
        let raw = to_bytes(&blank_block(streams, samples));
        assert!(!verify_chip_id_in_probe(&raw, streams, samples));
    }

    #[test]
    fn railed_fraction_picks_best_stream_over_second_half() {
        let streams = 2;
        let samples = 4; // second half = samples 2,3
        let mut block = blank_block(streams, samples);
        // Stream 1 fully railed in the second half; stream 0 stays valid.
        for s in 2..samples {
            for intra in 0..CHANNELS_PER_STREAM {
                set_word(&mut block, streams, s, amp_word(streams, intra, 1), 0xFFFF);
            }
        }
        let raw = to_bytes(&block);
        // min over streams -> stream 0 is 0% railed.
        assert_eq!(min_stream_railed_fraction(&raw, streams, samples), 0.0);
    }

    #[test]
    fn railed_fraction_all_railed_is_one() {
        let streams = 1;
        let samples = 4;
        let mut block = blank_block(streams, samples);
        for s in 2..samples {
            for intra in 0..CHANNELS_PER_STREAM {
                set_word(&mut block, streams, s, amp_word(streams, intra, 0), 0x0000);
            }
        }
        let raw = to_bytes(&block);
        assert_eq!(min_stream_railed_fraction(&raw, streams, samples), 1.0);
    }

    #[test]
    fn railed_fraction_truncated_block_is_one() {
        // Too-short buffer must not panic and should report fully railed.
        let raw = vec![0u8; 8];
        assert_eq!(min_stream_railed_fraction(&raw, 2, 16), 1.0);
    }
}
