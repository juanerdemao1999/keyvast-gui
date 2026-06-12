//! Real-time block statistics for the GUI status bar and performance display.
//!
//! `PreviewState`, `start_preview`, and the old mpsc-based preview thread have
//! been replaced by `live_pipeline.rs` (Device mode) and the demo generator
//! (Demo mode).  This file now contains only the shared `BlockStats` /
//! `ChannelStats` types and the `compute_block_stats` helper.

use kv_types::SampleBlock;

// ── Statistics types ─────────────────────────────────────────────────

/// Per-channel real-time statistics derived from the most recent block.
#[derive(Debug, Clone)]
pub struct ChannelStats {
    #[allow(dead_code)] // pending per-channel stats panel
    pub rms: f64,
    #[allow(dead_code)] // pending per-channel stats panel
    pub peak_to_peak: i16,
    #[allow(dead_code)]
    pub min: i16,
    #[allow(dead_code)]
    pub max: i16,
}

/// Aggregated statistics for the latest block (shown in the status bar).
#[derive(Debug, Clone)]
pub struct BlockStats {
    /// Per-channel RMS / peak-to-peak (reserved for future CHANNELS panel use).
    #[allow(dead_code)]
    pub channels: Vec<ChannelStats>,
    pub data_rate_mb_s: f64,
    pub block_rate_hz: f64,
    pub total_blocks: u64,
    #[allow(dead_code)]
    pub total_samples: u64,
    pub elapsed_seconds: f64,
    pub dropped_blocks: u64,
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Compute `BlockStats` from a single block.
///
/// Used by Demo mode (`tick_demo`) and the live pipeline (`tick_device`).
/// `dropped_blocks` is the number of gaps detected via packet-ID discontinuity.
pub fn compute_block_stats(
    block: &SampleBlock,
    total_blocks: u64,
    elapsed_seconds: f64,
    dropped_blocks: u64,
) -> BlockStats {
    let channel_stats = compute_channel_stats(block);
    let total_samples = total_blocks * (block.samples_per_channel * block.channel_count) as u64;
    let bytes_total = total_samples * 2;
    let data_rate_mb_s = if elapsed_seconds > 0.0 {
        bytes_total as f64 / elapsed_seconds / 1_000_000.0
    } else {
        0.0
    };
    let block_rate_hz = if elapsed_seconds > 0.0 {
        total_blocks as f64 / elapsed_seconds
    } else {
        0.0
    };
    BlockStats {
        channels: channel_stats,
        data_rate_mb_s,
        block_rate_hz,
        total_blocks,
        total_samples,
        elapsed_seconds,
        dropped_blocks,
    }
}

fn compute_channel_stats(block: &SampleBlock) -> Vec<ChannelStats> {
    let ch_count = block.channel_count;
    let spc = block.samples_per_channel;
    if ch_count == 0 || spc == 0 {
        return Vec::new();
    }

    (0..ch_count)
        .map(|ch| {
            let mut sum_sq: f64 = 0.0;
            let mut min_val = i16::MAX;
            let mut max_val = i16::MIN;

            for s in 0..spc {
                let idx = s * ch_count + ch;
                let v = *block.data.get(idx).unwrap_or(&0);
                sum_sq += (v as f64) * (v as f64);
                min_val = min_val.min(v);
                max_val = max_val.max(v);
            }

            ChannelStats {
                rms: (sum_sq / spc as f64).sqrt(),
                peak_to_peak: max_val.saturating_sub(min_val),
                min: min_val,
                max: max_val,
            }
        })
        .collect()
}
