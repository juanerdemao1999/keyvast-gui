//! Pre-computed display ring buffer (SpikeGLX WrapBuffer pattern).
//!
//! Inspired by SpikeGLX's `WrapBuffer` / `MGraphX` architecture:
//! - Fixed-rate decimation at **ingestion time** (not render time)
//! - Ring buffer holds the last N seconds of display-ready samples
//! - Render reads linearly from the ring — O(output_points) with no
//!   block history iteration, no binary search, no timestamp comparison
//!
//! ## Layout
//! One `VecDeque<f32>` per channel, all of the same length.
//! Index `i` corresponds to abs_sample_index `t0 + i * dwnsp`.
//!
//! ## Decimation
//! `RING_DWNSP` input samples → 1 ring sample (last-sample mode, fast).
//! At 30 kHz with RING_DWNSP=4: 7,500 ring samples/sec.
//! Ring capacity for 22s: 165,000 entries × 4 bytes × 16 ch = ~10 MB.

use kv_types::SampleBlock;

/// Fixed decimation factor: 1 ring sample per RING_DWNSP input samples.
/// Value 4 gives 0.13 ms resolution at 30 kHz — sufficient for all
/// time windows ≥ 500 ms (where render stride ≥ 8 input samples).
pub const RING_DWNSP: usize = 4;

/// Ring stores this many seconds of pre-decimated data.
/// 120s (2 min) allows paused browsing of a large history window.
/// Memory: 120 * 30000/4 = 900,000 entries × 4 bytes × 16 ch ≈ 57 MB.
const RING_SECS: f64 = 120.0;

/// Pre-allocated display ring buffer.
///
/// Maintained by `app.rs::ingest_block()`.
/// Consumed by `waveform.rs::draw_waveform_area()`.
pub struct DisplayRing {
    /// Per-channel circular buffer of normalized f32 y-values.
    /// All channels always have the same length (`self.len`).
    y: Vec<VecDeque32>,
    /// Absolute sample index of ring slot 0 (front of the deque).
    t0: u64,
    /// Current number of entries in the ring (≤ capacity).
    pub len: usize,
    /// Maximum entries (auto-computed from sample_rate + RING_SECS).
    pub capacity: usize,
    /// Decimation factor (input samples per ring sample).
    pub dwnsp: usize,
    /// Sample rate of the source data (Hz).
    pub sample_rate: f64,
    /// Number of channels stored.
    pub channel_count: usize,
    /// Next ring-aligned absolute index expected from the input stream.
    next_expected: u64,
    /// Whether the ring has been initialized (received at least one block).
    pub ready: bool,
    /// Peak-preserving mode. When set, each ring sample stores the most
    /// extreme (largest |value|) input sample within its `dwnsp` window, and
    /// `collect_channel_minmax` is used at render time. This keeps narrow
    /// spikes visible in the high-pass / AP band, where plain last-sample
    /// decimation would drop them (#4b).
    pub peak_hold: bool,
}

// VecDeque<f32> alias for clarity
type VecDeque32 = std::collections::VecDeque<f32>;

