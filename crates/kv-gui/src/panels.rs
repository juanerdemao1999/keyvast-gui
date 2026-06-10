//! Side panels and status bar for the Keyvast professional GUI.
//!
//! Layout follows Intan RHX / Open Ephys patterns:
//!   Left panel: Device info, Display controls, Channel list, Recording.
//!   Bottom: Status bar with acquisition clock, data rate, buffer health.

use std::path::PathBuf;

use eframe::egui;
use kv_types::SampleBlock;
use rfd;

use crate::preview::BlockStats;
use crate::theme;

// ── Display settings state ──────────────────────────────────────────

/// Time-window presets in seconds (total visible window width).
pub const TIME_WINDOWS: &[f64] = &[1.0, 2.0, 5.0, 10.0, 20.0];

/// Amplitude presets in microvolts per division (display only — raw i16 scaled).
pub const AMP_SCALES: &[f64] = &[50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0];

/// Initial channel toggle capacity.  The vec grows dynamically if the device
/// has more channels, so this is just the default pre-allocation size.
const INITIAL_CHANNEL_TOGGLES: usize = 64;

#[derive(Debug, Clone)]
pub struct DisplaySettings {
    pub visible_channels: usize,
    pub time_scale_idx: usize,
    pub amp_scale_idx: usize,
    pub show_grid: bool,
    pub show_channel_labels: bool,
    pub overlay_mode: bool,
    /// When true: hovering over a channel highlights it white and dims others.
    /// When false (default): all channels always render at full brightness.
    pub hover_highlight: bool,
    /// Scroll step when browsing history while paused, as a percentage of
    /// the current time window. Default 10 = 10% of window per scroll click.
    pub browse_step_pct: f64,
    /// Per-channel enable/disable (true = visible).
    pub channel_enabled: Vec<bool>,
    /// Vertical spacing between channel baselines (1.0 = dense, 6.0 = spread).
    pub channel_spacing: f64,
}

/// Minimum allowed channel spacing.
pub const SPACING_MIN: f64 = 1.0;
/// Maximum allowed channel spacing.
pub const SPACING_MAX: f64 = 6.0;
/// Step for keyboard +/- adjustment.
pub const SPACING_STEP: f64 = 0.2;

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            visible_channels: 16,
            time_scale_idx: 2, // 5 s window
            amp_scale_idx: 4,  // 1000 uV/div
            show_grid: true,
            show_channel_labels: true,
            overlay_mode: false,
            hover_highlight: false,
            browse_step_pct: 10.0,
            channel_enabled: vec![true; INITIAL_CHANNEL_TOGGLES],
            channel_spacing: crate::waveform::DEFAULT_CHANNEL_SPACING,
        }
    }
}

impl DisplaySettings {
    /// Total visible window width in seconds.
    pub fn time_window_secs(&self) -> f64 {
        TIME_WINDOWS[self.time_scale_idx]
    }

    /// Total visible window width in milliseconds.
    pub fn time_window_ms(&self) -> f64 {
        self.time_window_secs() * 1000.0
    }

    pub fn amp_scale_uv(&self) -> f64 {
        AMP_SCALES[self.amp_scale_idx]
    }

    /// Check if a channel is enabled for display.
    pub fn is_channel_enabled(&self, ch: usize) -> bool {
        self.channel_enabled.get(ch).copied().unwrap_or(true)
    }
}

// ── Filter / signal-processing settings ─────────────────────────────

/// Notch frequency presets — line noise on most regions.
pub const NOTCH_FREQS: &[f64] = &[50.0, 60.0];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterSettings {
    pub hp_enabled: bool,
    pub hp_cutoff_hz: f64,
    pub lp_enabled: bool,
    pub lp_cutoff_hz: f64,
    pub notch_enabled: bool,
    pub notch_idx: usize, // index into NOTCH_FREQS
    pub car_enabled: bool,
    pub spike_threshold_enabled: bool,
    /// Threshold expressed as multiples of channel RMS (negative-going).
    pub spike_threshold_sigma: f64,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            hp_enabled: false,
            hp_cutoff_hz: 300.0, // standard for spike-band view
            lp_enabled: false,
            lp_cutoff_hz: 250.0, // standard for LFP-band view
            notch_enabled: false,
            notch_idx: 0,        // default 50 Hz (CN/EU mains)
            car_enabled: false,
            spike_threshold_enabled: false,
            spike_threshold_sigma: 4.0,
        }
    }
}

impl FilterSettings {
    pub fn notch_freq_hz(&self) -> f64 {
        NOTCH_FREQS[self.notch_idx]
    }

