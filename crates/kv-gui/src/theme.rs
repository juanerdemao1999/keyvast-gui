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

// ── Typography scale ────────────────────────────────────────────────
//
// A single source of truth for font sizes.  Same-purpose UI elements share
// one constant so the whole interface scales consistently from one place,
// instead of the previous scatter of hand-written 9 / 9.5 / 10 / 11 / 12px.
//
//   HEADING  — section titles, brand sub-labels, emphasized state pills.
//   BODY     — default control text (labels, checkboxes, buttons).
//   CAPTION  — secondary / hint / footnote text.
//   MICRO    — densest status-bar metrics and tiny annotations.

/// Section headings and emphasized labels.
pub const FONT_HEADING: f32 = 11.0;
/// Default body / control text.
pub const FONT_BODY: f32 = 10.0;
/// Secondary, hint, and footnote text.
pub const FONT_CAPTION: f32 = 9.0;
/// Densest metrics / micro annotations.
pub const FONT_MICRO: f32 = 9.0;
/// Large numeric readouts (toolbar clock, transport button glyphs).
pub const FONT_DISPLAY: f32 = 15.0;

/// Section heading `RichText`: bold, secondary color, heading size.
pub fn heading(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(FONT_HEADING)
        .strong()
        .color(TEXT_SECONDARY)
}

/// Body `RichText`: default size, primary text color.
pub fn body(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).size(FONT_BODY)
}

/// Dim caption `RichText`: caption size, dim color.
pub fn caption(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(FONT_CAPTION)
        .color(TEXT_DIM)
}

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

// ── Fonts ───────────────────────────────────────────────────────────

/// Append a Windows symbol font as a glyph *fallback* so the geometric shapes,
/// arrows, and dingbats used across the UI (disclosure carets `\u{25B8}` /
/// `\u{25BE}`, `\u{25CF} REC`, `\u{2192}`, `\u{26A0}`, toast icons, …) render
/// instead of showing as missing-glyph boxes.  egui's bundled fonts cover the
/// Latin text but not these symbols.
///
/// The font is added at the *end* of each family so normal text keeps using the
/// bundled Ubuntu/Hack faces and only otherwise-missing glyphs fall through to
/// it.  Best-effort: if no symbol font is present the UI still works, just with
/// a few boxes, so a missing file is silently ignored.
fn install_symbol_font(ctx: &egui::Context) {
    // Segoe UI Symbol ships with every supported Windows release and covers the
    // arrows / geometric-shape / dingbat blocks we use.
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\seguisym.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "kv_symbols".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("kv_symbols".to_owned());
    }
    ctx.set_fonts(fonts);
}

// ── Apply theme to egui context ─────────────────────────────────────

pub fn apply(ctx: &egui::Context) {
    install_symbol_font(ctx);

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
    ui.label(heading(text));
    ui.separator();
}

/// Key-value label pair for status display.
pub fn kv_label(ui: &mut egui::Ui, key: &str, value: &str) {
    kv_label_colored(ui, key, value, TEXT_PRIMARY);
}

/// Key-value with colored value.
pub fn kv_label_colored(ui: &mut egui::Ui, key: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(key).size(FONT_BODY).color(TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(FONT_BODY)
                    .monospace()
                    .color(color),
            );
        });
    });
}

// ── Section card (A2) ───────────────────────────────────────────────

