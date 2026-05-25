//! Professional dark theme for Keyvast GUI.
//!
//! Modeled after Intan RHX / Open Ephys style: dark backgrounds,
//! high-contrast text, colored accent indicators, clean typography.

#![allow(dead_code)] // palette constants used progressively as features land

use eframe::egui;

// ── Background tones ────────────────────────────────────────────────

pub const BG_DARKEST: egui::Color32 = egui::Color32::from_rgb(18, 18, 22);
pub const BG_DARK: egui::Color32 = egui::Color32::from_rgb(24, 24, 30);
pub const BG_PANEL: egui::Color32 = egui::Color32::from_rgb(30, 30, 38);
pub const BG_WIDGET: egui::Color32 = egui::Color32::from_rgb(40, 40, 50);
pub const BG_HOVER: egui::Color32 = egui::Color32::from_rgb(50, 50, 64);
pub const BG_TOOLBAR: egui::Color32 = egui::Color32::from_rgb(22, 22, 28);

// ── Text ────────────────────────────────────────────────────────────

pub const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(210, 210, 220);
pub const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(140, 140, 155);
pub const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(90, 90, 105);

// ── Accent colors ───────────────────────────────────────────────────

pub const ACCENT_GREEN: egui::Color32 = egui::Color32::from_rgb(60, 200, 80);
pub const ACCENT_RED: egui::Color32 = egui::Color32::from_rgb(220, 60, 60);
pub const ACCENT_YELLOW: egui::Color32 = egui::Color32::from_rgb(230, 200, 50);
pub const ACCENT_BLUE: egui::Color32 = egui::Color32::from_rgb(70, 140, 255);
pub const ACCENT_ORANGE: egui::Color32 = egui::Color32::from_rgb(240, 150, 40);
pub const ACCENT_CYAN: egui::Color32 = egui::Color32::from_rgb(60, 200, 210);

// ── Transport button colors (high-contrast for dark toolbar) ────────

pub const BTN_PLAY: egui::Color32 = egui::Color32::from_rgb(40, 180, 70);
pub const BTN_PLAY_ACTIVE: egui::Color32 = egui::Color32::from_rgb(60, 210, 90);
pub const BTN_STOP: egui::Color32 = egui::Color32::from_rgb(200, 55, 55);
pub const BTN_RECORD: egui::Color32 = egui::Color32::from_rgb(220, 50, 50);
pub const BTN_RECORD_ACTIVE: egui::Color32 = egui::Color32::from_rgb(255, 70, 70);
pub const BTN_DISABLED: egui::Color32 = egui::Color32::from_rgb(65, 65, 78);

// ── Grid / guides ───────────────────────────────────────────────────

pub const GRID_ZERO_LINE: egui::Color32 = egui::Color32::from_rgb(55, 55, 68);

// ── Status indicator colors ─────────────────────────────────────────

pub const STATUS_CONNECTED: egui::Color32 = ACCENT_GREEN;
pub const STATUS_RECORDING: egui::Color32 = ACCENT_RED;
pub const STATUS_ARMED: egui::Color32 = ACCENT_YELLOW;
pub const STATUS_IDLE: egui::Color32 = egui::Color32::from_rgb(80, 80, 95);

// ── Channel trace palette (32 distinct colors) ─────────────────────
//
// Hand-tuned to be perceptually distinct on a dark background.  The first
// 16 are the original palette; the next 16 are shifted in hue/lightness
// so that adjacent channels never share a similar color even at 32+ ch.