    pub fn any_filter_enabled(&self) -> bool {
        self.hp_enabled || self.lp_enabled || self.notch_enabled
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

// ── Device source settings ──────────────────────────────────────────

/// Which acquisition backend Device mode should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Synthetic simulator — the default; runs without any hardware.
    Simulator,
    /// Real Intan RHD / Opal Kelly XEM7310 board via the `kv-rhd` backend.
    Rhd,
}

/// Upper bound for the RHD headstage (32-channel data stream) count.
pub const RHD_MAX_STREAMS: usize = 2;

/// User-selected acquisition source for Device mode.
#[derive(Debug, Clone)]
pub struct DeviceSettings {
    pub kind: DeviceKind,
    /// FPGA bitfile uploaded when opening the RHD board.
    pub rhd_bitfile: Option<PathBuf>,
    /// Number of 32-channel RHD headstages to enable (1 or 2).
    pub rhd_streams: usize,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            kind: DeviceKind::Simulator,
            rhd_bitfile: default_bitfile_path(),
            rhd_streams: RHD_MAX_STREAMS,
        }
    }
}

/// Best-effort default bitfile shipped alongside the workspace, if present.
/// Prefers a KeyVast FPGA build (`keyvast_combined_download.bit`, verified to
/// acquire real data). On the KeyVast PCB the 8 RHD SPI buses are re-routed
/// through the module-IO ring (see `keyvast_top.sv` / `modules.xdc`), so only a
/// KeyVast bitstream reaches the headstage. The stock Intan/Open Ephys build
/// (`intan_rec_controller_7310.bit`) drives SPI on the *original* Intan pins,
/// which are not wired to the headstage on this board — it reads flat `0xFFFF`
/// on every port here, so it is kept only as a last-resort fallback for a
/// genuine Intan recording controller. All three expose the same `board_id=700`
/// data plane this backend speaks. Only a convenience pre-fill — the user can
/// always pick another file, and `None` is returned when none can be located so
/// nothing about acquisition is hard-coded.
fn default_bitfile_path() -> Option<PathBuf> {
    let bitfile_names = [
        "keyvast_combined_download.bit",
        "keyvast_260607_with_UART.bit",
        "intan_rec_controller_7310.bit",
    ];

    // 1. Search relative to the executable (works in deployed builds).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for name in &bitfile_names {
                let candidate = exe_dir.join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    // 2. Search current working directory.
    if let Ok(cwd) = std::env::current_dir() {
        for name in &bitfile_names {
            let candidate = cwd.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. Fallback: compile-time source tree (development only).
    #[cfg(debug_assertions)]
    {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for name in &bitfile_names {
            if let Ok(path) = manifest.join("../../../..").join(name).canonicalize() {
                return Some(path);
            }
        }
    }

    None
}

// ── Left control panel ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_left_panel(
    ui: &mut egui::Ui,
    acquiring: bool,
    device: &mut DeviceSettings,
    start_clicked: &mut bool,
    stop_clicked: &mut bool,
    toggle_rec: &mut bool,
    display: &mut DisplaySettings,
    filters: &mut FilterSettings,
    recording: &mut RecordingSettings,
    block: Option<&SampleBlock>,
    // Elapsed recording wall-clock seconds (None when not recording).
    rec_elapsed_secs: Option<f64>,
    // Recorder buffer fill level 0.0..=1.0 (from live pipeline).
    buffer_occupancy: f64,
    // Last recorder error message, if any.
    recording_error: Option<&str>,
    // Set to true by the panel when the user clicks "dismiss error".
    dismiss_error: &mut bool,
) {
    ui.set_min_width(220.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
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
            recording_error,
            dismiss_error,
        );
        ui.add_space(4.0);
        // Then view / processing controls.
        draw_display_settings(ui, display);
        ui.add_space(4.0);
        draw_filter_settings(ui, filters);
        ui.add_space(4.0);
        draw_channel_list(ui, display, block);
    });
}

// ── Device info ─────────────────────────────────────────────────────

