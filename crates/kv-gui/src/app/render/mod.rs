use super::*;

mod central;
mod sidebar;
mod toolbar;

impl KvApp {
    /// Handle keyboard shortcuts.
    pub(crate) fn handle_keys(&mut self, ctx: &egui::Context) {
        // Only when no text field is focused
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }

        // Snapshot the source outside the input closure to avoid borrowing
        // self twice.
        let playback = self.data_source == DataSource::Playback;

        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                if playback {
                    self.playback_mgr.toggle_play_pause();
                } else {
                    self.toggle_acquisition();
                }
            }
            // Recording is a live-acquisition action only.
            if i.key_pressed(egui::Key::R) && !playback {
                self.toggle_recording();
            }
            // Toggle the shortcut cheat-sheet overlay.
            if i.key_pressed(egui::Key::Questionmark) || i.key_pressed(egui::Key::F1) {
                self.show_help = !self.show_help;
            }
            if i.key_pressed(egui::Key::Escape) {
                self.show_help = false;
            }
            if i.key_pressed(egui::Key::G) {
                self.display.show_grid = !self.display.show_grid;
            }
            if i.key_pressed(egui::Key::P) {
                self.toggle_pause_display();
            }
            if i.key_pressed(egui::Key::F) {
                self.show_perf_overlay = !self.show_perf_overlay;
            }
            // [ / ] for time window prev/next
            if i.key_pressed(egui::Key::OpenBracket) {
                let idx = self.display.time_scale_idx.saturating_sub(1);
                self.display.time_scale_idx = idx;
            }
            if i.key_pressed(egui::Key::CloseBracket) {
                let max_idx = panels::TIME_WINDOWS.len() - 1;
                self.display.time_scale_idx = (self.display.time_scale_idx + 1).min(max_idx);
            }
            // 1-9: quick channel count (multiply by 4)
            for (key, num) in [
                (egui::Key::Num1, 4),
                (egui::Key::Num2, 8),
                (egui::Key::Num3, 12),
                (egui::Key::Num4, 16),
                (egui::Key::Num5, 20),
                (egui::Key::Num6, 24),
                (egui::Key::Num7, 28),
                (egui::Key::Num8, 32),
                (egui::Key::Num9, 36),
            ] {
                if i.key_pressed(key) {
                    self.display.visible_channels = num;
                }
            }
            // +/- for channel spacing
            if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                self.display.channel_spacing =
                    (self.display.channel_spacing + panels::SPACING_STEP).min(panels::SPACING_MAX);
            }
            if i.key_pressed(egui::Key::Minus) {
                self.display.channel_spacing =
                    (self.display.channel_spacing - panels::SPACING_STEP).max(panels::SPACING_MIN);
            }
        });
    }

    /// Draw the keyboard-shortcut cheat-sheet overlay (B4).  Shown on demand
    /// via the toolbar `?` button or the `?` / F1 key; dismissed with Esc, a
    /// click outside, or the close button.
    ///
    /// Rendered as an [`egui::Modal`] so it sits on a dimmed backdrop — the busy
    /// running waveform behind it is darkened, keeping the shortcut list legible
    /// instead of competing with the moving traces.
    pub(crate) fn draw_help_overlay(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("kv_help_modal"))
            // Darken the running waveform clearly so the dialog reads as modal.
            .backdrop_color(egui::Color32::from_black_alpha(160))
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Keyboard shortcuts")
                            .size(theme::FONT_HEADING)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(egui::RichText::new("\u{2715}").size(theme::FONT_BODY))
                            .on_hover_text("Close (Esc)")
                            .clicked()
                        {
                            self.show_help = false;
                        }
                    });
                });
                ui.separator();
                ui.add_space(2.0);
                let rows = [
                    (
                        "Space",
                        "Start / stop acquisition (play / pause in Playback)",
                    ),
                    ("R", "Arm \u{2192} record \u{2192} stop recording"),
                    ("P", "Pause / resume the display (acquisition continues)"),
                    ("G", "Toggle the waveform grid"),
                    ("F", "Toggle the performance overlay"),
                    ("[  ]", "Decrease / increase the time window"),
                    (
                        "1 \u{2013} 9",
                        "Quick-set visible channel count (\u{00D7}4)",
                    ),
                    ("+  \u{2212}", "Increase / decrease channel spacing"),
                    ("?  /  F1", "Toggle this help (Esc to close)"),
                ];
                egui::Grid::new("kv_help_grid")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        for (key, desc) in rows {
                            ui.label(
                                egui::RichText::new(key)
                                    .size(theme::FONT_BODY)
                                    .strong()
                                    .monospace()
                                    .color(theme::ACCENT_BLUE),
                            );
                            ui.label(theme::body(desc));
                            ui.end_row();
                        }
                    });
            });
        if modal.should_close() {
            self.show_help = false;
        }
    }
}
