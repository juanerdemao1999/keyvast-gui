//! Side panels and status bar for the Keyvast professional GUI.
//!
//! Layout follows Intan RHX / Open Ephys patterns:
//!   Left panel: Device info, Display controls, Channel list, Recording.
//!   Bottom: Status bar with acquisition clock, data rate, buffer health.

use eframe::egui;
use kv_types::SampleBlock;

use crate::preview::BlockStats;
use crate::theme;

mod format;
mod settings;

use crate::theme::format_bytes;
use format::{format_large_number, format_time_window, format_uv};
pub use settings::*;

// ── Left control panel ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_acquire_core(
    ui: &mut egui::Ui,
    acquiring: bool,
    device: &mut DeviceSettings,
    start_clicked: &mut bool,
    stop_clicked: &mut bool,
    toggle_rec: &mut bool,
    recording: &mut RecordingSettings,
    block: Option<&SampleBlock>,
    // Elapsed recording wall-clock seconds (None when not recording).
    rec_elapsed_secs: Option<f64>,
    // Recorder buffer fill level 0.0..=1.0 (from live pipeline).
    buffer_occupancy: f64,
    // Cumulative blocks dropped by fanout-buffer overflow this session.
    dropped_blocks: u64,
    // Last recorder error message, if any.
    recording_error: Option<&str>,
    // Set to true by the panel when the user clicks "dismiss error".
    dismiss_error: &mut bool,
) {
    // Primary actions first: connect → acquire → record.
    draw_device_section(ui, acquiring, device, block);
    ui.add_space(4.0);
    draw_acquisition_controls(ui, acquiring, start_clicked, stop_clicked);
    ui.add_space(4.0);
    draw_recording_section(
        ui,
        recording,
        acquiring,
        toggle_rec,
        rec_elapsed_secs,
        buffer_occupancy,
        dropped_blocks,
        recording_error,
        dismiss_error,
        block,
    );
}

// ── Device info ─────────────────────────────────────────────────────

fn draw_device_section(
    ui: &mut egui::Ui,
    connected: bool,
    device: &mut DeviceSettings,
    block: Option<&SampleBlock>,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("DEVICE")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(true)
    .show(ui, |ui| {
        ui.horizontal(|ui| {
            if connected {
                theme::status_dot(ui, theme::STATUS_CONNECTED);
                ui.label(
                    egui::RichText::new("Connected")
                        .size(11.0)
                        .color(theme::STATUS_CONNECTED),
                );
            } else {
                theme::status_dot(ui, theme::STATUS_IDLE);
                ui.label(
                    egui::RichText::new("Disconnected")
                        .size(11.0)
                        .color(theme::STATUS_IDLE),
                );
            }
        });

        ui.add_space(4.0);

        // Source selector + RHD configuration. Disabled while acquiring so the
        // backend cannot change mid-run.
        ui.add_enabled_ui(!connected, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Source")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
                ui.selectable_value(&mut device.kind, DeviceKind::Simulator, "Simulator");
                ui.selectable_value(&mut device.kind, DeviceKind::Rhd, "RHD");
            });

            if device.kind == DeviceKind::Rhd {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("Bitfile\u{2026}").clicked() {
                        let mut dialog =
                            rfd::FileDialog::new().add_filter("FPGA bitfile", &["bit"]);
                        if let Some(dir) = device.rhd_bitfile.as_ref().and_then(|p| p.parent()) {
                            dialog = dialog.set_directory(dir);
                        }
                        if let Some(path) = dialog.pick_file() {
                            device.rhd_bitfile = Some(path);
                        }
                    }
                    let (label, color) = match device.rhd_bitfile.as_ref() {
                        Some(path) => (
                            path.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string()),
                            theme::TEXT_SECONDARY,
                        ),
                        None => ("(no bitfile)".to_string(), theme::ACCENT_YELLOW),
                    };
                    let hover = device
                        .rhd_bitfile
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Select an FPGA bitfile to upload".to_string());
                    ui.label(egui::RichText::new(label).size(10.0).color(color))
                        .on_hover_text(hover);
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Headstages")
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                    ui.selectable_value(&mut device.rhd_streams, 1, "1 (32ch)");
                    ui.selectable_value(&mut device.rhd_streams, 2, "2 (64ch)");
                });
            }
        });

        ui.add_space(4.0);

        if let Some(b) = block {
            theme::kv_label(ui, "Device ID", &b.device_id);
            theme::kv_label(ui, "Sample Rate", &format!("{:.0} Hz", b.sample_rate));
            theme::kv_label(ui, "Channels", &b.channel_count.to_string());
            theme::kv_label(ui, "Samples/Pkt", &b.samples_per_channel.to_string());
        } else {
            theme::kv_label(ui, "Device ID", "\u{2014}");
            theme::kv_label(ui, "Sample Rate", "\u{2014}");
            theme::kv_label(ui, "Channels", "\u{2014}");
        }
        let backend_label = match device.kind {
            DeviceKind::Simulator => "Simulator",
            DeviceKind::Rhd => "RHD (Opal Kelly)",
        };
        theme::kv_label(ui, "Backend", backend_label);
    });
}

