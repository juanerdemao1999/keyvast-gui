//! Real-time FFT spectrum analysis panel.
//!
//! Displays power spectral density (PSD) for a selected channel using an
//! in-place radix-2 Cooley-Tukey FFT — no external dependency needed.

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use crate::disp_ring::{DisplayRing, RING_DWNSP};
use crate::theme;

/// FFT analysis state.
#[derive(Debug, Clone)]
pub struct FftState {
    /// Which channel to analyze (physical index).
    pub selected_channel: usize,
    /// FFT size (power of 2).
    pub fft_size: usize,
    /// Whether to show the FFT panel.
    pub enabled: bool,
    /// Cached PSD result: (frequency_hz, power_db) pairs.
    pub spectrum: Vec<[f64; 2]>,
    /// Whether to use log scale on Y axis.
    pub log_scale: bool,
    /// Low frequency cutoff for display (Hz).
    pub freq_min: f64,
    /// High frequency cutoff for display (Hz).
    pub freq_max: f64,
}

impl Default for FftState {
    fn default() -> Self {
        Self {
            selected_channel: 0,
            fft_size: 1024,
            enabled: false,
            spectrum: Vec::new(),
            log_scale: true,
            freq_min: 1.0,
            freq_max: 5000.0,
        }
    }
}

/// Available FFT sizes.
const FFT_SIZES: &[usize] = &[256, 512, 1024, 2048, 4096];

/// Compute the PSD from the display ring for a given channel.
///
/// `hardware_sample_rate` is the raw device rate (e.g. 30 000 Hz). The
/// effective ring sample rate is `hardware_sample_rate / RING_DWNSP`.
pub fn compute_spectrum(
    ring: &DisplayRing,
    channel: usize,
    fft_size: usize,
    hardware_sample_rate: f64,
) -> Vec<[f64; 2]> {
    if !ring.ready || hardware_sample_rate <= 0.0 {
        return Vec::new();
    }

    // The ring stores decimated data — use the decimated rate for frequency
    // axis calculations.
    let ring_sr = hardware_sample_rate / RING_DWNSP as f64;

    // Extract the most recent `fft_size` samples for this channel from the ring.
    let raw = ring.last_n_samples(channel, fft_size);
    if raw.len() < fft_size {
        return Vec::new();
    }

    // Apply Hann window and convert to f64.
    let n = fft_size;
    let mut real = Vec::with_capacity(n);
    let mut imag = vec![0.0_f64; n];
    let pi2_over_n = 2.0 * std::f64::consts::PI / n as f64;

    // Compute coherent gain of Hann window for amplitude correction.
    let mut win_sum = 0.0_f64;
    for i in 0..n {
        win_sum += 0.5 * (1.0 - (pi2_over_n * i as f64).cos());
    }
    let win_norm = win_sum / n as f64;

    for (i, &sample) in raw.iter().enumerate().take(n) {
        let w = 0.5 * (1.0 - (pi2_over_n * i as f64).cos()); // Hann window
        real.push(sample as f64 * kv_rhd::RHD_AMPLIFIER_MICROVOLTS_PER_COUNT as f64 * w);
    }

    // In-place radix-2 FFT.
    fft_radix2(&mut real, &mut imag);

    // Compute one-sided PSD in dB (µV²/Hz).
    let bin_width = ring_sr / n as f64;
    let n_bins = n / 2 + 1;
    let mut spectrum = Vec::with_capacity(n_bins);
    for k in 0..n_bins {
        let freq = k as f64 * bin_width;
        // Normalise by window power (win_norm²) so PSD amplitude is correct.
        let power =
            (real[k] * real[k] + imag[k] * imag[k]) / (n as f64 * ring_sr * win_norm * win_norm);
        // Double one-sided bins (except DC and Nyquist).
        let power = if k > 0 && k < n / 2 {
            power * 2.0
        } else {
            power
        };
        let db = 10.0 * (power.max(1e-20)).log10();
        spectrum.push([freq, db]);
    }

    spectrum
}

/// In-place radix-2 Cooley-Tukey FFT. `real` and `imag` must have power-of-2 length.
fn fft_radix2(real: &mut [f64], imag: &mut [f64]) {
    let n = real.len();
    debug_assert!(n.is_power_of_two() && imag.len() == n);

    // Bit-reversal permutation.
    let mut j = 0;
    for i in 0..n {
        if i < j {
            real.swap(i, j);
            imag.swap(i, j);
        }
        let mut m = n >> 1;
        while m >= 1 && j >= m {
            j -= m;
            m >>= 1;
        }
        j += m;
    }

    // Butterfly stages.
    let mut stage_len = 2;
    while stage_len <= n {
        let half = stage_len / 2;
        let angle = -2.0 * std::f64::consts::PI / stage_len as f64;
        let w_re = angle.cos();
        let w_im = angle.sin();

        let mut group_start = 0;
        while group_start < n {
            let mut tw_re = 1.0;
            let mut tw_im = 0.0;
            for k in 0..half {
                let even = group_start + k;
                let odd = even + half;
                let t_re = tw_re * real[odd] - tw_im * imag[odd];
                let t_im = tw_re * imag[odd] + tw_im * real[odd];
                real[odd] = real[even] - t_re;
                imag[odd] = imag[even] - t_im;
                real[even] += t_re;
                imag[even] += t_im;
                let new_tw_re = tw_re * w_re - tw_im * w_im;
                let new_tw_im = tw_re * w_im + tw_im * w_re;
                tw_re = new_tw_re;
                tw_im = new_tw_im;
            }
            group_start += stage_len;
        }
        stage_len <<= 1;
    }
}

