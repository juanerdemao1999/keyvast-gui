//! Pure Rhythm frame-layout analysis helpers used during MISO bring-up and
//! impedance measurement. They operate on raw little-endian USB block bytes
//! and have no hardware dependency, which keeps them unit-testable.

use crate::commands::AuxCommandSlot;
use crate::protocol::{CHANNELS_PER_STREAM, FrameLayout};

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

    let layout = FrameLayout::new(enabled_streams);
    let pattern: [u8; 5] = *b"INTAN";

    for stream in 0..enabled_streams {
        let word_offset_in_frame = layout.auxcmd3_word_offset(stream);
        let mut aux_bytes: Vec<u8> = Vec::with_capacity(samples);
        for s in 0..samples {
            let off = layout.word_byte_offset(s, word_offset_in_frame);
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
    let layout = FrameLayout::new(enabled_streams);
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
        offset += layout.filler_words() * 2; // alignment padding
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
    let layout = FrameLayout::new(enabled_streams);
    let word_idx = layout.amp_word_offset(intra_ch, stream);

    let mut out = Vec::with_capacity(samples);
    for s in 0..samples {
        let byte_off = layout.word_byte_offset(s, word_idx);
        if byte_off + 2 > raw.len() {
            break;
        }
        let word = u16::from_le_bytes([raw[byte_off], raw[byte_off + 1]]);
        out.push(raw_word_to_signed_count(word));
    }
    out
}

/// Diagnostic: mean of the raw amplifier u16 words on `stream` over the second
/// half of a probe block. A correctly-aligned RHD2000 with DSP on sits near the
/// 0x8000 midscale; a value near 0x4000 means the 16-bit word is sampled one SPI
/// bit early (right-shifted), i.e. a wrong MISO phase.
pub(crate) fn amplifier_mean_raw_word(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
    stream: usize,
) -> Option<u32> {
    if enabled_streams == 0 || samples == 0 || stream >= enabled_streams {
        return None;
    }
    let layout = FrameLayout::new(enabled_streams);
    let from = samples / 2;
    let mut sum: u64 = 0;
    let mut n: u64 = 0;
    for s in from..samples {
        for intra in 0..CHANNELS_PER_STREAM {
            let off = layout.word_byte_offset(s, layout.amp_word_offset(intra, stream));
            if off + 2 > raw.len() {
                return (n > 0).then(|| (sum / n) as u32);
            }
            sum += u16::from_le_bytes([raw[off], raw[off + 1]]) as u64;
            n += 1;
        }
    }
    (n > 0).then(|| (sum / n) as u32)
}

/// Diagnostic: locate the "INTAN" signature in the AuxCmd3 results for `stream`
/// and return the register-63 chip-ID byte (1=RHD2132, 2=RHD2216, 4=RHD2164). In
/// `create_command_list_register_config` the ROM read block is
/// `[63, 62, 61, 60, 59, 48..55, 40('I'), 41('N'), 42('T'), 43('A'), 44('N')]`,
/// so the reg-63 result lands exactly 13 result words before the 'I'.
pub(crate) fn probe_chip_id(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
    stream: usize,
) -> Option<u8> {
    if enabled_streams == 0 || samples < 18 || stream >= enabled_streams {
        return None;
    }
    let layout = FrameLayout::new(enabled_streams);
    let word_offset_in_frame = layout.auxcmd3_word_offset(stream);
    let mut aux: Vec<u8> = Vec::with_capacity(samples);
    for s in 0..samples {
        let off = layout.word_byte_offset(s, word_offset_in_frame);
        if off + 2 > raw.len() {
            return None;
        }
        aux.push((u16::from_le_bytes([raw[off], raw[off + 1]]) & 0xff) as u8);
    }
    let pattern: [u8; 5] = *b"INTAN";
    for k in 13..aux.len().saturating_sub(4) {
        if aux[k..k + 5] == pattern {
            return Some(aux[k - 13]);
        }
    }
    None
}

/// Extract the AuxCmd3 result low-bytes (the RHD register-readback stream) for
/// `stream` across `samples` probe frames. These are the raw MISO bytes the chip
/// returned for the register-config command list: a responding chip shows the
/// ASCII "INTAN" (`49 4e 54 41 4e`) with the reg-63 chip-ID byte 13 words
/// earlier, an idle/unpowered line shows all `ff` (or `00`), and a wrong MISO
/// phase shows shifted garbage. Used for the debug-level bring-up hex dump.
pub(crate) fn auxcmd3_bytes(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
    stream: usize,
) -> Vec<u8> {
    if enabled_streams == 0 || stream >= enabled_streams {
        return Vec::new();
    }
    let layout = FrameLayout::new(enabled_streams);
    let word_offset = layout.auxcmd3_word_offset(stream);
    let mut out = Vec::with_capacity(samples);
    for s in 0..samples {
        let off = layout.word_byte_offset(s, word_offset);
        if off + 2 > raw.len() {
            break;
        }
        out.push((u16::from_le_bytes([raw[off], raw[off + 1]]) & 0xff) as u8);
    }
    out
}

/// First `count` raw amplifier words (u16) of `stream`, from sample 0 onward,
/// for a debug hex peek at what the MISO line actually deserialised.
pub(crate) fn first_amp_words(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
    stream: usize,
    count: usize,
) -> Vec<u16> {
    if enabled_streams == 0 || stream >= enabled_streams {
        return Vec::new();
    }
    let layout = FrameLayout::new(enabled_streams);
    let mut out = Vec::with_capacity(count);
    'outer: for s in 0..samples {
        for intra in 0..CHANNELS_PER_STREAM {
            if out.len() >= count {
                break 'outer;
            }
            let off = layout.word_byte_offset(s, layout.amp_word_offset(intra, stream));
            if off + 2 > raw.len() {
                break 'outer;
            }
            out.push(u16::from_le_bytes([raw[off], raw[off + 1]]));
        }
    }
    out
}

