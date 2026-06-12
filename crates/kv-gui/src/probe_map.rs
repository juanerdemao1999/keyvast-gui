//! Probe Map visualization — 2D physical layout of electrode sites with
//! real-time activity coloring.
//!
//! Supports common linear probe geometries (Neuropixels-style single column,
//! dual-column, tetrode grids) and allows the user to define custom X/Y
//! positions. Each site is colored by RMS amplitude of the most recent data
//! window for that channel.

use eframe::egui;

use crate::disp_ring::DisplayRing;
use crate::theme;

// ── Probe geometry presets ──────────────────────────────────────────

/// Pre-defined probe layout geometries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeGeometry {
    /// Single column, 25 µm pitch (Neuropixels-like).
    LinearSingle,
    /// Dual-column staggered, 25 µm pitch.
    LinearDual,
    /// 2×2 tetrode groups stacked vertically.
    Tetrode,
    /// 4×8 grid (common 32-ch probes).
    Grid4x8,
    /// User-provided (x, y) pairs.
    Custom,
}

impl ProbeGeometry {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LinearSingle => "Linear 1-col",
            Self::LinearDual => "Linear 2-col",
            Self::Tetrode => "Tetrode",
            Self::Grid4x8 => "Grid 4×8",
            Self::Custom => "Custom",
        }
    }

    /// Generate (x, y) positions in µm for each channel.
    pub fn positions(&self, channel_count: usize) -> Vec<(f32, f32)> {
        match self {
            Self::LinearSingle => (0..channel_count)
                .map(|i| (0.0, i as f32 * 25.0))
                .collect(),
            Self::LinearDual => (0..channel_count)
                .map(|i| {
                    let col = (i % 2) as f32 * 22.0;
                    let row = (i / 2) as f32 * 25.0;
                    (col, row)
                })
                .collect(),
            Self::Tetrode => (0..channel_count)
                .map(|i| {
                    let tet = i / 4;
                    let within = i % 4;
                    let x = (within % 2) as f32 * 25.0;
                    let y = tet as f32 * 80.0 + (within / 2) as f32 * 25.0;
                    (x, y)
                })
                .collect(),
            Self::Grid4x8 => (0..channel_count)
                .map(|i| {
                    let col = (i % 4) as f32 * 30.0;
                    let row = (i / 4) as f32 * 30.0;
                    (col, row)
                })
                .collect(),
            Self::Custom => (0..channel_count)
                .map(|i| (0.0, i as f32 * 25.0))
                .collect(),
        }
    }
}

// ── Probe Map state ─────────────────────────────────────────────────

/// Activity level mapped from RMS → color.
#[derive(Debug, Clone, Copy)]
pub struct SiteActivity {
    /// RMS amplitude of recent data in µV.
    pub rms_uv: f32,
    /// Normalized activity (0.0 = silent, 1.0 = maximal).
    pub normalized: f32,
}

pub struct ProbeMapState {
    pub visible: bool,
    pub geometry: ProbeGeometry,
    /// Custom positions (only used when geometry == Custom).
    pub custom_positions: Vec<(f32, f32)>,
    /// Custom position text input buffer.
    pub custom_input: String,
    /// Per-channel activity computed each frame.
    pub activities: Vec<SiteActivity>,
    /// Number of samples to compute RMS over.
    pub rms_window_samples: usize,
    /// Max RMS for normalization (auto-scaled or fixed).
    pub rms_max_uv: f32,
    /// Whether to auto-scale the color range.
    pub auto_scale: bool,
    /// Site radius in pixels.
    pub site_radius: f32,
}

impl Default for ProbeMapState {
    fn default() -> Self {
        Self {
            visible: false,
            geometry: ProbeGeometry::LinearDual,
            custom_positions: Vec::new(),
            custom_input: String::new(),
            activities: Vec::new(),
            rms_window_samples: 1024,
            rms_max_uv: 500.0,
            auto_scale: true,
            site_radius: 6.0,
        }
    }
}

impl ProbeMapState {
    /// Compute per-channel RMS from the display ring.
    pub fn update_activity(&mut self, ring: &DisplayRing, channel_count: usize) {
        self.activities.resize(
            channel_count,
            SiteActivity { rms_uv: 0.0, normalized: 0.0 },
        );

        let mut max_rms: f32 = 0.0;

        for ch in 0..channel_count {
            let samples = ring.last_n_samples(ch, self.rms_window_samples);
            let rms = if samples.is_empty() {
                0.0
            } else {
                let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
                let mean_sq = sum_sq / samples.len() as f64;
                // Convert raw ADC units to µV: RHD uses 0.195 µV/count
                (mean_sq.sqrt() * 0.195) as f32
            };
            self.activities[ch].rms_uv = rms;
            if rms > max_rms {
                max_rms = rms;
            }
        }

        // Auto-scale: use the max observed RMS (with a floor to avoid div-by-zero)
        if self.auto_scale && max_rms > 1.0 {
            self.rms_max_uv = max_rms;
        }

        // Normalize
        let scale = if self.rms_max_uv > 0.0 { self.rms_max_uv } else { 1.0 };
        for act in &mut self.activities {
            act.normalized = (act.rms_uv / scale).min(1.0);
        }
    }
}

