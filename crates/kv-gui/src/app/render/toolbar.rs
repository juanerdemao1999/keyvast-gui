use super::super::*;

impl KvApp {
    pub(crate) fn render_toolbar(&mut self, ctx: &egui::Context, elapsed: f64) {
        // ── Top toolbar ─────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_TOOLBAR)
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Brand
                    ui.label(
                        egui::RichText::new("KEYVAST")
                            .size(16.0)
                            .strong()
                            .color(theme::ACCENT_BLUE),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Acquisition System")
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // Transport buttons.  In Playback mode the live transport
                    // is replaced by playback controls so only one source is
                    // ever driven from the toolbar.
                    let running = self.is_running();
                    let mut open_playback = false;
                    if self.data_source == DataSource::Playback {
                        let loaded = self.playback_mgr.is_loaded();
                        let playing = self.playback_mgr.state == playback::PlaybackState::Playing;
                        if theme::transport_button_sized(
                            ui,
                            if playing { " Pause " } else { "  Play  " },
                            if playing {
                                theme::TEXT_SECONDARY
                            } else {
                                theme::BTN_PLAY
                            },
                            loaded,
                            "Play / pause the recording (Space)",
                            88.0,
                        ) {
                            self.playback_mgr.toggle_play_pause();
                        }
                        if theme::transport_button_tip(
                            ui,
                            " Restart ",
                            theme::BG_WIDGET,
                            loaded,
                            "Jump back to the start of the recording",
                        ) {
                            self.playback_mgr.seek_to(0);
                        }
                        ui.add_space(8.0);
                        if loaded {
                            let name = self
                                .playback_mgr
                                .file_path
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .unwrap_or("recording")
                                .to_string();
                            ui.label(theme::caption(format!("\u{25B6} {name}")));
                        } else if theme::transport_button_tip(
                            ui,
                            " Open\u{2026} ",
                            theme::BTN_PLAY,
                            true,
                            "Open a .kvraw recording to play back",
                        ) {
                            open_playback = true;
                        }
                    } else {
                        if theme::transport_button_sized(
                            ui,
                            if running { "  Stop  " } else { "  Start  " },
                            if running {
                                theme::BTN_STOP
                            } else {
                                theme::BTN_PLAY
                            },
                            true,
                            "Start / stop acquisition (Space)",
                            88.0,
                        ) {
                            self.toggle_acquisition();
                        }

                        // Record button — always clickable when running.
                        let rec_label = match self.recording.state {
                            RecordingState::Idle => " Record ",
                            RecordingState::Armed => " ARMED ",
                            RecordingState::Recording => " STOP REC ",
                        };
                        let rec_color = match self.recording.state {
                            RecordingState::Idle => theme::BTN_RECORD,
                            RecordingState::Armed => theme::ACCENT_YELLOW,
                            RecordingState::Recording => theme::BTN_RECORD_ACTIVE,
                        };
                        let rec_enabled = running || self.recording.state != RecordingState::Idle;
                        let rec_tip = match self.recording.state {
                            RecordingState::Idle => "Arm recording (R)",
                            RecordingState::Armed => "Begin recording (R)",
                            RecordingState::Recording => "Stop recording (R)",
                        };
                        // Fixed width fits the widest label ("STOP REC") so
                        // arming / recording never shoves the toolbar sideways.
                        if theme::transport_button_sized(
                            ui,
                            rec_label,
                            rec_color,
                            rec_enabled,
                            rec_tip,
                            104.0,
                        ) {
                            self.toggle_recording();
                        }

                        // Pause button — only shown when running or already paused.
                        if running || self.display_paused {
                            let pause_label = if self.display_paused {
                                " Resume "
                            } else {
                                " Pause "
                            };
                            let pause_color = if self.display_paused {
                                theme::ACCENT_BLUE
                            } else {
                                theme::TEXT_SECONDARY
                            };
                            if theme::transport_button_sized(
                                ui,
                                pause_label,
                                pause_color,
                                true,
                                "Freeze / resume the display; acquisition continues (P)",
                                88.0,
                            ) {
                                self.toggle_pause_display();
                            }
                        }
                    }
                    if open_playback {
                        self.open_playback_file_dialog();
                    }

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // Data-source selector (B1): one switch for Demo / Device /
                    // Playback so it is always clear where the signal comes from.
                    ui.label(
                        egui::RichText::new("Source:")
                            .size(theme::FONT_BODY)
                            .color(theme::TEXT_DIM),
                    );
                    let device_tip = match self.device.kind {
                        DeviceKind::Simulator => "Live acquisition \u{2014} Simulator backend",
                        DeviceKind::Rhd => {
                            "Live acquisition \u{2014} RHD hardware (set bitfile in DEVICE panel)"
                        }
                    };
                    // Segmented control: the three sources sit flush inside one
                    // recessed frame so they read as a single "pick one" switch
                    // rather than three independent buttons.
                    let mut pick: Option<DataSource> = None;
                    egui::Frame::new()
                        .fill(theme::BG_DARKEST)
                        .corner_radius(egui::CornerRadius::same(5))
                        .inner_margin(egui::Margin::same(2))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (src, label, tip) in [
                                (
                                    DataSource::Demo,
                                    "Demo",
                                    "Synthetic neural data \u{2014} no hardware",
                                ),
                                (DataSource::Device, "Device", device_tip),
                                (
                                    DataSource::Playback,
                                    "Playback",
                                    "Replay a saved .kvraw recording",
                                ),
                            ] {
                                let selected = self.data_source == src;
                                if ui
                                    .add_sized(
                                        [66.0, 22.0],
                                        egui::SelectableLabel::new(
                                            selected,
                                            egui::RichText::new(label).size(theme::FONT_HEADING),
                                        ),
                                    )
                                    .on_hover_text(tip)
                                    .clicked()
                                    && !selected
                                {
                                    pick = Some(src);
                                }
                            }
                        });
                    if let Some(src) = pick {
                        self.select_source(src);
                    }

