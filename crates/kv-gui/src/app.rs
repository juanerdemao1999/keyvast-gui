//! Main application struct implementing `eframe::App`.
//!
//! Layout follows professional acquisition software patterns:
//!   Top:    Toolbar with transport controls, mode selector, clock
//!   Left:   Control panel (collapsible sections)
//!   Center: Multi-channel waveform display
//!   Bottom: Status bar with key metrics
//!
//! Keyboard shortcuts:
//!   Space  — Toggle acquisition start/stop
//!   R      — Toggle recording (arm → record → stop)
//!   G      — Toggle grid
//!   1..9   — Quick-set visible channels (x4: 4,8,12,16,20,24,28,32,36)

use std::collections::VecDeque;
use std::time::Instant;

use eframe::egui;
use kv_simulator::SimulatorConfig;
use kv_types::SampleBlock;

use crate::demo::DemoPreview;
use crate::panels::{self, DisplaySettings, RecordingSettings, RecordingState};
use crate::preview::{BlockStats, PreviewState};
use crate::theme;
use crate::waveform;

// ── Acquisition mode ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcqMode {
    Demo,
    Device,
}

// ── Application state ───────────────────────────────────────────────

pub struct KvApp {
    // Mode
    mode: AcqMode,
    // Demo
    demo: DemoPreview,
    demo_started: bool,
    demo_last_tick: Instant,
    demo_blocks_generated: u64,
    demo_start_time: Instant,
    // Device (simulator backend)
    device_preview: PreviewState,
    // Shared view state
    block_history: VecDeque<SampleBlock>,
    history_capacity: usize,
    latest_block: Option<SampleBlock>,
    latest_stats: Option<BlockStats>,
    // UI state
    display: DisplaySettings,
    recording: RecordingSettings,
    theme_applied: bool,
}

impl KvApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let now = Instant::now();
        Self {
            mode: AcqMode::Demo,
            demo: DemoPreview::default_neural(),
            demo_started: false,
            demo_last_tick: now,
            demo_blocks_generated: 0,
            demo_start_time: now,
            device_preview: PreviewState::new(),
            block_history: VecDeque::with_capacity(128),
            history_capacity: 128,
            latest_block: None,
            latest_stats: None,
            display: DisplaySettings::default(),
            recording: RecordingSettings::default(),
            theme_applied: false,
        }
    }

    /// Switch to Demo mode and start generating.
    fn start_demo(&mut self) {
        self.mode = AcqMode::Demo;
        self.demo = DemoPreview::default_neural();
        self.demo_started = true;
        self.demo_last_tick = Instant::now();
        self.demo_start_time = Instant::now();
        self.demo_blocks_generated = 0;
        self.block_history.clear();
        self.latest_block = None;
        self.latest_stats = None;
        self.device_preview.stop();
    }

    /// Switch to Device mode and start simulator backend.
    fn start_device(&mut self) {
        self.mode = AcqMode::Device;
        self.demo_started = false;
        self.block_history.clear();
        self.latest_block = None;
        self.latest_stats = None;
        let config = SimulatorConfig::default();
        self.device_preview.start(config);
    }

    fn stop_all(&mut self) {
        self.demo_started = false;
        self.device_preview.stop();
    }

    fn is_running(&self) -> bool {
        match self.mode {
            AcqMode::Demo => self.demo_started,
            AcqMode::Device => self.device_preview.acquiring,
        }
    }

    fn toggle_acquisition(&mut self) {
        if self.is_running() {
            self.stop_all();
        } else {
            match self.mode {
                AcqMode::Demo => self.start_demo(),
                AcqMode::Device => self.start_device(),
            }
        }
    }

    fn toggle_recording(&mut self) {
        match self.recording.state {
            RecordingState::Idle => {
                if self.is_running() {
                    self.recording.state = RecordingState::Armed;
                }
            }
            RecordingState::Armed => {
                self.recording.state = RecordingState::Recording;
                self.recording.recorded_blocks = 0;
                self.recording.recorded_bytes = 0;
            }
            RecordingState::Recording => {
                self.recording.state = RecordingState::Idle;
            }
        }
    }

    /// Elapsed time since acquisition started.
    fn elapsed_seconds(&self) -> f64 {
        if self.is_running() {
            match self.mode {
                AcqMode::Demo => Instant::now()
                    .duration_since(self.demo_start_time)
                    .as_secs_f64(),
                AcqMode::Device => self
                    .latest_stats
                    .as_ref()
                    .map(|s| s.elapsed_seconds)
                    .unwrap_or(0.0),
            }
        } else {
            self.latest_stats
                .as_ref()
                .map(|s| s.elapsed_seconds)
                .unwrap_or(0.0)
        }
    }

    /// Tick the demo generator to produce blocks at real-time cadence.
    fn tick_demo(&mut self) {
        if !self.demo_started {
            return;
        }

        let now = Instant::now();
        self.demo_last_tick = now;

        let elapsed_total = now.duration_since(self.demo_start_time).as_secs_f64();

        // How many blocks should exist by now
        let target_blocks = self.demo.blocks_for_elapsed(elapsed_total) as u64;
        let needed = target_blocks.saturating_sub(self.demo_blocks_generated);
        // Cap to avoid frame-time spikes
        let generate = needed.min(16) as usize;

        let mut last_block: Option<SampleBlock> = None;
        for _ in 0..generate {
            let block = self.demo.next_block();
            self.block_history.push_back(block.clone());
            while self.block_history.len() > self.history_capacity {
                self.block_history.pop_front();
            }
            last_block = Some(block);
            self.demo_blocks_generated += 1;
        }

        if let Some(block) = last_block {
            let stats = crate::preview::compute_block_stats(
                &block,
                self.demo_blocks_generated,
                elapsed_total,
            );
            self.latest_stats = Some(stats);
            self.latest_block = Some(block);
        }
    }

    /// Poll device preview and update shared state.
    fn tick_device(&mut self) {
        if self.device_preview.poll() {
            if let Some(ref block) = self.device_preview.latest_block {
                self.block_history.push_back(block.clone());
                while self.block_history.len() > self.history_capacity {
                    self.block_history.pop_front();
                }
            }
            self.latest_block = self.device_preview.latest_block.clone();
            self.latest_stats = self.device_preview.latest_stats.clone();
        }
    }

    /// Handle keyboard shortcuts.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Only when no text field is focused
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }

        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                self.toggle_acquisition();
            }
            if i.key_pressed(egui::Key::R) {
                self.toggle_recording();
            }
            if i.key_pressed(egui::Key::G) {
                self.display.show_grid = !self.display.show_grid;
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
        });
    }
}

