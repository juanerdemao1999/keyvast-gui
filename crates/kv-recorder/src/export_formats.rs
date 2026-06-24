//! Export recording data to third-party formats.
//!
//! Supported formats:
//! - **Intan .rhd** — Intan Technologies native format (header + raw amplifier data).
//!   Compatible with Intan RHX, NeuroScope, and other downstream tools.
//! - **Flat binary** — simple raw i16 interleaved file with a companion `.meta.json`.
//!   Compatible with SpikeGLX readers and custom analysis pipelines.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use kv_types::SampleBlock;

use crate::{RecorderError, escape_json_string};

// ── Export format enum ──────────────────────────────────────────────

/// Supported export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Intan .rhd format (v2.0 header + amplifier data block).
    IntanRhd,
    /// Flat binary with companion metadata JSON (SpikeGLX-compatible).
    FlatBinary,
}

impl ExportFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::IntanRhd => "Intan .rhd",
            Self::FlatBinary => "Flat Binary (.bin + .meta.json)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::IntanRhd => "rhd",
            Self::FlatBinary => "bin",
        }
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

/// Export blocks to Intan .rhd format.
///
/// Creates a single `.rhd` file containing a header followed by data blocks.
/// Each data block contains 128 timestamps + 128 amplifier samples per channel.
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

    let first = &blocks[0];
    let sample_rate = first.sample_rate;
    let channel_count = first.channel_count;

    let file = File::create(output_path).map_err(|source| RecorderError::Io {
        path: output_path.to_path_buf(),
        source,
    })?;
    let mut w = BufWriter::new(file);

    // Write header
    write_rhd_header(&mut w, sample_rate, channel_count, notes, output_path)?;

    // Stream blocks into RHD data blocks (128 samples each) without
    // accumulating all samples in memory.
    let e = |source| RecorderError::Io {
        path: output_path.to_path_buf(),
        source,
    };

    // Fixed-size staging buffer for one RHD data block (128 samples × channel_count).
    let mut staging: Vec<i16> = vec![0i16; RHD_SAMPLES_PER_BLOCK * channel_count];
    let mut staging_len: usize = 0; // samples filled so far
    let mut ts: u32 = 0;
    let mut ts_buf: Vec<u32> = vec![0u32; RHD_SAMPLES_PER_BLOCK];

    for block in blocks {
        let spc = block.samples_per_channel;
        let ch = block.channel_count.min(channel_count);
        for s in 0..spc {
            // Append one sample to staging
            ts_buf[staging_len] = ts;
            for c in 0..ch {
                staging[staging_len * channel_count + c] = block.data[s * block.channel_count + c];
            }
            staging_len += 1;
            ts = ts.wrapping_add(1);

            if staging_len == RHD_SAMPLES_PER_BLOCK {
                // Flush one RHD data block
                write_rhd_data_block(&mut w, &ts_buf, &staging, channel_count, &e)?;
                staging_len = 0;
            }
        }
    }
    // Remaining samples < 128: pad with zeros and write final block
    if staging_len > 0 {
        for i in staging_len..RHD_SAMPLES_PER_BLOCK {
            ts_buf[i] = ts;
            ts = ts.wrapping_add(1);
            for c in 0..channel_count {
                staging[i * channel_count + c] = 0;
            }
        }
        write_rhd_data_block(&mut w, &ts_buf, &staging, channel_count, &e)?;
    }

    w.flush().map_err(e)?;

    Ok(output_path.to_path_buf())
}

fn write_rhd_data_block(
    w: &mut BufWriter<File>,
    timestamps: &[u32],
    samples: &[i16],
    channel_count: usize,
    e: &dyn Fn(std::io::Error) -> RecorderError,
) -> Result<(), RecorderError> {
    // Timestamps (128 × i32 LE)
    for &t in &timestamps[..RHD_SAMPLES_PER_BLOCK] {
        w.write_all(&(t as i32).to_le_bytes()).map_err(e)?;
    }
    // Amplifier data (channel_count × 128 × u16 LE)
    for ch in 0..channel_count {
        for i in 0..RHD_SAMPLES_PER_BLOCK {
            let unsigned = (samples[i * channel_count + ch] as i32 + 32768) as u16;
            w.write_all(&unsigned.to_le_bytes()).map_err(e)?;
        }
    }
    Ok(())
}

// ── Streaming RHD writer ────────────────────────────────────────────
//
// Allows callers to feed blocks incrementally without holding the entire
// recording in memory.  Used by the GUI export path for large .kvraw files.

/// Streaming Intan .rhd writer.  Call [`Self::new`] to open and write the
/// header, then [`Self::write_blocks`] one or more times, and finally
/// [`Self::finish`] to flush and pad any trailing samples.
pub struct IntanRhdStreamWriter {
    w: BufWriter<File>,
    path: PathBuf,
    channel_count: usize,
    staging: Vec<i16>,
    ts_buf: Vec<u32>,
    staging_len: usize,
    ts: u32,
}

