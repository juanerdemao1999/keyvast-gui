use kv_rhd::{
    DEFAULT_RHD_SAMPLE_RATE, RHD_AMPLIFIER_MICROVOLTS_PER_COUNT, RhythmDataConfig,
    RhythmParseError, bytes_per_block, parse_rhythm_data_block, raw_word_to_signed_count,
    signed_count_to_microvolts, words_per_frame,
};

#[test]
fn calculates_usb_block_size_for_two_32_channel_streams() {
    assert_eq!(words_per_frame(2).expect("valid stream count"), 88);
    assert_eq!(
        bytes_per_block(2, 256).expect("valid block size"),
        45_056
    );
}

#[test]
fn converts_offset_binary_words_to_signed_counts() {
    assert_eq!(raw_word_to_signed_count(32_768), 0);
    assert_eq!(raw_word_to_signed_count(32_769), 1);
    assert_eq!(raw_word_to_signed_count(32_767), -1);
    assert_eq!(raw_word_to_signed_count(0), i16::MIN);
    assert_eq!(raw_word_to_signed_count(u16::MAX), i16::MAX);
}

#[test]
fn microvolt_scale_matches_open_ephys_rhd_display_conversion() {
    let microvolts = signed_count_to_microvolts(10);

    assert!((microvolts - 10.0 * RHD_AMPLIFIER_MICROVOLTS_PER_COUNT).abs() < f32::EPSILON);
    assert!((microvolts - 1.95).abs() < 0.0001);
}

#[test]
fn parses_two_streams_into_sample_major_stream_order() {
    let config = test_config(2, 2);
    let raw = build_raw_block(&config, 1234, 0x00ff);

    let block = parse_rhythm_data_block(7, &raw, &config).expect("raw block should parse");

    assert_eq!(block.device_id, "rhd-test");
    assert_eq!(block.packet_id, 7);
    assert_eq!(block.timestamp_start, 1234);
    assert_eq!(block.sample_rate, DEFAULT_RHD_SAMPLE_RATE);
    assert_eq!(block.channel_count, 64);
    assert_eq!(block.samples_per_channel, 2);
    assert_eq!(block.ttl_bits, 0x00ff);
    assert_eq!(block.data.len(), 128);

    assert_eq!(block.data[0], 0);
    assert_eq!(block.data[31], 31);
    assert_eq!(block.data[32], 1000);
    assert_eq!(block.data[63], 1031);
    assert_eq!(block.data[64], 100);
    assert_eq!(block.data[95], 131);
    assert_eq!(block.data[96], 1100);
    assert_eq!(block.data[127], 1131);
}

#[test]
fn rejects_bad_magic() {
    let config = test_config(1, 1);
    let mut raw = build_raw_block(&config, 0, 0);
    raw[0] = 0;

    let error = parse_rhythm_data_block(0, &raw, &config)
        .expect_err("corrupt magic should be rejected");

    assert!(matches!(error, RhythmParseError::BadMagic { sample_index: 0, .. }));
}

#[test]
fn rejects_short_buffers() {
    let config = test_config(2, 2);
    let raw = vec![0_u8; bytes_per_block(2, 2).expect("valid block size") - 1];

    let error = parse_rhythm_data_block(0, &raw, &config)
        .expect_err("short block should be rejected");

    assert!(matches!(
        error,
        RhythmParseError::LengthMismatch {
            expected: _,
            observed: _
        }
    ));
}

#[test]
fn rejects_in_frame_timestamp_gap() {
    let config = test_config(1, 2);
    let mut raw = build_raw_block(&config, 50, 0);
    let second_frame_timestamp_offset = words_per_frame(1).expect("valid words") * 2 + 8;
    raw[second_frame_timestamp_offset..second_frame_timestamp_offset + 4]
        .copy_from_slice(&99_u32.to_le_bytes());

    let error = parse_rhythm_data_block(0, &raw, &config)
        .expect_err("timestamp gap should be rejected");

    assert!(matches!(
        error,
        RhythmParseError::TimestampDiscontinuity {
            sample_index: 1,
            expected: 51,
            observed: 99
        }
    ));
}

fn test_config(enabled_streams: usize, samples_per_block: usize) -> RhythmDataConfig {
    RhythmDataConfig {
        device_id: "rhd-test".to_string(),
        stream_id: 0,
        enabled_streams,
        sample_rate: DEFAULT_RHD_SAMPLE_RATE,
        samples_per_block,
    }
}

fn build_raw_block(config: &RhythmDataConfig, timestamp_start: u32, ttl_bits: u16) -> Vec<u8> {
    let mut raw = Vec::with_capacity(
        bytes_per_block(config.enabled_streams, config.samples_per_block)
            .expect("valid test config"),
    );

    for sample in 0..config.samples_per_block {
        raw.extend_from_slice(&0xd7a2_2aaa_3813_2a53_u64.to_le_bytes());
        raw.extend_from_slice(&timestamp_start.wrapping_add(sample as u32).to_le_bytes());

        for _ in 0..3 {
            for stream in 0..config.enabled_streams {
                raw.extend_from_slice(&(0x0100_u16 + stream as u16).to_le_bytes());
            }
        }

        for channel in 0..32 {
            for stream in 0..config.enabled_streams {
                let signed = (sample as i32) * 100 + (stream as i32) * 1000 + channel;
                let word = (signed + 32_768) as u16;
                raw.extend_from_slice(&word.to_le_bytes());
            }
        }

        for _ in 0..(config.enabled_streams % 4) {
            raw.extend_from_slice(&0_u16.to_le_bytes());
        }

        for _ in 0..8 {
            raw.extend_from_slice(&0_u16.to_le_bytes());
        }

        raw.extend_from_slice(&ttl_bits.to_le_bytes());
        raw.extend_from_slice(&0_u16.to_le_bytes());
    }

    raw
}