// ── Acquisition controls ────────────────────────────────────────────

fn draw_acquisition_controls(
    ui: &mut egui::Ui,
    acquiring: bool,
    start_clicked: &mut bool,
    stop_clicked: &mut bool,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("ACQUISITION")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(true)
    .show(ui, |ui| {
        ui.horizontal(|ui| {
            if theme::transport_button(ui, "  Start  ", theme::BTN_PLAY, !acquiring) {
                *start_clicked = true;
            }
            if theme::transport_button(ui, "  Stop  ", theme::BTN_STOP, acquiring) {
                *stop_clicked = true;
            }
        });

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if acquiring {
                theme::status_dot(ui, theme::ACCENT_GREEN);
                ui.label(
                    egui::RichText::new("ACQUIRING")
                        .size(11.0)
                        .strong()
                        .color(theme::ACCENT_GREEN),
                );
            } else {
                theme::status_dot(ui, theme::STATUS_IDLE);
                ui.label(
                    egui::RichText::new("IDLE")
                        .size(11.0)
                        .color(theme::STATUS_IDLE),
                );
            }
        });
    });
}

// ── Display settings ────────────────────────────────────────────────

pub fn draw_display_settings(ui: &mut egui::Ui, display: &mut DisplaySettings) {
    egui::CollapsingHeader::new(
        egui::RichText::new("DISPLAY")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(true)
    .show(ui, |ui| {
        // Visible channels — slider
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Channels")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            let mut ch = display.visible_channels as i32;
            if ui
                .add(
                    egui::Slider::new(&mut ch, 1..=64)
                        .step_by(1.0)
                        .trailing_fill(true),
                )
                .changed()
            {
                display.visible_channels = ch.max(1) as usize;
            }
        });

        // Time window — dropdown
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Time")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            egui::ComboBox::from_id_salt("time_scale")
                .width(ui.available_width() - 4.0)
                .selected_text(
                    egui::RichText::new(format_time_window(display.time_window_secs()))
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT_PRIMARY),
                )
                .show_ui(ui, |ui| {
                    for (i, &secs) in TIME_WINDOWS.iter().enumerate() {
                        let label = format_time_window(secs);
                        ui.selectable_value(&mut display.time_scale_idx, i, &label);
                    }
                });
        });

        // Amplitude scale — dropdown
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Amp").size(10.0).color(theme::TEXT_DIM));
            egui::ComboBox::from_id_salt("amp_scale")
                .width(ui.available_width() - 4.0)
                .selected_text(
                    egui::RichText::new(format_uv(display.amp_scale_uv()))
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT_PRIMARY),
                )
                .show_ui(ui, |ui| {
                    for (i, &uv) in AMP_SCALES.iter().enumerate() {
                        ui.selectable_value(&mut display.amp_scale_idx, i, format_uv(uv));
                    }
                });
        });

        // Channel spacing — slider
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Spacing")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::Slider::new(&mut display.channel_spacing, SPACING_MIN..=SPACING_MAX)
                    .step_by(SPACING_STEP)
                    .trailing_fill(true),
            );
        });

        ui.add_space(2.0);

        // Toggles
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut display.show_grid,
                egui::RichText::new("Grid").size(10.0),
            );
            ui.checkbox(
                &mut display.show_channel_labels,
                egui::RichText::new("Labels").size(10.0),
            );
            ui.checkbox(
                &mut display.hover_highlight,
                egui::RichText::new("Hover hl").size(10.0),
            )
            .on_hover_text("Highlight hovered channel, dim others");
        });

        // Display mode — Sweep vs Roll
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Mode")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.selectable_value(
                &mut display.display_mode,
                DisplayMode::Sweep,
                egui::RichText::new("Sweep").size(10.0),
            )
            .on_hover_text("Fixed window, cursor sweeps right (SpikeGLX/Intan RHX style)");
            ui.selectable_value(
                &mut display.display_mode,
                DisplayMode::Roll,
                egui::RichText::new("Roll").size(10.0),
            )
            .on_hover_text("Continuous scrolling, latest data on the right");
        });

        // Channel colors
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut display.color_by_group,
                egui::RichText::new("Group colors").size(10.0),
            )
            .on_hover_text("Color channels by group (cycling 8-color palette)");
            if display.color_by_group {
                let mut g = display.channels_per_group as i32;
                if ui
                    .add(
                        egui::DragValue::new(&mut g)
                            .range(1..=64)
                            .speed(0.5)
                            .prefix("per ")
                            .suffix(" ch"),
                    )
                    .changed()
                {
                    display.channels_per_group = g.max(1) as usize;
                }
            }
        });

        // Browse step — how far each scroll click moves when paused
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Browse step")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::Slider::new(&mut display.browse_step_pct, 1.0_f64..=100.0)
                    .suffix("%")
                    .step_by(1.0)
                    .trailing_fill(true),
            )
            .on_hover_text("How far each scroll step moves when paused (% of time window)");
        });
    });
}

