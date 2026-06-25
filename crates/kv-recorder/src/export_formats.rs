//! Export recording data to third-party formats.
//!
//! Supported formats:
//! - **Intan .rhd** — Intan Technologies native format (header + raw amplifier data).
//!   Compatible with Intan RHX, NeuroScope, and other downstream tools.
//! - **Flat binary** — simple raw i16 interleaved file with a companion `.meta.json`.
//!   Compatible with SpikeGLX readers and custom analysis pipelines.

use std::borrow::Borrow;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use kv_types::SampleBlock;

use crate::{RecorderError, escape_json_string};

/// Sample-rate and channel-count header needed before streaming blocks.
///
/// The streaming exporters cannot peek at the first block to read these values
/// (the block iterator may be lazy and single-pass), so callers supply them
/// explicitly — typically from the source recording's metadata.
#[derive(Debug, Clone, Copy)]
pub struct ExportHeader {
    pub sample_rate: f64,
    pub channel_count: usize,
}

// ── Export format enum ──────────────────────────────────────────────

/// Supported data formats.
///
/// `KeyvastNative` is the application's own on-disk format (the same `.kvraw`
/// recordings are written in) and is the default — recordings are always
/// captured natively in this format.  The remaining variants are optional
/// conversions to third-party formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Keyvast native `.kvraw` — raw i16 + sidecar metadata (no conversion).
    KeyvastNative,
    /// Intan .rhd format (v2.0 header + amplifier data block).
    IntanRhd,
    /// Flat binary with companion metadata JSON (SpikeGLX-compatible).
    FlatBinary,
}

impl ExportFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::KeyvastNative => "Keyvast (.kvraw) — native",
            Self::IntanRhd => "Intan .rhd",
            Self::FlatBinary => "Flat Binary (.bin + .meta.json)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::KeyvastNative => "kvraw",
            Self::IntanRhd => "rhd",
            Self::FlatBinary => "bin",
        }
    }

    /// Whether this is the native Keyvast format (no conversion needed).
    pub fn is_native(self) -> bool {
        matches!(self, Self::KeyvastNative)
    }
}

// ── Intan .rhd format writer ────────────────────────────────────────
//
// Intan .rhd file structure (simplified, version 2.0):
//   - Header: magic number, version, sample rate, amplifier channels info
//   - Data blocks: timestamp + amplifier data per sample
//
// References:
//   - Intan RHD2000 Interface File Format Specification
//   - http://intantech.com/files/Intan_RHD2000_data_file_formats.pdf

/// Magic number for Intan .rhd files (0xC6912702 LE).
const INTAN_MAGIC: u32 = 0xC6912702;
/// Data file format version we write (2.0).
const INTAN_VERSION_MAJOR: i16 = 2;
const INTAN_VERSION_MINOR: i16 = 0;

/// Number of samples per data block in .rhd format.
const RHD_SAMPLES_PER_BLOCK: usize = 128;

/// Export blocks to Intan .rhd format (streaming, bounded memory).
///
/// Creates a single `.rhd` file containing a header followed by data blocks.
/// Each data block contains 128 timestamps + 128 amplifier samples per channel.
///
/// Trailing samples that do not fill a complete 128-sample RHD block are
/// zero-padded to form a final complete block, ensuring no data is silently
/// discarded.
pub fn export_intan_rhd(
    output_path: &Path,
    blocks: &[SampleBlock],
    notes: &str,
) -> Result<PathBuf, RecorderError> {
    if blocks.is_empty() {
        return Err(RecorderError::Io {
            path: output_path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no blocks to export"),
        });
    }
    let header = ExportHeader {
        sample_rate: blocks[0].sample_rate,
        channel_count: blocks[0].channel_count,
    };
    export_intan_rhd_streaming(output_path, header, blocks.iter(), notes)
}

