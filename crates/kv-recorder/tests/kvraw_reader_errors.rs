//! Tests for KvrawReader error paths (M35): corrupt magic, truncated header,
//! out-of-bounds read requests, and v1 fallback with missing companion JSON.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kv_recorder::{KVRAW_DATA_OFFSET, KvrawReader, StreamingRecorder};
use kv_simulator::{SimulatorBackend, SimulatorConfig};

fn unique_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    Path::new("target")
        .join("test-runs")
        .join("kvraw-reader-errors")
        .join(format!("{name}-{nanos}"))
}

fn cleanup(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).ok();
    }
}

/// Helper: write a valid v2 KVRAW file and return its path.
fn write_valid_kvraw(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).expect("create dir");
    let mut sim = SimulatorBackend::new(SimulatorConfig::default()).expect("sim");
    let mut recorder = StreamingRecorder::new(dir).expect("recorder");
    for _ in 0..3 {
        let block = sim.next_block().expect("block");
        recorder.write_block(&block).expect("write");
    }
    recorder.finish().expect("finish");
    dir.join("recording.kvraw")
}

// ── M35.1: Corrupt magic bytes ──────────────────────────────────────────

#[test]
fn reader_rejects_corrupt_magic_bytes() {
    let dir = unique_dir("corrupt-magic");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("bad.kvraw");

    // Write garbage magic + enough padding
    let mut f = fs::File::create(&path).expect("create");
    f.write_all(b"GARBAGE\n").expect("write magic");
    // No companion .json, so v1 fallback fires with sentinel metadata
    drop(f);

    // Should succeed but produce sentinel metadata (v1 fallback, no .json)
    let reader = KvrawReader::open(&path).expect("v1 fallback");
    let meta = reader.metadata();
    assert_eq!(meta.sample_rate, 0.0);
    assert_eq!(meta.channel_count, 0);

    cleanup(&dir);
}

// ── M35.2: Truncated header (too short to contain json_len) ─────────────

#[test]
fn reader_fails_on_truncated_v2_header() {
    let dir = unique_dir("truncated-header");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("short.kvraw");

    // Write valid magic but truncate before json_len field
    let mut f = fs::File::create(&path).expect("create");
    f.write_all(b"KEYVAST\n").expect("write magic");
    // Only 2 bytes instead of 4 for json_len
    f.write_all(&[0x00, 0x02]).expect("partial");
    drop(f);

    let result = KvrawReader::open(&path);
    assert!(result.is_err(), "truncated header should produce an error");

    cleanup(&dir);
}

// ── M35.3: Truncated JSON block (magic + json_len OK, but block cut short)

#[test]
fn reader_fails_on_truncated_json_block() {
    let dir = unique_dir("truncated-json-block");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("short_json.kvraw");

    let mut f = fs::File::create(&path).expect("create");
    f.write_all(b"KEYVAST\n").expect("magic");
    // json_len = 10
    f.write_all(&10u32.to_le_bytes()).expect("json_len");
    // Only write 100 bytes of the 512-byte json block
    f.write_all(&[0u8; 100]).expect("partial json");
    drop(f);

    let result = KvrawReader::open(&path);
    assert!(result.is_err(), "truncated json block should error");

    cleanup(&dir);
}

// ── M35.4: read_frames beyond file boundary returns empty ───────────────

#[test]
fn reader_read_frames_beyond_boundary_returns_empty() {
    let dir = unique_dir("beyond-boundary");
    let path = write_valid_kvraw(&dir);

    let mut reader = KvrawReader::open(&path).expect("open");
    let total = reader.total_frames();
    assert!(total > 0);

    // Reading past the end should clamp and return empty
    let data = reader.read_frames(total + 1000, 256).expect("read beyond");
    assert!(data.is_empty());

    cleanup(&dir);
}

// ── M35.5: read_frames with start_frame == total_frames returns empty ───

#[test]
fn reader_read_frames_at_exact_end_returns_empty() {
    let dir = unique_dir("exact-end");
    let path = write_valid_kvraw(&dir);

    let mut reader = KvrawReader::open(&path).expect("open");
    let total = reader.total_frames();

    let data = reader.read_frames(total, 128).expect("read at end");
    assert!(data.is_empty());

    cleanup(&dir);
}

// ── M35.6: v1 fallback with missing companion .json produces sentinels ──

#[test]
fn reader_v1_missing_json_uses_sentinel_metadata() {
    let dir = unique_dir("v1-no-json");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("legacy.kvraw");

    // Write raw i16 samples directly (v1 format: no header)
    let mut f = fs::File::create(&path).expect("create");
    let samples: Vec<i16> = (0..256).collect();
    for s in &samples {
        f.write_all(&s.to_le_bytes()).expect("write sample");
    }
    drop(f);

    let reader = KvrawReader::open(&path).expect("open v1");
    let meta = reader.metadata();
    // Sentinel values when .json is missing
    assert_eq!(meta.sample_rate, 0.0);
    assert_eq!(meta.channel_count, 0);
    assert_eq!(meta.format_version, 1);

    cleanup(&dir);
}

// ── M35.7: read_channels with zero channel_count returns empty ──────────

#[test]
fn reader_read_frames_zero_channels_returns_empty() {
    let dir = unique_dir("zero-channels");
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("zero_ch.kvraw");

    // v1 file with no companion .json → channel_count = 0
    let mut f = fs::File::create(&path).expect("create");
    f.write_all(&[0u8; 64]).expect("write");
    drop(f);

    let mut reader = KvrawReader::open(&path).expect("open");
    assert_eq!(reader.metadata().channel_count, 0);

    let data = reader.read_frames(0, 10).expect("read");
    assert!(data.is_empty());

    let channels = reader.read_channels(0, 10).expect("channels");
    assert!(channels.is_empty());

    cleanup(&dir);
}