/// One-line integrity summary of a probe block, independent of the MISO/SPI
/// amplifier data: checks the Rhythm frame magic sits at every frame boundary
/// and that the per-frame 32-bit timestamps increment by 1. Magic + timestamp
/// come from the FPGA framer, not the amplifier readback, so this isolates a
/// data-plane bitstream that is not producing valid frames at all (bad/absent
/// magic, or a timestamp that never advances) from one whose frames are fine but
/// whose amplifier MISO phase is wrong (magic OK, amplifier words railed).
/// Returns `(frames_ok, human_summary)`.
pub(crate) fn probe_frame_integrity(
    raw: &[u8],
    enabled_streams: usize,
    samples: usize,
) -> (bool, String) {
    use crate::protocol::RHYTHM_HEADER_MAGIC;

    if enabled_streams == 0 || samples == 0 {
        return (false, "no samples".to_string());
    }
    let layout = FrameLayout::new(enabled_streams);
    let bytes_per_frame = layout.bytes_per_frame();
    let mut checked = 0_usize;
    let mut magic_ok = 0_usize;
    let mut ts_ok = true;
    let mut first_ts: Option<u32> = None;

    for s in 0..samples {
        let base = s * bytes_per_frame;
        if base + 12 > raw.len() {
            break;
        }
        checked += 1;
        let magic = u64::from_le_bytes([
            raw[base],
            raw[base + 1],
            raw[base + 2],
            raw[base + 3],
            raw[base + 4],
            raw[base + 5],
            raw[base + 6],
            raw[base + 7],
        ]);
        if magic == RHYTHM_HEADER_MAGIC {
            magic_ok += 1;
        }
        let ts = u32::from_le_bytes([raw[base + 8], raw[base + 9], raw[base + 10], raw[base + 11]]);
        let expected = first_ts.get_or_insert(ts).wrapping_add(s as u32);
        if ts != expected {
            ts_ok = false;
        }
    }

    let nonzero = raw.iter().filter(|&&b| b != 0).count();
    let frac = if raw.is_empty() {
        0.0
    } else {
        nonzero as f64 / raw.len() as f64
    };
    let ok = checked > 0 && magic_ok == checked && ts_ok;

    // When the magic is not sitting at the expected frame boundaries, search the
    // whole block for the 8-byte magic and report (a) where it actually starts
    // and (b) the stride between consecutive magics. This distinguishes three
    // very different failures:
    //   * a constant non-zero first offset with the EXPECTED stride  => the read
    //     is merely frame-*misaligned* (e.g. stale FIFO bytes ahead of the run;
    //     fixable by flushing/resyncing before the probe);
    //   * a stride that differs from `bytes_per_frame`               => the FPGA
    //     emits a different frame SIZE than assumed (stream-count / filler /
    //     channel-order layout mismatch — the parser offsets are wrong);
    //   * no magic anywhere                                          => the data
    //     plane is not emitting Rhythm frames at all.
    let alignment = if magic_ok == checked {
        String::new()
    } else {
        let offs = magic_offsets(raw, 3);
        match offs.first() {
            None => {
                ", magic pattern NOT FOUND anywhere in block => FPGA not emitting Rhythm frames"
                    .to_string()
            }
            Some(&first) => {
                let misaligned_by = first % bytes_per_frame;
                match offs.get(1) {
                    Some(&second) => {
                        let stride = second - first;
                        let verdict = if stride == bytes_per_frame {
                            "MISALIGNED only (frame size matches)"
                        } else {
                            "FRAME-SIZE MISMATCH (parser offsets are wrong)"
                        };
                        format!(
                            ", first magic at byte {first} (misaligned_by={misaligned_by}), \
                             magic-to-magic stride={stride} B vs expected {bytes_per_frame} B => \
                             {verdict}"
                        )
                    }
                    None => format!(
                        ", one magic at byte {first} (misaligned_by={misaligned_by}), \
                         no second magic to measure stride"
                    ),
                }
            }
        }
    };

    let summary = format!(
        "magic {}/{} frames OK, timestamps {}, nonzero_bytes={:.1}% (words/frame={}, bytes/frame={}){}",
        magic_ok,
        checked,
        if ts_ok {
            "monotonic +1"
        } else {
            "DISCONTINUOUS"
        },
        frac * 100.0,
        layout.words_per_frame(),
        bytes_per_frame,
        alignment,
    );
    (ok, summary)
}