/// Map normalized activity [0, 1] to a color (blue → green → yellow → red).
fn activity_color(normalized: f32) -> egui::Color32 {
    let t = normalized.clamp(0.0, 1.0);
    if t < 0.33 {
        // Blue → Cyan
        let u = t / 0.33;
        egui::Color32::from_rgb(
            20,
            (60.0 + 160.0 * u) as u8,
            (200.0 - 40.0 * u) as u8,
        )
    } else if t < 0.66 {
        // Cyan → Yellow
        let u = (t - 0.33) / 0.33;
        egui::Color32::from_rgb(
            (20.0 + 220.0 * u) as u8,
            (220.0 - 20.0 * u) as u8,
            (160.0 - 160.0 * u) as u8,
        )
    } else {
        // Yellow → Red
        let u = (t - 0.66) / 0.34;
        egui::Color32::from_rgb(
            240,
            (200.0 - 160.0 * u) as u8,
            (0.0 + 20.0 * u) as u8,
        )
    }
}

// ── Draw functions ──────────────────────────────────────────────────

/// Draw probe map sidebar config section.
pub fn draw_probe_map_section(
    ui: &mut egui::Ui,
    state: &mut ProbeMapState,
    channel_count: usize,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("PROBE MAP")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.checkbox(
            &mut state.visible,
            egui::RichText::new("Show probe map").size(10.0),
        );

        if !state.visible {
            return;
        }

        // Geometry selector
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Layout:").size(10.0).color(theme::TEXT_DIM));
            egui::ComboBox::from_id_salt("probe_geom")
                .width(90.0)
                .selected_text(egui::RichText::new(state.geometry.label()).size(10.0))
                .show_ui(ui, |ui| {
                    for geom in [
                        ProbeGeometry::LinearSingle,
                        ProbeGeometry::LinearDual,
                        ProbeGeometry::Tetrode,
                        ProbeGeometry::Grid4x8,
                        ProbeGeometry::Custom,
                    ] {
                        ui.selectable_value(
                            &mut state.geometry,
                            geom,
                            egui::RichText::new(geom.label()).size(10.0),
                        );
                    }
                });
        });

        // Site radius
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Site size:").size(10.0).color(theme::TEXT_DIM));
            ui.add(egui::Slider::new(&mut state.site_radius, 3.0..=12.0).show_value(false));
        });

        // Auto-scale toggle
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut state.auto_scale,
                egui::RichText::new("Auto-scale colors").size(10.0),
            );
        });

        if !state.auto_scale {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Max µV:").size(10.0).color(theme::TEXT_DIM));
                ui.add(
                    egui::DragValue::new(&mut state.rms_max_uv)
                        .range(10.0..=10000.0)
                        .speed(10.0),
                );
            });
        }

        // RMS window
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("RMS window:").size(10.0).color(theme::TEXT_DIM));
            ui.add(
                egui::DragValue::new(&mut state.rms_window_samples)
                    .range(128..=8192)
                    .speed(64.0),
            );
            ui.label(egui::RichText::new("samp").size(9.0).color(theme::TEXT_DIM));
        });

        // Stats summary
        if !state.activities.is_empty() {
            let max_act = state.activities.iter().map(|a| a.rms_uv).fold(0.0f32, f32::max);
            let min_act = state.activities.iter().map(|a| a.rms_uv).fold(f32::MAX, f32::min);
            ui.label(
                egui::RichText::new(format!("RMS range: {min_act:.0}–{max_act:.0} µV"))
                    .size(9.0)
                    .color(theme::TEXT_DIM),
            );
        }

        let _ = channel_count; // used in update_activity, not needed here
    });
}

