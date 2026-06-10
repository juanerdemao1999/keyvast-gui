//! Impedance measurement support for RHD amplifiers.
//!
//! Implements the Intan RHD2000 impedance measurement algorithm:
//! 1. Inject a sine wave through the on-chip DAC (Register 6) at a test frequency.
//! 2. Read back the amplifier data from each channel.
//! 3. Compute impedance magnitude and phase via DFT at the test frequency.
//! 4. Auto-select the capacitor scale (100 fF / 1 pF / 10 pF) for best accuracy.
//!
//! Reference: Intan RHX `impedancereader.cpp`, `jonnew/impedance` repo.

use std::fmt;

use crate::commands::ZcheckScale;

/// Default test frequency for impedance measurement (Hz).
pub const DEFAULT_TEST_FREQUENCY: f64 = 1000.0;

/// Number of periods of the test waveform to acquire for impedance measurement.
pub const DEFAULT_NUM_PERIODS: usize = 20;

/// DAC amplitude (0..128).  Intan default = 128 (full scale).
pub const DEFAULT_DAC_AMPLITUDE: f64 = 128.0;

/// Impedance measurement result for a single channel.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelImpedance {
    pub channel: usize,
    pub magnitude_ohms: f64,
    pub phase_degrees: f64,
    pub scale_used: ZcheckScale,
    pub valid: bool,
}

/// Impedance test configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpedanceTestConfig {
    pub frequency_hz: f64,
    pub num_periods: usize,
    pub dac_amplitude: f64,
    pub sample_rate: f64,
    pub channel_count: usize,
}

impl Default for ImpedanceTestConfig {
    fn default() -> Self {
        Self {
            frequency_hz: DEFAULT_TEST_FREQUENCY,
            num_periods: DEFAULT_NUM_PERIODS,
            dac_amplitude: DEFAULT_DAC_AMPLITUDE,
            sample_rate: 30_000.0,
            channel_count: 64,
        }
    }
}

impl ImpedanceTestConfig {
    /// Samples per period of the test waveform.
    pub fn samples_per_period(&self) -> usize {
        if self.frequency_hz <= 0.0 {
            return 0;
        }
        (self.sample_rate / self.frequency_hz).round() as usize
    }

    /// Total samples needed for the measurement (num_periods complete cycles).
    pub fn total_samples(&self) -> usize {
        self.samples_per_period()
            .saturating_mul(self.num_periods)
    }
}

/// Full impedance measurement result set.
#[derive(Debug, Clone)]
pub struct ImpedanceResult {
    pub config: ImpedanceTestConfig,
    pub channels: Vec<ChannelImpedance>,
}

impl ImpedanceResult {
    /// Impedance quality label for display (good / acceptable / high / bad).
    pub fn quality_label(magnitude: f64) -> &'static str {
        if magnitude < 100_000.0 {
            "good"
        } else if magnitude < 500_000.0 {
            "acceptable"
        } else if magnitude < 5_000_000.0 {
            "high"
        } else {
            "bad"
        }
    }

    /// RGBA color for the impedance magnitude (green→yellow→red→gray).
    pub fn quality_color(magnitude: f64) -> [u8; 4] {
        if magnitude < 100_000.0 {
            [0, 200, 0, 255]       // green
        } else if magnitude < 500_000.0 {
            [200, 200, 0, 255]     // yellow
        } else if magnitude < 5_000_000.0 {
            [220, 80, 0, 255]      // orange/red
        } else {
            [180, 0, 0, 255]       // dark red
        }
    }
}

