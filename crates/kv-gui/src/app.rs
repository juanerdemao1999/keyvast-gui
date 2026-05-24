//! Main application struct implementing `eframe::App`.

use eframe::egui;
use kv_simulator::SimulatorConfig;
use kv_types::SampleBlock;

use crate::preview::{PreviewHandle, start_preview};
use crate::waveform;

/// Application state for the Keyvast GUI.
pub struct KvApp {
    preview: Option<PreviewHandle>,
    latest_block: Option<SampleBlock>,
    block_count: u64,
    visible_channels: usize,
    acquiring: bool,
}

impl KvApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            preview: None,
            latest_block: None,
            block_count: 0,
            visible_channels: waveform::default_visible_channels(),
            acquiring: false,
        }
    }

    fn start_acquisition(&mut self) {
        if self.acquiring {
            return;
        }
        let config = SimulatorConfig::default();
        self.preview = Some(start_preview(config));
        self.acquiring = true;
        self.block_count = 0;
        self.latest_block = None;
    }

    fn stop_acquisition(&mut self) {
        if let Some(ref handle) = self.preview {
            handle.stop();
        }
        self.preview = None;
        self.acquiring = false;
    }

    fn poll_preview(&mut self) {
        if let Some(ref handle) = self.preview
            && let Some(block) = handle.latest_block()
        {
            self.block_count = self.block_count.saturating_add(1);
            self.latest_block = Some(block);
        }
    }
}

impl eframe::App for KvApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_preview();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Keyvast");
                ui.separator();

                if self.acquiring {
                    if ui.button("Stop").clicked() {
                        self.stop_acquisition();
                    }
                    ui.label(
                        egui::RichText::new("ACQUIRING")
                            .color(egui::Color32::LIGHT_GREEN)
                            .strong(),
                    );
                } else if ui.button("Start").clicked() {
                    self.start_acquisition();
                }

                ui.separator();

                ui.label("Channels:");
                for &count in &[16_usize, 32, 64] {
                    if ui
                        .selectable_label(self.visible_channels == count, format!("{count}"))
                        .clicked()
                    {
                        self.visible_channels = count;
                    }
                }
            });
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            if let Some(ref block) = self.latest_block {
                waveform::draw_status_panel(ui, block, self.block_count, self.visible_channels);
            } else {
                ui.label("Idle — press Start to begin acquisition");
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(ref block) = self.latest_block {
                waveform::draw_waveform_panel(ui, block, self.visible_channels);
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new("No data — start acquisition to view waveforms")
                            .size(18.0)
                            .color(egui::Color32::from_gray(100)),
                    );
                });
            }
        });

        // Request continuous repaints while acquiring
        if self.acquiring {
            ctx.request_repaint();
        }
    }
}
