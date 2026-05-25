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

// ── FFT (Cooley-Tukey radix-2 in-place) ─────────────────────────────

/// Minimal complex number type — kept inline so we don't pull in
/// `num-complex` for one use site.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cplx {
    pub re: f64,
    pub im: f64,
}

impl Cplx {
    pub const ZERO: Cplx = Cplx { re: 0.0, im: 0.0 };

    #[inline]
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    /// |x|² (cheaper than `norm` since power spectra use it directly).
    #[inline]
    pub fn norm_sq(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
}

impl std::ops::Add for Cplx {
    type Output = Self;
    #[inline]
    fn add(self, o: Self) -> Self {
        Self {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}

impl std::ops::Sub for Cplx {
    type Output = Self;
    #[inline]
    fn sub(self, o: Self) -> Self {
        Self {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
}

impl std::ops::Mul for Cplx {
    type Output = Self;
    #[inline]
    fn mul(self, o: Self) -> Self {
        Self {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}

impl std::ops::MulAssign for Cplx {
    #[inline]
    fn mul_assign(&mut self, o: Self) {
        let re = self.re * o.re - self.im * o.im;
        let im = self.re * o.im + self.im * o.re;
        self.re = re;
        self.im = im;
    }
}

/// In-place radix-2 FFT.  `x.len()` must be a power of two.
pub fn fft_in_place(x: &mut [Cplx]) {
    let n = x.len();
    assert!(n.is_power_of_two(), "FFT length must be a power of 2");

    // Bit-reversal permutation
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while (j & bit) != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            x.swap(i, j);
        }
    }

    // Butterflies (negative-frequency convention: e^{-j 2π k / N})
    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        let theta = -2.0 * PI / len as f64;
        let wn = Cplx::new(theta.cos(), theta.sin());
        let mut i = 0usize;
        while i < n {
            let mut w = Cplx::new(1.0, 0.0);
            for k in 0..half {
                let t = w * x[i + k + half];
                let u = x[i + k];
                x[i + k] = u + t;
                x[i + k + half] = u - t;
                w *= wn;
            }
            i += len;
        }
        len *= 2;
    }
}

/// Hann window of length `n` (symmetric, suitable for spectrum analysis).
pub fn hann_window(n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![1.0; n];
    }
    let denom = (n - 1) as f64;
    (0..n)
        .map(|i| {
            let x = (2.0 * PI * i as f64) / denom;
            0.5 * (1.0 - x.cos())
        })
        .collect()
}

/// One-sided power spectral density of a real-valued signal.
///
/// Applies a Hann window, runs the FFT, and returns `(freqs_hz, psd_db)`
/// for bins `0..N/2` (DC through Nyquist).  Output is in dB referenced
/// to peak power so traces are easy to read on a -80 to 0 dB scale.
pub fn power_spectrum_db(samples: &[f64], sample_rate: f64) -> (Vec<f64>, Vec<f64>) {
    if samples.is_empty() || sample_rate <= 0.0 {
        return (Vec::new(), Vec::new());
    }
    // Round down to nearest power of two
    let mut n = samples.len().next_power_of_two();
    if n > samples.len() {
        n /= 2;
    }
    if n < 2 {
        return (Vec::new(), Vec::new());
    }
    let win = hann_window(n);

    // DC-remove + window
    let mean = samples[..n].iter().sum::<f64>() / n as f64;
    let mut buf: Vec<Cplx> = (0..n)
        .map(|i| Cplx::new((samples[i] - mean) * win[i], 0.0))
        .collect();
    fft_in_place(&mut buf);

    let half = n / 2;
    let bin_hz = sample_rate / n as f64;
    let mut power: Vec<f64> = (0..half).map(|k| buf[k].norm_sq()).collect();
    // Reference to peak (skip DC bin)
    let peak = power.iter().skip(1).cloned().fold(0.0_f64, f64::max);
    if peak > 0.0 {
        for p in &mut power {
            *p /= peak;
        }
    }
    let freqs: Vec<f64> = (0..half).map(|k| k as f64 * bin_hz).collect();
    let psd_db: Vec<f64> = power
        .iter()
        .map(|&p| 10.0 * (p.max(1e-12)).log10())
        .collect();
    (freqs, psd_db)
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

    // ── FFT tests ───────────────────────────────────────────────

    #[test]
    fn cplx_arithmetic() {
        let a = Cplx::new(1.0, 2.0);
        let b = Cplx::new(3.0, -1.0);
        assert_eq!(a + b, Cplx::new(4.0, 1.0));
        assert_eq!(a - b, Cplx::new(-2.0, 3.0));
        // (1+2i)(3-1i) = 3 - i + 6i - 2i² = 3 + 5i + 2 = 5 + 5i
        assert_eq!(a * b, Cplx::new(5.0, 5.0));
    }

    #[test]
    fn fft_dc_signal() {
        // Pure DC: all energy in bin 0
        let mut x: Vec<Cplx> = (0..16).map(|_| Cplx::new(1.0, 0.0)).collect();
        fft_in_place(&mut x);
        assert!((x[0].re - 16.0).abs() < 1e-9);
        assert!(x[0].im.abs() < 1e-9);
        for bin in x.iter().skip(1) {
            assert!(bin.norm_sq() < 1e-18);
        }
    }

    #[test]
    fn fft_pure_tone_peaks_at_correct_bin() {
        // 8 samples of cos(2π·2·n/8) → bins 2 and 6 (= N-2) carry equal energy
        let n: usize = 8;
        let target_k: usize = 2;
        let mut x: Vec<Cplx> = (0..n)
            .map(|i| {
                let phase = 2.0 * PI * target_k as f64 * i as f64 / n as f64;
                Cplx::new(phase.cos(), 0.0)
            })
            .collect();
        fft_in_place(&mut x);

        // Find the bin with the largest magnitude in [1, N/2]
        let mut max_bin = 1;
        let mut max_mag = x[1].norm_sq();
        for (k, val) in x.iter().enumerate().take(n / 2 + 1).skip(1) {
            let m = val.norm_sq();
            if m > max_mag {
                max_mag = m;
                max_bin = k;
            }
        }
        assert_eq!(max_bin, target_k, "expected peak at bin {target_k}");
    }

    #[test]
    fn power_spectrum_detects_500hz_tone() {
        // 1024 samples at 30 kHz of a 500 Hz sine wave → peak near 500 Hz
        let fs = 30_000.0;
        let f0 = 500.0;
        let n = 1024;
        let signal: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * f0 * i as f64 / fs).sin())
            .collect();
        let (freqs, psd_db) = power_spectrum_db(&signal, fs);
        assert_eq!(freqs.len(), n / 2);

        // Find peak bin
        let (peak_idx, _) = psd_db
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_freq = freqs[peak_idx];
        // Allow ±1 bin width tolerance
        let bin_hz = fs / n as f64;
        assert!(
            (peak_freq - f0).abs() < 2.0 * bin_hz,
            "peak at {} Hz, expected {} Hz",
            peak_freq,
            f0,
        );
    }

    #[test]
    fn hann_window_basic_properties() {
        let w = hann_window(64);
        assert_eq!(w.len(), 64);
        assert!(w[0].abs() < 1e-12);
        // Symmetric around center
        for i in 0..32 {
            assert!((w[i] - w[63 - i]).abs() < 1e-12);
        }
        // Peak near the middle (≥ 0.99)
        assert!(w[31] > 0.99 || w[32] > 0.99);
    }
}
