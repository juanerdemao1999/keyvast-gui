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

/// Maximum frequency shown on the spectrum (Hz).
/// 500 Hz covers the spike band and LFP; above that is mostly noise
/// for extracellular neural recordings.
const MAX_DISPLAY_FREQ: f64 = 500.0;

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

    // Collect the most recent samples for this channel from the history.
    // Data is interleaved: data[s * channel_count + ch].
    let min_samples = 512;
    let target_samples = 2048;

    let mut raw: Vec<f64> = Vec::with_capacity(target_samples);
    for block in history.iter().rev() {
        if ch >= block.channel_count {
            break;
        }
        let spc = block.samples_per_channel;
        let ch_count = block.channel_count;
        // Extract this channel's samples in reverse sample order
        for s in (0..spc).rev() {
            let data_idx = s * ch_count + ch;
            if data_idx < block.data.len() {
                raw.push(block.data[data_idx] as f64 / i16::MAX as f64);
            }
        }
        if raw.len() >= target_samples {
            break;
        }
    }

    if raw.len() < min_samples {
        draw_placeholder(ui);
        return;
    }

    // Reverse so samples are in chronological order
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

    // Build plot points, capped at MAX_DISPLAY_FREQ
    let points: Vec<[f64; 2]> = freqs
        .iter()
        .zip(psd_db.iter())
        .filter(|&(&f, _)| f <= MAX_DISPLAY_FREQ)
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

    let plot_height = ui.available_height().max(60.0);
    Plot::new("spectrum_plot")
        .height(plot_height)
        .show_axes([true, true])
        .show_grid([true, true])
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .auto_bounds(egui::Vec2b::new(false, false))
        .x_axis_label("Hz")
        .show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
                [0.0, -80.0],
                [MAX_DISPLAY_FREQ, 5.0],
            ));
            plot_ui.line(line);
        });
}

fn draw_placeholder(ui: &mut egui::Ui) {
    let available = ui.available_size();
    let height = available.y.max(40.0);
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
