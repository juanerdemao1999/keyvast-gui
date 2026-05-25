//! Real-time digital signal processing for waveform display.
//!
//! Implements 2nd-order biquad IIR filters using the RBJ Audio EQ
//! Cookbook designs and Direct Form II Transposed structure for
//! numerical stability.  These filters are applied only to displayed
//! data; recorded data is always raw, matching standard practice in
//! Open Ephys, Intan RHX, and similar acquisition packages.
//!
//! Each filter is sample-by-sample with persistent state so it must
//! not be reset between frames during steady-state operation.

use std::f64::consts::PI;

/// Default Q for Butterworth-flat 2nd-order HP/LP response.
pub const Q_BUTTERWORTH: f64 = std::f64::consts::FRAC_1_SQRT_2; // 1/sqrt(2)

/// Default Q for a narrow notch (50/60 Hz line removal).
pub const Q_NOTCH: f64 = 30.0;

/// 2nd-order biquad filter (Direct Form II Transposed).
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
    s1: f64,
    s2: f64,
}

impl Biquad {
    /// Identity filter — passes input through unchanged.
    pub fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// Process a single input sample, updating internal state.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.s1;
        self.s1 = self.b1 * x - self.a1 * y + self.s2;
        self.s2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Clear filter state (use when changing parameters).
    #[allow(dead_code)] // public API, used in tests + future runtime resets
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// 2nd-order Butterworth-style high-pass filter (RBJ cookbook).
    pub fn highpass(cutoff_hz: f64, sample_rate_hz: f64, q: f64) -> Self {
        let w0 = 2.0 * PI * cutoff_hz / sample_rate_hz;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos_w0) / 2.0 / a0,
            b1: -(1.0 + cos_w0) / a0,
            b2: (1.0 + cos_w0) / 2.0 / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// 2nd-order Butterworth-style low-pass filter (RBJ cookbook).
    pub fn lowpass(cutoff_hz: f64, sample_rate_hz: f64, q: f64) -> Self {
        let w0 = 2.0 * PI * cutoff_hz / sample_rate_hz;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos_w0) / 2.0 / a0,
            b1: (1.0 - cos_w0) / a0,
            b2: (1.0 - cos_w0) / 2.0 / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// Notch (band-reject) filter centered on `freq_hz`.
    pub fn notch(freq_hz: f64, sample_rate_hz: f64, q: f64) -> Self {
        let w0 = 2.0 * PI * freq_hz / sample_rate_hz;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: 1.0 / a0,
            b1: -2.0 * cos_w0 / a0,
            b2: 1.0 / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }
}

/// Per-channel filter chain (HP → LP → Notch).  Stages can be disabled
/// individually by leaving them at `Biquad::identity()`.
#[derive(Clone, Copy, Debug)]
pub struct FilterChain {
    pub hp: Biquad,
    pub lp: Biquad,
    pub notch: Biquad,
    pub hp_enabled: bool,
    pub lp_enabled: bool,
    pub notch_enabled: bool,
}

impl FilterChain {
    /// Pass-through chain (all stages disabled).
    pub fn passthrough() -> Self {
        Self {
            hp: Biquad::identity(),
            lp: Biquad::identity(),
            notch: Biquad::identity(),
            hp_enabled: false,
            lp_enabled: false,
            notch_enabled: false,
        }
    }

    /// Whether any stage is enabled (callers can use this to bypass the
    /// filter pipeline entirely for performance).
    #[allow(dead_code)] // public API helper, mirrored by `FilterSettings::any_filter_enabled`
    pub fn any_enabled(&self) -> bool {
        self.hp_enabled || self.lp_enabled || self.notch_enabled
    }