/// Draw the FFT panel section in the left sidebar.
pub fn draw_fft_section(
    ui: &mut egui::Ui,
    state: &mut FftState,
    ring: &DisplayRing,
    sample_rate: f64,
    total_channels: usize,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("SPECTRUM (FFT)")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut state.enabled, egui::RichText::new("Enable").size(10.0));
        });

        if !state.enabled {
            return;
        }

        // Channel selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Channel")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            let mut ch = state.selected_channel as i32;
            let max_ch = (total_channels.saturating_sub(1)) as i32;
            if ui
                .add(egui::DragValue::new(&mut ch).range(0..=max_ch).speed(0.3))
                .changed()
            {
                state.selected_channel = ch.max(0) as usize;
            }
        });

        // FFT size selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("FFT size")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            egui::ComboBox::from_id_salt("fft_size")
                .width(80.0)
                .selected_text(format!("{}", state.fft_size))
                .show_ui(ui, |ui| {
                    for &sz in FFT_SIZES {
                        ui.selectable_value(&mut state.fft_size, sz, format!("{sz}"));
                    }
                });
        });

        // Frequency range
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Range")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::DragValue::new(&mut state.freq_min)
                    .range(0.0..=state.freq_max - 1.0)
                    .speed(1.0)
                    .suffix(" Hz"),
            );
            ui.label(egui::RichText::new("–").size(10.0));
            let ring_nyquist = sample_rate / RING_DWNSP as f64 / 2.0;
            ui.add(
                egui::DragValue::new(&mut state.freq_max)
                    .range(state.freq_min + 1.0..=ring_nyquist)
                    .speed(10.0)
                    .suffix(" Hz"),
            );
        });

        ui.checkbox(
            &mut state.log_scale,
            egui::RichText::new("Log scale (dB)").size(10.0),
        );

        // Compute spectrum
        if ring.ready {
            state.spectrum =
                compute_spectrum(ring, state.selected_channel, state.fft_size, sample_rate);
        }
    });
}

/// Draw the FFT spectrum plot in the central area.
pub fn draw_fft_plot(ui: &mut egui::Ui, state: &FftState, sample_rate: f64) {
    if !state.enabled || state.spectrum.is_empty() {
        return;
    }

    // Filter to display range.
    let points: Vec<[f64; 2]> = state
        .spectrum
        .iter()
        .filter(|p| p[0] >= state.freq_min && p[0] <= state.freq_max)
        .copied()
        .collect();

    if points.is_empty() {
        return;
    }

    let plot = Plot::new("fft_spectrum")
        .height(ui.available_height().min(200.0))
        .width(ui.available_width())
        .show_axes([true, true])
        .show_grid(true)
        .allow_drag(true)
        .allow_zoom(true)
        .allow_scroll(false)
        .auto_bounds(egui::Vec2b::new(true, true))
        .x_axis_label("Frequency (Hz)")
        .y_axis_label(if state.log_scale {
            "Power (dB)"
        } else {
            "Power (µV²/Hz)"
        })
        .set_margin_fraction(egui::vec2(0.02, 0.05));

    let ch = state.selected_channel;
    let color = theme::channel_color(ch);

    plot.show(ui, |plot_ui| {
        plot_ui.line(
            Line::new(PlotPoints::from(points))
                .color(color)
                .width(1.5)
                .name(format!("CH{ch} PSD")),
        );

        // Draw notable frequency markers
        for &freq in &[50.0, 60.0] {
            if freq >= state.freq_min && freq <= state.freq_max {
                plot_ui.line(
                    Line::new(PlotPoints::from(vec![[freq, -200.0], [freq, 100.0]]))
                        .color(egui::Color32::from_rgba_unmultiplied(255, 100, 100, 40))
                        .width(0.8)
                        .style(egui_plot::LineStyle::dashed_dense())
                        .name(format!("{freq} Hz")),
                );
            }
        }

        // Nyquist marker
        let nyquist = sample_rate / 2.0;
        if nyquist >= state.freq_min && nyquist <= state.freq_max {
            plot_ui.line(
                Line::new(PlotPoints::from(vec![[nyquist, -200.0], [nyquist, 100.0]]))
                    .color(egui::Color32::from_rgba_unmultiplied(200, 200, 0, 40))
                    .width(0.8)
                    .style(egui_plot::LineStyle::dashed_dense())
                    .name("Nyquist"),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_pure_sine_peak() {
        let n = 1024;
        let sr = 1000.0;
        let freq = 100.0;

        let mut real: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sr).sin())
            .collect();
        let mut imag = vec![0.0; n];
        fft_radix2(&mut real, &mut imag);

        // Find peak bin.
        let bin_width = sr / n as f64;
        let expected_bin = (freq / bin_width).round() as usize;
        let mut max_bin = 0;
        let mut max_power = 0.0_f64;
        for k in 1..n / 2 {
            let power = real[k] * real[k] + imag[k] * imag[k];
            if power > max_power {
                max_power = power;
                max_bin = k;
            }
        }
        assert_eq!(max_bin, expected_bin, "peak should be at {freq} Hz bin");
    }

    #[test]
    fn fft_dc_signal() {
        let n = 256;
        let mut real = vec![1.0; n];
        let mut imag = vec![0.0; n];
        fft_radix2(&mut real, &mut imag);
        // DC bin should have all the energy.
        assert!((real[0] - n as f64).abs() < 1e-10);
        for k in 1..n {
            let power = real[k] * real[k] + imag[k] * imag[k];
            assert!(power < 1e-20, "non-DC bin {k} should be zero");
        }
    }
}
