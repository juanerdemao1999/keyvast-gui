//! Main application struct implementing `eframe::App`.
//!
//! Professional layout:
//!   Top:    thin toolbar (title + quick controls)
//!   Left:   control panel (device, acquisition, recording, display)
//!   Center: multi-channel waveform area
//!   Right:  statistics panel (throughput, buffer, per-channel)
//!   Bottom: status bar with indicators

use eframe::egui;
use kv_simulator::SimulatorConfig;

use crate::panels::{self, DisplaySettings, RecordingSettings};
use crate::preview::PreviewState;
use crate::theme;
use crate::waveform;

/// Application state for the Keyvast GUI.
pub struct KvApp {
    preview: PreviewState,
    display: DisplaySettings,
    recording: RecordingSettings,
    theme_applied: bool,
}

impl KvApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            preview: PreviewState::new(),
            display: DisplaySettings::default(),
            recording: RecordingSettings::default(),
            theme_applied: false,
        }
    }
}

impl eframe::App for KvApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Apply theme once
        if !self.theme_applied {
            theme::apply(ctx);
            self.theme_applied = true;
        }

        // Poll for new data
        self.preview.poll();

        // ── Top toolbar ─────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::new().fill(theme::BG_DARK).inner_margin(6.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("KEYVAST")
                            .size(14.0)
                            .strong()
                            .color(theme::ACCENT_BLUE),
                    );
                    ui.label(
                        egui::RichText::new("Acquisition System")
                            .size(11.0)
                            .color(theme::TEXT_DIM),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("v0.1.0")
                                .size(9.0)
                                .color(theme::TEXT_DIM),
                        );
                    });
                });
            });

        // ── Bottom status bar ───────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar")
            .frame(egui::Frame::new().fill(theme::BG_DARK).inner_margin(4.0))
            .show(ctx, |ui| {
                panels::draw_status_bar(
                    ui,
                    self.preview.acquiring,
                    &self.recording,
                    self.preview.latest_stats.as_ref(),
                    self.preview.latest_block.as_ref(),
                );
            });

        // ── Left control panel ──────────────────────────────────
        egui::SidePanel::left("control_panel")
            .resizable(true)
            .default_width(230.0)
            .width_range(180.0..=320.0)
            .frame(egui::Frame::new().fill(theme::BG_PANEL).inner_margin(8.0))
            .show(ctx, |ui| {
                let mut start = false;
                let mut stop = false;

                panels::draw_left_panel(
                    ui,
                    self.preview.acquiring,
                    &mut start,
                    &mut stop,
                    &mut self.display,
                    &mut self.recording,
                    self.preview.latest_block.as_ref(),
                );

                if start {
                    let config = SimulatorConfig::default();
                    self.preview.start(config);
                }
                if stop {
                    self.preview.stop();
                }
            });

        // ── Right statistics panel ──────────────────────────────
        egui::SidePanel::right("stats_panel")
            .resizable(true)
            .default_width(210.0)
            .width_range(160.0..=300.0)
            .frame(egui::Frame::new().fill(theme::BG_PANEL).inner_margin(8.0))
            .show(ctx, |ui| {
                panels::draw_right_panel(
                    ui,
                    self.preview.latest_stats.as_ref(),
                    self.preview.latest_block.as_ref(),
                    self.display.visible_channels,
                );
            });

        // ── Central waveform area ───────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG_DARKEST).inner_margin(0.0))
            .show(ctx, |ui| {
                waveform::draw_waveform_area(
                    ui,
                    &self.preview.block_history,
                    self.preview.latest_block.as_ref(),
                    &self.display,
                );
            });

        // Request continuous repaints while acquiring
        if self.preview.acquiring {
            ctx.request_repaint();
        }
    }
}
