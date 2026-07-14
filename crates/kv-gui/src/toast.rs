//! Lightweight toast notification system (B5).
//!
//! Transient, non-blocking messages stacked in the top-right corner of the
//! window.  Info / success / warning toasts auto-dismiss after a short delay;
//! error toasts are *sticky* (stay until the user dismisses them) so a failure
//! is never missed.  Each toast has a colored accent bar matching its level
//! and a close button.
//!
//! Usage:
//! ```ignore
//! toasts.success("Recording saved");
//! toasts.error("Disk full");
//! // once per frame, after the rest of the UI:
//! toasts.show(ctx);
//! ```

use std::time::{Duration, Instant};

use eframe::egui;

use crate::theme;

/// Severity of a toast, controlling its color, icon, and default lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    fn accent(self) -> egui::Color32 {
        match self {
            ToastLevel::Info => theme::ACCENT_BLUE,
            ToastLevel::Success => theme::ACCENT_GREEN,
            ToastLevel::Warning => theme::ACCENT_YELLOW,
            ToastLevel::Error => theme::ACCENT_RED,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            ToastLevel::Info => "\u{2139}",    // ℹ
            ToastLevel::Success => "\u{2714}", // ✔
            ToastLevel::Warning => "\u{26A0}", // ⚠
            ToastLevel::Error => "\u{2716}",   // ✖
        }
    }

    /// Default lifetime; `None` means the toast is sticky until dismissed.
    fn default_duration(self) -> Option<Duration> {
        match self {
            ToastLevel::Error => None,
            ToastLevel::Warning => Some(Duration::from_secs(6)),
            _ => Some(Duration::from_secs(4)),
        }
    }
}

/// A single notification.
struct Toast {
    message: String,
    level: ToastLevel,
    created: Instant,
    duration: Option<Duration>,
}

/// Stack of active toasts.  Drop-in: construct with `Default`, push messages,
/// and call [`Toasts::show`] once per frame.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    /// Push a toast with the level's default lifetime.
    pub fn push(&mut self, level: ToastLevel, message: impl Into<String>) {
        let message = message.into();
        // Coalesce: if the most recent toast is identical and still alive,
        // refresh it instead of stacking duplicates.
        if let Some(last) = self.items.last_mut()
            && last.level == level
            && last.message == message
        {
            last.created = Instant::now();
            return;
        }
        self.items.push(Toast {
            message,
            level,
            created: Instant::now(),
            duration: level.default_duration(),
        });
        // Cap the stack so a runaway producer cannot fill the screen.
        const MAX_TOASTS: usize = 5;
        if self.items.len() > MAX_TOASTS {
            self.items.remove(0);
        }
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Info, message);
    }

    pub fn success(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Success, message);
    }

    pub fn warning(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Warning, message);
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.push(ToastLevel::Error, message);
    }

    /// Render the stack and prune expired toasts.  Call once per frame.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();

        // Drop timed-out toasts (sticky errors have duration == None).
        self.items.retain(|t| match t.duration {
            Some(d) => now.duration_since(t.created) < d,
            None => true,
        });

        if self.items.is_empty() {
            return;
        }

        // Keep animating smoothly so timed toasts disappear and the slide-in
        // plays even if nothing else requests a repaint.
        ctx.request_repaint_after(Duration::from_millis(33));

        let mut dismissed: Option<usize> = None;

        egui::Area::new(egui::Id::new("kv_toast_area"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 12.0))
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_max_width(300.0);
                for (idx, toast) in self.items.iter().enumerate() {
                    let accent = toast.level.accent();

                    // Slide-in + fade from the right edge over ~160 ms (#14).
                    let age = now.duration_since(toast.created).as_secs_f32();
                    let t = (age / 0.16).clamp(0.0, 1.0);
                    let eased = 1.0 - (1.0 - t) * (1.0 - t); // ease-out quad
                    let off = ((1.0 - eased) * 40.0).round() as i8;

                    let frame_resp = egui::Frame::new()
                        .fill(theme::BG_PANEL)
                        .stroke(egui::Stroke::new(1.0_f32, accent))
                        .corner_radius(egui::CornerRadius::same(5))
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .outer_margin(egui::Margin {
                            bottom: 6,
                            right: -off, // negative pushes the card off the right edge, then settles
                            ..egui::Margin::ZERO
                        })
                        .show(ui, |ui| {
                            ui.set_opacity(eased.max(0.05));
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(toast.level.icon())
                                        .size(theme::FONT_HEADING)
                                        .color(accent),
                                );
                                ui.add_space(2.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&toast.message)
                                            .size(theme::FONT_BODY)
                                            .color(theme::TEXT_PRIMARY),
                                    )
                                    .wrap(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Min),
                                    |ui| {
                                        if ui
                                            .small_button(
                                                egui::RichText::new("\u{2715}")
                                                    .size(theme::FONT_CAPTION)
                                                    .color(theme::TEXT_DIM),
                                            )
                                            .clicked()
                                        {
                                            dismissed = Some(idx);
                                        }
                                    },
                                );
                            });
                        });

                    // Click anywhere on the card (not just the ✕) to dismiss.
                    let click = frame_resp
                        .response
                        .interact(egui::Sense::click())
                        .on_hover_text("Click to dismiss");
                    if click.clicked() {
                        dismissed = Some(idx);
                    }
                }
            });

        if let Some(idx) = dismissed
            && idx < self.items.len()
        {
            self.items.remove(idx);
        }
    }
}
