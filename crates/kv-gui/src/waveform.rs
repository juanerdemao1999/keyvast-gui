//! Professional multi-channel waveform display.
//!
//! Renders all visible channels as vertically-stacked traces in a **single**
//! `egui_plot::Plot` widget — matching the approach used by Intan RHX,
//! Open Ephys, and other professional electrophysiology acquisition software.
//!
//! Each channel is offset vertically so traces form a waterfall display.
//! The X axis auto-scrolls to always show the most recent data window.
//! Grid lines, zero-reference lines, and per-channel coloring are supported.

use std::collections::VecDeque;

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use kv_types::SampleBlock;

use crate::panels::DisplaySettings;
use crate::theme;

/// Maximum rendered points per channel (decimation target for performance).
const MAX_DISPLAY_POINTS: usize = 4096;

/// Vertical spacing (in normalized units) between channel baselines.
const CHANNEL_SPACING: f64 = 2.2;

// ── Public entry point ──────────────────────────────────────────────

/// Draw the full waveform area — one large Plot with all channels stacked.
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

    let total_channels = block.channel_count;
    let visible = settings.visible_channels.min(total_channels);
    if visible == 0 {
        draw_empty_state(ui);
        return;
    }

    let amp_scale = settings.amp_scale_uv();
    let time_window_ms = settings.time_window_ms();

    // Gain maps normalized i16 data (±0.06 typical neural) to fill the channel lane.
    let gain = CHANNEL_SPACING * 3.0 * (1000.0 / amp_scale.max(1.0));

    // Build lines for each channel and track the global X range
    let mut lines: Vec<(usize, Vec<[f64; 2]>)> = Vec::with_capacity(visible);
    let mut x_min = f64::MAX;
    let mut x_max = f64::MIN;

    for ch in 0..visible {
        if !settings.is_channel_enabled(ch) {
            continue;
        }
        let raw_pts = collect_channel_points(
            ch,
            history,
            latest,
            total_channels,
            block.sample_rate,
            time_window_ms,
        );

        // Track X range from actual data
        if let Some(first) = raw_pts.first() {
            x_min = x_min.min(first[0]);
        }
        if let Some(last) = raw_pts.last() {
            x_max = x_max.max(last[0]);
        }

        // Apply vertical offset: channel 0 at top, channel N at bottom
        let y_offset = -(ch as f64) * CHANNEL_SPACING;
        let pts: Vec<[f64; 2]> = raw_pts
            .into_iter()
            .map(|[x, y]| [x, y * gain + y_offset])
            .collect();
        lines.push((ch, pts));
    }

    // Fallback if no data
    if x_min >= x_max {
        x_min = 0.0;
        x_max = 100.0;
    }
    // Small margin
    let x_margin = (x_max - x_min) * 0.02;

    // Y axis bounds
    let y_min = -(visible as f64) * CHANNEL_SPACING + CHANNEL_SPACING * 0.5;
    let y_max = CHANNEL_SPACING * 0.5;

    // Channel label formatter for Y-axis
    let ch_count_for_fmt = visible;
    let y_formatter = move |mark: egui_plot::GridMark, _range: &std::ops::RangeInclusive<f64>| {
        let val = mark.value;
        let ch_idx = (-val / CHANNEL_SPACING).round() as i64;
        if ch_idx >= 0 && (ch_idx as usize) < ch_count_for_fmt {
            format!("CH{}", ch_idx)
        } else {
            String::new()
        }
    };

    // Draw the combined plot — explicit bounds, no auto-fit (prevents Y-axis jitter)
    let plot = Plot::new("waveform_main")
        .height(ui.available_height())
        .width(ui.available_width())
        .show_axes([true, true])
        .show_grid(settings.show_grid)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .auto_bounds(egui::Vec2b::new(false, false))
        .show_x(true)
        .show_y(true)
        .x_axis_label("Time (ms)")
        .y_axis_formatter(y_formatter)
        .set_margin_fraction(egui::vec2(0.0, 0.01));

    plot.show(ui, |plot_ui| {
        // Lock to exact bounds every frame — X follows data, Y is fixed to channel layout
        plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
            [x_min - x_margin, y_min],
            [x_max + x_margin, y_max],
        ));
        // Draw zero-reference lines — span only the actual data range
        if settings.show_grid {
            for ch in 0..visible {
                if !settings.is_channel_enabled(ch) {
                    continue;
                }
                let y_off = -(ch as f64) * CHANNEL_SPACING;
                let zero_line = Line::new(PlotPoints::from(vec![
                    [x_min - x_margin, y_off],
                    [x_max + x_margin, y_off],
                ]))
                .color(theme::GRID_ZERO_LINE)
                .width(0.5)
                .name("");
                plot_ui.line(zero_line);
            }
        }

        // Draw waveform traces
        for (ch, pts) in &lines {
            let color = theme::channel_color(*ch);
            let line = Line::new(PlotPoints::from(pts.clone()))
                .color(color)
                .width(1.2)
                .name(format!("CH{ch}"));
            plot_ui.line(line);
        }
    });
}

// ── Data collection ─────────────────────────────────────────────────

/// Collect (time_ms, raw_normalized) pairs for one channel from the history ring.
///
/// Only keeps the most recent `window_ms` milliseconds of data so the display
/// width matches the user-selected time window.  Time is re-zeroed so the
/// visible window always starts at 0.
fn collect_channel_points(
    ch: usize,
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    channel_count: usize,
    sample_rate: f64,
    window_ms: f64,
) -> Vec<[f64; 2]> {
    let ms_per_sample = if sample_rate > 0.0 {
        1000.0 / sample_rate
    } else {
        1.0
    };

    // How many raw samples fit in the requested window
    let window_samples = (window_ms / ms_per_sample).ceil() as usize;

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

    // Keep only the tail that fits the time window
    if all_points.len() > window_samples {
        let skip = all_points.len() - window_samples;
        all_points.drain(..skip);
    }

    // Decimate to MAX_DISPLAY_POINTS for rendering performance
    if all_points.len() > MAX_DISPLAY_POINTS {
        let step = all_points.len() as f64 / MAX_DISPLAY_POINTS as f64;
        let mut decimated = Vec::with_capacity(MAX_DISPLAY_POINTS);
        let mut idx = 0.0_f64;
        while (idx as usize) < all_points.len() && decimated.len() < MAX_DISPLAY_POINTS {
            decimated.push(all_points[idx as usize]);
            idx += step;
        }
        all_points = decimated;
    }

    // Re-zero time so the window starts at 0
    if let Some(t0) = all_points.first().map(|p| p[0]) {
        if t0 != 0.0 {
            for p in &mut all_points {
                p[0] -= t0;
            }
        }
    }

    all_points
}

// ── Empty state ─────────────────────────────────────────────────────

fn draw_empty_state(ui: &mut egui::Ui) {
    let available = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());

    ui.painter().rect_filled(rect, 0.0, theme::BG_DARKEST);

    ui.painter().text(
        rect.center() + egui::vec2(0.0, -12.0),
        egui::Align2::CENTER_CENTER,
        "No Data",
        egui::FontId::proportional(18.0),
        theme::TEXT_DIM,
    );
    ui.painter().text(
        rect.center() + egui::vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        "Press Start or switch to Demo mode",
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
}