/// Compute impedance magnitude and phase from amplifier data using DFT at the
/// test frequency.
///
/// `amplifier_data` — raw i16 amplifier samples for one channel.
/// `sample_rate` — in Hz.
/// `frequency` — test frequency in Hz.
/// `cap_scale` — the ZcheckScale used.
///
/// Returns `(magnitude_ohms, phase_degrees)`.
///
/// Port of Intan `measureComplexAmplitude` + `approximateSaturationVoltage`.
pub fn compute_impedance(
    amplifier_data: &[i16],
    sample_rate: f64,
    frequency: f64,
    cap_scale: ZcheckScale,
) -> (f64, f64) {
    if amplifier_data.is_empty() || frequency <= 0.0 || sample_rate <= 0.0 {
        return (f64::INFINITY, 0.0);
    }

    let period = (sample_rate / frequency).round() as usize;
    if period == 0 {
        return (f64::INFINITY, 0.0);
    }

    // Use integer number of complete periods (discard partial tail).
    let num_complete = amplifier_data.len() / period;
    if num_complete == 0 {
        return (f64::INFINITY, 0.0);
    }
    let n = num_complete * period;
    let data = &amplifier_data[..n];

    // DFT at the test frequency — compute the single-bin Fourier coefficient.
    let two_pi = 2.0 * std::f64::consts::PI;
    let mut sum_cos = 0.0_f64;
    let mut sum_sin = 0.0_f64;
    for (i, &sample) in data.iter().enumerate() {
        let microvolts = sample as f64 * 0.195; // RHD µV/count
        let phase = two_pi * frequency * (i as f64) / sample_rate;
        sum_cos += microvolts * phase.cos();
        sum_sin += microvolts * phase.sin();
    }

    let real = 2.0 * sum_cos / n as f64; // µV
    let imag = 2.0 * sum_sin / n as f64; // µV

    // Convert voltage amplitude to impedance.
    // V_measured = I * Z, where I = Cs * (2π*f) * V_dac.
    // V_dac peak ≈ 128 * 1.225V / 256 ≈ 0.6125V (half-scale DAC output).
    let v_dac_peak = 128.0 * 1.225 / 256.0; // ~0.6125 V
    let cap_farads = cap_scale.capacitance_farads();
    let omega = two_pi * frequency;
    let i_current = cap_farads * omega * v_dac_peak; // Amps

    let v_amplitude_uv = (real * real + imag * imag).sqrt(); // µV
    let v_amplitude_v = v_amplitude_uv * 1.0e-6; // V

    let magnitude = if v_amplitude_v < 1.0e-15 {
        // No measurable signal — open circuit.
        f64::INFINITY
    } else if i_current > 0.0 {
        v_amplitude_v / i_current
    } else {
        f64::INFINITY
    };

    let phase_rad = imag.atan2(real);
    let phase_deg = phase_rad.to_degrees();

    (magnitude, phase_deg)
}

/// Select the best capacitor scale for a given impedance magnitude.
/// Mirrors Intan's auto-range logic.
pub fn auto_select_scale(magnitude_ohms: f64) -> ZcheckScale {
    if magnitude_ohms > 1_000_000.0 {
        ZcheckScale::Cs100fF
    } else if magnitude_ohms > 100_000.0 {
        ZcheckScale::Cs1pF
    } else {
        ZcheckScale::Cs10pF
    }
}

/// Error returned by the impedance measurement procedure.
#[derive(Debug)]
pub enum ImpedanceError {
    Hardware(String),
    NoData,
}

impl fmt::Display for ImpedanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hardware(msg) => write!(f, "impedance hardware error: {msg}"),
            Self::NoData => write!(f, "no impedance data collected"),
        }
    }
}

impl std::error::Error for ImpedanceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dc_impedance() {
        // All-zero data → infinite impedance
        let data = vec![0i16; 1000];
        let (mag, _phase) = compute_impedance(&data, 30_000.0, 1000.0, ZcheckScale::Cs1pF);
        assert!(
            mag > 1.0e12 || mag.is_infinite(),
            "expected very high impedance for zero signal, got {mag}"
        );
    }

    #[test]
    fn test_sine_impedance() {
        // Generate a known sine wave and verify the impedance computation
        // produces a finite, positive result.
        let sample_rate = 30_000.0;
        let freq = 1000.0;
        let n = 600; // 20 periods at 30 samples/period
        let amplitude_uv = 50.0; // 50 µV peak
        let amplitude_counts = amplitude_uv / 0.195;
        let data: Vec<i16> = (0..n)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * freq * (i as f64) / sample_rate;
                (amplitude_counts * phase.sin()).round() as i16
            })
            .collect();

        let (mag, _phase) = compute_impedance(&data, sample_rate, freq, ZcheckScale::Cs1pF);
        assert!(mag.is_finite() && mag > 0.0, "expected finite impedance, got {mag}");
    }

    #[test]
    fn test_auto_select_scale() {
        assert_eq!(auto_select_scale(5_000_000.0), ZcheckScale::Cs100fF);
        assert_eq!(auto_select_scale(500_000.0), ZcheckScale::Cs1pF);
        assert_eq!(auto_select_scale(50_000.0), ZcheckScale::Cs10pF);
    }

    #[test]
    fn test_quality_label() {
        assert_eq!(ImpedanceResult::quality_label(50_000.0), "good");
        assert_eq!(ImpedanceResult::quality_label(200_000.0), "acceptable");
        assert_eq!(ImpedanceResult::quality_label(1_000_000.0), "high");
        assert_eq!(ImpedanceResult::quality_label(10_000_000.0), "bad");
    }

    #[test]
    fn test_capacitance_values() {
        assert!((ZcheckScale::Cs100fF.capacitance_farads() - 0.1e-12).abs() < 1e-15);
        assert!((ZcheckScale::Cs1pF.capacitance_farads() - 1.0e-12).abs() < 1e-15);
        assert!((ZcheckScale::Cs10pF.capacitance_farads() - 10.0e-12).abs() < 1e-15);
    }
}
