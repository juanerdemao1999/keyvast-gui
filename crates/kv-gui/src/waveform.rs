//! Professional multi-channel waveform display.
//!
//! Renders all visible channels as vertically-stacked traces in a **single**
//! `egui_plot::Plot` widget — matching the approach used by Intan RHX,
//! Open Ephys, and other professional electrophysiology acquisition software.
//!
//! Each channel is offset vertically so traces form a waterfall display.
//! The X axis auto-scrolls to always show the most recent data window.
//! Grid lines, zero-reference lines, and per-channel coloring are supported.
//!
//! ## Performance architecture (SpikeGLX-inspired)
//! - Data comes from `DisplayRing`: pre-decimated at ingestion time.
//! - Render reads O(output_points) with no block-history iteration.
//! - `show_x` / `show_y` disabled to skip egui_plot's O(N) hover search.
//! - Zero-reference lines drawn via painter (no extra Line object allocations).
//! - `.points` moved (not cloned) into Line to eliminate per-frame alloc.

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use kv_types::SampleBlock;

use crate::disp_ring::DisplayRing;
use crate::panels::{DisplaySettings, FilterSettings};
use crate::theme;

/// RHD amplifier full-scale range in µV. The 16-bit offset-binary ADC maps
/// ±32767 counts at 0.195 µV/count → ±6389.6 µV. Display normalization
/// divides by i16::MAX, so a normalized value of 1.0 corresponds to this
/// many microvolts.
const RHD_FULL_SCALE_UV: f64 = 32_767.0 * 0.195;

/// Maximum rendered points per channel.
/// SpikeGLX uses ~2× screen width; for a 1920-wide display that is ~3840.
/// We use a conservative budget to keep egui_plot tessellation cheap.
const MAX_DISPLAY_POINTS: usize = 2000;

/// Default vertical spacing (in normalized units) between channel baselines.
pub const DEFAULT_CHANNEL_SPACING: f64 = 2.2;

/// Per-channel rendered trace plus optional spike detection metadata.
struct ChannelTrace {
    /// Physical channel index (for colour, label, hover matching).
    channel: usize,
    /// Slot position in the display stack (0 = top lane, 1 = next, …).
    /// Used for Y-axis offset; independent of the physical channel number.
    display_pos: usize,
    points: Vec<[f64; 2]>,
    /// RMS sigma in normalized-input units (only set when threshold is enabled).
    sigma: Option<f64>,
    /// Negative-going threshold crossings within the window.
    spike_count: u32,
}

/// Spike metadata retained after `points` is moved into a `Line`.
struct SpikeMeta {
    /// Physical channel index (used for badge label; dead_code until spike overlay is wired up).
    #[allow(dead_code)]
    channel: usize,
    /// Display-stack position (same as ChannelTrace::display_pos).
    display_pos: usize,
    #[allow(dead_code)]
    sigma: f64,
    spike_count: u32,
    thresh_y: f64,
}

// ── Public entry point ──────────────────────────────────────────────

