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
/// 22s exceeds the maximum display window (20s) with a small margin.
const RING_SECS: f64 = 22.0;

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
            self.next_expected = if block_start % dwnsp == 0 {
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

    /// Collect display points for `ch` in the time window [t_left_ms, t_right_ms].
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

        let visible = ri_end - ri_start;
        // Secondary stride (applied on top of RING_DWNSP)
        let stride2 = (visible / max_points).max(1);
        let pts_cap = (visible + stride2 - 1) / stride2;
        let mut pts = Vec::with_capacity(pts_cap);

        let deque = &self.y[ch];
        let mut i = ri_start;
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
