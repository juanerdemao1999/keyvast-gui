use super::super::*;

impl KvApp {
    pub(crate) fn render_sidebar(&mut self, ctx: &egui::Context) {
        // ── Left control panel ──────────────────────────────────
        egui::SidePanel::left("control_panel")
            .resizable(true)
            .default_width(240.0)
            .width_range(200.0..=350.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ctx, |ui| {
                let mut start = false;
                let mut stop = false;
                let mut toggle_rec = false;
                let mut dismiss_error = false;
                let mut start_impedance = false;
                let mut open_playback_file = false;
                let mut save_clicked = false;
                let mut load_clicked = false;

                // Compute elapsed recording seconds for the clock display.
                let rec_elapsed_secs = self
                    .recording_start_time
                    .map(|t| t.elapsed().as_secs_f64());
                let acq_running = self.is_running();
                let total_ch = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(16);
                let sr = self.latest_block.as_ref().map(|b| b.sample_rate).unwrap_or(30000.0);
                let prev_remote_enabled = self.remote_api_state.enabled;

                ui.set_min_width(220.0);

                // Tab strip grouping the sidebar sections by purpose.
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SidebarTab::Acquire, "ACQUIRE"),
                        (SidebarTab::Display, "DISPLAY"),
                        (SidebarTab::Tools, "TOOLS"),
                    ] {
                        ui.selectable_value(
                            &mut self.sidebar_tab,
                            tab,
                            egui::RichText::new(label).size(10.0).strong(),
                        );
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    match self.sidebar_tab {
                        SidebarTab::Acquire => {
                            panels::draw_acquire_core(
                                ui,
                                acq_running,
                                &mut self.device,
                                &mut start,
                                &mut stop,
                                &mut toggle_rec,
                                &mut self.recording,
                                self.latest_block.as_ref(),
                                rec_elapsed_secs,
                                self.recorder_buffer_occupancy,
                                self.recorder_dropped_blocks,
                                self.recording_error.as_deref(),
                                &mut dismiss_error,
                            );

                            ui.add_space(4.0);
                            trigger::draw_trigger_section(ui, &mut self.trigger);

                            ui.add_space(4.0);
                            egui::CollapsingHeader::new(
                                egui::RichText::new("DATA FORMAT")
                                    .size(11.0)
                                    .strong()
                                    .color(theme::TEXT_SECONDARY),
                            )
                            .default_open(false)
                            .show(ui, |ui| {
                                use kv_recorder::export_formats::ExportFormat;
                                ui.label(
                                    egui::RichText::new(
                                        "Recordings are saved in the native Keyvast .kvraw format. \
                                         Optionally convert a recording to another format below.",
                                    )
                                    .size(9.0)
                                    .color(theme::TEXT_DIM),
                                );
                                ui.add_space(2.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.selectable_value(
                                        &mut self.export_format,
                                        ExportFormat::KeyvastNative,
                                        egui::RichText::new("Keyvast .kvraw").size(10.0),
                                    );
                                    ui.selectable_value(
                                        &mut self.export_format,
                                        ExportFormat::IntanRhd,
                                        egui::RichText::new("Intan .rhd").size(10.0),
                                    );
                                    ui.selectable_value(
                                        &mut self.export_format,
                                        ExportFormat::FlatBinary,
                                        egui::RichText::new("Flat binary").size(10.0),
                                    );
                                });
                                ui.label(
                                    egui::RichText::new(self.export_format.label())
                                        .size(9.0)
                                        .italics()
                                        .color(theme::TEXT_DIM),
                                );
                                ui.add_space(2.0);
                                let exporting = self.export_rx.is_some();
                                if self.export_format.is_native() {
                                    ui.label(
                                        egui::RichText::new(
                                            "Native format — recordings are already saved as .kvraw. \
                                             Pick a third-party format above to convert.",
                                        )
                                        .size(9.0)
                                        .color(theme::TEXT_DIM),
                                    );
                                } else {
                                    if ui
                                        .add_enabled(
                                            !exporting,
                                            egui::Button::new(
                                                egui::RichText::new("Convert .kvraw…").size(10.0),
                                            ),
                                        )
                                        .on_hover_text(
                                            "Convert a .kvraw recording to the selected format",
                                        )
                                        .clicked()
                                        && let Some(path) = playback::pick_kvraw_file() {
                                            self.start_export(path);
                                        }
                                    if exporting {
                                        ui.label(
                                            egui::RichText::new("Converting…")
                                                .size(9.0)
                                                .color(theme::TEXT_DIM),
                                        );
                                    } else if let Some(ref status) = self.export_status {
                                        ui.label(
                                            egui::RichText::new(status)
                                                .size(9.0)
                                                .color(theme::TEXT_DIM),
                                        );
                                    }
                                }
                            });

                            ui.add_space(4.0);
                            self.channel_select.sync_channel_count(total_ch);
                            ui.label(
                                egui::RichText::new(format!(
                                    "Recording {} of {} channels \u{00B7} configure in DISPLAY \u{25B8} CHANNELS",
                                    self.channel_select.selected_count(),
                                    total_ch,
                                ))
                                .size(theme::FONT_CAPTION)
                                .color(theme::TEXT_DIM),
                            );
                        }
                        SidebarTab::Display => {
                            panels::draw_display_settings(ui, &mut self.display);

                            ui.add_space(4.0);
                            panels::draw_filter_settings(ui, &mut self.filters);

                            ui.add_space(4.0);
                            channel_select::draw_unified_channels(
                                ui,
                                &mut self.display,
                                &mut self.channel_select,
                                self.latest_block.as_ref(),
                                self.recording.state == RecordingState::Recording,
                            );

                            ui.add_space(4.0);
                            channel_map::draw_channel_map_section(
                                ui,
                                &mut self.channel_map,
                                &mut self.display,
                                total_ch,
                            );

                            ui.add_space(4.0);
                            fft_panel::draw_fft_section(ui, &mut self.fft, sr, total_ch);
                        }
                        SidebarTab::Tools => {
                            let can_measure = self.device.kind == DeviceKind::Rhd
                                && self.device.rhd_bitfile.is_some();
                            impedance_panel::draw_impedance_section(
                                ui,
                                &mut self.impedance,
                                can_measure,
                                &mut start_impedance,
                            );

                            ui.add_space(4.0);
                            playback::draw_playback_section(
                                ui,
                                &mut self.playback_mgr,
                                &mut open_playback_file,
                            );

                            ui.add_space(4.0);
                            remote_api::draw_remote_api_section(
                                ui,
                                &mut self.remote_api_state,
                            );

                            ui.add_space(4.0);
                            config_persist::draw_config_section(
                                ui,
                                &mut self.config_persist,
                                &mut self.ui_scale,
                                &mut save_clicked,
                                &mut load_clicked,
                            );
                        }
                    }
                });

                if dismiss_error {
                    self.recording_error = None;
                }
                if start_impedance {
                    self.start_impedance_test();
                }
                if start {
                    match self.mode {
                        AcqMode::Demo => self.start_demo(),
                        AcqMode::Device => self.start_device(),
                    }
                }
                if stop {
                    self.stop_all();
                }
                if toggle_rec {
                    self.toggle_recording();
                }

                // Handle playback file open (outside borrow scope). Switches
                // the top-level source to Playback so the file actually drives
                // the display (live acquisition is stopped first).
                if open_playback_file {
                    self.open_playback_file_dialog();
                }

                // Start/stop remote API server based on enabled toggle
                let prev_enabled = prev_remote_enabled;
                if self.remote_api_state.enabled && !prev_enabled {
                    match remote_api::start_server(self.remote_api_state.port) {
                        Ok(handle) => {
                            self.remote_api_handle = Some(handle);
                            self.remote_api_state.running = true;
                            self.remote_api_state.error = None;
                        }
                        Err(e) => {
                            self.remote_api_state.error = Some(e);
                            self.remote_api_state.enabled = false;
                        }
                    }
                } else if !self.remote_api_state.enabled && prev_enabled {
                    if let Some(mut handle) = self.remote_api_handle.take() {
                        handle.stop();
                    }
                    self.remote_api_state.running = false;
                }
                // Update client count
                if let Some(ref handle) = self.remote_api_handle {
                    self.remote_api_state.client_count =
                        *remote_api::lock_recover(&handle.client_count);
                }

                if save_clicked {
                    let cfg = self.capture_persistent(ctx);
                    match config_persist::save_config(&self.config_persist.config_path, &cfg) {
                        Ok(()) => {
                            self.config_persist.status_message = Some("Saved".to_string());
                            self.toasts.success("Configuration saved");
                        }
                        Err(e) => {
                            self.toasts.error(format!("Save failed: {e}"));
                            self.config_persist.status_message = Some(e);
                        }
                    }
                }
                if load_clicked {
                    match config_persist::load_config(&self.config_persist.config_path) {
                        Ok(cfg) => {
                            cfg.apply_to(
                                &mut self.display,
                                &mut self.filters,
                                &mut self.recording.output_dir,
                                &mut self.recording.file_prefix,
                                &mut self.remote_api_state.port,
                            );
                            self.ui_scale = cfg
                                .ui_scale
                                .clamp(config_persist::UI_SCALE_MIN, config_persist::UI_SCALE_MAX);
                            self.config_persist.status_message = Some("Loaded".to_string());
                            self.config_persist.loaded = true;
                            self.toasts.success("Configuration loaded");
                        }
                        Err(e) => {
                            self.toasts.error(format!("Load failed: {e}"));
                            self.config_persist.status_message = Some(e);
                        }
                    }
                }
            });
    }
}
