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

use crate::panels::{DisplaySettings, FilterSettings};
use crate::theme;

/// Maximum rendered points per channel (decimation target for performance).
const MAX_DISPLAY_POINTS: usize = 4096;

/// Default vertical spacing (in normalized units) between channel baselines.
pub const DEFAULT_CHANNEL_SPACING: f64 = 2.2;

/// Per-channel rendered trace plus optional spike detection metadata.
struct ChannelTrace {
    channel: usize,
    points: Vec<[f64; 2]>,
    /// RMS sigma in normalized-input units (only set when threshold is enabled).
    sigma: Option<f64>,
    /// Negative-going threshold crossings within the window.
    spike_count: u32,
}

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
    filters: &FilterSettings,
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
    let ch_spacing = settings.channel_spacing;

    // Gain maps normalized i16 data to visual Y-units.  Independent of
    // ch_spacing so that adjusting spacing does NOT change waveform amplitude.
    // Uses the default lane height as the reference (like Intan RHX where
    // amplitude scale and channel spacing are independent controls).
    let gain = DEFAULT_CHANNEL_SPACING * 3.0 * (1000.0 / amp_scale.max(1.0));

    // X-axis window driven by wall clock — smooth continuous scroll
    let x_right = elapsed_secs * 1000.0; // current time in ms
    let x_left = (x_right - time_window_ms).max(0.0);

    // Data is pre-filtered by app.rs (incremental pipeline) — always use
    // the fast decimation path.  Spike threshold detection runs inline on
    // the (already-filtered) decimated data when enabled.
    let traces = collect_lines_fast(
        history, latest, settings, filters, visible, total_channels,
        block.sample_rate, x_left, x_right, gain, ch_spacing,
    );

    // Y axis bounds
    let y_min = -(visible as f64) * ch_spacing + ch_spacing * 0.5;
    let y_max = ch_spacing * 0.5;

    // Channel label formatter for Y-axis
    let ch_count_for_fmt = visible;
    let spacing_for_fmt = ch_spacing;
    let y_formatter = move |mark: egui_plot::GridMark, _range: &std::ops::RangeInclusive<f64>| {
        let val = mark.value;
        let ch_idx = (-val / spacing_for_fmt).round() as i64;
        if ch_idx >= 0 && (ch_idx as usize) < ch_count_for_fmt {
            format!("CH{}", ch_idx)
        } else {
            String::new()
        }
    };

    // Time-axis formatter: show seconds when window is large, ms otherwise
    let window_ms = x_right - x_left;
    let x_formatter = move |mark: egui_plot::GridMark, _: &std::ops::RangeInclusive<f64>| {
        let v = mark.value;
        if window_ms >= 2000.0 {
            format!("{:.1}s", v / 1000.0)
        } else if window_ms >= 200.0 {
            format!("{:.0}ms", v)
        } else {
            format!("{:.1}ms", v)
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
        .x_axis_label("Time")
        .x_axis_formatter(x_formatter)
        .y_axis_formatter(y_formatter)
        .set_margin_fraction(egui::vec2(0.0, 0.01));

    let response = plot.show(ui, |plot_ui| {
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
                let y_off = -(ch as f64) * ch_spacing;
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

        // Determine which channel the cursor is hovering over (Y → channel)
        let hovered_ch: Option<usize> = plot_ui.pointer_coordinate().and_then(|pos| {
            let ch_idx = (-pos.y / ch_spacing).round() as i64;
            if ch_idx >= 0 && (ch_idx as usize) < visible {
                Some(ch_idx as usize)
            } else {
                None
            }
        });

        // Draw spike threshold lines (negative-going) when enabled
        if filters.spike_threshold_enabled {
            for trace in &traces {
                if let Some(sigma) = trace.sigma {
                    let y_off = -(trace.channel as f64) * ch_spacing;
                    let thresh_y =
                        -filters.spike_threshold_sigma * sigma * gain + y_off;
                    let line = Line::new(PlotPoints::from(vec![
                        [x_left, thresh_y],
                        [x_right, thresh_y],
                    ]))
                    .color(theme::ACCENT_RED)
                    .width(0.8)
                    .style(egui_plot::LineStyle::dashed_dense());
                    plot_ui.line(line);
                }
            }
        }

        // Draw waveform traces — highlight hovered channel
        for trace in &traces {
            let base_color = theme::channel_color(trace.channel);
            let is_hovered = hovered_ch == Some(trace.channel);
            let (color, width) = if is_hovered {
                (egui::Color32::WHITE, 1.8)
            } else if hovered_ch.is_some() {
                // Dim non-hovered channels when something is hovered
                (dim_color(base_color, 0.45), 1.0)
            } else {
                (base_color, 1.2)
            };
            let line = Line::new(PlotPoints::from(trace.points.clone()))
                .color(color)
                .width(width)
                .name(format!("CH{}", trace.channel));
            plot_ui.line(line);
        }

        hovered_ch
    });

    // Spike-count badges on the right edge of each lane (overlay painted in screen space)
    if filters.spike_threshold_enabled {
        let painter = ui.painter();
        for trace in &traces {
            if trace.spike_count == 0 {
                continue;
            }
            let y_lane = -(trace.channel as f64) * ch_spacing;
            let plot_pos = egui_plot::PlotPoint::new(x_right, y_lane);
            let screen_pos = response.transform.position_from_point(&plot_pos);
            let badge_pos = screen_pos + egui::vec2(-6.0, -1.0);
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_CENTER,
                format!("{}", trace.spike_count),
                egui::FontId::monospace(10.0),
                theme::ACCENT_RED,
            );
        }
    }

    // Tooltip with the hovered channel + time
    if response.response.hovered()
        && let Some(hovered_ch) = response.inner
            && let Some(ptr_pos) = response.response.hover_pos() {
                let time_at_cursor = response.transform.value_from_position(ptr_pos).x;
                let tip = format_time_tooltip(hovered_ch, time_at_cursor);
                egui::containers::popup::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    egui::Id::new("waveform_hover_tooltip"),
                    |ui| {
                        ui.label(
                            egui::RichText::new(tip)
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_PRIMARY),
                        );
                    },
                );
            }

    // Voltage scale bar — small vertical reference on the bottom-right
    draw_scale_bar(ui, &response, amp_scale, ch_spacing);
}