impl DisplayRing {
    /// Create an empty ring for `channel_count` channels at `sample_rate`.
    pub fn new(channel_count: usize, sample_rate: f64) -> Self {
        let dwnsp = RING_DWNSP;
        let capacity = ((RING_SECS * sample_rate) as usize / dwnsp).max(1024);
        let y = (0..channel_count)
            .map(|_| VecDeque32::with_capacity(capacity + 4))
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
            peak_hold: false,
        }
    }

    /// Enable peak-preserving decimation (see [`DisplayRing::peak_hold`]).
    pub fn with_peak_hold(mut self) -> Self {
        self.peak_hold = true;
        self
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
            .map(|_| VecDeque32::with_capacity(new_cap + 4))
            .collect();
        self.t0 = 0;
        self.len = 0;
        self.next_expected = 0;
        self.ready = false;
    }

    /// Feed a (possibly filtered) block into the ring.
    ///
    /// Only samples at ring-aligned positions (every `dwnsp`-th sample
    /// in the absolute sample stream) are stored.
    pub fn push_block(&mut self, block: &SampleBlock) {
        if block.channel_count != self.channel_count || block.samples_per_channel == 0 {
            return;
        }

        let ch_count = self.channel_count;
        let spc = block.samples_per_channel;
        let block_start = block.timestamp_start;
        let norm = 1.0_f32 / i16::MAX as f32;
        let dwnsp = self.dwnsp as u64;

        // Initialize the expected pointer on first block
        if !self.ready {
            // Align next_expected to the ring stride boundary at or after block_start
            self.next_expected = if block_start.is_multiple_of(dwnsp) {
                block_start
            } else {
                block_start + dwnsp - (block_start % dwnsp)
            };
            self.t0 = self.next_expected;
            self.ready = true;
        }

        // Walk through ring-aligned positions within this block
        let dwnsp_us = self.dwnsp;
        let mut abs = self.next_expected;
        while abs < block_start + spc as u64 {
            let s = (abs - block_start) as usize;
            if s >= spc {
                break;
            }

            // Push one sample per channel. In peak-hold mode the stored value is
            // the most extreme sample in the [s, s+dwnsp) window so narrow spikes
            // survive ingestion-time decimation.
            let w_end = if self.peak_hold {
                (s + dwnsp_us).min(spc)
            } else {
                s + 1
            };
            for ch in 0..ch_count {
                let v = if self.peak_hold {
                    let mut best = 0.0_f32;
                    let mut best_abs = -1.0_f32;
                    for w in s..w_end {
                        let idx = w * ch_count + ch;
                        if idx < block.data.len() {
                            let val = block.data[idx] as f32 * norm;
                            if val.abs() > best_abs {
                                best_abs = val.abs();
                                best = val;
                            }
                        }
                    }
                    best
                } else {
                    let idx = s * ch_count + ch;
                    if idx < block.data.len() {
                        block.data[idx] as f32 * norm
                    } else {
                        0.0
                    }
                };
                self.y[ch].push_back(v);
            }
            self.len += 1;

            // Evict oldest if over capacity
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

    /// Extract the last `n` ring samples for `ch` as i16 values (de-normalized).
    /// Used by FFT panel for spectrum computation.
    pub fn last_n_samples(&self, ch: usize, n: usize) -> Vec<i16> {
        if ch >= self.channel_count || self.len == 0 || !self.ready {
            return Vec::new();
        }
        let ring = &self.y[ch];
        let avail = ring.len().min(n);
        let start = ring.len() - avail;
        // Ring stores normalized f32 in [-1, 1]. Convert back to i16.
        ring.iter()
            .skip(start)
            .map(|&v| (v as f64 * 32767.0).round() as i16)
            .collect()
    }

    /// Collect display points for `ch` in the time window [t_left_ms, t_right_ms].
    ///
    /// `window_ring_entries` is the **full** window size expressed in ring slots:
    ///   `(t_right_ms - t_left_ms) / ms_per_ring`
    ///
    /// This value drives the secondary stride, which must be computed over the
    /// *intended* window, NOT the currently-filled portion.  Using the filled
    /// portion would cause stride2 to grow during a sweep (early data appears
    /// coarse while new data is fine, creating a "stretch/zoom" visual artifact).
    ///
    /// Returns a `Vec<[f64; 2]>` of (time_ms, normalized_y) pairs,
    /// decimated to at most `max_points` entries using a secondary stride.
    /// The y values are **un-offset, un-gained** normalized amplitudes in [-1, 1].
    pub fn collect_channel(
        &self,
        ch: usize,
        t_left_ms: f64,
        t_right_ms: f64,
        max_points: usize,
        window_ring_entries: usize,
    ) -> Vec<[f64; 2]> {
        if ch >= self.channel_count || self.len == 0 || !self.ready {
            return Vec::new();
        }

        let ms_per_ring = self.dwnsp as f64 * 1000.0 / self.sample_rate;
        let t0_ms = self.t0 as f64 * 1000.0 / self.sample_rate;

        // Map time bounds to ring indices (clamp to [0, len))
        let f_start = ((t_left_ms - t0_ms) / ms_per_ring).floor() as i64;
        let f_end = ((t_right_ms - t0_ms) / ms_per_ring).ceil() as i64 + 1;
        let ri_start = f_start.clamp(0, self.len as i64) as usize;
        let ri_end = f_end.clamp(0, self.len as i64) as usize;

        if ri_end <= ri_start {
            return Vec::new();
        }

        // Secondary stride based on the FULL window, not just current fill level.
        // This keeps stride2 constant for the entire sweep duration.
        let stride_denom = window_ring_entries.max(ri_end - ri_start);
        let stride2 = (stride_denom / max_points).max(1);
        let visible = ri_end - ri_start;
        let pts_cap = visible.div_ceil(stride2);
        let mut pts = Vec::with_capacity(pts_cap);

        let deque = &self.y[ch];

        // Align the first sample to the global absolute-sample grid.
        // Without alignment, ri_start shifts slightly each frame (as t0 advances
        // and x_left advances by slightly different amounts due to floating-point
        // and integer-block rounding), causing the stride2 phase to drift and
        // making the rendered trace appear to "jitter" horizontally.
        //
        // Fix: snap ri_start to the nearest index where
        //   (t0 + ri * dwnsp) % (stride2 * dwnsp) == 0
        // i.e. the absolute sample index is a multiple of stride2 * dwnsp.
        let stride_abs = stride2 as u64 * self.dwnsp as u64;
        let abs_start = self.t0 + ri_start as u64 * self.dwnsp as u64;
        let phase = abs_start % stride_abs;
        let aligned_ri_start = if phase == 0 {
            ri_start
        } else {
            let skip_abs = stride_abs - phase;
            let skip_ring = (skip_abs / self.dwnsp as u64) as usize;
            ri_start + skip_ring
        };

        let mut i = aligned_ri_start;
        while i < ri_end {
            let t_ms = t0_ms + i as f64 * ms_per_ring;
            // Safety: i < self.len ≤ deque.len()
            let y = deque[i];
            pts.push([t_ms, y as f64]);
            i += stride2;
        }

        pts
    }

    /// Peak-preserving variant of [`DisplayRing::collect_channel`].
    ///
    /// Each render bucket of `stride2` ring samples emits both its minimum and
    /// maximum sample (in time order), so the envelope of narrow features —
    /// spikes in the AP band — is preserved instead of being skipped over by
    /// last-sample decimation. Falls back to a single point per bucket when the
    /// bucket holds only one sample.
    pub fn collect_channel_minmax(
        &self,
        ch: usize,
        t_left_ms: f64,
        t_right_ms: f64,
        max_points: usize,
        window_ring_entries: usize,
    ) -> Vec<[f64; 2]> {
        if ch >= self.channel_count || self.len == 0 || !self.ready {
            return Vec::new();
        }

        let ms_per_ring = self.dwnsp as f64 * 1000.0 / self.sample_rate;
        let t0_ms = self.t0 as f64 * 1000.0 / self.sample_rate;

        let f_start = ((t_left_ms - t0_ms) / ms_per_ring).floor() as i64;
        let f_end = ((t_right_ms - t0_ms) / ms_per_ring).ceil() as i64 + 1;
        let ri_start = f_start.clamp(0, self.len as i64) as usize;
        let ri_end = f_end.clamp(0, self.len as i64) as usize;

        if ri_end <= ri_start {
            return Vec::new();
        }

        // Two points per bucket, so target half as many buckets as the point
        // budget to keep the total comparable to `collect_channel`.
        let bucket_budget = (max_points / 2).max(1);
        let stride_denom = window_ring_entries.max(ri_end - ri_start);
        let stride2 = (stride_denom / bucket_budget).max(1);

        let deque = &self.y[ch];
        let mut pts = Vec::with_capacity(ri_end.saturating_sub(ri_start).div_ceil(stride2) * 2);

        let mut i = ri_start;
        while i < ri_end {
            let end = (i + stride2).min(ri_end);
            let mut min_v = f32::INFINITY;
            let mut max_v = f32::NEG_INFINITY;
            let mut min_i = i;
            let mut max_i = i;
            let mut j = i;
            while j < end {
                let y = deque[j];
                if y < min_v {
                    min_v = y;
                    min_i = j;
                }
                if y > max_v {
                    max_v = y;
                    max_i = j;
                }
                j += 1;
            }
            let t_min = t0_ms + min_i as f64 * ms_per_ring;
            let t_max = t0_ms + max_i as f64 * ms_per_ring;
            // Emit in time order so the rendered line never steps backwards.
            if min_i <= max_i {
                pts.push([t_min, min_v as f64]);
                if max_i != min_i {
                    pts.push([t_max, max_v as f64]);
                }
            } else {
                pts.push([t_max, max_v as f64]);
                pts.push([t_min, min_v as f64]);
            }
            i = end;
        }

        pts
    }
}