// ── Filter / signal-processing settings UI ──────────────────────────

pub fn draw_filter_settings(ui: &mut egui::Ui, filters: &mut FilterSettings) {
    egui::CollapsingHeader::new(
        egui::RichText::new("FILTERS")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        // High-pass
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut filters.hp_enabled,
                egui::RichText::new("HP").size(10.0).strong(),
            );
            ui.add(
                egui::DragValue::new(&mut filters.hp_cutoff_hz)
                    .speed(1.0)
                    .range(0.1..=10_000.0)
                    .suffix(" Hz"),
            );
        });

        // Low-pass
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut filters.lp_enabled,
                egui::RichText::new("LP").size(10.0).strong(),
            );
            ui.add(
                egui::DragValue::new(&mut filters.lp_cutoff_hz)
                    .speed(1.0)
                    .range(1.0..=15_000.0)
                    .suffix(" Hz"),
            );
        });

        // Notch
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut filters.notch_enabled,
                egui::RichText::new("Notch").size(10.0).strong(),
            );
            for (i, &f) in NOTCH_FREQS.iter().enumerate() {
                ui.selectable_value(
                    &mut filters.notch_idx,
                    i,
                    egui::RichText::new(format!("{}Hz", f as u32)).size(10.0),
                );
            }
        });

        ui.add_space(2.0);

        // Common Average Reference
        ui.checkbox(
            &mut filters.car_enabled,
            egui::RichText::new("CAR (Common Avg Ref)").size(10.0),
        );

        // Spike threshold
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut filters.spike_threshold_enabled,
                egui::RichText::new("Spike σ").size(10.0),
            );
            ui.add(
                egui::DragValue::new(&mut filters.spike_threshold_sigma)
                    .speed(0.1)
                    .range(1.0..=20.0)
                    .suffix("σ"),
            );
        });

        if filters.hp_enabled || filters.lp_enabled || filters.notch_enabled {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Display only — recording is raw")
                    .size(9.0)
                    .italics()
                    .color(theme::TEXT_DIM),
            );
        }
    });
}

