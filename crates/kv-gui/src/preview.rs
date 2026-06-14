//! Real-time block statistics for the GUI status bar and performance display.
//!
//! `PreviewState`, `start_preview`, and the old mpsc-based preview thread have
//! been replaced by `live_pipeline.rs` (Device mode) and the demo generator
//! (Demo mode).  This file now contains only the shared `BlockStats` type and
//! the `compute_block_stats` helper.

use kv_types::SampleBlock;

// ── Statistics types ─────────────────────────────────────────────────

/// Aggregated statistics for the latest block (shown in the status bar).
#[derive(Debug, Clone)]
pub struct BlockStats {
    pub data_rate_mb_s: f64,
    pub block_rate_hz: f64,
    pub total_blocks: u64,
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
        data_rate_mb_s,
        block_rate_hz,
        total_blocks,
        elapsed_seconds,
        dropped_blocks,
    }
}
