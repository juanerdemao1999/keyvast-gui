//! Impedance measurement panel for the Keyvast GUI.
//!
//! Displays per-channel impedance magnitude/phase with color coding.
//! Provides controls to start/stop an impedance test and configure
//! the test frequency.

use eframe::egui;
use kv_rhd::impedance::{ChannelImpedance, ImpedanceResult};

use crate::theme;

/// Messages sent from the background impedance-measurement thread to the GUI.
#[derive(Debug)]
pub enum ImpedanceMsg {
    /// Per-channel progress: (current_channel, total_channels).
    Progress(usize, usize),
    /// Measurement finished successfully.
    Done(ImpedanceResult),
    /// Measurement failed.
    Failed(String),
}

/// State for the impedance panel.
#[derive(Debug)]
pub struct ImpedanceState {
    /// Test frequency in Hz.
    pub frequency_hz: f64,
    /// Number of periods.
    pub num_periods: usize,
    /// Whether a measurement is in progress.
    pub measuring: bool,
    /// Progress: (current_channel, total_channels).
    pub progress: (usize, usize),
    /// Results from the last measurement (if any).
    pub results: Option<ImpedanceResult>,
    /// Error from the last measurement attempt.
    pub error: Option<String>,
}

impl Default for ImpedanceState {
    fn default() -> Self {
        Self {
            frequency_hz: 1000.0,
            num_periods: 20,
            measuring: false,
            progress: (0, 0),
            results: None,
            error: None,
        }
    }
}

/// Draw the impedance measurement panel inside a collapsing header.
pub fn draw_impedance_section(
    ui: &mut egui::Ui,
    state: &mut ImpedanceState,
    can_measure: bool,
    start_impedance: &mut bool,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("IMPEDANCE")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        // Configuration
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Freq (Hz)")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::DragValue::new(&mut state.frequency_hz)
                    .range(10.0..=10_000.0)
                    .speed(10.0)
                    .suffix(" Hz"),
            );
        });

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Periods")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::DragValue::new(&mut state.num_periods)
                    .range(5..=100)
                    .speed(1),
            );
        });

        ui.add_space(4.0);

        // Start/Stop button
        if state.measuring {
            let (cur, total) = state.progress;
            let progress = if total > 0 {
                cur as f32 / total as f32
            } else {
                0.0
            };
            ui.add(egui::ProgressBar::new(progress).text(format!("Channel {cur}/{total}")));
        } else {
            let enabled = can_measure;
            let tooltip = if !can_measure {
                "Select the RHD source and an FPGA bitfile in the DEVICE panel first"
            } else {
                "Measure impedance on all channels (stops acquisition during the test)"
            };
            if ui
                .add_enabled(enabled, egui::Button::new("Measure Impedance"))
                .on_hover_text(tooltip)
                .clicked()
            {
                *start_impedance = true;
            }
        }

        // Error display
        if let Some(ref err) = state.error {
            ui.colored_label(theme::ACCENT_RED, err);
        }

        // Results table
        if let Some(ref result) = state.results {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!(
                    "Results @ {:.0} Hz ({} channels)",
                    result.config.frequency_hz,
                    result.channels.len(),
                ))
                .size(10.0)
                .color(theme::TEXT_DIM),
            );

            ui.add_space(2.0);

            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    draw_impedance_table(ui, &result.channels);
                });
        }
    });
}

/// Draw a compact impedance results table.
fn draw_impedance_table(ui: &mut egui::Ui, channels: &[ChannelImpedance]) {
    egui::Grid::new("impedance_grid")
        .num_columns(4)
        .spacing([8.0, 2.0])
        .striped(true)
        .show(ui, |ui| {
            // Header
            ui.label(
                egui::RichText::new("Ch")
                    .size(10.0)
                    .strong()
                    .color(theme::TEXT_DIM),
            );
            ui.label(
                egui::RichText::new("Magnitude")
                    .size(10.0)
                    .strong()
                    .color(theme::TEXT_DIM),
            );
            ui.label(
                egui::RichText::new("Phase")
                    .size(10.0)
                    .strong()
                    .color(theme::TEXT_DIM),
            );
            ui.label(
                egui::RichText::new("Quality")
                    .size(10.0)
                    .strong()
                    .color(theme::TEXT_DIM),
            );
            ui.end_row();

            // Rows
            for ch in channels {
                ui.label(
                    egui::RichText::new(format!("{:2}", ch.channel))
                        .size(10.0)
                        .monospace(),
                );

                let mag_text = format_impedance(ch.magnitude_ohms);
                let rgba = ImpedanceResult::quality_color(ch.magnitude_ohms);
                let color =
                    egui::Color32::from_rgba_premultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);

                ui.label(
                    egui::RichText::new(&mag_text)
                        .size(10.0)
                        .monospace()
                        .color(color),
                );

                ui.label(
                    egui::RichText::new(format!("{:+.1}°", ch.phase_degrees))
                        .size(10.0)
                        .monospace(),
                );

                let quality = ImpedanceResult::quality_label(ch.magnitude_ohms);
                ui.label(egui::RichText::new(quality).size(10.0).color(color));

                ui.end_row();
            }
        });
}

/// Format impedance magnitude with appropriate unit (Ω, kΩ, MΩ).
fn format_impedance(ohms: f64) -> String {
    if !ohms.is_finite() {
        return "Open".to_string();
    }
    if ohms >= 1_000_000.0 {
        format!("{:.2} MΩ", ohms / 1_000_000.0)
    } else if ohms >= 1_000.0 {
        format!("{:.1} kΩ", ohms / 1_000.0)
    } else {
        format!("{:.0} Ω", ohms)
    }
}
