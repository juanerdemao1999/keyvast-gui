//! Side panels and status bar for the Keyvast professional GUI.
//!
//! Left panel: device info, acquisition controls, display settings.
//! Right panel: channel statistics, buffer health.
//! Bottom: status bar with indicators.

use eframe::egui;
use kv_types::SampleBlock;

use crate::preview::BlockStats;
use crate::theme;

// ── Display settings state ──────────────────────────────────────────

/// Time-base presets in milliseconds per division.
pub const TIME_SCALES: &[f64] = &[0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0];

/// Amplitude presets in microvolts per division (display only — raw i16 scaled).
pub const AMP_SCALES: &[f64] = &[50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0];

#[derive(Debug, Clone)]
pub struct DisplaySettings {
    pub visible_channels: usize,
    pub time_scale_idx: usize,
    pub amp_scale_idx: usize,
    pub show_grid: bool,
    pub show_channel_labels: bool,
    pub overlay_mode: bool,
    pub auto_scale: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            visible_channels: 16,
            time_scale_idx: 3, // 5 ms/div
            amp_scale_idx: 4,  // 1000 uV/div
            show_grid: true,
            show_channel_labels: true,
            overlay_mode: false,
            auto_scale: false,
        }
    }
}

impl DisplaySettings {
    pub fn time_scale_ms(&self) -> f64 {
        TIME_SCALES[self.time_scale_idx]
    }

    pub fn amp_scale_uv(&self) -> f64 {
        AMP_SCALES[self.amp_scale_idx]
    }
}

// ── Recording state ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Armed,
    Recording,
}

#[derive(Debug, Clone)]
pub struct RecordingSettings {
    pub state: RecordingState,
    pub output_dir: String,
    pub file_prefix: String,
    pub recorded_blocks: u64,
    pub recorded_bytes: u64,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            state: RecordingState::Idle,
            output_dir: "recordings".to_string(),
            file_prefix: "session".to_string(),
            recorded_blocks: 0,
            recorded_bytes: 0,
        }
    }
}

// ── Left control panel ──────────────────────────────────────────────

pub fn draw_left_panel(
    ui: &mut egui::Ui,
    acquiring: bool,
    start_clicked: &mut bool,
    stop_clicked: &mut bool,
    display: &mut DisplaySettings,
    recording: &mut RecordingSettings,
    block: Option<&SampleBlock>,
) {
    ui.set_min_width(220.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        draw_device_section(ui, acquiring, block);
        draw_acquisition_controls(ui, acquiring, start_clicked, stop_clicked);
        draw_recording_section(ui, recording, acquiring);
        draw_display_settings(ui, display);
    });
}

fn draw_device_section(ui: &mut egui::Ui, connected: bool, block: Option<&SampleBlock>) {
    theme::section_heading(ui, "DEVICE");

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

    if let Some(b) = block {
        theme::kv_label(ui, "Device ID", &b.device_id);
        theme::kv_label(ui, "Sample Rate", &format!("{:.0} Hz", b.sample_rate));
        theme::kv_label(ui, "Channels", &b.channel_count.to_string());
        theme::kv_label(ui, "Samples/Pkt", &b.samples_per_channel.to_string());
        theme::kv_label(ui, "Backend", "Simulator");
    } else {
        theme::kv_label(ui, "Device ID", "—");
        theme::kv_label(ui, "Sample Rate", "—");
        theme::kv_label(ui, "Channels", "—");
    }
}

fn draw_acquisition_controls(
    ui: &mut egui::Ui,
    acquiring: bool,
    start_clicked: &mut bool,
    stop_clicked: &mut bool,
) {
    theme::section_heading(ui, "ACQUISITION");

    ui.horizontal(|ui| {
        let start_btn =
            egui::Button::new(egui::RichText::new("  Start  ").size(13.0).strong().color(
                if acquiring {
                    theme::TEXT_DIM
                } else {
                    egui::Color32::WHITE
                },
            ))
            .fill(if acquiring {
                theme::BG_WIDGET
            } else {
                egui::Color32::from_rgb(30, 140, 50)
            });

        if ui.add_enabled(!acquiring, start_btn).clicked() {
            *start_clicked = true;
        }

        let stop_btn =
            egui::Button::new(egui::RichText::new("  Stop  ").size(13.0).strong().color(
                if acquiring {
                    egui::Color32::WHITE
                } else {
                    theme::TEXT_DIM
                },
            ))
            .fill(if acquiring {
                egui::Color32::from_rgb(180, 40, 40)
            } else {
                theme::BG_WIDGET
            });

        if ui.add_enabled(acquiring, stop_btn).clicked() {
            *stop_clicked = true;
        }
    });

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
}

