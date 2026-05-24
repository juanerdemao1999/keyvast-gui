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
///
/// `elapsed_secs` is the wall-clock time since acquisition started; it drives
/// the X-axis window edge so scrolling is smooth and continuous.
pub fn draw_waveform_area(
    ui: &mut egui::Ui,
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    settings: &DisplaySettings,
    elapsed_secs: f64,
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

    // X-axis window driven by wall clock — smooth continuous scroll
    let x_right = elapsed_secs * 1000.0; // current time in ms
    let x_left = (x_right - time_window_ms).max(0.0);

    // Build lines for each channel
    let mut lines: Vec<(usize, Vec<[f64; 2]>)> = Vec::with_capacity(visible);

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
            x_left,
            x_right,
        );

        // Apply vertical offset: channel 0 at top, channel N at bottom
        let y_offset = -(ch as f64) * CHANNEL_SPACING;
        let pts: Vec<[f64; 2]> = raw_pts
            .into_iter()
            .map(|[x, y]| [x, y * gain + y_offset])
            .collect();
        lines.push((ch, pts));
    }

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
        // Lock to exact bounds — X from wall clock, Y from channel layout
        plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
            [x_left, y_min],
            [x_right, y_max],
        ));

        // Draw zero-reference lines spanning the visible window
        if settings.show_grid {
            for ch in 0..visible {
                if !settings.is_channel_enabled(ch) {
                    continue;
                }
                let y_off = -(ch as f64) * CHANNEL_SPACING;
                let zero_line = Line::new(PlotPoints::from(vec![
                    [x_left, y_off],
                    [x_right, y_off],
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

/// Collect (time_ms, raw_normalized) pairs for one channel.
///
/// Uses `block.timestamp_start` for absolute time positioning.  Decimation
/// is **anchored to absolute sample index** (modulo stride) so the same
/// physical samples are picked regardless of when they entered the window —
/// this is what makes scrolling look smooth instead of flickering.
fn collect_channel_points(
    ch: usize,
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    channel_count: usize,
    sample_rate: f64,
    t_left_ms: f64,
    t_right_ms: f64,
) -> Vec<[f64; 2]> {
    let ms_per_sample = if sample_rate > 0.0 {
        1000.0 / sample_rate
    } else {
        1.0
    };

    // Total samples that would fit in the visible window
    let window_samples = ((t_right_ms - t_left_ms) / ms_per_sample).ceil() as u64;
    // Stride to keep total displayed points around MAX_DISPLAY_POINTS.
    // Anchored to absolute sample index → same physical samples chosen each frame.
    let stride = (window_samples / MAX_DISPLAY_POINTS as u64).max(1);

    let mut all_points: Vec<[f64; 2]> = Vec::with_capacity(MAX_DISPLAY_POINTS + 16);

    let blocks_iter = history.iter().chain(latest);

    for block in blocks_iter {
        if block.channel_count != channel_count {
            continue;
        }
        let spc = block.samples_per_channel;
        let block_start_ms = block.timestamp_start as f64 * ms_per_sample;
        let block_end_ms = block_start_ms + spc as f64 * ms_per_sample;

        // Skip blocks entirely outside the visible window
        if block_end_ms < t_left_ms || block_start_ms > t_right_ms {
            continue;
        }

        for s in 0..spc {
            let abs_idx = block.timestamp_start + s as u64;
            // Anchored decimation: only keep samples on the global stride
            if stride > 1 && abs_idx % stride != 0 {
                continue;
            }
            let time_ms = abs_idx as f64 * ms_per_sample;
            if time_ms < t_left_ms || time_ms > t_right_ms {
                continue;
            }
            let data_idx = s * channel_count + ch;
            let value = if data_idx < block.data.len() {
                block.data[data_idx] as f64 / i16::MAX as f64
            } else {
                0.0
            };
            all_points.push([time_ms, value]);
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