/// Streaming variant of [`export_intan_rhd`].
///
/// Accepts any iterator of blocks (owned or borrowed) together with an explicit
/// [`ExportHeader`], so callers can feed data lazily from disk without
/// materializing the whole recording in memory. Memory usage stays
/// `O(RHD_SAMPLES_PER_BLOCK * channel_count)` regardless of recording length.
pub fn export_intan_rhd_streaming<I, B>(
    output_path: &Path,
    header: ExportHeader,
    blocks: I,
    notes: &str,
) -> Result<PathBuf, RecorderError>
where
    I: IntoIterator<Item = B>,
    B: Borrow<SampleBlock>,
{
    let sample_rate = header.sample_rate;
    let channel_count = header.channel_count;

    let file = File::create(output_path).map_err(|source| RecorderError::Io {
        path: output_path.to_path_buf(),
        source,
    })?;
    let mut w = BufWriter::new(file);

    // Write header
    write_rhd_header(&mut w, sample_rate, channel_count, notes, output_path)?;

    // Stream blocks through a fixed-size accumulator (128 samples × channels).
    // Memory usage is O(RHD_SAMPLES_PER_BLOCK * channel_count), not O(total).
    let rhd_block_len = RHD_SAMPLES_PER_BLOCK * channel_count;
    let mut buf_samples: Vec<i16> = Vec::with_capacity(rhd_block_len);
    let mut buf_timestamps: Vec<u32> = Vec::with_capacity(RHD_SAMPLES_PER_BLOCK);
    let mut ts = 0u32;

    for block in blocks {
        let block = block.borrow();
        // Reject malformed/partial blocks before indexing so a short `data`
        // vector surfaces as a recoverable error instead of panicking.
        block
            .validate()
            .map_err(|source| RecorderError::InvalidBlock {
                packet_id: block.packet_id,
                source,
            })?;
        if block.channel_count != channel_count {
            return Err(RecorderError::InconsistentBlockConfig {
                packet_id: block.packet_id,
                field: "channel_count",
            });
        }
        for s in 0..block.samples_per_channel {
            buf_timestamps.push(ts);
            for ch in 0..block.channel_count {
                buf_samples.push(block.data[s * block.channel_count + ch]);
            }
            ts = ts.wrapping_add(1);

            if buf_timestamps.len() == RHD_SAMPLES_PER_BLOCK {
                write_rhd_data_block(
                    &mut w,
                    &buf_timestamps,
                    &buf_samples,
                    channel_count,
                    RHD_SAMPLES_PER_BLOCK,
                    output_path,
                )?;
                buf_samples.clear();
                buf_timestamps.clear();
            }
        }
    }

    // Zero-pad any trailing samples into a final complete RHD block.
    if !buf_timestamps.is_empty() {
        let valid = buf_timestamps.len();
        let pad_samples = RHD_SAMPLES_PER_BLOCK - valid;
        buf_timestamps.extend((0..pad_samples).map(|i| ts.wrapping_add(i as u32)));
        buf_samples.extend(std::iter::repeat_n(0i16, pad_samples * channel_count));
        write_rhd_data_block(
            &mut w,
            &buf_timestamps,
            &buf_samples,
            channel_count,
            RHD_SAMPLES_PER_BLOCK,
            output_path,
        )?;
        eprintln!(
            "RHD export: zero-padded final block ({valid}/{RHD_SAMPLES_PER_BLOCK} samples valid)"
        );
    }

    w.flush().map_err(|source| RecorderError::Io {
        path: output_path.to_path_buf(),
        source,
    })?;

    Ok(output_path.to_path_buf())
}