/// Draw the probe map visualization window.
pub fn draw_probe_map_window(
    ctx: &egui::Context,
    state: &ProbeMapState,
    channel_count: usize,
) {
    if !state.visible || channel_count == 0 {
        return;
    }

    let positions = state.geometry.positions(channel_count);

    egui::Window::new("Probe Map")
        .default_size(egui::vec2(200.0, 400.0))
        .resizable(true)
        .collapsible(true)
        .show(ctx, |ui| {
            let avail = ui.available_size();
            let (response, painter) =
                ui.allocate_painter(avail, egui::Sense::hover());
            let rect = response.rect;

            if positions.is_empty() {
                return;
            }

            // Compute bounding box of positions
            let (mut min_x, mut min_y, mut max_x, mut max_y) =
                (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for &(x, y) in &positions {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }

            // Add padding
            let pad = state.site_radius * 3.0;
            let data_w = (max_x - min_x).max(1.0);
            let data_h = (max_y - min_y).max(1.0);
            let canvas_w = rect.width() - pad * 2.0;
            let canvas_h = rect.height() - pad * 2.0;

            let scale = (canvas_w / data_w).min(canvas_h / data_h);

            // Center offset
            let offset_x = rect.left() + pad + (canvas_w - data_w * scale) / 2.0;
            let offset_y = rect.top() + pad + (canvas_h - data_h * scale) / 2.0;

            // Draw sites
            for (ch, &(px, py)) in positions.iter().enumerate() {
                let sx = offset_x + (px - min_x) * scale;
                let sy = offset_y + (py - min_y) * scale;
                let center = egui::pos2(sx, sy);

                let color = if ch < state.activities.len() {
                    activity_color(state.activities[ch].normalized)
                } else {
                    egui::Color32::from_rgb(40, 40, 50)
                };

                painter.circle_filled(center, state.site_radius, color);
                painter.circle_stroke(
                    center,
                    state.site_radius,
                    egui::Stroke::new(0.5, egui::Color32::from_rgb(80, 80, 90)),
                );

                // Channel label on hover
                let hover_rect = egui::Rect::from_center_size(
                    center,
                    egui::vec2(state.site_radius * 2.0, state.site_radius * 2.0),
                );
                if ui.rect_contains_pointer(hover_rect) {
                    let rms_str = if ch < state.activities.len() {
                        format!("{:.0} µV", state.activities[ch].rms_uv)
                    } else {
                        "N/A".to_string()
                    };
                    egui::show_tooltip_at_pointer(
                        ui.ctx(),
                        ui.layer_id(),
                        egui::Id::new("probe_tooltip"),
                        |ui| {
                            ui.label(format!("CH{ch} — {rms_str}"));
                        },
                    );
                }
            }

            // Color bar legend (right edge)
            let bar_x = rect.right() - 16.0;
            let bar_top = rect.top() + 20.0;
            let bar_bottom = rect.bottom() - 20.0;
            let bar_h = bar_bottom - bar_top;
            let steps = 20;
            for i in 0..steps {
                let t = i as f32 / steps as f32;
                let y0 = bar_bottom - t * bar_h;
                let y1 = bar_bottom - (t + 1.0 / steps as f32) * bar_h;
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(bar_x, y1),
                        egui::pos2(bar_x + 10.0, y0),
                    ),
                    egui::CornerRadius::ZERO,
                    activity_color(t),
                );
            }
            // Labels
            painter.text(
                egui::pos2(bar_x - 2.0, bar_top - 10.0),
                egui::Align2::RIGHT_BOTTOM,
                format!("{:.0}", state.rms_max_uv),
                egui::FontId::monospace(8.0),
                theme::TEXT_DIM,
            );
            painter.text(
                egui::pos2(bar_x - 2.0, bar_bottom + 10.0),
                egui::Align2::RIGHT_TOP,
                "0",
                egui::FontId::monospace(8.0),
                theme::TEXT_DIM,
            );
        });
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_single_positions_correct() {
        let pos = ProbeGeometry::LinearSingle.positions(4);
        assert_eq!(pos.len(), 4);
        assert_eq!(pos[0], (0.0, 0.0));
        assert_eq!(pos[3], (0.0, 75.0));
    }

    #[test]
    fn linear_dual_alternates_columns() {
        let pos = ProbeGeometry::LinearDual.positions(4);
        assert_eq!(pos[0], (0.0, 0.0));   // col 0, row 0
        assert_eq!(pos[1], (22.0, 0.0));  // col 1, row 0
        assert_eq!(pos[2], (0.0, 25.0));  // col 0, row 1
        assert_eq!(pos[3], (22.0, 25.0)); // col 1, row 1
    }

    #[test]
    fn grid4x8_positions_correct() {
        let pos = ProbeGeometry::Grid4x8.positions(32);
        assert_eq!(pos.len(), 32);
        // ch 5 = row 1, col 1
        assert_eq!(pos[5], (30.0, 30.0));
    }

    #[test]
    fn activity_color_range() {
        let c0 = activity_color(0.0);
        let c1 = activity_color(1.0);
        // Low activity should be blueish (high B)
        assert!(c0.b() > c0.r());
        // High activity should be reddish (high R)
        assert!(c1.r() > c1.b());
    }

    #[test]
    fn probe_map_state_default() {
        let s = ProbeMapState::default();
        assert!(!s.visible);
        assert!(s.auto_scale);
        assert_eq!(s.geometry, ProbeGeometry::LinearDual);
    }
}