impl eframe::App for KvApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Apply theme once
        if !self.theme_applied {
            theme::apply(ctx);
            self.theme_applied = true;
        }

        // Auto-start demo on first frame
        if !self.demo_started && self.mode == AcqMode::Demo && self.latest_block.is_none() {
            self.start_demo();
        }

        // Handle keyboard shortcuts
        self.handle_keys(ctx);

        // Tick
        match self.mode {
            AcqMode::Demo => self.tick_demo(),
            AcqMode::Device => self.tick_device(),
        }

        let elapsed = self.elapsed_seconds();

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

                    // Transport buttons
                    let running = self.is_running();
                    if theme::transport_button(
                        ui,
                        if running { "  Stop  " } else { "  Start  " },
                        if running {
                            theme::BTN_STOP
                        } else {
                            theme::BTN_PLAY
                        },
                        true,
                    ) {
                        self.toggle_acquisition();
                    }

                    // Record button
                    let rec_label = match self.recording.state {
                        RecordingState::Idle => " Record ",
                        RecordingState::Armed => " ARM ",
                        RecordingState::Recording => " STOP REC ",
                    };
                    let rec_color = match self.recording.state {
                        RecordingState::Idle => theme::BTN_DISABLED,
                        RecordingState::Armed => theme::ACCENT_YELLOW,
                        RecordingState::Recording => theme::BTN_RECORD_ACTIVE,
                    };
                    let rec_enabled =
                        running || self.recording.state != RecordingState::Idle;
                    if theme::transport_button(ui, rec_label, rec_color, rec_enabled) {
                        self.toggle_recording();
                    }

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // Mode selector
                    ui.label(
                        egui::RichText::new("Mode:")
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                    let demo_selected = self.mode == AcqMode::Demo;
                    if ui
                        .selectable_label(
                            demo_selected,
                            egui::RichText::new("Demo").size(11.0),
                        )
                        .on_hover_text("Synthetic neural data (Space to toggle)")
                        .clicked()
                        && !demo_selected
                    {
                        self.start_demo();
                    }
                    if ui
                        .selectable_label(
                            !demo_selected,
                            egui::RichText::new("Device").size(11.0),
                        )
                        .on_hover_text("Simulator backend")
                        .clicked()
                        && demo_selected
                    {
                        self.start_device();
                    }

                    // Right-aligned clock + version
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new("v0.2.0")
                                    .size(9.0)
                                    .color(theme::TEXT_DIM),
                            );
                            ui.add_space(8.0);

                            // Acquisition clock
                            let clock_color = if self.recording.state
                                == RecordingState::Recording
                            {
                                theme::ACCENT_RED
                            } else if running {
                                theme::ACCENT_YELLOW
                            } else {
                                theme::TEXT_DIM
                            };
                            ui.label(
                                egui::RichText::new(theme::format_clock(elapsed))
                                    .size(14.0)
                                    .monospace()
                                    .strong()
                                    .color(clock_color),
                            );
                        },
                    );
                });
            });

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

                panels::draw_left_panel(
                    ui,
                    self.is_running(),
                    &mut start,
                    &mut stop,
                    &mut self.display,
                    &mut self.recording,
                    self.latest_block.as_ref(),
                );

                if start {
                    match self.mode {
                        AcqMode::Demo => self.start_demo(),
                        AcqMode::Device => self.start_device(),
                    }
                }
                if stop {
                    self.stop_all();
                }
            });

        // ── Central waveform area ───────────────────────────────
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_DARKEST)
                    .inner_margin(egui::Margin::symmetric(4, 4)),
            )
            .show(ctx, |ui| {
                waveform::draw_waveform_area(
                    ui,
                    &self.block_history,
                    self.latest_block.as_ref(),
                    &self.display,
                );
            });

        // Request continuous repaints while running
        if self.is_running() {
            ctx.request_repaint();
        }
    }
}