fn draw_recording_section(ui: &mut egui::Ui, recording: &mut RecordingSettings, acquiring: bool) {
    theme::section_heading(ui, "RECORDING");

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

    // Output directory
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Dir:")
                .size(10.0)
                .color(theme::TEXT_DIM),
        );
        let _response = ui.add(
            egui::TextEdit::singleline(&mut recording.output_dir)
                .desired_width(140.0)
                .font(egui::FontId::monospace(10.0)),
        );
    });

    // Arm / Record button
    ui.horizontal(|ui| match recording.state {
        RecordingState::Idle => {
            if ui
                .add_enabled(
                    acquiring,
                    egui::Button::new(egui::RichText::new("Arm").size(11.0)),
                )
                .clicked()
            {
                recording.state = RecordingState::Armed;
            }
        }
        RecordingState::Armed => {
            if ui
                .button(
                    egui::RichText::new("Record")
                        .size(11.0)
                        .color(theme::ACCENT_RED),
                )
                .clicked()
            {
                recording.state = RecordingState::Recording;
                recording.recorded_blocks = 0;
                recording.recorded_bytes = 0;
            }
            if ui
                .button(egui::RichText::new("Disarm").size(11.0))
                .clicked()
            {
                recording.state = RecordingState::Idle;
            }
        }
        RecordingState::Recording => {
            if ui
                .button(
                    egui::RichText::new("Stop Rec")
                        .size(11.0)
                        .color(theme::ACCENT_RED),
                )
                .clicked()
            {
                recording.state = RecordingState::Idle;
            }
        }
    });

    if recording.state == RecordingState::Recording {
        theme::kv_label(ui, "Blocks", &recording.recorded_blocks.to_string());
        theme::kv_label(ui, "Size", &format_bytes(recording.recorded_bytes));
    }
}

fn draw_display_settings(ui: &mut egui::Ui, display: &mut DisplaySettings) {
    theme::section_heading(ui, "DISPLAY");

    // ── Visible channels — slider (drag or click anywhere) ──────
    ui.label(
        egui::RichText::new("Visible Channels")
            .size(10.0)
            .color(theme::TEXT_DIM),
    );
    let mut ch = display.visible_channels as i32;
    let slider = egui::Slider::new(&mut ch, 1..=64)
        .step_by(1.0)
        .suffix(" ch")
        .trailing_fill(true);
    if ui
        .add(slider)
        .on_hover_text("Drag to change, or click the number to type")
        .changed()
    {
        display.visible_channels = ch.max(1) as usize;
    }

    ui.add_space(2.0);

    // ── Time scale — dropdown ───────────────────────────────────
    ui.label(
        egui::RichText::new("Time Scale")
            .size(10.0)
            .color(theme::TEXT_DIM),
    );
    egui::ComboBox::from_id_salt("time_scale")
        .width(ui.available_width() - 4.0)
        .selected_text(
            egui::RichText::new(format!("{:.1} ms/div", display.time_scale_ms()))
                .monospace()
                .size(11.0),
        )
        .show_ui(ui, |ui| {
            for (i, &ms) in TIME_SCALES.iter().enumerate() {
                let label = format!("{ms:.1} ms/div");
                ui.selectable_value(&mut display.time_scale_idx, i, &label);
            }
        });

    ui.add_space(2.0);

    // ── Amplitude scale — dropdown ──────────────────────────────
    ui.label(
        egui::RichText::new("Amplitude Scale")
            .size(10.0)
            .color(theme::TEXT_DIM),
    );
    egui::ComboBox::from_id_salt("amp_scale")
        .width(ui.available_width() - 4.0)
        .selected_text(
            egui::RichText::new(format_uv(display.amp_scale_uv()))
                .monospace()
                .size(11.0),
        )
        .show_ui(ui, |ui| {
            for (i, &uv) in AMP_SCALES.iter().enumerate() {
                ui.selectable_value(&mut display.amp_scale_idx, i, format_uv(uv));
            }
        });

    ui.add_space(4.0);

    // ── Toggles ─────────────────────────────────────────────────
    ui.checkbox(
        &mut display.show_grid,
        egui::RichText::new("Grid lines").size(10.0),
    )
    .on_hover_text("Show grid lines on waveform plots");

    ui.checkbox(
        &mut display.show_channel_labels,
        egui::RichText::new("Channel labels").size(10.0),
    )
    .on_hover_text("Show CH0, CH1… labels on the left");

    ui.checkbox(
        &mut display.overlay_mode,
        egui::RichText::new("Overlay mode").size(10.0),
    )
    .on_hover_text("Stack all channels on a single plot");
}