/// Draw the full waveform area — one large Plot with all channels stacked.
///
/// `sweep_left_ms` is the left edge of the current sweep window (ms since
/// acquisition start).  The right edge is `sweep_left_ms + time_window_ms`.
///
/// In sweep mode these bounds are **fixed** for the duration of one sweep —
/// the display is stationary and a cursor sweeps from left to right.  This
/// is the display model used by SpikeGLX and Intan RHX, and eliminates the
/// "twitching" caused by continuously shifting plot bounds.
///
/// Data is read from `ring` (pre-computed at ingestion time) — no per-frame
/// block history iteration.
pub fn draw_waveform_area(
    ui: &mut egui::Ui,
    ring: &DisplayRing,
    latest: Option<&SampleBlock>,
    start_ch: usize, // first physical channel to display (0 = no scroll offset)
    settings: &DisplaySettings,
    filters: &FilterSettings,
    sweep_left_ms: f64,
) {
    let block = match latest {
        Some(b) => b,
        None => {
            draw_empty_state(ui);
            return;
        }
    };

    if !ring.ready {
        draw_empty_state(ui);
        return;
    }

    let total_channels = block.channel_count;
    // Clamp start_ch so we never go past the last channel.
    let start_ch = start_ch.min(total_channels.saturating_sub(1));
    let visible = settings.visible_channels.min(total_channels.saturating_sub(start_ch));
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
    //
    // The ring stores values normalized to [-1, 1] (i16 / 32767).  A norm
    // value of 1.0 corresponds to RHD_FULL_SCALE_UV microvolts.  We want
    // amp_scale µV to fill one lane (= 3 * DEFAULT_CHANNEL_SPACING Y-units).
    let gain = DEFAULT_CHANNEL_SPACING * 3.0 * (RHD_FULL_SCALE_UV / amp_scale.max(1.0));

    // Sweep-mode window: FIXED bounds for the duration of this sweep.
    // The cursor (ring.latest_time_ms) sweeps from x_left to x_right.
    // When it overflows, app.rs advances sweep_left_ms and the display resets.
    let x_left = sweep_left_ms;
    let x_right = x_left + time_window_ms;

    // Latest ring data time — used to draw the sweep cursor line
    let cursor_ms = ring.latest_time_ms();

    // Collect display data from the pre-computed ring buffer.
    // Data is already filtered (ring is fed from the filtered pipeline).
    let traces = collect_from_ring(
        ring, settings, filters, start_ch, visible,
        block.sample_rate, x_left, x_right, gain, ch_spacing,
    );

    // Y axis bounds
    let y_min = -(visible as f64) * ch_spacing + ch_spacing * 0.5;
    let y_max = ch_spacing * 0.5;

    // Channel label formatter for Y-axis.
    // Y = -(display_pos * ch_spacing), so display_pos = round(-Y / ch_spacing).
    // Physical channel = start_ch + display_pos.
    let ch_count_for_fmt = visible;
    let spacing_for_fmt = ch_spacing;
    let start_ch_for_fmt = start_ch;
    let y_formatter = move |mark: egui_plot::GridMark, _range: &std::ops::RangeInclusive<f64>| {
        let pos = -mark.value / spacing_for_fmt;
        let disp_pos = pos.round();
        // Only label grid marks that sit on a lane center. egui_plot also emits
        // minor marks between lanes; without this guard they round to the same
        // channel and every label prints twice (CH0 CH0 CH1 CH1 ...).
        if (pos - disp_pos).abs() > 0.1 {
            return String::new();
        }
        let disp_pos = disp_pos as i64;
        if disp_pos >= 0 && (disp_pos as usize) < ch_count_for_fmt {
            format!("CH{}", start_ch_for_fmt + disp_pos as usize)
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

    // Draw the combined plot — explicit bounds, no auto-fit (prevents Y-axis jitter).
    //
    // show_x(false) / show_y(false): disables egui_plot's built-in coordinate
    // readout which triggers an O(total_points) nearest-item search every hover
    // frame.  We implement our own hover tooltip below.
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
        .show_x(false)
        .show_y(false)
        .x_axis_formatter(x_formatter)
        .y_axis_formatter(y_formatter)
        .set_margin_fraction(egui::vec2(0.0, 0.01));

    // Extract spike metadata BEFORE consuming traces (points will be moved into Lines).
    // Y position is keyed on display_pos (not physical channel).
    let spike_metas: Vec<SpikeMeta> = if filters.spike_threshold_enabled {
        traces
            .iter()
            .filter_map(|t| {
                t.sigma.map(|sigma| {
                    let y_off = -(t.display_pos as f64) * ch_spacing;
                    SpikeMeta {
                        channel: t.channel,
                        display_pos: t.display_pos,
                        sigma,
                        spike_count: t.spike_count,
                        thresh_y: -filters.spike_threshold_sigma * sigma * gain + y_off,
                    }
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // Draw zero-reference lines using the painter (screen-space) to avoid
    // creating a Line object + Vec allocation per channel.
    // These are drawn BEFORE the plot so they appear behind traces.

    let response = plot.show(ui, |plot_ui| {
        // Lock to exact bounds — X from wall clock, Y from channel layout
        plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
            [x_left, y_min],
            [x_right, y_max],
        ));

        // Determine which channel the cursor is hovering over.
        // Returns the PHYSICAL channel index for consistent comparison with trace.channel.
        let hovered_ch: Option<usize> = plot_ui.pointer_coordinate().and_then(|pos| {
            let disp_pos = (-pos.y / ch_spacing).round() as i64;
            if disp_pos >= 0 && (disp_pos as usize) < visible {
                Some(start_ch + disp_pos as usize)
            } else {
                None
            }
        });

        // Draw spike threshold lines (negative-going) when enabled
        if filters.spike_threshold_enabled {
            for meta in &spike_metas {
                let line = Line::new(PlotPoints::from(vec![
                    [x_left, meta.thresh_y],
                    [x_right, meta.thresh_y],
                ]))
                .color(theme::ACCENT_RED)
                .width(0.8)
                .style(egui_plot::LineStyle::dashed_dense());
                plot_ui.line(line);
            }
        }

        // Draw zero-reference lines as egui_plot Lines (needed for correct
        // plot-coordinate rendering behind traces).
        // Use display_pos for Y-offset; check physical channel for enable/disable.
        if settings.show_grid {
            for disp_pos in 0..visible {
                if !settings.is_channel_enabled(start_ch + disp_pos) {
                    continue;
                }
                let y_off = -(disp_pos as f64) * ch_spacing;
                plot_ui.line(
                    Line::new(PlotPoints::from(vec![
                        [x_left, y_off],
                        [x_right, y_off],
                    ]))
                    .color(theme::GRID_ZERO_LINE)
                    .width(0.5)
                    .name(""),
                );
            }
        }

        // Sweep cursor — vertical line at the latest data position.
        // This is the SpikeGLX-style "write cursor" that sweeps right.
        if cursor_ms > x_left && cursor_ms < x_right {
            plot_ui.line(
                Line::new(PlotPoints::from(vec![
                    [cursor_ms, y_min],
                    [cursor_ms, y_max],
                ]))
                .color(egui::Color32::from_rgba_unmultiplied(180, 180, 180, 80))
                .width(1.0)
                .name(""),
            );
        }

        // Draw waveform traces — highlight hovered channel.
        // Points are MOVED (not cloned) into Line to avoid per-channel alloc.
        for trace in traces {
            let base_color = theme::channel_color(trace.channel);
            let is_hovered = hovered_ch == Some(trace.channel);
            // Default: all channels always at full brightness (1.3× base color).
            // Hover highlight mode (settings.hover_highlight) must be ON for
            // dimming to activate — matches professional tool conventions where
            // the waveform display is always fully lit until user requests focus.
            let (color, width) = if settings.hover_highlight && is_hovered {
                (egui::Color32::WHITE, 2.0)
            } else if settings.hover_highlight && hovered_ch.is_some() {
                (dim_color(base_color, 0.35), 1.0)
            } else {
                (brighten_color(base_color, 1.3), 1.5)
            };
            plot_ui.line(
                Line::new(PlotPoints::from(trace.points))
                    .color(color)
                    .width(width)
                    .name(""),
            );
        }

        hovered_ch
    });

    // Spike-count badges on the right edge of each lane (overlay painted in screen space).
    // Y position uses display_pos, not physical channel.
    if filters.spike_threshold_enabled {
        let painter = ui.painter();
        for meta in &spike_metas {
            if meta.spike_count == 0 {
                continue;
            }
            let y_lane = -(meta.display_pos as f64) * ch_spacing;
            let plot_pos = egui_plot::PlotPoint::new(x_right, y_lane);
            let screen_pos = response.transform.position_from_point(&plot_pos);
            let badge_pos = screen_pos + egui::vec2(-6.0, -1.0);
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_CENTER,
                format!("{}", meta.spike_count),
                egui::FontId::monospace(10.0),
                theme::ACCENT_RED,
            );
        }
    }

    // Hover info overlay — drawn in the top-left corner of the plot so it
    // never covers the waveform under the cursor.
    if response.response.hovered()
        && let Some(hovered_ch) = response.inner
            && let Some(ptr_pos) = response.response.hover_pos()
    {
        let plot_val = response.transform.value_from_position(ptr_pos);
        let time_at_cursor = plot_val.x;

        // Reverse the gain/offset transform to recover amplitude in µV.
        // finalize_channel applies: y_plot = value_norm * gain + y_offset
        // where y_offset = -(ch * ch_spacing).
        // Scale bar definition: DEFAULT_CHANNEL_SPACING Y-units = amp_scale/3 µV
        // → 1 Y-unit = amp_scale / (3 * DEFAULT_CHANNEL_SPACING) µV
        // hovered_ch is the physical channel; display_pos = physical_ch - start_ch.
        let disp_pos = hovered_ch.saturating_sub(start_ch);
        let y_baseline = -(disp_pos as f64) * ch_spacing;
        let delta_y = plot_val.y - y_baseline;
        let amp_uv = delta_y * RHD_FULL_SCALE_UV / (3.0 * DEFAULT_CHANNEL_SPACING);
        let amp_str = if amp_uv.abs() >= 1000.0 {
            format!("{:+.2} mV", amp_uv / 1000.0)
        } else {
            format!("{:+.1} µV", amp_uv)
        };

        let tip = format!("{}  {}",
            format_time_tooltip(hovered_ch, time_at_cursor),
            amp_str);
        let plot_rect = response.response.rect;
        let painter = ui.painter();
        let text_pos = plot_rect.left_top() + egui::vec2(10.0, 8.0);
        // Background pill
        let font = egui::FontId::monospace(11.0);
        let galley = painter.layout_no_wrap(tip.clone(), font.clone(), theme::TEXT_PRIMARY);
        let bg_rect = egui::Rect::from_min_size(
            text_pos - egui::vec2(4.0, 2.0),
            galley.size() + egui::vec2(8.0, 4.0),
        );
        painter.rect_filled(
            bg_rect,
            egui::CornerRadius::same(3),
            egui::Color32::from_rgba_premultiplied(18, 18, 24, 210),
        );
        painter.text(
            text_pos,
            egui::Align2::LEFT_TOP,
            tip,
            font,
            theme::TEXT_PRIMARY,
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
    // gain maps amp_scale µV to one full lane (3 * DEFAULT_CHANNEL_SPACING Y-units).
    // Show 1/3 of the lane = amp_scale/3 µV:
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

/// Brighten a color by `factor` (1.0 = unchanged, >1.0 = brighter), clamped to 255.
fn brighten_color(c: egui::Color32, factor: f32) -> egui::Color32 {
    let r = (c.r() as f32 * factor).min(255.0) as u8;
    let g = (c.g() as f32 * factor).min(255.0) as u8;
    let b = (c.b() as f32 * factor).min(255.0) as u8;
    egui::Color32::from_rgb(r, g, b)
}

// ── Data collection ─────────────────────────────────────────────────

/// Collect display traces from the pre-computed `DisplayRing`.
///
/// **O(output_points) per channel** — no block-history iteration, no binary
/// search, no per-sample timestamp comparison.  The ring stores pre-decimated
/// f32 values already filtered by the incremental pipeline.
///
/// Secondary stride is fixed for the full sweep window so the decimation
/// level stays constant as data fills in during a sweep.
///
/// Spike detection (sigma + threshold crossings) runs inline when enabled.
#[allow(clippy::too_many_arguments)]
fn collect_from_ring(
    ring: &DisplayRing,
    settings: &DisplaySettings,
    filters: &FilterSettings,
    start_ch: usize,
    visible: usize,
    sample_rate: f64,
    t_left_ms: f64,
    t_right_ms: f64,
    gain: f64,
    ch_spacing: f64,
) -> Vec<ChannelTrace> {
    // Pre-compute the full-window ring-entry count for stable stride2.
    // stride2 must be based on the *intended* window width, not the currently
    // filled portion, otherwise stride2 grows during a sweep and early data
    // appears progressively coarser (the "stretch/zoom" visual artifact).
    let ms_per_ring = ring.dwnsp as f64 * 1000.0 / ring.sample_rate.max(1.0);
    let window_ring_entries =
        ((t_right_ms - t_left_ms) / ms_per_ring).ceil() as usize + 1;

    let mut traces: Vec<ChannelTrace> = Vec::with_capacity(visible);

    for disp_pos in 0..visible {
        let phys_ch = start_ch + disp_pos;

        // Per-channel enable/disable is indexed by physical channel.
        if !settings.is_channel_enabled(phys_ch) {
            continue;
        }

        // Read display-ready points from the ring using the PHYSICAL channel index.
        let mut pts = ring.collect_channel(
            phys_ch, t_left_ms, t_right_ms, MAX_DISPLAY_POINTS, window_ring_entries,
        );

        // Spike detection on the pre-finalize (un-offset, un-gained) values.
        let (sigma, spike_count) = if filters.spike_threshold_enabled && !pts.is_empty() {
            let mean = pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64;
            let var =
                pts.iter().map(|p| (p[1] - mean).powi(2)).sum::<f64>() / pts.len() as f64;
            let sig = var.sqrt();
            let thresh = -filters.spike_threshold_sigma * sig;
            let refractory = (sample_rate * 0.001).max(1.0) as usize;
            let mut count = 0u32;
            let mut last_cross: Option<usize> = None;
            let mut prev = 0.0_f64;
            for (i, p) in pts.iter().enumerate() {
                let centered = p[1] - mean;
                if i > 0
                    && prev >= thresh
                    && centered < thresh
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

        // finalize_channel applies Y-offset using DISPLAY position (not physical channel).
        finalize_channel(&mut pts, disp_pos, gain, ch_spacing);
        traces.push(ChannelTrace {
            channel: phys_ch,
            display_pos: disp_pos,
            points: pts,
            sigma,
            spike_count,
        });
    }
    traces
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
