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
        let mut abs = self.next_expected;
        while abs < block_start + spc as u64 {
            let s = (abs - block_start) as usize;
            if s >= spc {
                break;
            }

            // Push one sample per channel
            for ch in 0..ch_count {
                let idx = s * ch_count + ch;
                let v = if idx < block.data.len() {
                    block.data[idx] as f32 * norm
                } else {
                    0.0
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

    /// Extract the last `n` ring samples for `ch` as de-normalized ADC counts
    /// in `f64`. Used by the FFT panel, whose math is `f64` end-to-end, so we
    /// avoid a lossy round-trip through `i16` that would only add quantization
    /// error to the spectrum.
    pub fn last_n_samples_f64(&self, ch: usize, n: usize) -> Vec<f64> {
        if ch >= self.channel_count || self.len == 0 || !self.ready {
            return Vec::new();
        }
        let ring = &self.y[ch];
        let avail = ring.len().min(n);
        let start = ring.len() - avail;
        // Ring stores normalized f32 in [-1, 1]; scale back to ADC counts.
        ring.iter()
            .skip(start)
            .map(|&v| v as f64 * 32767.0)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert two sample slices match within the f32 round-trip tolerance.
    /// Values are stored as normalized f32 then scaled back, so small
    /// quantization error is expected.
    fn assert_samples_eq(actual: &[f64], expected: &[f64]) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 0.01, "got {a}, expected {e}");
        }
    }

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
    fn push_block_decimates_by_ring_dwnsp() {
        let mut ring = DisplayRing::new(2, 30_000.0);
        // 16 samples at stride 4 -> ring slots for absolute samples 0,4,8,12.
        ring.push_block(&block(0, 2, 16));
        assert!(ring.ready);
        assert_eq!(ring.len, 4);
        // Channel 0 stores raw value (s * 2 + 0) for s in {0,4,8,12}.
        assert_samples_eq(&ring.last_n_samples_f64(0, 4), &[0.0, 8.0, 16.0, 24.0]);
        // Channel 1 stores (s * 2 + 1).
        assert_samples_eq(&ring.last_n_samples_f64(1, 4), &[1.0, 9.0, 17.0, 25.0]);
    }

    #[test]
    fn push_block_aligns_first_sample_to_stride_boundary() {
        let mut ring = DisplayRing::new(1, 30_000.0);
        // Start at 6: first ring-aligned absolute sample is 8.
        ring.push_block(&block(6, 1, 10)); // covers abs 6..16, aligned: 8, 12
        assert_eq!(ring.len, 2);
        // value at abs 8 -> local sample 2 -> (2*1+0) = 2; abs 12 -> local 6 -> 6.
        assert_samples_eq(&ring.last_n_samples_f64(0, 2), &[2.0, 6.0]);
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
        // 5000 input samples -> 1250 ring-aligned slots (0,4,..,4996).
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
    fn collect_channel_returns_points_within_window() {
        let mut ring = DisplayRing::new(1, 30_000.0);
        ring.push_block(&block(0, 1, 30_000)); // 1 s of data
        let ms_per_ring = RING_DWNSP as f64 * 1000.0 / 30_000.0;
        let window_entries = (500.0 / ms_per_ring) as usize;
        let pts = ring.collect_channel(0, 0.0, 500.0, 256, window_entries);
        assert!(!pts.is_empty());
        // Decimated well below the ~3750 ring slots spanning the window.
        assert!(pts.len() < 512);
        // Times stay inside the requested window (allow one ring slot of slack).
        assert!(pts.iter().all(|p| p[0] >= 0.0 && p[0] <= 520.0));
    }
}
