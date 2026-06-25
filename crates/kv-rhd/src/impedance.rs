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
        self.samples_per_period().saturating_mul(self.num_periods)
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
            [0, 200, 0, 255] // green
        } else if magnitude < 500_000.0 {
            [200, 200, 0, 255] // yellow
        } else if magnitude < 5_000_000.0 {
            [220, 80, 0, 255] // orange/red
        } else {
            [180, 0, 0, 255] // dark red
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
        let microvolts = sample as f64 * crate::protocol::RHD_AMPLIFIER_MICROVOLTS_PER_COUNT as f64;
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

use crate::backend::RhdReadError;
use crate::commands::{AuxCommandSlot, Rhd2000Registers};
use crate::frame_analysis::extract_channel_from_raw;
use crate::protocol::CHANNELS_PER_STREAM;
use crate::rhythm_board::RhythmFrontPanelBoard;

impl RhythmFrontPanelBoard {
    /// Run impedance measurement across all channels using the on-chip DAC.
    ///
    /// Algorithm (port of Intan RHX `impedancereader.cpp`):
    /// 1. Upload DC waveform to AuxCmd1 Bank 0, sine wave to AuxCmd1 Bank 1.
    /// 2. Upload register configs with zcheck enabled + 3 cap scales to
    ///    AuxCmd3 Banks 2/3/4.
    /// 3. For each channel: set zcheck_select, switch banks, run acquisition.
    /// 4. Compute impedance magnitude/phase via DFT at the test frequency.
    /// 5. Auto-select the best capacitor scale and re-measure if needed.
    pub(crate) fn run_impedance_test(
        &self,
        config: &crate::impedance::ImpedanceTestConfig,
        enabled_streams: usize,
        progress_callback: Option<&dyn Fn(usize, usize)>,
    ) -> Result<crate::impedance::ImpedanceResult, RhdReadError> {
        use crate::commands::ZcheckScale;

        log::info!(
            "starting impedance test: freq={:.0} Hz, {} channels, {} periods",
            config.frequency_hz,
            config.channel_count,
            config.num_periods
        );

        let mut registers = Rhd2000Registers::open_ephys_default();

        // ── Step 1: Upload DAC waveforms to AuxCmd1 ──────────────
        // Bank 0: DC (flat mid-scale).
        let dc_dac = registers
            .create_command_list_zcheck_dac(0.0, 0.0)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&dc_dac, AuxCommandSlot::AuxCmd1, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, dc_dac.len() - 1)?;

        // Bank 1: sine wave at the test frequency.
        let sine_dac = registers
            .create_command_list_zcheck_dac(config.frequency_hz, config.dac_amplitude)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&sine_dac, AuxCommandSlot::AuxCmd1, 1)?;

        // ── Step 2: Upload zcheck register configs to AuxCmd3 ────
        registers.enable_zcheck(true);

        registers.set_zcheck_scale(ZcheckScale::Cs100fF);
        registers.set_zcheck_polarity(false);
        let zcheck_100ff = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&zcheck_100ff, AuxCommandSlot::AuxCmd3, 2)?;

        registers.set_zcheck_scale(ZcheckScale::Cs1pF);
        let zcheck_1pf = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&zcheck_1pf, AuxCommandSlot::AuxCmd3, 3)?;

        registers.set_zcheck_scale(ZcheckScale::Cs10pF);
        let zcheck_10pf = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&zcheck_10pf, AuxCommandSlot::AuxCmd3, 4)?;

        // ── Step 3: Measure each channel ─────────────────────────
        let samples_needed = config.total_samples();
        let channels = config
            .channel_count
            .min(CHANNELS_PER_STREAM * enabled_streams);

        let mut results: Vec<crate::impedance::ChannelImpedance> = Vec::with_capacity(channels);

        // Start with sine wave on AuxCmd1 Bank 1.
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd1, 1)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, sine_dac.len() - 1)?;

        for ch in 0..channels {
            if let Some(cb) = progress_callback {
                cb(ch, channels);
            }

            let chip_channel = (ch % CHANNELS_PER_STREAM) as u8;
            registers.set_zcheck_channel(chip_channel);

            // Initial measurement with 1 pF (Bank 3).
            registers.set_zcheck_scale(ZcheckScale::Cs1pF);
            let updated_cfg = registers
                .create_command_list_register_config(false)
                .map_err(RhdReadError::Command)?;
            self.upload_command_list(&updated_cfg, AuxCommandSlot::AuxCmd3, 3)?;
            self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, 3)?;

            self.flush_fifo()?;
            self.set_max_time_step(samples_needed as u32)?;
            self.set_continuous_run_mode(false)?;
            self.run()?;
            self.wait_until_not_running()?;

            let raw = self.read_pipe_block(enabled_streams, samples_needed)?;
            let amp_data = extract_channel_from_raw(&raw, enabled_streams, samples_needed, ch);

            let (mag_1pf, phase_1pf) = crate::impedance::compute_impedance(
                &amp_data,
                config.sample_rate,
                config.frequency_hz,
                ZcheckScale::Cs1pF,
            );

            // Auto-select the best scale and re-measure if needed.
            let best_scale = crate::impedance::auto_select_scale(mag_1pf);

            let (magnitude, phase, scale) = if best_scale != ZcheckScale::Cs1pF {
                registers.set_zcheck_scale(best_scale);
                let re_cfg = registers
                    .create_command_list_register_config(false)
                    .map_err(RhdReadError::Command)?;

                let bank = match best_scale {
                    ZcheckScale::Cs100fF => 2,
                    ZcheckScale::Cs1pF => 3,
                    ZcheckScale::Cs10pF => 4,
                };

                self.upload_command_list(&re_cfg, AuxCommandSlot::AuxCmd3, bank)?;
                self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, bank)?;

                self.flush_fifo()?;
                self.set_max_time_step(samples_needed as u32)?;
                self.set_continuous_run_mode(false)?;
                self.run()?;
                self.wait_until_not_running()?;

                let raw2 = self.read_pipe_block(enabled_streams, samples_needed)?;
                let amp2 = extract_channel_from_raw(&raw2, enabled_streams, samples_needed, ch);

                let (mag, ph) = crate::impedance::compute_impedance(
                    &amp2,
                    config.sample_rate,
                    config.frequency_hz,
                    best_scale,
                );
                (mag, ph, best_scale)
            } else {
                (mag_1pf, phase_1pf, ZcheckScale::Cs1pF)
            };

            results.push(crate::impedance::ChannelImpedance {
                channel: ch,
                magnitude_ohms: magnitude,
                phase_degrees: phase,
                scale_used: scale,
                valid: magnitude.is_finite() && magnitude > 0.0,
            });
        }

        // ── Step 4: Restore normal operation ─────────────────────
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd1, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, dc_dac.len() - 1)?;

        registers.enable_zcheck(false);
        let normal_cfg = registers
            .create_command_list_register_config(false)
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&normal_cfg, AuxCommandSlot::AuxCmd3, 1)?;
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd3, 1)?;

        registers.set_dig_out_low();
        let dig_out = registers
            .create_command_list_update_dig_out()
            .map_err(RhdReadError::Command)?;
        self.upload_command_list(&dig_out, AuxCommandSlot::AuxCmd1, 0)?;
        self.select_aux_command_length(AuxCommandSlot::AuxCmd1, 0, dig_out.len() - 1)?;
        self.select_aux_command_bank_all_ports(AuxCommandSlot::AuxCmd1, 0)?;

        self.flush_fifo()?;

        log::info!(
            "impedance test complete: {} channels measured",
            results.len()
        );

        Ok(crate::impedance::ImpedanceResult {
            config: config.clone(),
            channels: results,
        })
    }
}

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
        let amplitude_counts =
            amplitude_uv / crate::protocol::RHD_AMPLIFIER_MICROVOLTS_PER_COUNT as f64;
        let data: Vec<i16> = (0..n)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * freq * (i as f64) / sample_rate;
                (amplitude_counts * phase.sin()).round() as i16
            })
            .collect();

        let (mag, _phase) = compute_impedance(&data, sample_rate, freq, ZcheckScale::Cs1pF);
        assert!(
            mag.is_finite() && mag > 0.0,
            "expected finite impedance, got {mag}"
        );
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
