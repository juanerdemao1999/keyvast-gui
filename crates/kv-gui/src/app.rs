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
//!   P      — Pause/resume display (acquisition continues)
//!   F      — Toggle performance overlay (FPS / render time)
//!   [ / ]  — Decrease / increase time window
//!   1..9   — Quick-set visible channels (x4: 4,8,12,16,20,24,28,32,36)
//!
//! Mouse: scroll-wheel over the plot also adjusts the time window.

use std::collections::VecDeque;
use std::time::Instant;

use eframe::egui;
use kv_simulator::SimulatorConfig;
use kv_types::SampleBlock;

use crate::demo::DemoPreview;
use crate::panels::{self, DisplaySettings, FilterSettings, RecordingSettings, RecordingState};
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
    filters: FilterSettings,
    recording: RecordingSettings,
    theme_applied: bool,
    /// When true, the waveform display is frozen at the current view but
    /// acquisition and recording continue uninterrupted.
    pub display_paused: bool,
    /// The elapsed time captured the moment the display was paused.
    paused_elapsed: f64,
    /// Show performance overlay (FPS, render time).
    pub show_perf_overlay: bool,
    // Performance metrics
    last_frame: Instant,
    frame_ms_ema: f64,
    render_ms_ema: f64,
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
            // 20s at 30kHz / 64spc ≈ 9375 blocks; round up with margin
            block_history: VecDeque::with_capacity(10_000),
            history_capacity: 10_000,
            latest_block: None,
            latest_stats: None,
            display: DisplaySettings::default(),
            filters: FilterSettings::default(),
            recording: RecordingSettings::default(),
            theme_applied: false,
            display_paused: false,
            paused_elapsed: 0.0,
            show_perf_overlay: false,
            last_frame: now,
            frame_ms_ema: 16.7,
            render_ms_ema: 0.0,
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

    /// Freeze/unfreeze the display.  Acquisition continues regardless —
    /// the user can examine a snapshot of the trace without stopping
    /// recording.  Capture the elapsed time on pause so the X window
    /// stays at the frozen position.
    fn toggle_pause_display(&mut self) {
        if self.display_paused {
            self.display_paused = false;
        } else {
            self.paused_elapsed = self.elapsed_seconds();
            self.display_paused = true;
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
                    (self.display.channel_spacing + panels::SPACING_STEP)
                        .min(panels::SPACING_MAX);
            }
            if i.key_pressed(egui::Key::Minus) {
                self.display.channel_spacing =
                    (self.display.channel_spacing - panels::SPACING_STEP)
                        .max(panels::SPACING_MIN);
            }
        });
    }
}

