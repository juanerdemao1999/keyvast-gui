//! Professional waveform rendering for egui.
//!
//! Draws multi-channel signal traces with:
//! - Time grid (major/minor divisions)
//! - Amplitude scale bars
//! - Channel labels with color coding
//! - Zero-line reference
//! - Scrollable channel list

use std::collections::VecDeque;

use eframe::egui;
use kv_types::SampleBlock;

use crate::panels::DisplaySettings;
use crate::theme;

/// Left margin reserved for channel labels.
const LABEL_WIDTH: f32 = 48.0;
/// Right margin reserved for amplitude scale.
const SCALE_WIDTH: f32 = 6.0;
/// Minimum channel row height in pixels.
const MIN_CHANNEL_HEIGHT: f32 = 24.0;

/// Draw the full waveform area: channel labels + traces + grid.
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
    let trace_width = (available.x - LABEL_WIDTH - SCALE_WIDTH).max(60.0);
    let channel_height = (available.y / channels as f32).max(MIN_CHANNEL_HEIGHT);

    // Collect sample data across history for continuous trace
    let total_samples_per_channel = collect_sample_count(history, latest, block.channel_count);

    for ch in 0..channels {
        ui.horizontal(|ui| {
            // Channel label column
            draw_channel_label(ui, ch, channel_height, settings.show_channel_labels);

            // Trace area
            let (response, painter) = ui.allocate_painter(
                egui::vec2(trace_width, channel_height),
                egui::Sense::hover(),
            );
            let rect = response.rect;

            // Background (alternating)
            let bg = if ch % 2 == 0 {
                theme::BG_WAVEFORM_EVEN
            } else {
                theme::BG_WAVEFORM_ODD
            };
            painter.rect_filled(rect, 0.0, bg);

            // Grid
            if settings.show_grid {
                draw_grid(&painter, rect, block, settings);
            }

            // Zero line
            let mid_y = rect.center().y;
            painter.line_segment(
                [
                    egui::pos2(rect.left(), mid_y),
                    egui::pos2(rect.right(), mid_y),
                ],
                egui::Stroke::new(0.5, theme::GRID_ZERO_LINE),
            );

            // Trace
            draw_channel_trace(&TraceParams {
                painter: &painter,
                rect,
                ch,
                history,
                latest,
                channel_count: block.channel_count,
                total_samples: total_samples_per_channel,
            });

            // Top/bottom border
            painter.line_segment(
                [rect.left_top(), rect.right_top()],
                egui::Stroke::new(0.5, theme::GRID_MINOR),
            );
        });
    }
}

fn draw_empty_state(ui: &mut egui::Ui) {
    let available = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());

    ui.painter().rect_filled(rect, 0.0, theme::BG_DARKEST);

    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "No data — press Start to begin acquisition",
        egui::FontId::proportional(16.0),
        theme::TEXT_DIM,
    );
}

fn draw_channel_label(ui: &mut egui::Ui, ch: usize, height: f32, show: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(LABEL_WIDTH, height), egui::Sense::hover());

    if !show {
        return;
    }

    let color = theme::channel_color(ch);

    // Color bar on the left edge
    let bar = egui::Rect::from_min_size(rect.left_top(), egui::vec2(3.0, height));
    ui.painter().rect_filled(bar, 0.0, color);

    // Channel number
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("CH{ch}"),
        egui::FontId::monospace(9.0),
        theme::TEXT_SECONDARY,
    );
}

