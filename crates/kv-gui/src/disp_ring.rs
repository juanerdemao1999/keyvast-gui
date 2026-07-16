//! Pre-computed display ring buffer (SpikeGLX WrapBuffer pattern).
//!
//! Inspired by SpikeGLX's `WrapBuffer` / `MGraphX` architecture:
//! - Fixed-rate decimation at **ingestion time** (not render time)
//! - Ring buffer holds the last N seconds of display-ready samples
//! - Render reads linearly from the ring — O(output_points) with no
//!   block history iteration, no binary search, no timestamp comparison
//!
//! ## Layout
//! One `VecDeque<(i16, i16)>` per channel, all of the same length. Each slot
//! holds the **(min, max)** of the `dwnsp` input samples it decimates, so the
//! true signal envelope is preserved rather than a single magnitude-picked
//! point. Index `i` corresponds to abs_sample_index `t0 + i * dwnsp`.
//!
//! ## Why min/max (Open Ephys / Intan RHX style)
//! When there are many more samples than horizontal pixels, professional
//! electrophysiology scopes (Open Ephys LFP Viewer, Intan RHX, SpikeGLX BinMax)
//! draw a per-column **min→max vertical envelope**, NOT a single decimated or
//! `argmax|value|` point. Keeping only one extreme sample per bucket draws a
//! jagged line that swings between +peak and −peak and reads like noise. Storing
//! (min, max) lets [`DisplayRing::collect_channel_band`] emit a filled envelope
//! that matches those tools, and keeps narrow spikes (captured by max/min).
//!
//! ## Decimation
//! `RING_DWNSP` input samples → 1 ring slot. At 30 kHz with RING_DWNSP=4:
//! 7,500 slots/sec. Storing two i16 per slot keeps memory identical to the old
//! single-f32 layout: 120 s × 7500 × 4 bytes × 16 ch ≈ 57 MB.

use kv_types::SampleBlock;

/// Fixed decimation factor: 1 ring slot per RING_DWNSP input samples.
/// Value 4 gives 0.13 ms resolution at 30 kHz — sufficient for all
/// time windows ≥ 500 ms (where render stride ≥ 8 input samples).
pub const RING_DWNSP: usize = 4;

/// Ring stores this many seconds of pre-decimated data.
/// 120s (2 min) allows paused browsing of a large history window.
const RING_SECS: f64 = 120.0;

/// Normalization: i16 count → [-1, 1].
const NORM: f64 = 1.0 / i16::MAX as f64;

/// One decimated slot: the (min, max, mean) of its `dwnsp` input samples, in raw
/// i16 counts. The mean marks where the signal actually dwells, which drives the
/// density-graded fill (bright core at the mean, faint at the rare extremes) so
/// a wide time window shows structure instead of a solid block.
type MinMax = (i16, i16, i16);

/// Per-channel display envelope for one time window: aligned column centre
/// times plus per-column min, max, and mean (normalized to [-1, 1]).
pub struct ChannelBand {
    /// Column time (ms), ascending. Same length as `min` / `max` / `mean`.
    pub t: Vec<f64>,
    /// Per-column minimum (normalized).
    pub min: Vec<f64>,
    /// Per-column maximum (normalized).
    pub max: Vec<f64>,
    /// Per-column mean (normalized) — the density-fill bright core.
    pub mean: Vec<f64>,
}

impl ChannelBand {
    fn empty() -> Self {
        Self {
            t: Vec::new(),
            min: Vec::new(),
            max: Vec::new(),
            mean: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }
}

/// Pre-allocated display ring buffer.
///
/// Maintained by `app.rs::ingest_block()`.
/// Consumed by `waveform.rs::draw_waveform_area()`.
pub struct DisplayRing {
    /// Per-channel circular buffer of (min, max) decimated slots.
    /// All channels always have the same length (`self.len`).
    y: Vec<VecDequeMinMax>,
    /// Absolute sample index of ring slot 0 (front of the deque).
    t0: u64,
    /// Current number of entries in the ring (≤ capacity).
    pub len: usize,
    /// Maximum entries (auto-computed from sample_rate + RING_SECS).
    pub capacity: usize,
    /// Decimation factor (input samples per ring slot).
    pub dwnsp: usize,
    /// Sample rate of the source data (Hz).
    pub sample_rate: f64,
    /// Number of channels stored.
    pub channel_count: usize,
    /// Next ring-aligned absolute index expected from the input stream.
    next_expected: u64,
    /// Whether the ring has been initialized (received at least one block).
    pub ready: bool,
}

type VecDequeMinMax = std::collections::VecDeque<MinMax>;

impl DisplayRing {
    /// Create an empty ring for `channel_count` channels at `sample_rate`.
    pub fn new(channel_count: usize, sample_rate: f64) -> Self {
        let dwnsp = RING_DWNSP;
        let capacity = ((RING_SECS * sample_rate) as usize / dwnsp).max(1024);
        let y = (0..channel_count)
            .map(|_| VecDequeMinMax::with_capacity(capacity + 4))
            .collect();
        Self {
            y,
            t0: 0,
            len: 0,
            capacity,
            dwnsp,
            sample_rate,
            channel_count,
            next_expected: 0,
            ready: false,
        }
    }

