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

use crate::dsp::{Biquad, FilterChain, Q_BUTTERWORTH, Q_NOTCH};
use crate::panels::{DisplaySettings, FilterSettings};
use crate::theme;

/// Maximum rendered points per channel (decimation target for performance).
const MAX_DISPLAY_POINTS: usize = 4096;

/// Vertical spacing (in normalized units) between channel baselines.
const CHANNEL_SPACING: f64 = 2.2;

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

    // Gain maps normalized i16 data (±0.06 typical neural) to fill the channel lane.
    let gain = CHANNEL_SPACING * 3.0 * (1000.0 / amp_scale.max(1.0));

    // X-axis window driven by wall clock — smooth continuous scroll
    let x_right = elapsed_secs * 1000.0; // current time in ms
    let x_left = (x_right - time_window_ms).max(0.0);

    // Decide pipeline: fast path (raw decimated) or full path (filter/CAR).
    let needs_full_pipeline =
        filters.any_filter_enabled() || filters.car_enabled || filters.spike_threshold_enabled;

    let traces: Vec<ChannelTrace> = if needs_full_pipeline {
        collect_lines_filtered(
            history,
            latest,
            settings,
            filters,
            visible,
            total_channels,
            block.sample_rate,
            x_left,
            x_right,
            gain,
        )
    } else {
        collect_lines_fast(
            history,
            latest,
            settings,
            visible,
            total_channels,
            block.sample_rate,
            x_left,
            x_right,
            gain,
        )
    };

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

        // Determine which channel the cursor is hovering over (Y → channel)
        let hovered_ch: Option<usize> = plot_ui.pointer_coordinate().and_then(|pos| {
            let ch_idx = (-pos.y / CHANNEL_SPACING).round() as i64;
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
                    let y_off = -(trace.channel as f64) * CHANNEL_SPACING;
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
            let y_lane = -(trace.channel as f64) * CHANNEL_SPACING;
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

/// **Fast path** — used when no filter / CAR / spike-detection is enabled.
///
/// Per-channel anchored decimation: the same physical samples are picked
/// each frame, so the visible trace is rock-solid even as the viewport
/// slides.  Per-channel DC mean is removed at the end.
#[allow(clippy::too_many_arguments)]
fn collect_lines_fast(
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    settings: &DisplaySettings,
    visible: usize,
    channel_count: usize,
    sample_rate: f64,
    t_left_ms: f64,
    t_right_ms: f64,
    gain: f64,
) -> Vec<ChannelTrace> {
    let ms_per_sample = if sample_rate > 0.0 {
        1000.0 / sample_rate
    } else {
        1.0
    };
    let window_samples = ((t_right_ms - t_left_ms) / ms_per_sample).ceil() as u64;
    let stride = (window_samples / MAX_DISPLAY_POINTS as u64).max(1);

    let mut traces: Vec<ChannelTrace> = Vec::with_capacity(visible);
    for ch in 0..visible {
        if !settings.is_channel_enabled(ch) {
            continue;
        }
        let mut pts: Vec<[f64; 2]> = Vec::with_capacity(MAX_DISPLAY_POINTS + 16);
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
                if stride > 1 && !abs_idx.is_multiple_of(stride) {
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
                pts.push([time_ms, value]);
            }
        }
        finalize_channel(&mut pts, ch, gain);
        traces.push(ChannelTrace {
            channel: ch,
            points: pts,
            sigma: None,
            spike_count: 0,
        });
    }
    traces
}

/// **Full pipeline** — used when any HP/LP/Notch/CAR is enabled.
///
/// Collects every raw sample within the visible window for every channel
/// (no decimation yet), optionally subtracts the common-average reference
/// at each time index, runs each channel through its own biquad chain in
/// sample order to maintain phase coherence, then anchored-decimates the
/// filtered result for rendering.
#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
fn collect_lines_filtered(
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
) -> Vec<ChannelTrace> {
    let ms_per_sample = if sample_rate > 0.0 {
        1000.0 / sample_rate
    } else {
        1.0
    };
    let window_samples = ((t_right_ms - t_left_ms) / ms_per_sample).ceil() as u64;
    let stride = (window_samples / MAX_DISPLAY_POINTS as u64).max(1);

    // Build a flat per-channel buffer of (abs_idx, raw_value).  We allocate
    // once for the maximum window length to avoid reallocation churn.
    let cap = window_samples as usize + 1024;
    let mut times: Vec<u64> = Vec::with_capacity(cap);
    let mut buffers: Vec<Vec<f64>> = (0..visible).map(|_| Vec::with_capacity(cap)).collect();

    let mut times_initialized = false;
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
            times.push(abs_idx);
            for (slot, ch) in (0..visible).enumerate() {
                let data_idx = s * channel_count + ch;
                let v = if data_idx < block.data.len() {
                    block.data[data_idx] as f64 / i16::MAX as f64
                } else {
                    0.0
                };
                buffers[slot].push(v);
            }
        }
        times_initialized = true;
    }
    if !times_initialized || times.is_empty() {
        return Vec::new();
    }

    // CAR: subtract the mean of all visible-and-enabled channels at each
    // time index from every channel.  Common neuroscience practice for
    // removing common-mode noise.
    if filters.car_enabled {
        let n = times.len();
        for i in 0..n {
            let mut sum = 0.0;
            let mut count = 0;
            for ch in 0..visible {
                if !settings.is_channel_enabled(ch) {
                    continue;
                }
                sum += buffers[ch][i];
                count += 1;
            }
            if count > 0 {
                let mean = sum / count as f64;
                for ch in 0..visible {
                    buffers[ch][i] -= mean;
                }
            }
        }
    }

    // Apply per-channel filter chain in sample order.  Note: filter state
    // is fresh each frame, so the leftmost ~10 ms have a small transient.
    if filters.any_filter_enabled() {
        for ch in 0..visible {
            if !settings.is_channel_enabled(ch) {
                continue;
            }
            let mut chain = make_chain(filters, sample_rate);
            for v in &mut buffers[ch] {
                *v = chain.process(*v);
            }
        }
    }

    // Per-channel sigma + spike detection on full-resolution filtered data.
    // Using DC-removed RMS, then negative-going threshold crossings with a
    // ~1 ms refractory period (32 samples at 32 kHz / 30 at 30 kHz).
    let refractory_samples = (sample_rate * 0.001).max(1.0) as usize;
    let mut sigmas: Vec<Option<f64>> = vec![None; visible];
    let mut spike_counts: Vec<u32> = vec![0; visible];
    if filters.spike_threshold_enabled {
        for ch in 0..visible {
            if !settings.is_channel_enabled(ch) {
                continue;
            }
            let buf = &buffers[ch];
            if buf.is_empty() {
                continue;
            }
            // DC mean (we DC-remove for sigma estimation only — buf is left untouched here)
            let mean: f64 = buf.iter().sum::<f64>() / buf.len() as f64;
            let var: f64 =
                buf.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / buf.len() as f64;
            let sigma = var.sqrt();
            sigmas[ch] = Some(sigma);

            let thresh = -filters.spike_threshold_sigma * sigma;
            let mut last_crossing: Option<usize> = None;
            let mut prev_centered = 0.0;
            for (i, v) in buf.iter().enumerate() {
                let centered = v - mean;
                if i > 0 && prev_centered >= thresh && centered < thresh
                    && last_crossing.is_none_or(|l| i - l >= refractory_samples) {
                        spike_counts[ch] = spike_counts[ch].saturating_add(1);
                        last_crossing = Some(i);
                    }
                prev_centered = centered;
            }
        }
    }

    // Anchored decimation + DC removal + gain/offset, per channel.
    let mut traces: Vec<ChannelTrace> = Vec::with_capacity(visible);
    for ch in 0..visible {
        if !settings.is_channel_enabled(ch) {
            continue;
        }
        let mut pts: Vec<[f64; 2]> = Vec::with_capacity(MAX_DISPLAY_POINTS + 16);
        for (i, &abs_idx) in times.iter().enumerate() {
            if stride > 1 && abs_idx % stride != 0 {
                continue;
            }
            pts.push([abs_idx as f64 * ms_per_sample, buffers[ch][i]]);
        }
        finalize_channel(&mut pts, ch, gain);
        traces.push(ChannelTrace {
            channel: ch,
            points: pts,
            sigma: sigmas[ch],
            spike_count: spike_counts[ch],
        });
    }
    traces
}

/// DC-remove + apply gain + per-channel vertical offset.  Mutates in place.
fn finalize_channel(pts: &mut [[f64; 2]], ch: usize, gain: f64) {
    if pts.is_empty() {
        return;
    }
    let mean = pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64;
    let y_offset = -(ch as f64) * CHANNEL_SPACING;
    for p in pts.iter_mut() {
        p[1] = (p[1] - mean) * gain + y_offset;
    }
}

/// Build a fresh `FilterChain` from the user's settings.
fn make_chain(filters: &FilterSettings, sample_rate: f64) -> FilterChain {
    let mut c = FilterChain::passthrough();
    if filters.hp_enabled && filters.hp_cutoff_hz > 0.0 {
        c.hp = Biquad::highpass(filters.hp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
        c.hp_enabled = true;
    }
    if filters.lp_enabled && filters.lp_cutoff_hz < sample_rate / 2.0 {
        c.lp = Biquad::lowpass(filters.lp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
        c.lp_enabled = true;
    }
    if filters.notch_enabled {
        c.notch = Biquad::notch(filters.notch_freq_hz(), sample_rate, Q_NOTCH);
        c.notch_enabled = true;
    }
    c
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