fn draw_grid(
    painter: &egui::Painter,
    rect: egui::Rect,
    block: &SampleBlock,
    settings: &DisplaySettings,
) {
    // Vertical grid lines (time divisions)
    let time_per_sample = if block.sample_rate > 0.0 {
        1000.0 / block.sample_rate // ms per sample
    } else {
        1.0
    };
    let ms_per_div = settings.time_scale_ms();
    let samples_per_div = (ms_per_div / time_per_sample).max(1.0);
    let pixels_per_sample = rect.width() / block.samples_per_channel.max(1) as f32;
    let pixels_per_div = (samples_per_div as f32 * pixels_per_sample).max(20.0);

    // Major vertical lines
    let mut x = rect.left() + pixels_per_div;
    while x < rect.right() {
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(0.5, theme::GRID_MAJOR),
        );
        x += pixels_per_div;
    }

    // Minor vertical lines (5 subdivisions)
    let minor_step = pixels_per_div / 5.0;
    if minor_step > 8.0 {
        let mut x = rect.left() + minor_step;
        while x < rect.right() {
            // Skip major lines
            let near_major = ((x - rect.left()) % pixels_per_div).abs() < 1.0;
            if !near_major {
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    egui::Stroke::new(0.3, theme::GRID_MINOR),
                );
            }
            x += minor_step;
        }
    }

    // Horizontal grid lines (amplitude — at ±25%, ±50%, ±75%)
    for frac in [0.25_f32, 0.5, 0.75] {
        let y_up = rect.center().y - rect.height() * frac * 0.5;
        let y_down = rect.center().y + rect.height() * frac * 0.5;
        let stroke = if (frac - 0.5).abs() < 0.01 {
            egui::Stroke::new(0.4, theme::GRID_MAJOR)
        } else {
            egui::Stroke::new(0.3, theme::GRID_MINOR)
        };
        painter.line_segment(
            [
                egui::pos2(rect.left(), y_up),
                egui::pos2(rect.right(), y_up),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(rect.left(), y_down),
                egui::pos2(rect.right(), y_down),
            ],
            stroke,
        );
    }
}

struct TraceParams<'a> {
    painter: &'a egui::Painter,
    rect: egui::Rect,
    ch: usize,
    history: &'a VecDeque<SampleBlock>,
    latest: Option<&'a SampleBlock>,
    channel_count: usize,
    total_samples: usize,
}

fn draw_channel_trace(params: &TraceParams<'_>) {
    if params.total_samples == 0 || params.channel_count == 0 {
        return;
    }

    let color = theme::channel_color(params.ch);
    let half_h = (params.rect.height() * 0.42).max(1.0);
    let mid_y = params.rect.center().y;

    // Decide how many samples to show (fill the trace width)
    let display_samples = params.total_samples.min(params.rect.width() as usize * 2);
    let skip = params.total_samples.saturating_sub(display_samples);

    let x_step = params.rect.width() / display_samples.max(1) as f32;

    // Build points by iterating through history blocks + latest
    let mut points: Vec<egui::Pos2> = Vec::with_capacity(display_samples);
    let mut sample_idx: usize = 0;
    let mut drawn: usize = 0;

    let blocks_iter = params.history.iter().chain(params.latest);

    for block in blocks_iter {
        if block.channel_count != params.channel_count {
            continue;
        }
        let spc = block.samples_per_channel;
        for s in 0..spc {
            if sample_idx >= skip && drawn < display_samples {
                let data_idx = s * params.channel_count + params.ch;
                let value = if data_idx < block.data.len() {
                    block.data[data_idx] as f32 / i16::MAX as f32
                } else {
                    0.0
                };
                let x = params.rect.left() + drawn as f32 * x_step;
                let y = mid_y - value * half_h;
                points.push(egui::pos2(x, y));
                drawn += 1;
            }
            sample_idx += 1;
        }
    }

    // Draw trace as connected line segments
    if points.len() >= 2 {
        let stroke = egui::Stroke::new(1.2, color);
        for pair in points.windows(2) {
            params.painter.line_segment([pair[0], pair[1]], stroke);
        }
    }
}

fn collect_sample_count(
    history: &VecDeque<SampleBlock>,
    latest: Option<&SampleBlock>,
    channel_count: usize,
) -> usize {
    let mut total = 0usize;
    for block in history.iter() {
        if block.channel_count == channel_count {
            total += block.samples_per_channel;
        }
    }
    if let Some(b) = latest
        && b.channel_count == channel_count
    {
        total += b.samples_per_channel;
    }
    total
}