/// Write one RHD data block (timestamps + channel-major amplifier data).
fn write_rhd_data_block(
    w: &mut BufWriter<File>,
    timestamps: &[u32],
    samples: &[i16],
    channel_count: usize,
    block_size: usize,
    path: &Path,
) -> Result<(), RecorderError> {
    // Timestamps
    for &ts in &timestamps[..block_size] {
        w.write_all(&(ts as i32).to_le_bytes())
            .map_err(|source| RecorderError::Io {
                path: path.to_path_buf(),
                source,
            })?;
    }

    // Amplifier data (channel-major: all samples for ch0, then ch1, ...)
    for ch in 0..channel_count {
        for i in 0..block_size {
            let sample_idx = i * channel_count + ch;
            let unsigned = (samples[sample_idx] as i32 + 32768) as u16;
            w.write_all(&unsigned.to_le_bytes())
                .map_err(|source| RecorderError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
    }

    Ok(())
}

fn write_rhd_header(
    w: &mut BufWriter<File>,
    sample_rate: f64,
    channel_count: usize,
    notes: &str,
    path: &Path,
) -> Result<(), RecorderError> {
    let p = path.to_path_buf();
    let e = |source| RecorderError::Io {
        path: p.clone(),
        source,
    };

    // Magic number
    w.write_all(&INTAN_MAGIC.to_le_bytes()).map_err(e)?;
    // Version
    w.write_all(&INTAN_VERSION_MAJOR.to_le_bytes()).map_err(e)?;
    w.write_all(&INTAN_VERSION_MINOR.to_le_bytes()).map_err(e)?;
    // Sample rate (f32)
    w.write_all(&(sample_rate as f32).to_le_bytes())
        .map_err(e)?;
    // DSP enabled (i16: 1 = yes)
    w.write_all(&1_i16.to_le_bytes()).map_err(e)?;
    // DSP cutoff frequency (f32)
    w.write_all(&1.0_f32.to_le_bytes()).map_err(e)?;
    // Lower bandwidth (f32)
    w.write_all(&0.1_f32.to_le_bytes()).map_err(e)?;
    // Upper bandwidth (f32)
    w.write_all(&(sample_rate as f32 / 2.0).to_le_bytes())
        .map_err(e)?;
    // Desired lower bandwidth (f32)
    w.write_all(&0.1_f32.to_le_bytes()).map_err(e)?;
    // Desired upper bandwidth (f32)
    w.write_all(&((sample_rate / 2.0) as f32).to_le_bytes())
        .map_err(e)?;
    // Notch filter mode (i16: 0 = none)
    w.write_all(&0_i16.to_le_bytes()).map_err(e)?;
    // Desired impedance test frequency (f32)
    w.write_all(&1000.0_f32.to_le_bytes()).map_err(e)?;
    // Actual impedance test frequency (f32)
    w.write_all(&1000.0_f32.to_le_bytes()).map_err(e)?;

    // Notes (3 × QString: note1, note2, note3)
    write_qstring(w, notes, path)?;
    write_qstring(w, "", path)?;
    write_qstring(w, "", path)?;

    // Number of temp sensor channels (i16: 0)
    w.write_all(&0_i16.to_le_bytes()).map_err(e)?;
    // Board mode (i16: 0)
    w.write_all(&0_i16.to_le_bytes()).map_err(e)?;

    // Reference channel (QString — empty)
    write_qstring(w, "", path)?;

    // Number of signal groups (i16)
    w.write_all(&1_i16.to_le_bytes()).map_err(e)?;

    // Signal group header
    write_qstring(w, "Port A", path)?; // group name
    write_qstring(w, "A", path)?; // group prefix
    w.write_all(&1_i16.to_le_bytes()).map_err(e)?; // enabled
    w.write_all(&(channel_count as i16).to_le_bytes())
        .map_err(e)?; // num channels
    w.write_all(&(channel_count as i16).to_le_bytes())
        .map_err(e)?; // num amp channels

    // Channel headers
    for ch in 0..channel_count {
        let name = format!("A-{:03}", ch);
        write_qstring(w, &name, path)?; // native channel name
        write_qstring(w, &name, path)?; // custom channel name
        w.write_all(&(ch as i16).to_le_bytes()).map_err(e)?; // native order
        w.write_all(&(ch as i16).to_le_bytes()).map_err(e)?; // custom order
        w.write_all(&0_i16.to_le_bytes()).map_err(e)?; // signal type (0 = amp)
        w.write_all(&1_i16.to_le_bytes()).map_err(e)?; // channel enabled
        w.write_all(&(ch as i16).to_le_bytes()).map_err(e)?; // chip channel
        w.write_all(&0_i16.to_le_bytes()).map_err(e)?; // board stream
        w.write_all(&0_i16.to_le_bytes()).map_err(e)?; // spike scope trigger
        w.write_all(&0_i16.to_le_bytes()).map_err(e)?; // voltage trigger mode
        w.write_all(&0_i16.to_le_bytes()).map_err(e)?; // voltage threshold
        w.write_all(&0_i16.to_le_bytes()).map_err(e)?; // digital trigger channel
        w.write_all(&0_i16.to_le_bytes()).map_err(e)?; // digital edge polarity
        w.write_all(&0.0_f32.to_le_bytes()).map_err(e)?; // electrode impedance mag
        w.write_all(&0.0_f32.to_le_bytes()).map_err(e)?; // electrode impedance phase
    }

    Ok(())
}

/// Write a Qt-style QString (4-byte LE length in bytes, then UTF-16LE data).
fn write_qstring(w: &mut BufWriter<File>, s: &str, path: &Path) -> Result<(), RecorderError> {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let byte_len = (utf16.len() * 2) as u32;
    w.write_all(&byte_len.to_le_bytes())
        .map_err(|source| RecorderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    for code_unit in &utf16 {
        w.write_all(&code_unit.to_le_bytes())
            .map_err(|source| RecorderError::Io {
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

// ── Flat binary format writer ───────────────────────────────────────
//
// Produces:
//   recording.bin       — raw i16 LE samples, interleaved by sample
//   recording.meta.json — companion metadata (sample rate, channels, etc.)
//
// Compatible with SpikeGLX .bin format readers and many Python loaders.

/// Export blocks to flat binary format with companion metadata.
pub fn export_flat_binary(
    output_dir: &Path,
    blocks: &[SampleBlock],
    notes: &str,
) -> Result<PathBuf, RecorderError> {
    if blocks.is_empty() {
        return Err(RecorderError::Io {
            path: output_dir.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no blocks to export"),
        });
    }
    let header = ExportHeader {
        sample_rate: blocks[0].sample_rate,
        channel_count: blocks[0].channel_count,
    };
    export_flat_binary_streaming(output_dir, header, blocks.iter(), notes)
}

/// Streaming variant of [`export_flat_binary`].
///
/// Accepts any iterator of blocks (owned or borrowed) together with an explicit
/// [`ExportHeader`], so callers can feed data lazily from disk without
/// materializing the whole recording in memory.
pub fn export_flat_binary_streaming<I, B>(
    output_dir: &Path,
    header: ExportHeader,
    blocks: I,
    notes: &str,
) -> Result<PathBuf, RecorderError>
where
    I: IntoIterator<Item = B>,
    B: Borrow<SampleBlock>,
{
    fs::create_dir_all(output_dir).map_err(|source| RecorderError::Io {
        path: output_dir.to_path_buf(),
        source,
    })?;

    let sample_rate = header.sample_rate;
    let channel_count = header.channel_count;

    // Write raw binary data
    let bin_path = output_dir.join("recording.bin");
    let file = File::create(&bin_path).map_err(|source| RecorderError::Io {
        path: bin_path.clone(),
        source,
    })?;
    let mut w = BufWriter::new(file);

    let mut total_samples: u64 = 0;
    for block in blocks {
        let block = block.borrow();
        block
            .validate()
            .map_err(|source| RecorderError::InvalidBlock {
                packet_id: block.packet_id,
                source,
            })?;
        if block.channel_count != channel_count {
            return Err(RecorderError::InconsistentBlockConfig {
                packet_id: block.packet_id,
                field: "channel_count",
            });
        }
        for sample in &block.data {
            w.write_all(&sample.to_le_bytes())
                .map_err(|source| RecorderError::Io {
                    path: bin_path.clone(),
                    source,
                })?;
        }
        total_samples += block.data.len() as u64;
    }
    w.flush().map_err(|source| RecorderError::Io {
        path: bin_path.clone(),
        source,
    })?;

    // Write companion metadata
    let meta_path = output_dir.join("recording.meta.json");
    let total_time_s = total_samples as f64 / (channel_count as f64 * sample_rate);
    let meta = format!(
        concat!(
            "{{\n",
            "  \"format\": \"flat_binary\",\n",
            "  \"sample_type\": \"int16\",\n",
            "  \"endianness\": \"little\",\n",
            "  \"layout\": \"interleaved_by_sample\",\n",
            "  \"sample_rate_hz\": {},\n",
            "  \"channel_count\": {},\n",
            "  \"total_samples\": {},\n",
            "  \"duration_seconds\": {:.6},\n",
            "  \"notes\": \"{}\",\n",
            "  \"data_file\": \"recording.bin\"\n",
            "}}\n"
        ),
        sample_rate,
        channel_count,
        total_samples,
        total_time_s,
        escape_json_string(notes),
    );
    fs::write(&meta_path, meta).map_err(|source| RecorderError::Io {
        path: meta_path.clone(),
        source,
    })?;

    Ok(bin_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_blocks(
        channels: usize,
        samples_per_ch: usize,
        n_blocks: usize,
    ) -> Vec<SampleBlock> {
        (0..n_blocks)
            .map(|i| {
                let data: Vec<i16> = (0..(channels * samples_per_ch))
                    .map(|s| ((s as i32 + i as i32 * 100) % 32767) as i16)
                    .collect();
                SampleBlock {
                    device_id: "test".to_string(),
                    stream_id: 0,
                    packet_id: i as u64,
                    timestamp_start: (i * samples_per_ch) as u64,
                    sample_rate: 30000.0,
                    channel_count: channels,
                    samples_per_channel: samples_per_ch,
                    ttl_bits: 0,
                    data,
                    aux_data: None,
                    board_adc_data: None,
                    ttl_in_per_sample: None,
                    ttl_out_per_sample: None,
                }
            })
            .collect()
    }

    #[test]
    fn flat_binary_roundtrip() {
        let dir = std::env::temp_dir().join("kv_flat_binary_test");
        let _ = fs::remove_dir_all(&dir);
        let blocks = make_test_blocks(4, 64, 3);
        let result = export_flat_binary(&dir, &blocks, "test export");
        assert!(result.is_ok());
        let bin_path = result.unwrap();
        assert!(bin_path.exists());

        // Verify size: 4 channels × 64 samples × 3 blocks × 2 bytes = 1536
        let file_size = fs::metadata(&bin_path).unwrap().len();
        assert_eq!(file_size, 4 * 64 * 3 * 2);

        // Verify metadata exists
        let meta_path = dir.join("recording.meta.json");
        assert!(meta_path.exists());
        let meta = fs::read_to_string(&meta_path).unwrap();
        assert!(meta.contains("\"channel_count\": 4"));
        assert!(meta.contains("\"sample_rate_hz\": 30000"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn intan_rhd_creates_file() {
        let dir = std::env::temp_dir().join("kv_intan_rhd_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let blocks = make_test_blocks(4, 128, 2);
        let rhd_path = dir.join("test.rhd");
        let result = export_intan_rhd(&rhd_path, &blocks, "test");
        assert!(result.is_ok());
        let out = result.unwrap();
        assert!(out.exists());

        // Verify magic at start
        let data = fs::read(&out).unwrap();
        assert!(data.len() > 8);
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(magic, INTAN_MAGIC);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_blocks_returns_error() {
        let dir = std::env::temp_dir().join("kv_empty_export");
        let _ = fs::remove_dir_all(&dir);
        let result = export_flat_binary(&dir, &[], "");
        assert!(result.is_err());
    }

    #[test]
    fn intan_rhd_header_encodes_version_and_sample_rate() {
        let dir = std::env::temp_dir().join("kv_intan_header_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let blocks = make_test_blocks(2, 128, 1);
        let rhd_path = dir.join("hdr.rhd");
        export_intan_rhd(&rhd_path, &blocks, "test").unwrap();
        let data = fs::read(&rhd_path).unwrap();

        // Fixed-offset header prefix: magic, version major/minor, sample rate.
        assert_eq!(
            u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            INTAN_MAGIC
        );
        assert_eq!(i16::from_le_bytes([data[4], data[5]]), INTAN_VERSION_MAJOR);
        assert_eq!(i16::from_le_bytes([data[6], data[7]]), INTAN_VERSION_MINOR);
        assert_eq!(
            f32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            30_000.0
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn intan_rhd_data_block_is_channel_major_and_offset_by_32768() {
        let dir = std::env::temp_dir().join("kv_intan_content_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Exactly one RHD block of one channel keeps the data section at the
        // file tail so we can locate it without parsing the variable header.
        let channels = 1;
        let blocks = make_test_blocks(channels, RHD_SAMPLES_PER_BLOCK, 1);
        let rhd_path = dir.join("content.rhd");
        export_intan_rhd(&rhd_path, &blocks, "test").unwrap();
        let data = fs::read(&rhd_path).unwrap();

        let ts_bytes = RHD_SAMPLES_PER_BLOCK * 4;
        let amp_bytes = channels * RHD_SAMPLES_PER_BLOCK * 2;
        let block_start = data.len() - ts_bytes - amp_bytes;

        // First timestamp is zero.
        assert_eq!(
            i32::from_le_bytes([
                data[block_start],
                data[block_start + 1],
                data[block_start + 2],
                data[block_start + 3],
            ]),
            0
        );

        // First amplifier sample (ch0, sample0) is the raw i16 shifted by 32768.
        let amp_start = data.len() - amp_bytes;
        let raw0 = blocks[0].data[0];
        assert_eq!(
            u16::from_le_bytes([data[amp_start], data[amp_start + 1]]),
            (raw0 as i32 + 32_768) as u16
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn intan_rhd_zero_pads_trailing_partial_block() {
        let dir = std::env::temp_dir().join("kv_intan_pad_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let channels = 2;

        // One full RHD block.
        let full = make_test_blocks(channels, RHD_SAMPLES_PER_BLOCK, 1);
        let full_path = dir.join("full.rhd");
        export_intan_rhd(&full_path, &full, "test").unwrap();
        let full_len = fs::metadata(&full_path).unwrap().len();

        // One extra sample forces a second block that must be zero-padded to a
        // full 128-sample block rather than silently dropping the remainder.
        let partial = make_test_blocks(channels, RHD_SAMPLES_PER_BLOCK + 1, 1);
        let partial_path = dir.join("partial.rhd");
        export_intan_rhd(&partial_path, &partial, "test").unwrap();
        let partial_len = fs::metadata(&partial_path).unwrap().len();

        let data_block_bytes =
            (RHD_SAMPLES_PER_BLOCK * 4 + channels * RHD_SAMPLES_PER_BLOCK * 2) as u64;
        assert_eq!(
            partial_len - full_len,
            data_block_bytes,
            "the trailing sample must produce exactly one zero-padded block"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