fn draw_device_section(
    ui: &mut egui::Ui,
    connected: bool,
    device: &mut DeviceSettings,
    block: Option<&SampleBlock>,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("DEVICE").size(11.0).strong().color(theme::TEXT_SECONDARY),
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
                ui.label(egui::RichText::new("Source").size(10.0).color(theme::TEXT_DIM));
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
                        egui::RichText::new("Headstages").size(10.0).color(theme::TEXT_DIM),
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
        egui::RichText::new("ACQUISITION").size(11.0).strong().color(theme::TEXT_SECONDARY),
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

fn draw_display_settings(ui: &mut egui::Ui, display: &mut DisplaySettings) {
    egui::CollapsingHeader::new(
        egui::RichText::new("DISPLAY").size(11.0).strong().color(theme::TEXT_SECONDARY),
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
            ui.label(
                egui::RichText::new("Amp")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
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

fn draw_filter_settings(ui: &mut egui::Ui, filters: &mut FilterSettings) {
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

// ── Channel list with enable/disable ────────────────────────────────

fn draw_channel_list(
    ui: &mut egui::Ui,
    display: &mut DisplaySettings,
    block: Option<&SampleBlock>,
) {
    let ch_count = block.map(|b| b.channel_count).unwrap_or(0);
    let visible = display.visible_channels.min(ch_count);

    egui::CollapsingHeader::new(
        egui::RichText::new(format!("CHANNELS ({visible})"))
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        if visible == 0 {
            ui.label(
                egui::RichText::new("No channels")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            return;
        }

        // All / None buttons
        ui.horizontal(|ui| {
            if ui
                .small_button(egui::RichText::new("All").size(9.0))
                .clicked()
            {
                for i in 0..visible {
                    if let Some(e) = display.channel_enabled.get_mut(i) {
                        *e = true;
                    }
                }
            }
            if ui
                .small_button(egui::RichText::new("None").size(9.0))
                .clicked()
            {
                for i in 0..visible {
                    if let Some(e) = display.channel_enabled.get_mut(i) {
                        *e = false;
                    }
                }
            }
        });

        // Scrollable channel checkboxes
        // min_scrolled_width reserves enough room so the scrollbar never
        // overlaps the channel name / checkbox content.
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .min_scrolled_width(160.0)
            .show(ui, |ui| {
                ui.set_min_width(160.0);
                for ch in 0..visible {
                    // Ensure vector is big enough
                    while display.channel_enabled.len() <= ch {
                        display.channel_enabled.push(true);
                    }
                    let color = theme::channel_color(ch);
                    let enabled = display.channel_enabled[ch];
                    let label_color = if enabled { color } else { theme::TEXT_DIM };
                    ui.horizontal(|ui| {
                        // Colored bar
                        let (bar_rect, _) =
                            ui.allocate_exact_size(egui::vec2(3.0, 14.0), egui::Sense::hover());
                        ui.painter().rect_filled(bar_rect, 0.0, color);

                        ui.checkbox(
                            &mut display.channel_enabled[ch],
                            egui::RichText::new(format!("CH{ch}"))
                                .size(10.0)
                                .monospace()
                                .color(label_color),
                        );
                    });
                }
            });
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
    recording_error: Option<&str>,
    dismiss_error: &mut bool,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("RECORDING").size(11.0).strong().color(theme::TEXT_SECONDARY),
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

        // Output directory — text field + folder picker button
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Dir:")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::TextEdit::singleline(&mut recording.output_dir)
                    .desired_width(110.0)
                    .font(egui::FontId::monospace(10.0)),
            );
            if ui
                .button(egui::RichText::new("📁").size(13.0))
                .on_hover_text("Browse for output folder")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Select recording output folder")
                    .pick_folder()
                {
                    recording.output_dir = path.to_string_lossy().into_owned();
                }
            }
        });

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

        if recording.state == RecordingState::Recording {
            theme::kv_label(ui, "Blocks", &recording.recorded_blocks.to_string());
            theme::kv_label(ui, "Size", &format_bytes(recording.recorded_bytes));

            // ── Real-time recording clock ────────────────────────
            if let Some(secs) = rec_elapsed_secs {
                let h = secs as u64 / 3600;
                let m = (secs as u64 % 3600) / 60;
                let s = secs as u64 % 60;
                theme::kv_label(
                    ui,
                    "Duration",
                    &format!("{h:02}:{m:02}:{s:02}"),
                );
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
                        ui.label(
                            egui::RichText::new("⚠")
                                .size(11.0)
                                .color(theme::ACCENT_RED),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(err)
                                .size(9.5)
                                .color(egui::Color32::from_rgb(255, 170, 170)),
                        );
                    });
                    if ui
                        .small_button(
                            egui::RichText::new("Dismiss").size(9.0),
                        )
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
                theme::status_dot(ui, theme::STATUS_RECORDING);
                ui.label(
                    egui::RichText::new("REC")
                        .size(10.0)
                        .strong()
                        .color(theme::STATUS_RECORDING),
                );
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
                egui::RichText::new(format!(
                    "{}ch @ {:.0}Hz",
                    b.channel_count, b.sample_rate
                ))
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

            // Buffer health
            let dropped = s.dropped_blocks;
            let health_color = if dropped == 0 {
                theme::ACCENT_GREEN
            } else {
                theme::ACCENT_RED
            };
            ui.label(
                egui::RichText::new(format!("Drop: {dropped}"))
                    .size(10.0)
                    .monospace()
                    .color(health_color),
            );
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

// ── Formatting helpers ──────────────────────────────────────────────

fn format_time_window(secs: f64) -> String {
    if secs >= 1.0 {
        format!("{:.0} s", secs)
    } else {
        format!("{:.0} ms", secs * 1000.0)
    }
}

fn format_uv(uv: f64) -> String {
    if uv >= 1000.0 {
        format!("{:.0} mV/div", uv / 1000.0)
    } else {
        format!("{:.0} uV/div", uv)
    }
}

fn format_bytes(bytes: u64) -> String {
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

fn format_large_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
