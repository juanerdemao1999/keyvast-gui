//! Tests for export_intan_rhd and export_flat_binary content verification (M36).
//!
//! Verifies: magic bytes, data block structure, amplifier data content,
//! truncation/padding when total_samples % 128 != 0, and flat binary round-trip.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kv_recorder::export_formats::{ExportFormat, export_flat_binary, export_intan_rhd};
use kv_types::SampleBlock;

fn unique_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    Path::new("target")
        .join("test-runs")
        .join("export-formats")
        .join(format!("{name}-{nanos}"))
}

fn cleanup(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).ok();
    }
}

fn make_block(channel_count: usize, samples_per_channel: usize, base_val: i16) -> SampleBlock {
    let total = channel_count * samples_per_channel;
    let data: Vec<i16> = (0..total)
        .map(|i| base_val.wrapping_add(i as i16))
        .collect();
    SampleBlock {
        device_id: "test-device".to_string(),
        stream_id: 0,
        packet_id: 0,
        timestamp_start: 0,
        sample_rate: 30_000.0,
        channel_count,
        samples_per_channel,
        ttl_bits: 0,
        data,
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
    }
}

// ── M36.1: RHD magic number is correct ──────────────────────────────────

#[test]
fn rhd_export_writes_correct_magic_number() {
    let dir = unique_dir("rhd-magic");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("test.rhd");

    let block = make_block(2, 128, 100);
    export_intan_rhd(&path, &[block], "test").expect("export");

    let bytes = fs::read(&path).expect("read");
    // Intan magic: 0xC6912702 LE
    assert_eq!(&bytes[0..4], &[0x02, 0x27, 0x91, 0xC6]);

    cleanup(&dir);
}

// ── M36.2: RHD version is 2.0 ──────────────────────────────────────────

#[test]
fn rhd_export_writes_version_2_0() {
    let dir = unique_dir("rhd-version");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("test.rhd");

    let block = make_block(2, 128, 0);
    export_intan_rhd(&path, &[block], "").expect("export");

    let bytes = fs::read(&path).expect("read");
    // After magic (4 bytes): version major (i16 LE) then minor (i16 LE)
    let major = i16::from_le_bytes([bytes[4], bytes[5]]);
    let minor = i16::from_le_bytes([bytes[6], bytes[7]]);
    assert_eq!(major, 2);
    assert_eq!(minor, 0);

    cleanup(&dir);
}

// ── M36.3: Amplifier data in data blocks is correctly offset by +32768 ──

#[test]
fn rhd_export_amplifier_data_offset_is_correct() {
    let dir = unique_dir("rhd-amp-offset");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("test.rhd");

    // 1 channel, 128 samples (exactly one RHD block)
    let ch = 1;
    let spc = 128;
    let data: Vec<i16> = (0..128).map(|i| i as i16 - 64).collect();
    let block = SampleBlock {
        device_id: "test".to_string(),
        stream_id: 0,
        packet_id: 0,
        timestamp_start: 0,
        sample_rate: 30_000.0,
        channel_count: ch,
        samples_per_channel: spc,
        ttl_bits: 0,
        data: data.clone(),
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
    };
    export_intan_rhd(&path, &[block], "").expect("export");

    let bytes = fs::read(&path).expect("read");
    // Find the data blocks section. The header size varies but we can scan for
    // the first data block by looking for the timestamp pattern.
    // Each data block: 128 timestamps (i32 LE) + channel_count * 128 samples (u16 LE)
    // Timestamps start at 0,1,2,...127
    // We search for the pattern 0x00000000 0x01000000 0x02000000 near the end of file.

    let ts_size = 128 * 4; // 128 i32 timestamps
    let amp_size = ch * 128 * 2; // amplifier data (u16)
    let block_size = ts_size + amp_size;

    // The data blocks are at the end of the file
    let file_len = bytes.len();
    assert!(file_len > block_size, "file too small for one data block");

    let data_block_start = file_len - block_size;

    // Verify first few timestamps
    let ts0 = i32::from_le_bytes([
        bytes[data_block_start],
        bytes[data_block_start + 1],
        bytes[data_block_start + 2],
        bytes[data_block_start + 3],
    ]);
    assert_eq!(ts0, 0);

    let ts1 = i32::from_le_bytes([
        bytes[data_block_start + 4],
        bytes[data_block_start + 5],
        bytes[data_block_start + 6],
        bytes[data_block_start + 7],
    ]);
    assert_eq!(ts1, 1);

    // Verify amplifier data: original[i] + 32768 stored as u16 LE
    let amp_start = data_block_start + ts_size;
    for i in 0..128 {
        let stored = u16::from_le_bytes([bytes[amp_start + i * 2], bytes[amp_start + i * 2 + 1]]);
        let expected = (data[i] as i32 + 32768) as u16;
        assert_eq!(stored, expected, "mismatch at sample {i}");
    }

    cleanup(&dir);
}

// ── M36.4: Padding when total_samples % 128 != 0 ───────────────────────