// ── Recording section ───────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn draw_recording_section(
    ui: &mut egui::Ui,
    recording: &mut RecordingSettings,
    acquiring: bool,
    toggle_rec: &mut bool,
    rec_elapsed_secs: Option<f64>,
    buffer_occupancy: f64,
    dropped_blocks: u64,
    recording_error: Option<&str>,
    dismiss_error: &mut bool,
    block: Option<&SampleBlock>,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("RECORDING")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(true)
    .show(ui, |ui| {
        // Recording state indicator
        ui.horizontal(|ui| {
            let (dot_color, label, label_color) = match recording.state {
                RecordingState::Idle => (theme::STATUS_IDLE, "Idle", theme::STATUS_IDLE),
                RecordingState::Armed => (theme::STATUS_ARMED, "Armed", theme::STATUS_ARMED),
                RecordingState::Recording => (
                    theme::STATUS_RECORDING,
                    "Recording",
                    theme::STATUS_RECORDING,
                ),
            };
            theme::status_dot(ui, dot_color);
            ui.label(egui::RichText::new(label).size(11.0).color(label_color));
        });

        // Output directory — text field + folder picker button.
        // Locked while recording so the write target cannot change mid-run
        // (the file layout is fixed the moment recording starts — B2).
        let recording_active = recording.state == RecordingState::Recording;
        ui.add_enabled_ui(!recording_active, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Dir:")
                        .size(theme::FONT_BODY)
                        .color(theme::TEXT_DIM),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut recording.output_dir)
                        .desired_width(110.0)
                        .font(egui::FontId::monospace(theme::FONT_BODY)),
                );
                if ui
                    .button(egui::RichText::new("📁").size(13.0))
                    .on_hover_text("Browse for output folder")
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("Select recording output folder")
                        .pick_folder()
                {
                    recording.output_dir = path.to_string_lossy().into_owned();
                }
            });
        });
        if recording_active {
            ui.label(
                egui::RichText::new("\u{1F512} Output locked while recording")
                    .size(theme::FONT_CAPTION)
                    .color(theme::TEXT_DIM),
            );
        }

        // Arm / Record / Stop buttons — all go through toggle_rec so that
        // app.rs::toggle_recording() handles the actual state transitions
        // (creating / finishing the StreamingRecorder).
        ui.horizontal(|ui| match recording.state {
            RecordingState::Idle => {
                if theme::transport_button(ui, " Arm ", theme::ACCENT_YELLOW, acquiring) {
                    *toggle_rec = true;
                }
            }
            RecordingState::Armed => {
                if theme::transport_button(ui, "Record", theme::BTN_RECORD, true) {
                    *toggle_rec = true;
                }
                if ui
                    .button(egui::RichText::new("Disarm").size(11.0))
                    .clicked()
                {
                    // Disarm: go directly back to Idle without creating a file
                    recording.state = RecordingState::Idle;
                }
            }
            RecordingState::Recording => {
                if theme::transport_button(ui, "Stop Rec", theme::BTN_STOP, true) {
                    *toggle_rec = true;
                }
            }
        });

        // ── Disk headroom + estimated record time (#13) ──────────
        // While recording we use the measured byte rate; otherwise we estimate
        // from the live block geometry (kvraw is raw i16 interleaved → 2 B/sample).
        let est_rate_bps: Option<f64> = if recording.state == RecordingState::Recording
            && let Some(secs) = rec_elapsed_secs
            && secs > 0.5
            && recording.recorded_bytes > 0
        {
            Some(recording.recorded_bytes as f64 / secs)
        } else {
            block
                .filter(|b| b.channel_count > 0 && b.sample_rate > 0.0)
                .map(|b| b.channel_count as f64 * b.sample_rate * 2.0)
        };

        if let Some(free) = crate::diskspace::free_bytes(&recording.output_dir) {
            ui.add_space(2.0);
            let free_gb = free as f64 / 1_000_000_000.0;
            let disk_color = if free_gb < 2.0 {
                theme::ACCENT_RED
            } else if free_gb < 10.0 {
                theme::ACCENT_YELLOW
            } else {
                theme::ACCENT_GREEN
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Disk")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{free_gb:.1} GB free"))
                        .size(10.0)
                        .monospace()
                        .color(disk_color),
                );
            });
            if let Some(rate) = est_rate_bps
                && rate > 0.0
            {
                let secs_left = free as f64 / rate;
                let prefix = if recording.state == RecordingState::Recording {
                    ""
                } else {
                    "~"
                };
                ui.label(
                    egui::RichText::new(format!(
                        "{prefix}{} left @ {}/s",
                        theme::format_clock(secs_left),
                        format_bytes(rate as u64)
                    ))
                    .size(9.0)
                    .color(theme::TEXT_DIM),
                )
                .on_hover_text("Estimated recording time remaining on this volume");
            }
        }

        if recording.state == RecordingState::Recording {
            theme::kv_label(ui, "Blocks", &recording.recorded_blocks.to_string());
            theme::kv_label(ui, "Size", &format_bytes(recording.recorded_bytes));

            // ── Real-time recording clock ────────────────────────
            if let Some(secs) = rec_elapsed_secs {
                let h = secs as u64 / 3600;
                let m = (secs as u64 % 3600) / 60;
                let s = secs as u64 % 60;
                theme::kv_label(ui, "Duration", &format!("{h:02}:{m:02}:{s:02}"));
            }

            // ── Buffer occupancy water-mark ──────────────────────
            // Shows how full the recorder's input queue is.
            // Green = healthy; yellow = disk may be slow; red = near overflow.
            ui.add_space(4.0);
            let occ_pct = (buffer_occupancy * 100.0) as u32;
            let bar_color = if buffer_occupancy > 0.75 {
                theme::ACCENT_RED
            } else if buffer_occupancy > 0.40 {
                theme::ACCENT_YELLOW
            } else {
                theme::ACCENT_GREEN
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Buffer")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{occ_pct:3}%"))
                        .size(10.0)
                        .monospace()
                        .color(bar_color),
                );
            });
            ui.add(
                egui::ProgressBar::new(buffer_occupancy as f32)
                    .fill(bar_color)
                    .desired_width(ui.available_width()),
            );
            if buffer_occupancy > 0.75 {
                ui.label(
                    egui::RichText::new("⚠ Disk may be too slow")
                        .size(9.0)
                        .color(theme::ACCENT_RED),
                );
            }
            if dropped_blocks > 0 {
                ui.label(
                    egui::RichText::new(format!("⚠ Dropped {dropped_blocks} blocks"))
                        .size(9.0)
                        .color(theme::ACCENT_RED),
                );
            }
        }

        // ── Recorder error banner (dismissable) ──────────────────
        if let Some(err) = recording_error {
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(70, 15, 15))
                .inner_margin(egui::Margin::same(6))
                .corner_radius(egui::CornerRadius::same(4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("⚠").size(11.0).color(theme::ACCENT_RED));
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(err)
                                .size(9.5)
                                .color(egui::Color32::from_rgb(255, 170, 170)),
                        );
                    });
                    if ui
                        .small_button(egui::RichText::new("Dismiss").size(9.0))
                        .clicked()
                    {
                        *dismiss_error = true;
                    }
                });
        }
    });
}

