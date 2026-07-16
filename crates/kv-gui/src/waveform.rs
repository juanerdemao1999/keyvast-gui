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
//! - Zero-reference / grid lines are thin two-point egui_plot Lines (one per
//!   channel per frame — negligible next to trace tessellation).
//! - Each channel is a **density-graded min/max envelope** (Open Ephys style):
//!   `collect_channel_band` returns per-column min / max / mean, drawn in screen
//!   space as a triangle-strip mesh with a solid colour core around the mean
//!   fading to faint at the extremes. A thin trace stays flat/uniform; a wide
//!   timebase (many samples/pixel) shows structure instead of a solid block.
//!   (A screen-space mesh is used because egui_plot's `Polygon` fills via
//!   `convex_polygon`, which mis-fills a concave spiky band.)

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use kv_types::SampleBlock;

use crate::disp_ring::{ChannelBand, DisplayRing};
use crate::panels::{DisplaySettings, FilterSettings};
use crate::theme;

/// RHD amplifier full-scale range in µV. The 16-bit offset-binary ADC maps
/// ±32767 counts at 0.195 µV/count → ±6389.6 µV. Display normalization
/// divides by i16::MAX, so a normalized value of 1.0 corresponds to this
/// many microvolts.
const RHD_FULL_SCALE_UV: f64 = 32_767.0 * kv_rhd::RHD_AMPLIFIER_MICROVOLTS_PER_COUNT as f64;

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
    /// Finalized min/max envelope (plot coords), drawn as a filled triangle-strip
    /// mesh between the max and min lines in screen space after the plot renders.
    band: ChannelBand,
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
#[allow(clippy::too_many_arguments)] // cohesive per-frame render inputs; a struct would not aid clarity
pub fn draw_waveform_area(
    ui: &mut egui::Ui,
    ring: &DisplayRing,
    latest: Option<&SampleBlock>,
    start_ch: usize, // first physical channel to display (0 = no scroll offset)
    settings: &DisplaySettings,
    filters: &FilterSettings,
    sweep_left_ms: f64,
    empty_hint: &str, // source-aware subtitle shown when there is nothing to draw
) {
    let block = match latest {
        Some(b) => b,
        None => {
            draw_empty_state(ui, empty_hint);
            return;
        }
    };

    if !ring.ready {
        draw_empty_state(ui, empty_hint);
        return;
    }

    let total_channels = block.channel_count;
    // Clamp start_ch so we never go past the last channel.
    let start_ch = start_ch.min(total_channels.saturating_sub(1));
    let visible = settings
        .visible_channels
        .min(total_channels.saturating_sub(start_ch));
    if visible == 0 {
        draw_empty_state(ui, empty_hint);
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
    // The ring stores values normalized to [-1, 1] (i16 / 32767).  A norm value
    // of 1.0 corresponds to RHD_FULL_SCALE_UV microvolts.  amp_scale µV maps to
    // DEFAULT_CHANNEL_SPACING Y-units (≈ 0.73 of a default lane), so a signal
    // smaller than amp_scale sits well within its lane with black gaps between
    // channels — the amplitude headroom that keeps a busy display from becoming a
    // wall of solid colour (Open Ephys ties amplitude to the channel height the
    // same way; ±50% of range = the lane edge).
    let gain = DEFAULT_CHANNEL_SPACING * (RHD_FULL_SCALE_UV / amp_scale.max(1.0));

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
        ring, settings, filters, start_ch, visible, x_left, x_right, gain, ch_spacing,
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

    // Label stride: one tick per channel when the stack is short, thinning out
    // as more channels are shown so the axis never turns into a solid column of
    // text.  egui_plot's default spacer only lands on "nice" Y values and so
    // labels just CH0 / CH9 — this places a tick on every Nth lane center
    // instead, which both labels and draws a baseline grid line there.
    let label_stride: usize = match visible {
        0..=16 => 1,
        17..=32 => 2,
        _ => 4,
    };
    let y_grid_spacer = move |_input: egui_plot::GridInput| -> Vec<egui_plot::GridMark> {
        (0..ch_count_for_fmt)
            .step_by(label_stride)
            .map(|i| egui_plot::GridMark {
                value: -(i as f64) * spacing_for_fmt,
                step_size: spacing_for_fmt * label_stride as f64,
            })
            .collect()
    };

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
        .y_grid_spacer(y_grid_spacer)
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

    // Zero-reference lines are drawn inside the plot closure below (plot-space,
    // behind the traces) as thin two-point egui_plot Lines — one per channel.

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
                .width(0.8_f32)
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
                    Line::new(PlotPoints::from(vec![[x_left, y_off], [x_right, y_off]]))
                        .color(theme::GRID_ZERO_LINE)
                        .width(0.5_f32)
                        .name(""),
                );
            }
        }

        // Emphasize whole-second gridlines so the time axis has a clear sense
        // of scale.  Only when the window is wide enough (>= 2 s) that integer
        // seconds aren't packed together; brighter than the default grid.
        if settings.show_grid && (x_right - x_left) >= 2000.0 {
            let first_sec = (x_left / 1000.0).ceil() as i64;
            let last_sec = (x_right / 1000.0).floor() as i64;
            for sec in first_sec..=last_sec {
                let x = sec as f64 * 1000.0;
                plot_ui.line(
                    Line::new(PlotPoints::from(vec![[x, y_min], [x, y_max]]))
                        .color(egui::Color32::from_rgb(70, 70, 86))
                        .width(1.0_f32)
                        .name(""),
                );
            }
        }

        // Sweep cursor — vertical line at the latest data position.
        // Only drawn in Sweep mode (SpikeGLX-style "write cursor").
        // In Roll mode the right edge IS the latest data, no cursor needed.
        //
        // A short translucent "trail" behind the cursor (the just-written
        // region) plus a brighter cursor line make the wrap read as a moving
        // write-head instead of a momentary blank.
        if matches!(settings.display_mode, crate::panels::DisplayMode::Sweep)
            && cursor_ms > x_left
            && cursor_ms < x_right
        {
            let trail = (x_right - x_left) * 0.04;
            let trail_left = (cursor_ms - trail).max(x_left);
            plot_ui.polygon(
                egui_plot::Polygon::new(PlotPoints::from(vec![
                    [trail_left, y_min],
                    [cursor_ms, y_min],
                    [cursor_ms, y_max],
                    [trail_left, y_max],
                ]))
                .fill_color(egui::Color32::from_rgba_unmultiplied(200, 210, 230, 22))
                .stroke(egui::Stroke::NONE)
                .name(""),
            );
            plot_ui.line(
                Line::new(PlotPoints::from(vec![
                    [cursor_ms, y_min],
                    [cursor_ms, y_max],
                ]))
                .color(egui::Color32::from_rgba_unmultiplied(220, 225, 235, 200))
                .width(1.5_f32)
                .name(""),
            );
        }

        // Waveform traces are drawn AFTER the plot (in screen space) — egui_plot's
        // Polygon fills via convex_polygon, which mis-fills a concave min/max band
        // (deep spikes produce phantom triangles). See the mesh block below.
        hovered_ch
    });

    // Draw each channel as a min/max envelope with a **density-graded** fill: a
    // solid same-colour core of fixed pixel half-width `CORE_PX` centred on the
    // per-column mean (where the signal actually dwells), fading to near-
    // transparent at the min/max extremes. This is a mesh port of Open Ephys's
    // intensity ("supersampled") shading. The key property: when the band is
    // thinner than the core (a clean trace at a narrow timebase) the whole band
    // is inside the solid core → a flat uniform colour with NO visible gradient;
    // only when a wide timebase packs many samples per pixel and the envelope
    // grows tall does the bright-core/faint-edge structure appear, so a busy
    // signal reads as a trace with a soft envelope instead of a solid block.
    // A mean-line stroke keeps a continuous ≥1 px trace. Screen-space triangle
    // strip fills the concave band correctly; one mesh per channel, clipped.
    {
        const CORE_PX: f32 = 1.6; // solid-core half-height in pixels
        let transform = &response.transform;
        let painter = ui.painter_at(*transform.frame());
        let hovered_ch = response.inner;
        for trace in &traces {
            let band = &trace.band;
            let n = band.t.len();
            if n < 2 {
                continue;
            }
            let base_color = if settings.color_by_group {
                settings.channel_color(trace.channel)
            } else {
                theme::channel_color(trace.channel)
            };
            let is_hovered = hovered_ch == Some(trace.channel);
            let line_color = if settings.hover_highlight && is_hovered {
                egui::Color32::WHITE
            } else if settings.hover_highlight && hovered_ch.is_some() {
                dim_color(base_color, 0.35)
            } else {
                brighten_color(base_color, 1.3)
            };
            let (r, g, b) = (line_color.r(), line_color.g(), line_color.b());
            let core_a: u8 = if is_hovered { 235 } else { 200 };
            let core = egui::Color32::from_rgba_unmultiplied(r, g, b, core_a);
            let edge = egui::Color32::from_rgba_unmultiplied(r, g, b, 14);
            let mean_stroke = egui::Stroke::new(if is_hovered { 1.3_f32 } else { 1.0_f32 }, core);

            let pt =
                |t: f64, y: f64| transform.position_from_point(&egui_plot::PlotPoint::new(t, y));
            let mut mesh = egui::epaint::Mesh::default();
            let mut mean_pts: Vec<egui::Pos2> = Vec::with_capacity(n);
            for k in 0..n {
                // Screen y grows downward, so max is the smaller y (top).
                let top_y = pt(band.t[k], band.max[k]).y;
                let bot_y = pt(band.t[k], band.min[k]).y;
                let md = pt(band.t[k], band.mean[k]);
                let x = md.x;
                // Solid core clamped inside [top_y, bot_y]; collapses to the whole
                // band when the band is thinner than the core (→ flat fill).
                let ct = (md.y - CORE_PX).max(top_y);
                let cb = (md.y + CORE_PX).min(bot_y);
                mesh.colored_vertex(egui::pos2(x, top_y), edge);
                mesh.colored_vertex(egui::pos2(x, ct), core);
                mesh.colored_vertex(egui::pos2(x, cb), core);
                mesh.colored_vertex(egui::pos2(x, bot_y), edge);
                mean_pts.push(md);
                if k > 0 {
                    let a = (4 * (k - 1)) as u32;
                    let b = (4 * k) as u32;
                    // top→core-top (faint→solid)
                    mesh.add_triangle(a, a + 1, b);
                    mesh.add_triangle(a + 1, b, b + 1);
                    // solid core
                    mesh.add_triangle(a + 1, a + 2, b + 1);
                    mesh.add_triangle(a + 2, b + 1, b + 2);
                    // core-bottom→bot (solid→faint)
                    mesh.add_triangle(a + 2, a + 3, b + 2);
                    mesh.add_triangle(a + 3, b + 2, b + 3);
                }
            }
            painter.add(egui::Shape::mesh(mesh));
            // Crisp trace along the mean — continuous ≥1 px even where the band is
            // sub-pixel thin, and marks where the signal spends its time.
            painter.add(egui::Shape::line(mean_pts, mean_stroke));
        }
    }

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

    // Colored lane tabs (#4): a small chip in each channel's trace color at
    // the left edge of its lane, tying the grey "CHn" axis label to its
    // colored waveform.  Only on labeled lanes so it tracks the visible ticks.
    {
        let painter = ui.painter();
        let frame = *response.transform.frame();
        for disp_pos in (0..visible).step_by(label_stride) {
            let phys_ch = start_ch + disp_pos;
            if !settings.is_channel_enabled(phys_ch) {
                continue;
            }
            let base = if settings.color_by_group {
                settings.channel_color(phys_ch)
            } else {
                theme::channel_color(phys_ch)
            };
            let y_lane = -(disp_pos as f64) * ch_spacing;
            let screen = response
                .transform
                .position_from_point(&egui_plot::PlotPoint::new(x_left, y_lane));
            let chip = egui::Rect::from_min_size(
                egui::pos2(frame.left() + 2.0, screen.y - 3.0),
                egui::vec2(5.0, 6.0),
            );
            painter.rect_filled(chip, egui::CornerRadius::same(1), brighten_color(base, 1.3));
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
        // finalize applies: y_plot = value_norm * gain + y_offset, gain =
        // DEFAULT_CHANNEL_SPACING * FULL_SCALE / amp_scale, so
        // µV = delta_y * amp_scale / DEFAULT_CHANNEL_SPACING.
        // hovered_ch is the physical channel; display_pos = physical_ch - start_ch.
        let disp_pos = hovered_ch.saturating_sub(start_ch);
        let y_baseline = -(disp_pos as f64) * ch_spacing;
        let delta_y = plot_val.y - y_baseline;
        let amp_uv = delta_y * amp_scale / DEFAULT_CHANNEL_SPACING;
        let amp_str = if amp_uv.abs() >= 1000.0 {
            format!("{:+.2} mV", amp_uv / 1000.0)
        } else {
            format!("{:+.1} µV", amp_uv)
        };

        let tip = format!(
            "{}  {}",
            format_time_tooltip(hovered_ch, time_at_cursor),
            amp_str
        );
        let plot_rect = response.response.rect;
        let painter = ui.painter();

        // Thin crosshair at the cursor (#8) — pairs with the readout pill so
        // the exact sample being inspected is unambiguous.
        let cross = egui::Stroke::new(
            0.75_f32,
            egui::Color32::from_rgba_unmultiplied(200, 200, 215, 90),
        );
        painter.line_segment(
            [
                egui::pos2(ptr_pos.x, plot_rect.top()),
                egui::pos2(ptr_pos.x, plot_rect.bottom()),
            ],
            cross,
        );
        painter.line_segment(
            [
                egui::pos2(plot_rect.left(), ptr_pos.y),
                egui::pos2(plot_rect.right(), ptr_pos.y),
            ],
            cross,
        );

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

    // gain maps amp_scale µV to DEFAULT_CHANNEL_SPACING Y-units, so a bar that
    // tall reads exactly amp_scale µV.
    let bar_y_units = DEFAULT_CHANNEL_SPACING;
    let bar_voltage_uv = amp_scale_uv;

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
    let bar_color = theme::TEXT_PRIMARY;
    let stroke = egui::Stroke::new(1.5_f32, bar_color);
    let tick_w = 4.0;

    // Dark backing panel so the bar + label stay readable over busy waveforms.
    let backing = egui::Rect::from_min_max(
        egui::pos2(bar_x - 58.0, bar_top - 4.0),
        egui::pos2(plot_rect.right() - 4.0, bar_bottom + 18.0),
    );
    painter.rect_filled(
        backing,
        egui::CornerRadius::same(3),
        egui::Color32::from_rgba_premultiplied(18, 18, 24, 190),
    );

    // Vertical line
    painter.line_segment(
        [egui::pos2(bar_x, bar_top), egui::pos2(bar_x, bar_bottom)],
        stroke,
    );
    // Top tick
    painter.line_segment(
        [
            egui::pos2(bar_x - tick_w, bar_top),
            egui::pos2(bar_x + tick_w, bar_top),
        ],
        stroke,
    );
    // Bottom tick
    painter.line_segment(
        [
            egui::pos2(bar_x - tick_w, bar_bottom),
            egui::pos2(bar_x + tick_w, bar_bottom),
        ],
        stroke,
    );

    // Label — format µV nicely (bar represents 1/3 of amp_scale)
    let label = if bar_voltage_uv >= 1000.0 {
        format!("{:.0} mV", bar_voltage_uv / 1000.0)
    } else {
        format!("{:.0} µV", bar_voltage_uv)
    };
    // Right-align the label to the bar's right tick so it grows leftward and
    // never clips against the plot's right edge (previously centered on bar_x,
    // which cut off the "µV"/"mV" suffix).
    painter.text(
        egui::pos2(bar_x + tick_w, bar_bottom + 4.0),
        egui::Align2::RIGHT_TOP,
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
    let window_ring_entries = ((t_right_ms - t_left_ms) / ms_per_ring).ceil() as usize + 1;

    let mut traces: Vec<ChannelTrace> = Vec::with_capacity(visible);

    for disp_pos in 0..visible {
        let phys_ch = start_ch + disp_pos;

        // Per-channel enable/disable is indexed by physical channel.
        if !settings.is_channel_enabled(phys_ch) {
            continue;
        }

        // Open-Ephys / Intan-RHX style min/max envelope for this column set.
        let mut band = ring.collect_channel_band(
            phys_ch,
            t_left_ms,
            t_right_ms,
            MAX_DISPLAY_POINTS,
            window_ring_entries,
        );
        // Spike detection on the un-offset min (most-negative) envelope.
        let (sigma, spike_count) = detect_neg_spikes_band(&band, filters);
        // DC-remove + gain + display-position offset applied to the envelope.
        finalize_band(&mut band, disp_pos, gain, ch_spacing);
        traces.push(ChannelTrace {
            channel: phys_ch,
            display_pos: disp_pos,
            band,
            sigma,
            spike_count,
        });
    }
    traces
}

/// DC-remove + apply gain + per-channel vertical offset to the min/max/mean band.
fn finalize_band(band: &mut ChannelBand, disp_pos: usize, gain: f64, ch_spacing: f64) {
    let n = band.min.len();
    if n == 0 {
        return;
    }
    // DC baseline: mean of the column midlines, so the band sits centred on its lane.
    let baseline = band
        .min
        .iter()
        .zip(&band.max)
        .map(|(lo, hi)| (lo + hi) * 0.5)
        .sum::<f64>()
        / n as f64;
    let y_off = -(disp_pos as f64) * ch_spacing;
    // Allow a large transient to bleed up to ~2 lanes (Open Ephys' default
    // overlap factor) before clipping, instead of hard-flattening at the lane
    // edge — flattening turns a saturated signal into a featureless block.
    let clip = ch_spacing * 2.0;
    let (lo, hi) = (y_off - clip, y_off + clip);
    for k in 0..n {
        band.min[k] = ((band.min[k] - baseline) * gain + y_off).clamp(lo, hi);
        band.max[k] = ((band.max[k] - baseline) * gain + y_off).clamp(lo, hi);
        band.mean[k] = ((band.mean[k] - baseline) * gain + y_off).clamp(lo, hi);
    }
}

/// Spike detection on the min (most-negative) envelope of a band. Builds the
/// `(time, value)` series the shared detector expects (only when enabled, so the
/// extra allocation is off the default path).
fn detect_neg_spikes_band(band: &ChannelBand, filters: &FilterSettings) -> (Option<f64>, u32) {
    if !filters.spike_threshold_enabled || band.is_empty() {
        return (None, 0);
    }
    let pts: Vec<[f64; 2]> = band
        .t
        .iter()
        .zip(&band.min)
        .map(|(&t, &v)| [t, v])
        .collect();
    detect_neg_spikes(&pts, filters)
}

/// Real-time refractory window (ms) between counted negative crossings.
const SPIKE_REFRACTORY_MS: f64 = 1.0;

/// Count negative-going threshold crossings (below `mean - N*sigma`) in a series
/// of `(time_ms, value)` points, with a 1 ms **time-based** refractory window.
/// Returns `(sigma, count)`; `sigma` is `None` when detection is disabled or the
/// series is empty.
///
/// Two properties make this robust to the display pipeline:
/// - The noise scale is the median absolute deviation (MAD/0.6745, Quiroga 2004)
///   rather than the standard deviation, so large spikes — and the peak-hold
///   magnitude bias of the decimated points — do not inflate the threshold the
///   way variance does.
/// - The refractory is expressed in real time via each point's timestamp, so it
///   stays ~1 ms regardless of the display window width / render decimation
///   (the old index-based refractory grew with the time window).
///
/// The count is still a coarse per-window indicator: peak-hold render
/// decimation merges spikes closer than one render bucket, so very high firing
/// rates at wide windows read low. It is a display aid, not a spike sorter.
fn detect_neg_spikes(pts: &[[f64; 2]], filters: &FilterSettings) -> (Option<f64>, u32) {
    if !filters.spike_threshold_enabled || pts.is_empty() {
        return (None, 0);
    }
    let n = pts.len();
    let mean = pts.iter().map(|p| p[1]).sum::<f64>() / n as f64;

    // Robust noise scale: MAD around the median, scaled to an equivalent sigma.
    let mut vals: Vec<f64> = pts.iter().map(|p| p[1]).collect();
    let med = median(&mut vals);
    for v in vals.iter_mut() {
        *v = (*v - med).abs();
    }
    let mad = median(&mut vals);
    // mad ≥ 0 (median of magnitudes) and var.sqrt() ≥ 0, so `<= 0.0` (not a
    // negated `>`) cleanly catches the degenerate/flat cases.
    let mut sigma = mad / 0.6745;
    if sigma <= 0.0 {
        // Degenerate window (flat/constant) — fall back to the std deviation.
        let var = pts.iter().map(|p| (p[1] - mean).powi(2)).sum::<f64>() / n as f64;
        sigma = var.sqrt();
    }
    if sigma <= 0.0 {
        return (Some(0.0), 0);
    }

    // Threshold in mean-centered coordinates so it aligns with the mean-centered
    // trace drawn by finalize_channel and the drawn threshold line.
    let thresh = -filters.spike_threshold_sigma * sigma;
    let mut count = 0u32;
    let mut last_cross_t: Option<f64> = None;
    let mut prev = pts[0][1] - mean;
    for p in pts.iter().skip(1) {
        let t = p[0];
        let centered = p[1] - mean;
        if prev >= thresh
            && centered < thresh
            && last_cross_t.is_none_or(|lt| t - lt >= SPIKE_REFRACTORY_MS)
        {
            count = count.saturating_add(1);
            last_cross_t = Some(t);
        }
        prev = centered;
    }
    (Some(sigma), count)
}

/// Median of `vals`, computed in place (partial-sorts the slice). Returns 0.0
/// for an empty slice.
fn median(vals: &mut [f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = vals.len() / 2;
    if vals.len() % 2 == 1 {
        vals[mid]
    } else {
        (vals[mid - 1] + vals[mid]) / 2.0
    }
}

// ── Empty state ─────────────────────────────────────────────────────

fn draw_empty_state(ui: &mut egui::Ui, hint: &str) {
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
        hint,
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::FilterSettings;

    fn spike_filters(sigma: f64) -> FilterSettings {
        FilterSettings {
            spike_threshold_enabled: true,
            spike_threshold_sigma: sigma,
            ..FilterSettings::default()
        }
    }

    /// Baseline of tiny alternating noise at `dt_ms` spacing (in `(t_ms, value)`
    /// points), with deep negative spikes injected at the given times (ms).
    fn series(dt_ms: f64, n: usize, spikes_ms: &[f64]) -> Vec<[f64; 2]> {
        let mut pts: Vec<[f64; 2]> = (0..n)
            .map(|i| {
                let v = if i % 2 == 0 { 0.005 } else { -0.005 };
                [i as f64 * dt_ms, v]
            })
            .collect();
        for &st in spikes_ms {
            let idx = (st / dt_ms).round() as usize;
            if idx < pts.len() {
                pts[idx][1] = -1.0;
            }
        }
        pts
    }

    #[test]
    fn refractory_suppresses_crossings_within_1ms() {
        // Two spikes 0.5 ms apart → counted once (time-based refractory).
        let pts = series(0.1, 60, &[2.0, 2.5]);
        let (_, count) = detect_neg_spikes(&pts, &spike_filters(4.0));
        assert_eq!(count, 1);
    }

    #[test]
    fn refractory_allows_crossings_beyond_1ms() {
        // Two spikes 2 ms apart → counted twice.
        let pts = series(0.1, 60, &[2.0, 4.0]);
        let (_, count) = detect_neg_spikes(&pts, &spike_filters(4.0));
        assert_eq!(count, 2);
    }

    #[test]
    fn count_is_independent_of_point_spacing() {
        // The same two well-separated spikes at two different point spacings
        // (narrow vs wide display window) must yield the same count — the old
        // index-based refractory made this window-dependent.
        let narrow = series(0.1, 60, &[2.0, 4.0]);
        let wide = series(0.5, 60, &[10.0, 20.0]);
        let (_, c_narrow) = detect_neg_spikes(&narrow, &spike_filters(4.0));
        let (_, c_wide) = detect_neg_spikes(&wide, &spike_filters(4.0));
        assert_eq!(c_narrow, 2);
        assert_eq!(c_wide, 2);
    }

    #[test]
    fn disabled_detection_returns_none() {
        let pts = series(0.1, 60, &[2.0]);
        let (sigma, count) = detect_neg_spikes(&pts, &FilterSettings::default());
        assert!(sigma.is_none());
        assert_eq!(count, 0);
    }
}