/// Draw a voltage scale bar in the bottom-right corner of the plot.
/// The bar height corresponds to the amplitude scale setting (µV).
fn draw_scale_bar(
    ui: &egui::Ui,
    response: &egui_plot::PlotResponse<Option<usize>>,
    amp_scale_uv: f64,
    _ch_spacing: f64,
) {
    let painter = ui.painter();
    let plot_rect = response.response.rect;

    // The scale bar shows the amp_scale setting as a visual reference.
    // gain = DEFAULT_CHANNEL_SPACING * 3.0 * (1000 / amp_scale).
    // A normalized amplitude of amp_scale/1000 (the "unit" signal) maps to:
    //   (amp_scale/1000) * gain = DEFAULT_CHANNEL_SPACING * 3.0 Y-units.
    // So the bar representing amp_scale µV has height DEFAULT_CHANNEL_SPACING * 3.0.
    // That's the entire lane (too tall). Instead show amp_scale/3 µV (1/3 lane):
    let bar_y_units = DEFAULT_CHANNEL_SPACING;
    let bar_voltage_uv = amp_scale_uv / 3.0;

    // Convert bar height from plot Y-units to screen pixels using the transform
    let top_point = egui_plot::PlotPoint::new(0.0, 0.0);
    let bot_point = egui_plot::PlotPoint::new(0.0, -bar_y_units);
    let top_screen = response.transform.position_from_point(&top_point);
    let bot_screen = response.transform.position_from_point(&bot_point);
    let bar_height_px = (bot_screen.y - top_screen.y).abs();

    // Only draw if bar is tall enough to be visible (at least 8 px)
    if bar_height_px < 8.0 {
        return;
    }

    // Position: bottom-right corner with some margin
    let margin = 16.0;
    let bar_x = plot_rect.right() - margin;
    let bar_bottom = plot_rect.bottom() - margin - 12.0; // leave room for label
    let bar_top = bar_bottom - bar_height_px;

    // Draw the vertical bar with small horizontal ticks at top and bottom
    let bar_color = theme::TEXT_SECONDARY;
    let stroke = egui::Stroke::new(1.5, bar_color);
    let tick_w = 4.0;

    // Vertical line
    painter.line_segment(
        [egui::pos2(bar_x, bar_top), egui::pos2(bar_x, bar_bottom)],
        stroke,
    );
    // Top tick
    painter.line_segment(
        [egui::pos2(bar_x - tick_w, bar_top), egui::pos2(bar_x + tick_w, bar_top)],
        stroke,
    );
    // Bottom tick
    painter.line_segment(
        [egui::pos2(bar_x - tick_w, bar_bottom), egui::pos2(bar_x + tick_w, bar_bottom)],
        stroke,
    );

    // Label — format µV nicely (bar represents 1/3 of amp_scale)
    let label = if bar_voltage_uv >= 1000.0 {
        format!("{:.0} mV", bar_voltage_uv / 1000.0)
    } else {
        format!("{:.0} µV", bar_voltage_uv)
    };
    painter.text(
        egui::pos2(bar_x, bar_bottom + 4.0),
        egui::Align2::CENTER_TOP,
        label,
        egui::FontId::monospace(10.0),
        bar_color,
    );
}