#[test]
fn rhd_export_pads_final_block_with_zeros() {
    let dir = unique_dir("rhd-padding");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("test.rhd");

    // 1 channel, 100 samples (less than 128 → needs padding to 128)
    let ch = 1;
    let spc = 100;
    let data: Vec<i16> = (0..100).map(|i| (i + 1) as i16).collect();
    let block = SampleBlock {
        device_id: "test".to_string(),
        stream_id: 0,
        packet_id: 0,
        timestamp_start: 0,
        sample_rate: 30_000.0,
        channel_count: ch,
        samples_per_channel: spc,
        ttl_bits: 0,
        data,
        aux_data: None,
        board_adc_data: None,
        ttl_in_per_sample: None,
        ttl_out_per_sample: None,
    };
    export_intan_rhd(&path, &[block], "").expect("export");

    let bytes = fs::read(&path).expect("read");
    let ts_size = 128 * 4;
    let amp_size = ch * 128 * 2;
    let block_size = ts_size + amp_size;

    let file_len = bytes.len();
    let data_block_start = file_len - block_size;
    let amp_start = data_block_start + ts_size;

    // Samples 100..127 should be zero (padded), stored as 0 + 32768 = 32768
    for i in 100..128 {
        let stored = u16::from_le_bytes([bytes[amp_start + i * 2], bytes[amp_start + i * 2 + 1]]);
        assert_eq!(
            stored, 32768,
            "padded sample {i} should be 32768 (zero + offset)"
        );
    }

    cleanup(&dir);
}

// ── M36.5: Multiple blocks spanning two RHD data blocks ─────────────────

#[test]
fn rhd_export_multiple_blocks_produce_correct_block_count() {
    let dir = unique_dir("rhd-multi-block");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("test.rhd");

    // 2 channels, 200 samples per channel = 400 total samples
    // Should produce 2 RHD data blocks (128 + 72 padded to 128)
    let blocks = vec![make_block(2, 100, 0), make_block(2, 100, 200)];
    export_intan_rhd(&path, &blocks, "multi").expect("export");

    let bytes = fs::read(&path).expect("read");
    let ts_size = 128 * 4;
    let amp_size = 2 * 128 * 2; // 2 channels × 128 samples × 2 bytes
    let block_size = ts_size + amp_size;

    // File should have header + 2 data blocks
    let header_size = bytes.len() - 2 * block_size;
    assert!(header_size > 0, "header should be non-empty");

    // Verify second data block's first timestamp is 128
    let second_block_start = header_size + block_size;
    let ts128 = i32::from_le_bytes([
        bytes[second_block_start],
        bytes[second_block_start + 1],
        bytes[second_block_start + 2],
        bytes[second_block_start + 3],
    ]);
    assert_eq!(ts128, 128);

    cleanup(&dir);
}

// ── M36.6: Empty blocks produces an error ───────────────────────────────

#[test]
fn rhd_export_empty_blocks_returns_error() {
    let dir = unique_dir("rhd-empty");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("test.rhd");

    let result = export_intan_rhd(&path, &[], "");
    assert!(result.is_err());

    cleanup(&dir);
}

// ── M36.7: Flat binary round-trip verifies correct data ─────────────────

#[test]
fn flat_binary_round_trips_data_correctly() {
    let dir = unique_dir("flat-roundtrip");

    let blocks = vec![make_block(4, 64, -100), make_block(4, 64, 500)];
    let total_samples: usize = blocks.iter().map(|b| b.data.len()).sum();

    let bin_path = export_flat_binary(&dir, &blocks, "round-trip test").expect("export");

    let bytes = fs::read(&bin_path).expect("read bin");
    assert_eq!(bytes.len(), total_samples * 2);

    // Verify first few samples match
    for (i, block) in blocks.iter().enumerate() {
        let offset = if i == 0 { 0 } else { blocks[0].data.len() * 2 };
        for (j, &sample) in block.data.iter().enumerate().take(10) {
            let stored = i16::from_le_bytes([bytes[offset + j * 2], bytes[offset + j * 2 + 1]]);
            assert_eq!(stored, sample, "mismatch at block {i} sample {j}");
        }
    }

    // Verify companion metadata exists
    let meta_path = dir.join("recording.meta.json");
    assert!(meta_path.exists());
    let meta_str = fs::read_to_string(&meta_path).expect("read meta");
    assert!(meta_str.contains("\"channel_count\": 4"));
    assert!(meta_str.contains("\"sample_rate_hz\": 30000"));

    cleanup(&dir);
}

// ── M36.8: ExportFormat label and extension ─────────────────────────────

#[test]
fn export_format_labels_and_extensions() {
    assert_eq!(ExportFormat::IntanRhd.extension(), "rhd");
    assert_eq!(ExportFormat::FlatBinary.extension(), "bin");
    assert!(!ExportFormat::IntanRhd.label().is_empty());
    assert!(!ExportFormat::FlatBinary.label().is_empty());
}