// ── Right statistics panel ──────────────────────────────────────────

pub fn draw_right_panel(
    ui: &mut egui::Ui,
    stats: Option<&BlockStats>,
    block: Option<&SampleBlock>,
    visible_channels: usize,
) {
    ui.set_min_width(200.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        draw_throughput_section(ui, stats);
        draw_buffer_section(ui, stats);
        draw_channel_stats_section(ui, stats, block, visible_channels);
    });
}

fn draw_throughput_section(ui: &mut egui::Ui, stats: Option<&BlockStats>) {
    theme::section_heading(ui, "THROUGHPUT");

    if let Some(s) = stats {
        theme::kv_label_colored(
            ui,
            "Data Rate",
            &format!("{:.2} MB/s", s.data_rate_mb_s),
            theme::ACCENT_CYAN,
        );
        theme::kv_label_colored(
            ui,
            "Block Rate",
            &format!("{:.1} Hz", s.block_rate_hz),
            theme::ACCENT_BLUE,
        );
        theme::kv_label(ui, "Total Blocks", &s.total_blocks.to_string());
        theme::kv_label(ui, "Total Samples", &format_large_number(s.total_samples));
        theme::kv_label(ui, "Elapsed", &format_duration(s.elapsed_seconds));
    } else {
        theme::kv_label(ui, "Data Rate", "—");
        theme::kv_label(ui, "Block Rate", "—");
    }
}

fn draw_buffer_section(ui: &mut egui::Ui, stats: Option<&BlockStats>) {
    theme::section_heading(ui, "BUFFER HEALTH");

    if let Some(s) = stats {
        let dropped = s.dropped_blocks;
        let color = if dropped == 0 {
            theme::ACCENT_GREEN
        } else {
            theme::ACCENT_RED
        };
        theme::kv_label_colored(ui, "Dropped", &dropped.to_string(), color);

        // Simple health bar
        let health = if s.total_blocks > 0 {
            1.0 - (dropped as f32 / s.total_blocks as f32)
        } else {
            1.0
        };
        let bar_color = if health > 0.99 {
            theme::ACCENT_GREEN
        } else if health > 0.95 {
            theme::ACCENT_YELLOW
        } else {
            theme::ACCENT_RED
        };

        ui.add_space(2.0);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, theme::BG_DARKEST);
        let filled =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * health, rect.height()));
        ui.painter().rect_filled(filled, 2.0, bar_color);
    } else {
        theme::kv_label(ui, "Status", "No data");
    }
}

fn draw_channel_stats_section(
    ui: &mut egui::Ui,
    stats: Option<&BlockStats>,
    _block: Option<&SampleBlock>,
    visible_channels: usize,
) {
    theme::section_heading(ui, "CHANNEL STATS");

    let Some(s) = stats else {
        ui.label(
            egui::RichText::new("No data")
                .size(10.0)
                .color(theme::TEXT_DIM),
        );
        return;
    };

    if s.channels.is_empty() {
        ui.label(
            egui::RichText::new("No channels")
                .size(10.0)
                .color(theme::TEXT_DIM),
        );
        return;
    }

    // Header
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("CH").size(9.0).color(theme::TEXT_DIM));
        ui.add_space(16.0);
        ui.label(egui::RichText::new("RMS").size(9.0).color(theme::TEXT_DIM));
        ui.add_space(16.0);
        ui.label(egui::RichText::new("P-P").size(9.0).color(theme::TEXT_DIM));
    });

    let show = visible_channels.min(s.channels.len());
    for ch in 0..show {
        let cs = &s.channels[ch];
        ui.horizontal(|ui| {
            let color = theme::channel_color(ch);
            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 12.0), egui::Sense::hover());
            ui.painter().rect_filled(dot_rect, 1.0, color);

            ui.label(
                egui::RichText::new(format!("{ch:>2}"))
                    .size(9.0)
                    .monospace()
                    .color(theme::TEXT_SECONDARY),
            );
            ui.label(
                egui::RichText::new(format!("{:>6.0}", cs.rms))
                    .size(9.0)
                    .monospace()
                    .color(theme::TEXT_PRIMARY),
            );
            ui.label(
                egui::RichText::new(format!("{:>6}", cs.peak_to_peak))
                    .size(9.0)
                    .monospace()
                    .color(theme::TEXT_PRIMARY),
            );
        });
    }

    if s.channels.len() > show {
        ui.label(
            egui::RichText::new(format!("... +{} more", s.channels.len() - show))
                .size(9.0)
                .color(theme::TEXT_DIM),
        );
    }
}

