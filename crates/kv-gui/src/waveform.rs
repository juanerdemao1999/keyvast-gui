//! Professional waveform rendering using egui_plot.
//!
//! Each channel is rendered as an individual Plot widget inside a vertical
//! ScrollArea.  egui_plot provides smooth antialiased lines, interactive
//! pan/zoom, and automatic axis formatting out of the box.

use std::collections::VecDeque;

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use kv_types::SampleBlock;

use crate::panels::DisplaySettings;
use crate::theme;

/// Minimum height per channel trace in pixels.
const MIN_CHANNEL_HEIGHT: f32 = 50.0;
/// Maximum samples to display per channel (performance limit).
const MAX_DISPLAY_SAMPLES: usize = 4096;

/// Draw the full waveform area using egui_plot.
pub fn draw_waveform_area(
    ui: &mut egui::Ui,
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    settings: &DisplaySettings,
) {
    let block = match latest {
        Some(b) => b,
        None => {
            draw_empty_state(ui);
            return;
        }
    };

    let channels = settings.visible_channels.min(block.channel_count);
    if channels == 0 {
        draw_empty_state(ui);
        return;
    }

    let available = ui.available_size();
    let channel_height = (available.y / channels as f32).max(MIN_CHANNEL_HEIGHT);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for ch in 0..channels {
                draw_channel_plot(
                    ui,
                    ch,
                    channel_height,
                    history,
                    latest,
                    block.channel_count,
                    block.sample_rate,
                    settings,
                );
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn draw_channel_plot(
    ui: &mut egui::Ui,
    ch: usize,
    height: f32,
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    channel_count: usize,
    sample_rate: f64,
    settings: &DisplaySettings,
) {
    let color = theme::channel_color(ch);
    let _bg = if ch.is_multiple_of(2) {
        theme::BG_WAVEFORM_EVEN
    } else {
        theme::BG_WAVEFORM_ODD
    };

    // Collect samples for this channel across history + latest
    let points = collect_channel_points(ch, history, latest, channel_count, sample_rate);

    let plot_id = format!("ch_plot_{ch}");

    // Channel label on the left
    ui.horizontal(|ui| {
        // Color bar + label
        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(4.0, height), egui::Sense::hover());
        ui.painter().rect_filled(bar_rect, 0.0, color);

        let (label_rect, _) =
            ui.allocate_exact_size(egui::vec2(38.0, height), egui::Sense::hover());
        ui.painter().text(
            egui::pos2(label_rect.left() + 4.0, label_rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("CH{ch}"),
            egui::FontId::monospace(9.0),
            if settings.show_channel_labels {
                theme::TEXT_SECONDARY
            } else {
                egui::Color32::TRANSPARENT
            },
        );

        // Plot
        let line = Line::new(PlotPoints::from(points)).color(color).width(1.2);

        Plot::new(&plot_id)
            .height(height)
            .width(ui.available_width())
            .show_axes(false)
            .show_grid(settings.show_grid)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .include_y(-1.0)
            .include_y(1.0)
            .show_x(false)
            .show_y(false)
            .set_margin_fraction(egui::vec2(0.0, 0.02))
            .show(ui, |plot_ui| {
                // Zero reference line
                let zero = Line::new(PlotPoints::from(vec![[0.0, 0.0], [1.0, 0.0]]))
                    .color(theme::GRID_ZERO_LINE)
                    .width(0.5);
                plot_ui.line(zero);
                plot_ui.line(line);
            });
    });
}

/// Collect (time_ms, normalized_value) pairs for one channel from the history ring.
fn collect_channel_points(
    ch: usize,
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    channel_count: usize,
    sample_rate: f64,
) -> Vec<[f64; 2]> {
    let ms_per_sample = if sample_rate > 0.0 {
        1000.0 / sample_rate
    } else {
        1.0
    };

    let mut all_points: Vec<[f64; 2]> = Vec::new();
    let mut sample_offset: u64 = 0;

    let blocks_iter = history.iter().chain(latest);

    for block in blocks_iter {
        if block.channel_count != channel_count {
            continue;
        }
        let spc = block.samples_per_channel;
        for s in 0..spc {
            let data_idx = s * channel_count + ch;
            let value = if data_idx < block.data.len() {
                block.data[data_idx] as f64 / i16::MAX as f64
            } else {
                0.0
            };
            let time_ms = (sample_offset + s as u64) as f64 * ms_per_sample;
            all_points.push([time_ms, value]);
        }
        sample_offset += spc as u64;
    }

    // Downsample if too many points (keep tail for real-time feel)
    if all_points.len() > MAX_DISPLAY_SAMPLES {
        let skip = all_points.len() - MAX_DISPLAY_SAMPLES;
        all_points.drain(..skip);
        // Re-zero time
        if let Some(t0) = all_points.first().map(|p| p[0]) {
            for p in &mut all_points {
                p[0] -= t0;
            }
        }
    }

    all_points
}

fn draw_empty_state(ui: &mut egui::Ui) {
    let available = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());

    ui.painter().rect_filled(rect, 0.0, theme::BG_DARKEST);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "No data",
        egui::FontId::proportional(16.0),
        theme::TEXT_DIM,
    );
}