/// Byte offsets of up to `max` occurrences of the 8-byte Rhythm frame magic
/// anywhere in the block (not just at frame boundaries). Consecutive offsets let
/// callers measure the true magic-to-magic stride = the real frame size.
pub(crate) fn magic_offsets(raw: &[u8], max: usize) -> Vec<usize> {
    let magic = crate::protocol::RHYTHM_HEADER_MAGIC.to_le_bytes();
    let mut out = Vec::new();
    let mut i = 0_usize;
    while i + 8 <= raw.len() && out.len() < max {
        if raw[i..i + 8] == magic {
            out.push(i);
            i += 8; // skip the matched magic; the next frame's magic is >=1 frame away
        } else {
            i += 1;
        }
    }
    out
}

/// Format bytes as compact space-separated hex for a log line, e.g.
/// `[49 4e 54 41 4e]`.
pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    let joined = bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("[{joined}]")
}

/// Format 16-bit words as compact space-separated hex for a log line, e.g.
/// `[8000 7fff 4000]`.
pub(crate) fn hex_words(words: &[u16]) -> String {
    let joined = words
        .iter()
        .map(|w| format!("{w:04x}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("[{joined}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Word count of one frame for `streams` enabled streams.
    fn frame_words(streams: usize) -> usize {
        FrameLayout::new(streams).words_per_frame()
    }

    /// Word offset (within a frame) of the amplifier sample for a given
    /// intra-stream channel and stream. Channel-major, stream-minor.
    fn amp_word(streams: usize, intra_ch: usize, stream: usize) -> usize {
        FrameLayout::new(streams).amp_word_offset(intra_ch, stream)
    }

    /// Word offset (within a frame) of the AuxCmd3 result for a stream.
    fn aux3_word(streams: usize, stream: usize) -> usize {
        FrameLayout::new(streams).auxcmd3_word_offset(stream)
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
        for (s, b) in (*b"INTAN").into_iter().enumerate() {
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

    /// Build a block of `samples` frames with valid Rhythm magic and per-frame
    /// timestamps 0,1,2,… (payload words left at 0x8000 mid-scale).
    fn write_valid_frames(streams: usize, samples: usize) -> Vec<u16> {
        use crate::protocol::RHYTHM_HEADER_MAGIC;
        let mut block = blank_block(streams, samples);
        let fw = frame_words(streams);
        for s in 0..samples {
            let base = s * fw;
            for (i, slot) in block[base..base + 4].iter_mut().enumerate() {
                *slot = ((RHYTHM_HEADER_MAGIC >> (16 * i)) & 0xffff) as u16;
            }
            let ts = s as u32;
            block[base + 4] = (ts & 0xffff) as u16;
            block[base + 5] = (ts >> 16) as u16;
        }
        block
    }

    #[test]
    fn frame_integrity_ok_on_valid_block() {
        let (streams, samples) = (1, 4);
        let raw = to_bytes(&write_valid_frames(streams, samples));
        let (ok, summary) = probe_frame_integrity(&raw, streams, samples);
        assert!(ok, "summary: {summary}");
        assert!(summary.contains("4/4 frames OK"), "summary: {summary}");
    }

    #[test]
    fn frame_integrity_flags_bad_magic() {
        let (streams, samples) = (1, 4);
        let mut block = write_valid_frames(streams, samples);
        let fw = frame_words(streams);
        block[2 * fw] = 0x0000; // corrupt frame 2's magic
        let raw = to_bytes(&block);
        assert!(!probe_frame_integrity(&raw, streams, samples).0);
    }

    #[test]
    fn frame_integrity_flags_timestamp_discontinuity() {
        let (streams, samples) = (1, 4);
        let mut block = write_valid_frames(streams, samples);
        let fw = frame_words(streams);
        block[3 * fw + 4] = 99; // frame 3 ts_lo jumps
        let raw = to_bytes(&block);
        assert!(!probe_frame_integrity(&raw, streams, samples).0);
    }

    #[test]
    fn auxcmd3_bytes_reads_low_byte_per_sample() {
        let (streams, samples) = (1, 3);
        let mut block = blank_block(streams, samples);
        for (s, b) in [0x11_u8, 0x22, 0x33].into_iter().enumerate() {
            set_word(
                &mut block,
                streams,
                s,
                aux3_word(streams, 0),
                0xAB00 | b as u16,
            );
        }
        let raw = to_bytes(&block);
        assert_eq!(
            auxcmd3_bytes(&raw, streams, samples, 0),
            vec![0x11, 0x22, 0x33]
        );
    }

    #[test]
    fn hex_helpers_format_compactly() {
        assert_eq!(hex_bytes(&[0x49, 0x4e, 0x54]), "[49 4e 54]");
        assert_eq!(hex_words(&[0x8000, 0x4000]), "[8000 4000]");
        assert_eq!(hex_bytes(&[]), "[]");
    }

    #[test]
    fn frame_integrity_reports_misalignment_offset() {
        let (streams, samples) = (1, 4);
        let aligned = to_bytes(&write_valid_frames(streams, samples));
        // Prepend 6 stray bytes so the magic is no longer at a frame boundary.
        let mut raw = vec![0xAA_u8; 6];
        raw.extend_from_slice(&aligned);
        let (ok, summary) = probe_frame_integrity(&raw, streams, samples);
        assert!(!ok);
        assert_eq!(magic_offsets(&raw, 1), vec![6]);
        assert!(
            summary.contains("first magic at byte 6"),
            "summary: {summary}"
        );
        assert!(summary.contains("MISALIGNED only"), "summary: {summary}");
    }

    #[test]
    fn frame_integrity_reports_no_magic_anywhere() {
        let (streams, samples) = (1, 4);
        let raw = to_bytes(&blank_block(streams, samples)); // 0x8000 fill, no magic
        let (ok, summary) = probe_frame_integrity(&raw, streams, samples);
        assert!(!ok);
        assert!(magic_offsets(&raw, 1).is_empty());
        assert!(summary.contains("NOT FOUND anywhere"), "summary: {summary}");
    }
}