// ── Bottom status bar ───────────────────────────────────────────────

pub fn draw_status_bar(
    ui: &mut egui::Ui,
    acquiring: bool,
    recording: &RecordingSettings,
    stats: Option<&BlockStats>,
    block: Option<&SampleBlock>,
) {
    ui.horizontal(|ui| {
        // Connection indicator
        if acquiring {
            theme::status_dot(ui, theme::STATUS_CONNECTED);
            ui.label(
                egui::RichText::new("ONLINE")
                    .size(10.0)
                    .monospace()
                    .color(theme::STATUS_CONNECTED),
            );
        } else {
            theme::status_dot(ui, theme::STATUS_IDLE);
            ui.label(
                egui::RichText::new("OFFLINE")
                    .size(10.0)
                    .monospace()
                    .color(theme::STATUS_IDLE),
            );
        }

        ui.separator();

        // Recording indicator
        match recording.state {
            RecordingState::Recording => {
                theme::status_dot(ui, theme::STATUS_RECORDING);
                ui.label(
                    egui::RichText::new("REC")
                        .size(10.0)
                        .monospace()
                        .strong()
                        .color(theme::STATUS_RECORDING),
                );
            }
            RecordingState::Armed => {
                theme::status_dot(ui, theme::STATUS_ARMED);
                ui.label(
                    egui::RichText::new("ARMED")
                        .size(10.0)
                        .monospace()
                        .color(theme::STATUS_ARMED),
                );
            }
            RecordingState::Idle => {
                ui.label(
                    egui::RichText::new("NO REC")
                        .size(10.0)
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
            }
        }

        ui.separator();

        // Data rate
        if let Some(s) = stats {
            ui.label(
                egui::RichText::new(format!("{:.2} MB/s", s.data_rate_mb_s))
                    .size(10.0)
                    .monospace()
                    .color(theme::ACCENT_CYAN),
            );
            ui.separator();
            ui.label(
                egui::RichText::new(format!("{:.1} blk/s", s.block_rate_hz))
                    .size(10.0)
                    .monospace()
                    .color(theme::ACCENT_BLUE),
            );
            ui.separator();
            ui.label(
                egui::RichText::new(format_duration(s.elapsed_seconds))
                    .size(10.0)
                    .monospace()
                    .color(theme::TEXT_PRIMARY),
            );
        }

        ui.separator();

        // Packet info
        if let Some(b) = block {
            ui.label(
                egui::RichText::new(format!(
                    "Pkt #{} | {}ch x {:.0}Hz | TTL {:#06x}",
                    b.packet_id, b.channel_count, b.sample_rate, b.ttl_bits
                ))
                .size(10.0)
                .monospace()
                .color(theme::TEXT_SECONDARY),
            );
        }
    });
}

// ── Formatting helpers ──────────────────────────────────────────────

fn format_bytes(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{bytes} B")
    } else if bytes < 1_048_576 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else if bytes < 1_073_741_824 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    }
}

fn format_duration(seconds: f64) -> String {
    let total = seconds as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

fn format_large_number(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n < 1_000_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else {
        format!("{:.2}G", n as f64 / 1_000_000_000.0)
    }
}

fn format_uv(uv: f64) -> String {
    if uv >= 1000.0 {
        format!("{:.0} mV/div", uv / 1000.0)
    } else {
        format!("{:.0} uV/div", uv)
    }
}