    /// Reset the ring (called on mode switch or channel-count change).
    pub fn reset(&mut self) {
        for ch in &mut self.y {
            ch.clear();
        }
        self.t0 = 0;
        self.len = 0;
        self.next_expected = 0;
        self.ready = false;
    }

    /// Rebuild channel buffers for a new channel count / sample rate.
    pub fn reconfigure(&mut self, channel_count: usize, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.channel_count = channel_count;
        let new_cap = ((RING_SECS * sample_rate) as usize / self.dwnsp).max(1024);
        self.capacity = new_cap;
        self.y = (0..channel_count)
            .map(|_| VecDequeMinMax::with_capacity(new_cap + 4))
            .collect();
        self.t0 = 0;
        self.len = 0;
        self.next_expected = 0;
        self.ready = false;
    }

    /// Feed a (possibly filtered) block into the ring.
    ///
    /// Each ring-aligned slot stores the (min, max) of the `dwnsp` input samples
    /// starting at that position, so the display envelope is preserved.
    pub fn push_block(&mut self, block: &SampleBlock) {
        if block.channel_count != self.channel_count || block.samples_per_channel == 0 {
            return;
        }

        let ch_count = self.channel_count;
        let spc = block.samples_per_channel;
        let block_start = block.timestamp_start;
        let block_end = block_start + spc as u64;
        let dwnsp = self.dwnsp as u64;

        // A block is only continuable if `next_expected` lands inside
        // [block_start, block_end]. A forward gap (next_expected < block_start)
        // or a backward / far-forward jump (next_expected > block_end) — e.g. a
        // playback seek, or under-sized non-contiguous playback blocks — is a
        // discontinuity. Restart the ring at this block instead of letting the
        // index arithmetic below underflow (which panicked in debug and silently
        // froze the ring in release).
        if self.ready && (self.next_expected < block_start || self.next_expected > block_end) {
            self.reset();
        }

        // Initialize the expected pointer on first block.
        if !self.ready {
            self.next_expected = if block_start.is_multiple_of(dwnsp) {
                block_start
            } else {
                block_start + dwnsp - (block_start % dwnsp)
            };
            self.t0 = self.next_expected;
            self.ready = true;
        }

        let dwnsp_us = self.dwnsp;
        let mut abs = self.next_expected;
        while abs < block_end {
            let s = (abs - block_start) as usize;
            if s >= spc {
                break;
            }
            // (min, max, mean) over the [s, s+dwnsp) window for each channel.
            let w_end = (s + dwnsp_us).min(spc);
            for ch in 0..ch_count {
                let mut mn = i16::MAX;
                let mut mx = i16::MIN;
                let mut sum = 0_i32;
                let mut cnt = 0_i32;
                for w in s..w_end {
                    let idx = w * ch_count + ch;
                    if idx < block.data.len() {
                        let v = block.data[idx];
                        if v < mn {
                            mn = v;
                        }
                        if v > mx {
                            mx = v;
                        }
                        sum += v as i32;
                        cnt += 1;
                    }
                }
                if mn > mx {
                    // Empty window (shouldn't happen) → flat zero.
                    mn = 0;
                    mx = 0;
                }
                let mean = if cnt > 0 { (sum / cnt) as i16 } else { 0 };
                self.y[ch].push_back((mn, mx, mean));
            }
            self.len += 1;

            // Evict oldest if over capacity.
            if self.len > self.capacity {
                for ch in 0..ch_count {
                    self.y[ch].pop_front();
                }
                self.t0 += dwnsp;
                self.len -= 1;
            }

            abs += dwnsp;
        }
        self.next_expected = abs;
    }