    /// Process a single sample through every enabled stage in order.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let mut y = x;
        if self.hp_enabled {
            y = self.hp.process(y);
        }
        if self.lp_enabled {
            y = self.lp.process(y);
        }
        if self.notch_enabled {
            y = self.notch.process(y);
        }
        y
    }

    /// Clear state on every stage.  Use when filter parameters change.
    #[allow(dead_code)] // public API, used when settings change at runtime
    pub fn reset(&mut self) {
        self.hp.reset();
        self.lp.reset();
        self.notch.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Estimate steady-state amplitude response of a filter at `freq_hz`
    /// by running a sine-wave for enough cycles to settle and measuring
    /// the peak of the last cycle.
    fn measure_gain(b: &mut Biquad, freq_hz: f64, sample_rate: f64) -> f64 {
        b.reset();
        let cycles = 200;
        let samples_per_cycle = (sample_rate / freq_hz) as usize;
        let total = cycles * samples_per_cycle;
        let mut max_y: f64 = 0.0;
        for n in 0..total {
            let t = n as f64 / sample_rate;
            let x = (2.0 * PI * freq_hz * t).sin();
            let y = b.process(x);
            // Only measure the last full cycle for peak amplitude
            if n >= total - samples_per_cycle {
                max_y = max_y.max(y.abs());
            }
        }
        max_y
    }

    #[test]
    fn identity_passes_through() {
        let mut b = Biquad::identity();
        for i in 0..10 {
            let x = i as f64 * 0.1;
            assert!((b.process(x) - x).abs() < 1e-12);
        }
    }

    #[test]
    fn highpass_blocks_dc() {
        let mut b = Biquad::highpass(100.0, 30_000.0, Q_BUTTERWORTH);
        // Feed DC; output should decay to ~0
        let mut last = 0.0;
        for _ in 0..2000 {
            last = b.process(1.0);
        }
        assert!(last.abs() < 1e-3, "highpass DC residual = {}", last);
    }

    #[test]
    fn highpass_passes_high_frequency() {
        let fs = 30_000.0;
        let mut b = Biquad::highpass(100.0, fs, Q_BUTTERWORTH);
        let gain = measure_gain(&mut b, 1000.0, fs);
        assert!(gain > 0.95, "expected ~unity gain at 1 kHz, got {}", gain);
    }

    #[test]
    fn lowpass_passes_dc() {
        let mut b = Biquad::lowpass(1000.0, 30_000.0, Q_BUTTERWORTH);
        let mut last = 0.0;
        for _ in 0..2000 {
            last = b.process(1.0);
        }
        assert!(
            (last - 1.0).abs() < 1e-3,
            "lowpass DC steady-state should be ~1, got {}",
            last
        );
    }

    #[test]
    fn lowpass_blocks_high_frequency() {
        let fs = 30_000.0;
        let mut b = Biquad::lowpass(100.0, fs, Q_BUTTERWORTH);
        let gain = measure_gain(&mut b, 5000.0, fs);
        assert!(
            gain < 0.05,
            "expected strong attenuation at 5 kHz with 100 Hz LP, got {}",
            gain
        );
    }

    #[test]
    fn notch_rejects_center_frequency() {
        let fs = 30_000.0;
        let mut b = Biquad::notch(60.0, fs, Q_NOTCH);
        let gain = measure_gain(&mut b, 60.0, fs);
        assert!(
            gain < 0.1,
            "expected strong rejection at 60 Hz, got {}",
            gain
        );
    }

    #[test]
    fn notch_passes_far_frequency() {
        let fs = 30_000.0;
        let mut b = Biquad::notch(60.0, fs, Q_NOTCH);
        // 1 kHz should pass with near-unity gain
        let gain = measure_gain(&mut b, 1000.0, fs);
        assert!(gain > 0.95, "expected ~unity gain at 1 kHz, got {}", gain);
    }

    #[test]
    fn chain_passthrough_returns_input() {
        let mut chain = FilterChain::passthrough();
        for i in 0..16 {
            let x = i as f64 * 0.05;
            assert_eq!(chain.process(x), x);
        }
    }

    #[test]
    fn chain_with_only_hp_blocks_dc() {
        let mut chain = FilterChain::passthrough();
        chain.hp = Biquad::highpass(100.0, 30_000.0, Q_BUTTERWORTH);
        chain.hp_enabled = true;
        let mut last = 0.0;
        for _ in 0..2000 {
            last = chain.process(1.0);
        }
        assert!(last.abs() < 1e-3);
    }
}
