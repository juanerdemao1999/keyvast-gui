use kv_types::{DeviceBackendKind, DeviceConfig, SampleBlock, SampleBlockError};

#[test]
fn simulator_default_config_matches_mvp_contract() {
    let config = DeviceConfig::simulator_default();

    assert_eq!(config.device_id, "simulator-0");
    assert_eq!(config.backend, DeviceBackendKind::Simulator);
    assert_eq!(config.sample_rate, 30_000.0);
    assert_eq!(config.channel_count, 64);
    assert_eq!(config.samples_per_packet, 64);
    assert_eq!(config.enabled_channels, (0..64).collect::<Vec<_>>());
    assert!(config.ttl_enabled);
    assert_eq!(config.ttl_line_count, 16);
}

#[test]
fn sample_block_accepts_documented_interleaved_layout() {
    let block = SampleBlock {
        device_id: "simulator-0".to_string(),
        stream_id: 0,
        packet_id: 42,
        timestamp_start: 128,
        sample_rate: 30_000.0,
        channel_count: 2,
        samples_per_channel: 3,
        ttl_bits: 0b101,
        data: vec![1, 10, 2, 20, 3, 30],
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
        host_time_ns: None,
    };

    block.validate().expect("documented layout should validate");

    assert_eq!(block.expected_sample_values(), 6);
    assert_eq!(block.timestamp_after_block(), 131);
}

#[test]
fn sample_block_rejects_data_length_mismatch() {
    let block = SampleBlock {
        device_id: "simulator-0".to_string(),
        stream_id: 0,
        packet_id: 0,
        timestamp_start: 0,
        sample_rate: 30_000.0,
        channel_count: 2,
        samples_per_channel: 3,
        ttl_bits: 0,
        data: vec![1, 10, 2, 20, 3],
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
        host_time_ns: None,
    };

    assert_eq!(
        block.validate(),
        Err(SampleBlockError::DataLengthMismatch {
            expected: 6,
            observed: 5
        })
    );
}

#[test]
fn sample_block_rejects_ttl_bits_outside_enabled_lines() {
    let block = SampleBlock {
        device_id: "simulator-0".to_string(),
        stream_id: 0,
        packet_id: 0,
        timestamp_start: 0,
        sample_rate: 30_000.0,
        channel_count: 1,
        samples_per_channel: 1,
        ttl_bits: 1 << 16,
        data: vec![0],
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
        host_time_ns: None,
    };

    assert_eq!(
        block.validate_against_ttl_lines(16),
        Err(SampleBlockError::TtlBitsOutOfRange {
            ttl_bits: 1 << 16,
            ttl_line_count: 16
        })
    );
}