impl eframe::App for KvApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        let frame_delta_ms = frame_start
            .duration_since(self.last_frame)
            .as_secs_f64()
            * 1000.0;
        self.last_frame = frame_start;
        // EMA of frame interval (~250ms time constant at 60fps)
        self.frame_ms_ema = self.frame_ms_ema * 0.9 + frame_delta_ms * 0.1;

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

        // Tick (acquisition runs regardless of display pause)
        match self.mode {
            AcqMode::Demo => self.tick_demo(),
            AcqMode::Device => self.tick_device(),
        }

        let elapsed_live = self.elapsed_seconds();
        // Use frozen elapsed for display when paused
        let elapsed = if self.display_paused {
            self.paused_elapsed
        } else {
            elapsed_live
        };

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

                    // Record button — always clickable when running
                    let rec_label = match self.recording.state {
                        RecordingState::Idle => " Record ",
                        RecordingState::Armed => "  ARM  ",
                        RecordingState::Recording => " STOP REC ",
                    };
                    let rec_color = match self.recording.state {
                        RecordingState::Idle => theme::BTN_RECORD,
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
                    &mut self.filters,
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
                let plot_rect = ui.available_rect_before_wrap();

                // Mouse wheel over the plot adjusts the time window;
                // drag when paused scrolls through history.
                let sense = if self.display_paused {
                    egui::Sense::click_and_drag()
                } else {
                    egui::Sense::hover()
                };
                let scroll_response =
                    ui.interact(plot_rect, egui::Id::new("waveform_wheel"), sense);
                if scroll_response.hovered() {
                    let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                    if scroll.abs() > 1.0 {
                        let max_idx = panels::TIME_WINDOWS.len() - 1;
                        if scroll < 0.0 {
                            self.display.time_scale_idx =
                                (self.display.time_scale_idx + 1).min(max_idx);
                        } else {
                            self.display.time_scale_idx =
                                self.display.time_scale_idx.saturating_sub(1);
                        }
                    }
                }
                // Drag-to-browse when paused: horizontal drag shifts the view time
                if self.display_paused && scroll_response.dragged() {
                    let drag_px = scroll_response.drag_delta().x;
                    let plot_width = plot_rect.width().max(1.0);
                    let time_window_ms = self.display.time_window_ms();
                    // dragging right → go back in time (reduce paused_elapsed)
                    let dt_ms = (drag_px as f64 / plot_width as f64) * time_window_ms;
                    self.paused_elapsed = (self.paused_elapsed - dt_ms / 1000.0).max(0.0);
                    // Clamp to live time (can't look into the future)
                    let live = self.elapsed_seconds();
                    if self.paused_elapsed > live {
                        self.paused_elapsed = live;
                    }
                }

                let render_start = Instant::now();
                waveform::draw_waveform_area(
                    ui,
                    &self.block_history,
                    self.latest_block.as_ref(),
                    &self.display,
                    &self.filters,
                    elapsed,
                );
                let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
                self.render_ms_ema = self.render_ms_ema * 0.9 + render_ms * 0.1;

                // Pause indicator overlay
                if self.display_paused {
                    draw_paused_overlay(ui, plot_rect);
                }
                // Performance overlay
                if self.show_perf_overlay {
                    draw_perf_overlay(
                        ui,
                        plot_rect,
                        self.frame_ms_ema,
                        self.render_ms_ema,
                        self.block_history.len(),
                    );
                }
            });

        // Request continuous repaints while running (or paused — for overlay)
        if self.is_running() || self.display_paused {
            ctx.request_repaint();
        }
    }
}

// ── Overlays ────────────────────────────────────────────────────────

fn draw_paused_overlay(ui: &egui::Ui, rect: egui::Rect) {
    let badge_pos = rect.center_top() + egui::vec2(0.0, 18.0);
    let painter = ui.painter();
    let text = "  PAUSED  (press P to resume)  ";
    painter.text(
        badge_pos,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(13.0),
        theme::ACCENT_YELLOW,
    );
}

fn draw_perf_overlay(
    ui: &egui::Ui,
    rect: egui::Rect,
    frame_ms: f64,
    render_ms: f64,
    history_blocks: usize,
) {
    let painter = ui.painter();
    let pos = rect.right_top() + egui::vec2(-8.0, 8.0);
    let fps = if frame_ms > 0.01 { 1000.0 / frame_ms } else { 0.0 };
    let lines = [
        format!("FPS    {:>6.1}", fps),
        format!("Frame  {:>5.1} ms", frame_ms),
        format!("Render {:>5.1} ms", render_ms),
        format!("Hist   {:>5} blk", history_blocks),
    ];

    // Background panel
    let bg = egui::Rect::from_min_size(pos + egui::vec2(-110.0, -2.0), egui::vec2(108.0, 60.0));
    painter.rect_filled(
        bg,
        egui::CornerRadius::same(3),
        egui::Color32::from_rgba_premultiplied(20, 20, 26, 200),
    );

    for (i, line) in lines.iter().enumerate() {
        painter.text(
            pos + egui::vec2(-6.0, 4.0 + i as f32 * 13.0),
            egui::Align2::RIGHT_TOP,
            line,
            egui::FontId::monospace(11.0),
            theme::TEXT_PRIMARY,
        );
    }
}