/// Visual tier for a section card's left accent bar.
///
/// A collapsible card with a subtly raised background, rounded corners, a
/// colored left accent bar, and consistent inner padding.  Replaces the bare
/// `CollapsingHeader` rows so section titles stand out from their contents and
/// groups are visually separated.
pub fn section_card<R>(
    ui: &mut egui::Ui,
    id_salt: &str,
    title: &str,
    accent: egui::Color32,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let mut ret = None;
    egui::Frame::new()
        .fill(BG_DARK)
        .corner_radius(egui::CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .outer_margin(egui::Margin {
            bottom: 6,
            ..egui::Margin::ZERO
        })
        .show(ui, |ui| {
            let id = ui.make_persistent_id(id_salt);
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                default_open,
            );
            let open = state.is_open();
            let header = ui
                .horizontal(|ui| {
                    // Left accent bar.
                    let (bar, _) =
                        ui.allocate_exact_size(egui::vec2(3.0, 14.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(bar, egui::CornerRadius::same(1), accent);
                    ui.add_space(2.0);
                    // Disclosure caret + title (whole row is clickable).
                    let caret = if open { "\u{25BE}" } else { "\u{25B8}" };
                    ui.label(
                        egui::RichText::new(caret)
                            .size(FONT_CAPTION)
                            .color(TEXT_DIM),
                    );
                    ui.label(heading(title))
                })
                .response
                .interact(egui::Sense::click());
            if header.clicked() {
                state.toggle(ui);
            }
            state.show_body_unindented(ui, |ui| {
                ui.add_space(2.0);
                ret = Some(add_contents(ui));
            });
        });
    ret
}

// ── Button tiers (A3) ───────────────────────────────────────────────

/// Visual emphasis tier for an action button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtnTier {
    /// Main affirmative action (blue).
    Primary,
    /// Neutral / secondary action (default widget gray).
    Secondary,
    /// Destructive action (red).
    Danger,
}

/// A consistently-styled tier button.  Like [`transport_button`], this gates
/// the click on `enabled` itself (rather than `add_enabled`) so the explicit
/// fill color is never overridden by egui's disabled dimming.
pub fn tier_button(ui: &mut egui::Ui, label: &str, tier: BtnTier, enabled: bool) -> bool {
    let (bg, text_color, stroke) = if !enabled {
        (
            BTN_DISABLED,
            egui::Color32::from_rgb(110, 110, 125),
            BTN_DISABLED,
        )
    } else {
        match tier {
            BtnTier::Primary => (ACCENT_BLUE, egui::Color32::WHITE, ACCENT_BLUE),
            BtnTier::Danger => (ACCENT_RED, egui::Color32::WHITE, ACCENT_RED),
            BtnTier::Secondary => (BG_WIDGET, TEXT_PRIMARY, BG_HOVER),
        }
    };

    let btn = egui::Button::new(
        egui::RichText::new(label)
            .size(FONT_BODY)
            .strong()
            .color(text_color),
    )
    .fill(bg)
    .stroke(egui::Stroke::new(1.0, stroke))
    .corner_radius(egui::CornerRadius::same(4));

    ui.add(btn).clicked() && enabled
}

/// Primary (blue) action button.
pub fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    tier_button(ui, label, BtnTier::Primary, enabled)
}

/// Secondary (neutral) action button.
pub fn secondary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    tier_button(ui, label, BtnTier::Secondary, enabled)
}

/// Danger (red) action button.
pub fn danger_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    tier_button(ui, label, BtnTier::Danger, enabled)
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

/// Default minimum width for a transport button.
pub const TRANSPORT_BTN_W: f32 = 76.0;

/// Like [`transport_button`] but with a hover tooltip (used to surface the
/// keyboard shortcut for each transport action — B4).
pub fn transport_button_tip(
    ui: &mut egui::Ui,
    label: &str,
    fill: egui::Color32,
    enabled: bool,
    tip: &str,
) -> bool {
    transport_button_sized(ui, label, fill, enabled, tip, TRANSPORT_BTN_W)
}

/// Tooltip transport button with an explicit minimum width.
///
/// A single logical control whose label changes with state (e.g.
/// `Record` → `ARMED` → `STOP REC`) would otherwise resize on every state
/// change and shove the rest of the toolbar sideways.  Passing a `min_width`
/// wide enough for the control's widest label pins the width so neighbouring
/// controls never reflow.
pub fn transport_button_sized(
    ui: &mut egui::Ui,
    label: &str,
    fill: egui::Color32,
    enabled: bool,
    tip: &str,
    min_width: f32,
) -> bool {
    let (bg, text_color) = if enabled {
        (fill, egui::Color32::WHITE)
    } else {
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
    .min_size(egui::vec2(min_width, 30.0))
    .corner_radius(egui::CornerRadius::same(5));

    let clicked = ui.add(btn).on_hover_text(tip).clicked();
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

/// Format a byte count into a human-readable size (B / KB / MB / GB).
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}