    /// Absolute time (ms) of the most recent entry in the ring.
    /// Returns 0.0 if the ring is empty.
    pub fn latest_time_ms(&self) -> f64 {
        if self.len == 0 || !self.ready {
            return 0.0;
        }
        (self.t0 + (self.len as u64 - 1) * self.dwnsp as u64) as f64 * 1000.0 / self.sample_rate
    }

    /// Collect a per-column min/max **envelope** for `ch` over the time window
    /// [t_left_ms, t_right_ms], decimated to at most `max_cols` columns.
    ///
    /// This is the Open-Ephys / Intan-RHX style rasterization: each output
    /// column holds the min and max of the ring slots that fall in it (each slot
    /// already carrying the min/max of `dwnsp` input samples), and adjacent
    /// columns are **bridged** — stretched to overlap vertically — so the filled
    /// envelope is continuous instead of a jagged line of disconnected extremes.
    ///
    /// `window_ring_entries` is the **full** intended window in ring slots; the
    /// column stride is based on it (not the currently-filled portion) so the
    /// decimation level stays constant as a sweep fills in.
    pub fn collect_channel_band(
        &self,
        ch: usize,
        t_left_ms: f64,
        t_right_ms: f64,
        max_cols: usize,
        window_ring_entries: usize,
    ) -> ChannelBand {
        if ch >= self.channel_count || self.len == 0 || !self.ready {
            return ChannelBand::empty();
        }

        let ms_per_ring = self.dwnsp as f64 * 1000.0 / self.sample_rate;
        let t0_ms = self.t0 as f64 * 1000.0 / self.sample_rate;

        let f_start = ((t_left_ms - t0_ms) / ms_per_ring).floor() as i64;
        let f_end = ((t_right_ms - t0_ms) / ms_per_ring).ceil() as i64 + 1;
        let ri_start = f_start.clamp(0, self.len as i64) as usize;
        let ri_end = f_end.clamp(0, self.len as i64) as usize;
        if ri_end <= ri_start {
            return ChannelBand::empty();
        }

        let stride_denom = window_ring_entries.max(ri_end - ri_start);
        let stride2 = (stride_denom / max_cols.max(1)).max(1);

        // Align the first column to the global absolute-sample grid so the
        // column phase does not drift frame-to-frame (horizontal jitter).
        let stride_abs = stride2 as u64 * self.dwnsp as u64;
        let abs_start = self.t0 + ri_start as u64 * self.dwnsp as u64;
        let phase = abs_start % stride_abs;
        let aligned_ri_start = if phase == 0 {
            ri_start
        } else {
            ri_start + ((stride_abs - phase) / self.dwnsp as u64) as usize
        };

        let deque = &self.y[ch];
        let cap = ri_end.saturating_sub(aligned_ri_start).div_ceil(stride2);
        let mut t = Vec::with_capacity(cap);
        let mut mins = Vec::with_capacity(cap);
        let mut maxs = Vec::with_capacity(cap);
        let mut means = Vec::with_capacity(cap);

        let mut i = aligned_ri_start;
        while i < ri_end {
            let end = (i + stride2).min(ri_end);
            let mut mn = i16::MAX;
            let mut mx = i16::MIN;
            let mut mean_sum = 0.0_f64;
            let mut mean_cnt = 0.0_f64;
            // Direct O(1) VecDeque indexing (NOT iter().skip(), which is O(i) per
            // column and would be O(len²) over the window).
            let mut j = i;
            while j < end {
                let (lo, hi, avg) = deque[j];
                if lo < mn {
                    mn = lo;
                }
                if hi > mx {
                    mx = hi;
                }
                mean_sum += avg as f64;
                mean_cnt += 1.0;
                j += 1;
            }
            // Left-edge time keeps columns monotonic and matches the sweep grid.
            t.push(t0_ms + i as f64 * ms_per_ring);
            mins.push(mn as f64 * NORM);
            maxs.push(mx as f64 * NORM);
            means.push(if mean_cnt > 0.0 {
                mean_sum / mean_cnt * NORM
            } else {
                0.0
            });
            i += stride2;
        }

        // Open-Ephys adjacent-column bridging: stretch each column's [min, max]
        // to overlap the previous column's so the envelope has no vertical gaps
        // (this is the specific step that removes the jagged-extremes look).
        for k in 1..mins.len() {
            if maxs[k] < mins[k - 1] {
                maxs[k] = mins[k - 1];
            }
            if mins[k] > maxs[k - 1] {
                mins[k] = maxs[k - 1];
            }
        }

        ChannelBand {
            t,
            mean: means,
            min: mins,
            max: maxs,
        }
    }