fn format_time_tooltip(ch: usize, time_ms: f64) -> String {
    if time_ms.abs() >= 1000.0 {
        format!("CH{}  •  t = {:.3} s", ch, time_ms / 1000.0)
    } else {
        format!("CH{}  •  t = {:.2} ms", ch, time_ms)
    }
}

/// Linearly dim a color toward black by `factor` (0.0 = black, 1.0 = unchanged).
fn dim_color(c: egui::Color32, factor: f32) -> egui::Color32 {
    let r = (c.r() as f32 * factor) as u8;
    let g = (c.g() as f32 * factor) as u8;
    let b = (c.b() as f32 * factor) as u8;
    egui::Color32::from_rgb(r, g, b)
}

// ── Data collection ─────────────────────────────────────────────────

/// **Fast path** — always used for rendering (data is pre-filtered by app).
///
/// Min-max decimation: for each stride bucket, we keep the sample with the
/// minimum value and the sample with the maximum value, emitted in time
/// order.  This guarantees that short transients (spikes) are never lost
/// during zoom-out, matching the approach used by Intan RHX.
///
/// When stride == 1 (zoomed in enough), all samples pass through directly.
/// Spike detection (sigma + threshold crossings) is computed inline when enabled.
#[allow(clippy::too_many_arguments)]
fn collect_lines_fast(
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    settings: &DisplaySettings,
    filters: &FilterSettings,
    visible: usize,
    channel_count: usize,
    sample_rate: f64,
    t_left_ms: f64,
    t_right_ms: f64,
    gain: f64,
    ch_spacing: f64,
) -> Vec<ChannelTrace> {
    let ms_per_sample = if sample_rate > 0.0 {
        1000.0 / sample_rate
    } else {
        1.0
    };
    let window_samples = ((t_right_ms - t_left_ms) / ms_per_sample).ceil() as u64;
    // Target ~MAX_DISPLAY_POINTS output points; with min-max each bucket
    // emits 2 points, so we use half the budget for bucket count.
    let stride = (window_samples / (MAX_DISPLAY_POINTS as u64 / 2)).max(1);

    let mut traces: Vec<ChannelTrace> = Vec::with_capacity(visible);
    for ch in 0..visible {
        if !settings.is_channel_enabled(ch) {
            continue;
        }
        let mut pts: Vec<[f64; 2]> = Vec::with_capacity(MAX_DISPLAY_POINTS + 16);

        if stride <= 1 {
            // No decimation needed — emit every sample in the window
            for block in history.iter().chain(latest) {
                if block.channel_count != channel_count {
                    continue;
                }
                let spc = block.samples_per_channel;
                let block_start_ms = block.timestamp_start as f64 * ms_per_sample;
                let block_end_ms = block_start_ms + spc as f64 * ms_per_sample;
                if block_end_ms < t_left_ms || block_start_ms > t_right_ms {
                    continue;
                }
                for s in 0..spc {
                    let abs_idx = block.timestamp_start + s as u64;
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
                    pts.push([time_ms, value]);
                }
            }
        } else {
            // Min-max decimation: partition samples into stride-sized buckets,
            // keep the min and max sample from each bucket (in time order).
            let mut bucket_min_val: f64 = f64::MAX;
            let mut bucket_max_val: f64 = f64::MIN;
            let mut bucket_min_time: f64 = 0.0;
            let mut bucket_max_time: f64 = 0.0;
            let mut bucket_has_data = false;
            let mut last_bucket_id: u64 = u64::MAX;

            for block in history.iter().chain(latest) {
                if block.channel_count != channel_count {
                    continue;
                }
                let spc = block.samples_per_channel;
                let block_start_ms = block.timestamp_start as f64 * ms_per_sample;
                let block_end_ms = block_start_ms + spc as f64 * ms_per_sample;
                if block_end_ms < t_left_ms || block_start_ms > t_right_ms {
                    continue;
                }
                for s in 0..spc {
                    let abs_idx = block.timestamp_start + s as u64;
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

                    let bucket_id = abs_idx / stride;
                    if bucket_id != last_bucket_id {
                        // Flush previous bucket
                        if bucket_has_data {
                            emit_minmax(
                                &mut pts,
                                bucket_min_time, bucket_min_val,
                                bucket_max_time, bucket_max_val,
                            );
                        }
                        // Start new bucket
                        bucket_min_val = value;
                        bucket_max_val = value;
                        bucket_min_time = time_ms;
                        bucket_max_time = time_ms;
                        bucket_has_data = true;
                        last_bucket_id = bucket_id;
                    } else {
                        if value < bucket_min_val {
                            bucket_min_val = value;
                            bucket_min_time = time_ms;
                        }
                        if value > bucket_max_val {
                            bucket_max_val = value;
                            bucket_max_time = time_ms;
                        }
                    }
                }
            }
            // Flush last bucket
            if bucket_has_data {
                emit_minmax(
                    &mut pts,
                    bucket_min_time, bucket_min_val,
                    bucket_max_time, bucket_max_val,
                );
            }
        }

        // Spike detection on the raw (pre-finalize) points when enabled.
        // Using un-offset, un-gained values (normalized amplitude).
        let (sigma, spike_count) = if filters.spike_threshold_enabled && !pts.is_empty() {
            let mean = pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64;
            let var = pts.iter().map(|p| (p[1] - mean).powi(2)).sum::<f64>() / pts.len() as f64;
            let sig = var.sqrt();
            let thresh = -filters.spike_threshold_sigma * sig;
            let refractory = (sample_rate * 0.001).max(1.0) as usize;
            let mut count = 0u32;
            let mut last_cross: Option<usize> = None;
            let mut prev = 0.0;
            for (i, p) in pts.iter().enumerate() {
                let centered = p[1] - mean;
                if i > 0 && prev >= thresh && centered < thresh
                    && last_cross.is_none_or(|l| i - l >= refractory)
                {
                    count = count.saturating_add(1);
                    last_cross = Some(i);
                }
                prev = centered;
            }
            (Some(sig), count)
        } else {
            (None, 0)
        };

        finalize_channel(&mut pts, ch, gain, ch_spacing);
        traces.push(ChannelTrace {
            channel: ch,
            points: pts,
            sigma,
            spike_count,
        });
    }
    traces
}

/// Emit min and max points from a bucket in time order.  If min and max
/// occur at the same time (identical sample), emit only one point.
#[inline]
fn emit_minmax(
    pts: &mut Vec<[f64; 2]>,
    min_time: f64, min_val: f64,
    max_time: f64, max_val: f64,
) {
    if (min_time - max_time).abs() < 1e-9 {
        // Same sample is both min and max (flat bucket or single sample)
        pts.push([min_time, min_val]);
    } else if min_time < max_time {
        pts.push([min_time, min_val]);
        pts.push([max_time, max_val]);
    } else {
        pts.push([max_time, max_val]);
        pts.push([min_time, min_val]);
    }
}

/// DC-remove + apply gain + per-channel vertical offset.  Mutates in place.
fn finalize_channel(pts: &mut [[f64; 2]], ch: usize, gain: f64, ch_spacing: f64) {
    if pts.is_empty() {
        return;
    }
    let mean = pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64;
    let y_offset = -(ch as f64) * ch_spacing;
    for p in pts.iter_mut() {
        p[1] = (p[1] - mean) * gain + y_offset;
    }
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
