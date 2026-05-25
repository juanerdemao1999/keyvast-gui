//! Bottom frequency-spectrum panel.
//!
//! When a channel is hovered on the waveform display, we compute its
//! power spectral density (PSD) via FFT and show a log-magnitude plot
//! in a compact bottom panel.  This is the same approach used by
//! Open Ephys and Intan RHX for quick spectral inspection.

use std::collections::VecDeque;

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use kv_types::SampleBlock;

use crate::dsp::power_spectrum_db;
use crate::theme;

/// Height of the spectrum panel in logical pixels.
const SPECTRUM_HEIGHT: f32 = 120.0;

/// Draw the spectrum panel.  `hovered_channel` selects which channel's
/// data we compute the PSD for.  Returns silently if no channel is hovered
/// or there is insufficient data.
pub fn draw_spectrum_panel(
    ui: &mut egui::Ui,
    history: &VecDeque<SampleBlock>,
    hovered_channel: Option<usize>,
    sample_rate: f64,
) {
    let ch = match hovered_channel {
        Some(c) => c,
        None => {
            draw_placeholder(ui);
            return;
        }
    };

    // Collect the most recent samples for this channel from the history
    // We want at least 512 samples for a useful spectrum; prefer 1024+
    let min_samples = 512;
    let target_samples = 2048;

    let mut raw: Vec<f64> = Vec::with_capacity(target_samples);
    for block in history.iter().rev() {
        if ch >= block.channel_count {
            break;
        }
        let spc = block.samples_per_channel;
        let start = ch * spc;
        let end = start + spc;
        // Prepend block samples (we iterate blocks in reverse)
        let chunk: Vec<f64> = block.data[start..end]
            .iter()
            .rev()
            .map(|&s| s as f64 / 32768.0)
            .collect();
        raw.extend(chunk);
        if raw.len() >= target_samples {
            break;
        }
    }

    if raw.len() < min_samples {
        draw_placeholder(ui);
        return;
    }

    // Since we iterated in reverse order, reverse the collected samples
    raw.reverse();
    // Trim to target
    if raw.len() > target_samples {
        let excess = raw.len() - target_samples;
        raw.drain(..excess);
    }

    let (freqs, psd_db) = power_spectrum_db(&raw, sample_rate);
    if freqs.is_empty() {
        draw_placeholder(ui);
        return;
    }

    // Build plot points
    let points: Vec<[f64; 2]> = freqs
        .iter()
        .zip(psd_db.iter())
        .map(|(&f, &db)| [f, db])
        .collect();

    let ch_color = theme::channel_color(ch);
    let line = Line::new(PlotPoints::new(points))
        .color(ch_color)
        .width(1.3);

    // Label
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("  Spectrum — CH{}", ch))
                .size(10.0)
                .color(theme::TEXT_SECONDARY),
        );
        ui.label(
            egui::RichText::new(format!("({} Hz sample rate)", sample_rate as u32))
                .size(9.0)
                .color(theme::TEXT_DIM),
        );
    });

    Plot::new("spectrum_plot")
        .height(SPECTRUM_HEIGHT - 18.0)
        .show_axes([true, true])
        .show_grid([true, true])
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .x_axis_label("Frequency (Hz)")
        .y_axis_label("dB")
        .include_x(0.0)
        .include_x(sample_rate / 2.0)
        .include_y(-80.0)
        .include_y(0.0)
        .show(ui, |plot_ui| {
            plot_ui.line(line);
        });
}

fn draw_placeholder(ui: &mut egui::Ui) {
    let available = ui.available_size();
    let height = available.y.min(SPECTRUM_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(available.x, height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG_DARKEST);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Hover a channel to see its frequency spectrum",
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
}