    /// Extract the last `n` ring slots for `ch` as de-normalized ADC counts
    /// (the per-slot midpoint), in `f64`. Diagnostic / test accessor — the FFT
    /// reads full-rate history instead, so this is not on the render path.
    #[cfg(test)]
    pub fn last_n_midpoints(&self, ch: usize, n: usize) -> Vec<f64> {
        if ch >= self.channel_count || self.len == 0 || !self.ready {
            return Vec::new();
        }
        let ring = &self.y[ch];
        let avail = ring.len().min(n);
        let start = ring.len() - avail;
        ring.iter()
            .skip(start)
            .map(|&(lo, hi, _)| (lo as f64 + hi as f64) * 0.5)
            .collect()
    }

    /// Test-only accessor for one slot's (min, max, mean) raw counts.
    #[cfg(test)]
    fn slot(&self, ch: usize, i: usize) -> MinMax {
        self.y[ch][i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a block where channel `ch` at sample `s` holds value
    /// `(s * channel_count + ch)` as i16, so each ring slot is identifiable.
    fn block(timestamp_start: u64, channel_count: usize, spc: usize) -> SampleBlock {
        let mut data = Vec::with_capacity(channel_count * spc);
        for s in 0..spc {
            for ch in 0..channel_count {
                data.push((s * channel_count + ch) as i16);
            }
        }
        SampleBlock {
            device_id: "test".to_string(),
            stream_id: 0,
            packet_id: 0,
            timestamp_start,
            sample_rate: 30_000.0,
            channel_count,
            samples_per_channel: spc,
            ttl_bits: 0,
            data,
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        }
    }

    #[test]
    fn new_ring_is_empty_and_not_ready() {
        let ring = DisplayRing::new(4, 30_000.0);
        assert!(!ring.ready);
        assert_eq!(ring.len, 0);
        assert_eq!(ring.channel_count, 4);
        assert_eq!(ring.dwnsp, RING_DWNSP);
        // 120 s * 30 kHz / 4 = 900_000.
        assert_eq!(ring.capacity, 900_000);
    }

    #[test]
    fn push_block_stores_minmax_per_bucket() {
        let mut ring = DisplayRing::new(2, 30_000.0);
        // 16 samples at stride 4 → ring slots for absolute samples 0,4,8,12,
        // each holding the (min,max) of its 4-sample window.
        ring.push_block(&block(0, 2, 16));
        assert!(ring.ready);
        assert_eq!(ring.len, 4);
        // Channel 0, value(s) = s*2. Bucket [0,4)=0,2,4,6 → (min0,max6,mean3).
        assert_eq!(ring.slot(0, 0), (0, 6, 3));
        assert_eq!(ring.slot(0, 1), (8, 14, 11));
        assert_eq!(ring.slot(0, 2), (16, 22, 19));
        assert_eq!(ring.slot(0, 3), (24, 30, 27));
        // Channel 1, value(s) = s*2+1. Bucket [0,4)=1,3,5,7 → (1,7,4).
        assert_eq!(ring.slot(1, 0), (1, 7, 4));
        // Midpoint accessor returns the slot centres.
        let mids = ring.last_n_midpoints(0, 4);
        assert_eq!(mids, vec![3.0, 11.0, 19.0, 27.0]);
    }

    #[test]
    fn push_block_aligns_first_sample_to_stride_boundary() {
        let mut ring = DisplayRing::new(1, 30_000.0);
        // Start at 6: first ring-aligned absolute sample is 8. Covers abs 6..16.
        ring.push_block(&block(6, 1, 10));
        assert_eq!(ring.len, 2);
        // Slot at abs 8 → local 2..6 → values 2,3,4,5 → (2,5,mean 3).
        assert_eq!(ring.slot(0, 0), (2, 5, 3));
        // Slot at abs 12 → local 6..10 → values 6,7,8,9 → (6,9,mean 7).
        assert_eq!(ring.slot(0, 1), (6, 9, 7));
    }

    #[test]
    fn push_block_rejects_channel_count_mismatch() {
        let mut ring = DisplayRing::new(2, 30_000.0);
        ring.push_block(&block(0, 3, 16));
        assert!(!ring.ready);
        assert_eq!(ring.len, 0);
    }

    #[test]
    fn ring_evicts_oldest_when_over_capacity() {
        // sample_rate 30 forces the minimum capacity floor of 1024.
        let mut ring = DisplayRing::new(1, 30.0);
        assert_eq!(ring.capacity, 1024);
        // 5000 input samples → 1250 ring-aligned slots (0,4,..,4996).
        ring.push_block(&block(0, 1, 5000));
        assert_eq!(ring.len, ring.capacity);
        // 1250 - 1024 = 226 slots evicted, each advancing t0 by dwnsp (4).
        assert_eq!(ring.t0, 226 * 4);
    }

    #[test]
    fn reset_clears_state_but_keeps_capacity() {
        let mut ring = DisplayRing::new(2, 30_000.0);
        ring.push_block(&block(0, 2, 16));
        let cap = ring.capacity;
        ring.reset();
        assert!(!ring.ready);
        assert_eq!(ring.len, 0);
        assert_eq!(ring.capacity, cap);
        assert_eq!(ring.latest_time_ms(), 0.0);
    }

    #[test]
    fn push_block_reseeds_on_forward_gap() {
        // Playback-style forward jump: a non-contiguous block must re-seed the
        // ring (not underflow/panic in debug, not freeze in release).
        let mut ring = DisplayRing::new(1, 30_000.0);
        ring.push_block(&block(0, 1, 256)); // covers abs [0, 256)
        assert!(ring.len > 0);
        ring.push_block(&block(1000, 1, 256)); // block_start 1000 ≫ next_expected
        assert!(ring.ready);
        assert!(
            ring.len > 0,
            "forward-gapped block must re-seed, not be dropped"
        );
        assert_eq!(ring.t0, 1000, "ring re-seeds at the jumped block's start");
    }

    #[test]
    fn push_block_reseeds_on_backward_jump() {
        let mut ring = DisplayRing::new(1, 30_000.0);
        ring.push_block(&block(10_000, 1, 256));
        ring.push_block(&block(0, 1, 256)); // jump backward (e.g. a rewind/seek)
        assert!(ring.ready);
        assert_eq!(ring.t0, 0, "backward jump re-seeds at the earlier block");
    }

    #[test]
    fn push_block_stays_continuous_for_adjacent_blocks() {
        // Contiguous stream (live / gapless playback) must NOT reset.
        let mut ring = DisplayRing::new(1, 30_000.0);
        ring.push_block(&block(0, 1, 256)); // [0, 256)
        let len1 = ring.len;
        ring.push_block(&block(256, 1, 256)); // [256, 512) — adjacent
        assert_eq!(ring.t0, 0, "adjacent block should not reset the ring");
        assert!(ring.len > len1, "adjacent block appends to the ring");
    }

    #[test]
    fn collect_channel_band_returns_columns_within_window() {
        let mut ring = DisplayRing::new(1, 30_000.0);
        ring.push_block(&block(0, 1, 30_000)); // 1 s of data
        let ms_per_ring = RING_DWNSP as f64 * 1000.0 / 30_000.0;
        let window_entries = (500.0 / ms_per_ring) as usize;
        let band = ring.collect_channel_band(0, 0.0, 500.0, 256, window_entries);
        assert!(!band.is_empty());
        assert_eq!(band.t.len(), band.min.len());
        assert_eq!(band.t.len(), band.max.len());
        // Decimated well below the ~3750 ring slots spanning the window.
        assert!(band.t.len() < 512);
        // Times stay inside the requested window (allow one column of slack).
        assert!(band.t.iter().all(|&x| (0.0..=520.0).contains(&x)));
        // max ≥ min for every column.
        assert!(band.min.iter().zip(&band.max).all(|(lo, hi)| hi >= lo));
    }
}