// ── Status bar (bottom) ─────────────────────────────────────────────

pub fn draw_status_bar(
    ui: &mut egui::Ui,
    acquiring: bool,
    recording: &RecordingSettings,
    stats: Option<&BlockStats>,
    block: Option<&SampleBlock>,
    elapsed_secs: f64,
) {
    ui.horizontal(|ui| {
        // Acquisition status
        if acquiring {
            theme::status_dot(ui, theme::ACCENT_GREEN);
            ui.label(
                egui::RichText::new("ACQ")
                    .size(10.0)
                    .strong()
                    .color(theme::ACCENT_GREEN),
            );
        } else {
            theme::status_dot(ui, theme::STATUS_IDLE);
            ui.label(
                egui::RichText::new("IDLE")
                    .size(10.0)
                    .color(theme::STATUS_IDLE),
            );
        }

        ui.separator();

        // Recording state
        match recording.state {
            RecordingState::Recording => {
                // Filled red badge so an active recording is unmistakable (A5).
                egui::Frame::new()
                    .fill(theme::STATUS_RECORDING)
                    .corner_radius(egui::CornerRadius::same(3))
                    .inner_margin(egui::Margin::symmetric(5, 1))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("\u{25CF} REC")
                                .size(theme::FONT_BODY)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                    });
            }
            RecordingState::Armed => {
                theme::status_dot(ui, theme::STATUS_ARMED);
                ui.label(
                    egui::RichText::new("ARMED")
                        .size(10.0)
                        .color(theme::STATUS_ARMED),
                );
            }
            RecordingState::Idle => {
                ui.label(
                    egui::RichText::new("REC OFF")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
            }
        }

        ui.separator();

        // Clock
        ui.label(
            egui::RichText::new(theme::format_clock(elapsed_secs))
                .size(11.0)
                .monospace()
                .color(if acquiring {
                    theme::ACCENT_YELLOW
                } else {
                    theme::TEXT_DIM
                }),
        );

        ui.separator();

        // Device info
        if let Some(b) = block {
            ui.label(
                egui::RichText::new(format!("{}ch @ {:.0}Hz", b.channel_count, b.sample_rate))
                    .size(10.0)
                    .monospace()
                    .color(theme::TEXT_SECONDARY),
            );
            ui.separator();
        }

        // Data rate & block rate
        if let Some(s) = stats {
            ui.label(
                egui::RichText::new(format!("{:.2} MB/s", s.data_rate_mb_s))
                    .size(10.0)
                    .monospace()
                    .color(theme::ACCENT_CYAN),
            );
            ui.separator();
            ui.label(
                egui::RichText::new(format!("{:.0} blk/s", s.block_rate_hz))
                    .size(10.0)
                    .monospace()
                    .color(theme::ACCENT_BLUE),
            );
            ui.separator();

            // Buffer health — three-tier green→amber→red on the drop count (#12).
            // Green = none lost; amber = a small fraction (transient hiccup);
            // red = a sustained loss rate that warrants attention.
            let dropped = s.dropped_blocks;
            let ratio = if s.total_blocks > 0 {
                dropped as f64 / s.total_blocks as f64
            } else {
                0.0
            };
            let (health_color, warn) = if dropped == 0 {
                (theme::ACCENT_GREEN, false)
            } else if ratio < 0.01 {
                (theme::ACCENT_YELLOW, true)
            } else {
                (theme::ACCENT_RED, true)
            };
            let drop_text = if warn {
                egui::RichText::new(format!("\u{26A0} Drop: {dropped}"))
                    .size(theme::FONT_BODY)
                    .monospace()
                    .strong()
                    .color(health_color)
            } else {
                egui::RichText::new("Drop: 0")
                    .size(theme::FONT_BODY)
                    .monospace()
                    .color(health_color)
            };
            ui.label(drop_text).on_hover_text(format!(
                "Blocks lost to packet-ID gaps (disk or CPU can't keep up)\n{dropped} of {} blocks ({:.3}%)",
                s.total_blocks,
                ratio * 100.0
            ));
        }

        // Right-aligned: total blocks and samples
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(s) = stats {
                ui.label(
                    egui::RichText::new(format!("{} blk", format_large_number(s.total_blocks)))
                        .size(9.0)
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
            }
        });
    });
}