                    ui.add_space(8.0);
                    if ui
                        .button(
                            egui::RichText::new(" ? ")
                                .size(theme::FONT_HEADING)
                                .strong(),
                        )
                        .on_hover_text("Keyboard shortcuts")
                        .clicked()
                    {
                        self.show_help = !self.show_help;
                    }

                    // Right-aligned: live status pill + clock + version
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("v0.2.0")
                                .size(9.0)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(10.0);

                        // Acquisition clock — colored by state.
                        let clock_color = if self.recording.state == RecordingState::Recording {
                            theme::ACCENT_RED
                        } else if running {
                            theme::ACCENT_YELLOW
                        } else {
                            theme::TEXT_DIM
                        };
                        ui.label(
                            egui::RichText::new(theme::format_clock(elapsed))
                                .size(15.0)
                                .monospace()
                                .strong()
                                .color(clock_color),
                        );

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);

                        // At-a-glance state pill: REC / ARMED / LIVE / IDLE.
                        let (dot, label, color) = match self.recording.state {
                            RecordingState::Recording => {
                                (theme::STATUS_RECORDING, "REC", theme::ACCENT_RED)
                            }
                            RecordingState::Armed => {
                                (theme::STATUS_ARMED, "ARMED", theme::ACCENT_YELLOW)
                            }
                            RecordingState::Idle if running => {
                                (theme::STATUS_CONNECTED, "LIVE", theme::ACCENT_GREEN)
                            }
                            RecordingState::Idle => {
                                (theme::STATUS_IDLE, "IDLE", theme::TEXT_SECONDARY)
                            }
                        };
                        // In a right-to-left layout, add the label first so the
                        // status dot lands to its left, reading "● LABEL".
                        // Fixed-width label keeps the dot from hopping as the
                        // state text (REC / ARMED / LIVE / IDLE) changes width.
                        ui.add_sized(
                            [44.0, 16.0],
                            egui::Label::new(
                                egui::RichText::new(label).size(12.0).strong().color(color),
                            ),
                        );
                        ui.add_space(5.0);
                        theme::status_dot(ui, dot);
                    });
                });
            });

        // ── Device error banner ─────────────────────────────────
        // Surfaced when the acquisition source fails to open or read.
        // Dismissible; the GUI and any other mode keep running regardless.
        // Borrow the message instead of cloning it every frame; record the
        // dismiss action in a local and apply it after the panel closure ends.
        let mut dismiss_device_error = false;
        if let Some(err) = self.device_error.as_ref() {
            egui::TopBottomPanel::top("device_error_banner")
                .frame(
                    egui::Frame::new()
                        .fill(theme::ACCENT_RED)
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("\u{26A0} Device error: {err}"))
                                .size(12.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Dismiss").clicked() {
                                dismiss_device_error = true;
                            }
                        });
                    });
                });
        }
        if dismiss_device_error {
            self.device_error = None;
        }
    }

    pub(crate) fn render_status_bar(&mut self, ctx: &egui::Context, elapsed: f64) {
        // ── Bottom status bar ───────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_TOOLBAR)
                    .inner_margin(egui::Margin::symmetric(8, 3)),
            )
            .show(ctx, |ui| {
                panels::draw_status_bar(
                    ui,
                    self.is_running(),
                    &self.recording,
                    self.latest_stats.as_ref(),
                    self.latest_block.as_ref(),
                    elapsed,
                );
            });
    }
}