pub const CHANNEL_PALETTE: &[egui::Color32] = &[
    // -- original 16 --
    egui::Color32::from_rgb(80, 200, 80),   //  0 green
    egui::Color32::from_rgb(80, 150, 255),  //  1 blue
    egui::Color32::from_rgb(255, 140, 80),  //  2 orange
    egui::Color32::from_rgb(220, 200, 60),  //  3 yellow
    egui::Color32::from_rgb(200, 100, 220), //  4 purple
    egui::Color32::from_rgb(60, 200, 200),  //  5 cyan
    egui::Color32::from_rgb(255, 90, 90),   //  6 red
    egui::Color32::from_rgb(150, 200, 255), //  7 light blue
    egui::Color32::from_rgb(180, 255, 120), //  8 lime
    egui::Color32::from_rgb(255, 180, 200), //  9 pink
    egui::Color32::from_rgb(120, 100, 255), // 10 indigo
    egui::Color32::from_rgb(255, 200, 100), // 11 gold
    egui::Color32::from_rgb(100, 255, 200), // 12 mint
    egui::Color32::from_rgb(255, 120, 180), // 13 hot pink
    egui::Color32::from_rgb(160, 200, 80),  // 14 olive
    egui::Color32::from_rgb(100, 200, 255), // 15 sky blue
    // -- extended 16 (shifted hue/lightness) --
    egui::Color32::from_rgb(140, 255, 140), // 16 bright green
    egui::Color32::from_rgb(100, 110, 220), // 17 slate blue
    egui::Color32::from_rgb(220, 100, 50),  // 18 burnt orange
    egui::Color32::from_rgb(255, 255, 120), // 19 pale yellow
    egui::Color32::from_rgb(160, 60, 180),  // 20 dark purple
    egui::Color32::from_rgb(80, 240, 240),  // 21 bright cyan
    egui::Color32::from_rgb(200, 50, 50),   // 22 dark red
    egui::Color32::from_rgb(180, 220, 255), // 23 ice blue
    egui::Color32::from_rgb(120, 200, 60),  // 24 grass
    egui::Color32::from_rgb(255, 140, 160), // 25 salmon
    egui::Color32::from_rgb(80, 70, 200),   // 26 deep indigo
    egui::Color32::from_rgb(200, 160, 60),  // 27 amber
    egui::Color32::from_rgb(60, 220, 160),  // 28 teal
    egui::Color32::from_rgb(220, 80, 140),  // 29 magenta
    egui::Color32::from_rgb(200, 220, 100), // 30 chartreuse
    egui::Color32::from_rgb(140, 180, 220), // 31 steel blue
];

pub fn channel_color(channel: usize) -> egui::Color32 {
    CHANNEL_PALETTE[channel % CHANNEL_PALETTE.len()]
}

// ── Apply theme to egui context ─────────────────────────────────────

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    // Visuals
    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = Some(TEXT_PRIMARY);

    v.window_fill = BG_PANEL;
    v.panel_fill = BG_PANEL;
    v.faint_bg_color = BG_WIDGET;
    v.extreme_bg_color = BG_DARKEST;

    v.widgets.noninteractive.bg_fill = BG_PANEL;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_SECONDARY);
    v.widgets.noninteractive.corner_radius = egui::CornerRadius::same(3);

    v.widgets.inactive.bg_fill = BG_WIDGET;
    v.widgets.inactive.weak_bg_fill = BG_WIDGET;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.inactive.corner_radius = egui::CornerRadius::same(3);

    v.widgets.hovered.bg_fill = BG_HOVER;
    v.widgets.hovered.weak_bg_fill = BG_HOVER;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    v.widgets.hovered.corner_radius = egui::CornerRadius::same(3);

    v.widgets.active.bg_fill = ACCENT_BLUE;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    v.widgets.active.corner_radius = egui::CornerRadius::same(3);

    v.selection.bg_fill = egui::Color32::from_rgba_premultiplied(70, 140, 255, 60);
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT_BLUE);

    // Spacing
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);

    ctx.set_style(style);
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Draw a small filled circle as a status indicator.
pub fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Section heading for side panels — collapsible style.
pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text)
            .size(11.0)
            .strong()
            .color(TEXT_SECONDARY),
    );
    ui.separator();
}

/// Key-value label pair for status display.
pub fn kv_label(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(key).size(11.0).color(TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(11.0)
                    .monospace()
                    .color(TEXT_PRIMARY),
            );
        });
    });
}

/// Key-value with colored value.
pub fn kv_label_colored(ui: &mut egui::Ui, key: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(key).size(11.0).color(TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(11.0)
                    .monospace()
                    .color(color),
            );
        });
    });
}

/// High-contrast transport button (play/stop/record style).
///
/// Does NOT use `add_enabled` — egui's disabled dimming overrides our
/// explicit colors, making text unreadable.  Instead we always `ui.add()`
/// and gate the click result on `enabled`.
pub fn transport_button(
    ui: &mut egui::Ui,
    label: &str,
    fill: egui::Color32,
    enabled: bool,
) -> bool {
    let (bg, text_color) = if enabled {
        (fill, egui::Color32::WHITE)
    } else {
        // Lighter disabled background with clearly readable text
        (
            egui::Color32::from_rgb(55, 55, 68),
            egui::Color32::from_rgb(110, 110, 125),
        )
    };

    let btn = egui::Button::new(
        egui::RichText::new(label)
            .size(14.0)
            .strong()
            .color(text_color),
    )
    .fill(bg)
    .stroke(egui::Stroke::new(1.0, bg))
    .min_size(egui::vec2(76.0, 30.0))
    .corner_radius(egui::CornerRadius::same(5));

    let clicked = ui.add(btn).clicked();
    enabled && clicked
}

/// Format seconds into HH:MM:SS.
pub fn format_clock(seconds: f64) -> String {
    let total_secs = seconds as u64;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
