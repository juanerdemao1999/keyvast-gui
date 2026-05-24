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

// ── Channel trace palette (16 distinct colors) ─────────────────────

pub const CHANNEL_PALETTE: &[egui::Color32] = &[
    egui::Color32::from_rgb(80, 200, 80),   // green
    egui::Color32::from_rgb(80, 150, 255),  // blue
    egui::Color32::from_rgb(255, 140, 80),  // orange
    egui::Color32::from_rgb(220, 200, 60),  // yellow
    egui::Color32::from_rgb(200, 100, 220), // purple
    egui::Color32::from_rgb(60, 200, 200),  // cyan
    egui::Color32::from_rgb(255, 90, 90),   // red
    egui::Color32::from_rgb(150, 200, 255), // light blue
    egui::Color32::from_rgb(180, 255, 120), // lime
    egui::Color32::from_rgb(255, 180, 200), // pink
    egui::Color32::from_rgb(120, 100, 255), // indigo
    egui::Color32::from_rgb(255, 200, 100), // gold
    egui::Color32::from_rgb(100, 255, 200), // mint
    egui::Color32::from_rgb(255, 120, 180), // hot pink
    egui::Color32::from_rgb(160, 200, 80),  // olive
    egui::Color32::from_rgb(100, 200, 255), // sky blue
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
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.inactive.corner_radius = egui::CornerRadius::same(3);

    v.widgets.hovered.bg_fill = BG_HOVER;
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
pub fn transport_button(
    ui: &mut egui::Ui,
    label: &str,
    fill: egui::Color32,
    enabled: bool,
) -> bool {
    let (bg, text_color, stroke_color) = if enabled {
        (fill, egui::Color32::WHITE, fill)
    } else {
        (BTN_DISABLED, egui::Color32::from_rgb(130, 130, 145), BTN_DISABLED)
    };

    let btn = egui::Button::new(
        egui::RichText::new(label)
            .size(14.0)
            .strong()
            .color(text_color),
    )
    .fill(bg)
    .stroke(egui::Stroke::new(1.5, stroke_color))
    .min_size(egui::vec2(76.0, 30.0))
    .corner_radius(egui::CornerRadius::same(5));

    ui.add_enabled(enabled, btn).clicked()
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