impl IntanRhdStreamWriter {
    /// Create a new streaming writer, writing the header immediately.
    pub fn new(
        output_path: &Path,
        sample_rate: f64,
        channel_count: usize,
        notes: &str,
    ) -> Result<Self, RecorderError> {
        let file = File::create(output_path).map_err(|source| RecorderError::Io {
            path: output_path.to_path_buf(),
            source,
        })?;
        let mut w = BufWriter::new(file);
        write_rhd_header(&mut w, sample_rate, channel_count, notes, output_path)?;
        Ok(Self {
            w,
            path: output_path.to_path_buf(),
            channel_count,
            staging: vec![0i16; RHD_SAMPLES_PER_BLOCK * channel_count],
            ts_buf: vec![0u32; RHD_SAMPLES_PER_BLOCK],
            staging_len: 0,
            ts: 0,
        })
    }

    /// Feed one batch of blocks into the writer.  Can be called repeatedly.
    pub fn write_blocks(&mut self, blocks: &[SampleBlock]) -> Result<(), RecorderError> {
        let e = |source: std::io::Error| RecorderError::Io {
            path: self.path.clone(),
            source,
        };
        for block in blocks {
            let ch = block.channel_count.min(self.channel_count);
            for s in 0..block.samples_per_channel {
                self.ts_buf[self.staging_len] = self.ts;
                for c in 0..ch {
                    self.staging[self.staging_len * self.channel_count + c] =
                        block.data[s * block.channel_count + c];
                }
                self.staging_len += 1;
                self.ts = self.ts.wrapping_add(1);

                if self.staging_len == RHD_SAMPLES_PER_BLOCK {
                    write_rhd_data_block(
                        &mut self.w,
                        &self.ts_buf,
                        &self.staging,
                        self.channel_count,
                        &e,
                    )?;
                    self.staging_len = 0;
                }
            }
        }
        Ok(())
    }

    /// Flush remaining samples (zero-padded to a full 128-sample block).
    pub fn finish(mut self) -> Result<PathBuf, RecorderError> {
        if self.staging_len > 0 {
            for i in self.staging_len..RHD_SAMPLES_PER_BLOCK {
                self.ts_buf[i] = self.ts;
                self.ts = self.ts.wrapping_add(1);
                for c in 0..self.channel_count {
                    self.staging[i * self.channel_count + c] = 0;
                }
            }
            let e = |source: std::io::Error| RecorderError::Io {
                path: self.path.clone(),
                source,
            };
            write_rhd_data_block(
                &mut self.w,
                &self.ts_buf,
                &self.staging,
                self.channel_count,
                &e,
            )?;
        }
        self.w.flush().map_err(|source| RecorderError::Io {
            path: self.path.clone(),
            source,
        })?;
        Ok(self.path)
    }
}

// ── Streaming Flat Binary writer ────────────────────────────────────

/// Streaming flat binary writer for large recordings.
pub struct FlatBinaryStreamWriter {
    w: BufWriter<File>,
    bin_path: PathBuf,
    output_dir: PathBuf,
    sample_rate: f64,
    channel_count: usize,
    total_samples: u64,
    notes: String,
}

impl FlatBinaryStreamWriter {
    /// Create writer; the output directory and .bin file are created immediately.
    pub fn new(
        output_dir: &Path,
        sample_rate: f64,
        channel_count: usize,
        notes: &str,
    ) -> Result<Self, RecorderError> {
        fs::create_dir_all(output_dir).map_err(|source| RecorderError::Io {
            path: output_dir.to_path_buf(),
            source,
        })?;
        let bin_path = output_dir.join("recording.bin");
        let file = File::create(&bin_path).map_err(|source| RecorderError::Io {
            path: bin_path.clone(),
            source,
        })?;
        let w = BufWriter::new(file);
        Ok(Self {
            w,
            bin_path,
            output_dir: output_dir.to_path_buf(),
            sample_rate,
            channel_count,
            total_samples: 0,
            notes: notes.to_string(),
        })
    }

    /// Append blocks to the flat binary file.
    pub fn write_blocks(&mut self, blocks: &[SampleBlock]) -> Result<(), RecorderError> {
        for block in blocks {
            for sample in &block.data {
                self.w
                    .write_all(&sample.to_le_bytes())
                    .map_err(|source| RecorderError::Io {
                        path: self.bin_path.clone(),
                        source,
                    })?;
            }
            self.total_samples += block.data.len() as u64;
        }
        Ok(())
    }

    /// Flush and write the companion metadata JSON.
    pub fn finish(mut self) -> Result<PathBuf, RecorderError> {
        self.w.flush().map_err(|source| RecorderError::Io {
            path: self.bin_path.clone(),
            source,
        })?;

        let meta_path = self.output_dir.join("recording.meta.json");
        let total_time_s =
            self.total_samples as f64 / (self.channel_count as f64 * self.sample_rate);
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
            self.sample_rate,
            self.channel_count,
            self.total_samples,
            total_time_s,
            escape_json_string(&self.notes),
        );
        fs::write(&meta_path, meta).map_err(|source| RecorderError::Io {
            path: meta_path.clone(),
            source,
        })?;

        Ok(self.bin_path)
    }
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

    fs::create_dir_all(output_dir).map_err(|source| RecorderError::Io {
        path: output_dir.to_path_buf(),
        source,
    })?;

    let first = &blocks[0];
    let sample_rate = first.sample_rate;
    let channel_count = first.channel_count;

    // Write raw binary data
    let bin_path = output_dir.join("recording.bin");
    let file = File::create(&bin_path).map_err(|source| RecorderError::Io {
        path: bin_path.clone(),
        source,
    })?;
    let mut w = BufWriter::new(file);

    let mut total_samples: u64 = 0;
    for block in blocks {
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
}
