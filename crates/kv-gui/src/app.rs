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

use kv_recorder::StreamingRecorder;

use crate::demo::DemoPreview;
use crate::disp_ring::DisplayRing;
use crate::dsp::{Biquad, FilterChain, Q_BUTTERWORTH, Q_NOTCH};
use crate::live_pipeline::{self, LivePipelineHandle, RecorderCmd, RecorderEvent};
use crate::multiview::{self, AddViewRequest, KvTileBehavior, TileKind};
use crate::spike_overlay::SpikeSnippetStore;
use crate::panels::{self, DisplaySettings, FilterSettings, RecordingSettings, RecordingState};
use crate::preview::{BlockStats, compute_block_stats};
use crate::theme;


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
    // Device (live pipeline: producer thread + recorder thread + preview channel)
    live_pipeline: Option<LivePipelineHandle>,
    // Shared view state
    block_history: VecDeque<SampleBlock>,
    /// Pre-filtered version of block_history (kept in sync).
    filtered_history: VecDeque<SampleBlock>,
    history_capacity: usize,
    latest_block: Option<SampleBlock>,
    latest_stats: Option<BlockStats>,
    // Persistent filter state — chains are maintained between frames so
    // only newly-arrived samples need to be processed (O(new) not O(window)).
    filter_chains: Vec<FilterChain>,
    /// The settings that `filter_chains` was built with.  When the user
    /// changes filter parameters we detect the mismatch and rebuild.
    filter_settings_snapshot: FilterSettings,
    /// Pre-computed display ring buffer — user-configured filter (main tile).
    disp_ring: DisplayRing,
    /// Fixed LP 250 Hz ring for the LFP tile.
    disp_ring_lfp: DisplayRing,
    /// Fixed HP 300 Hz ring for the AP / spike tile.
    disp_ring_ap: DisplayRing,
    /// Persistent filter chains for the fixed LFP ring.
    filter_chains_lfp: Vec<FilterChain>,
    /// Persistent filter chains for the fixed AP ring.
    filter_chains_ap: Vec<FilterChain>,
    /// egui_tiles layout tree — held as Option so it can be temporarily taken
    /// out during update() to allow field-level borrows alongside it.
    tile_tree: Option<egui_tiles::Tree<TileKind>>,
    /// Spike snippet store — fed from the AP-filtered blocks each ingest.
    snippet_store: SpikeSnippetStore,
    // UI state
    display: DisplaySettings,
    filters: FilterSettings,
    recording: RecordingSettings,
    /// Active streaming recorder — Some while RecordingState::Recording.
    active_recorder: Option<StreamingRecorder>,
    /// Wall-clock instant when the current recording session started.
    recording_start_time: Option<Instant>,
    /// Recorder buffer fill level (0.0 = empty, 1.0 = full), updated ~5/s.
    recorder_buffer_occupancy: f64,
    /// Latest error from the recorder thread (None = no error / dismissed).
    recording_error: Option<String>,
    theme_applied: bool,
    /// When true, the waveform display is frozen at the current view but
    /// acquisition and recording continue uninterrupted.
    pub display_paused: bool,
    /// The elapsed time captured the moment the display was paused.
    paused_elapsed: f64,
    /// Left edge of the current sweep window (ms since acquisition start).
    ///
    /// In sweep mode the X bounds are fixed: [sweep_start_ms, sweep_start_ms + window_ms].
    /// A cursor sweeps right.  When latest_data_time overflows the right edge, the
    /// sweep resets — matching the default display mode of SpikeGLX and Intan RHX.
    sweep_start_ms: f64,
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
        let filters = FilterSettings::default();
        Self {
            mode: AcqMode::Demo,
            demo: DemoPreview::default_neural(),
            demo_started: false,
            demo_last_tick: now,
            demo_blocks_generated: 0,
            demo_start_time: now,
            live_pipeline: None,
            // 20s at 30kHz / 64spc ≈ 9375 blocks; round up with margin
            block_history: VecDeque::with_capacity(10_000),
            filtered_history: VecDeque::with_capacity(10_000),
            history_capacity: 10_000,
            latest_block: None,
            latest_stats: None,
            filter_chains: Vec::new(),
            filter_settings_snapshot: filters,
            disp_ring: DisplayRing::new(16, 30_000.0),
            disp_ring_lfp: DisplayRing::new(16, 30_000.0),
            disp_ring_ap: DisplayRing::new(16, 30_000.0),
            filter_chains_lfp: Vec::new(),
            filter_chains_ap: Vec::new(),
            tile_tree: Some(multiview::make_initial_tree(16)),
            snippet_store: SpikeSnippetStore::new(16, 30_000.0),
            display: DisplaySettings::default(),
            filters,
            recording: RecordingSettings::default(),
            active_recorder: None,
            recording_start_time: None,
            recorder_buffer_occupancy: 0.0,
            recording_error: None,
            theme_applied: false,
            display_paused: false,
            paused_elapsed: 0.0,
            sweep_start_ms: 0.0,
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
        self.filtered_history.clear();
        self.filter_chains.clear();
        self.filter_chains_lfp.clear();
        self.filter_chains_ap.clear();
        self.disp_ring.reset();
        self.disp_ring_lfp.reset();
        self.disp_ring_ap.reset();
        self.snippet_store.reconfigure(16, 30_000.0);
        self.sweep_start_ms = 0.0;
        self.latest_block = None;
        self.latest_stats = None;
        // Stop any live pipeline that was running in Device mode
        self.live_pipeline = None;
    }

    /// Switch to Device mode: start the live pipeline (producer + recorder threads).
    fn start_device(&mut self) {
        self.mode = AcqMode::Device;
        self.demo_started = false;
        self.block_history.clear();
        self.filtered_history.clear();
        self.filter_chains.clear();
        self.filter_chains_lfp.clear();
        self.filter_chains_ap.clear();
        self.disp_ring.reset();
        self.disp_ring_lfp.reset();
        self.disp_ring_ap.reset();
        self.snippet_store.reconfigure(16, 30_000.0);
        self.sweep_start_ms = 0.0;
        self.latest_block = None;
        self.latest_stats = None;
        let config = SimulatorConfig::default();
        self.live_pipeline = Some(live_pipeline::start_live_pipeline(config));
    }

    fn stop_all(&mut self) {
        // Finalize any active recording first.
        if self.recording.state == RecordingState::Recording {
            match self.mode {
                AcqMode::Demo => {
                    if let Some(rec) = self.active_recorder.take() {
                        match rec.finish() {
                            Ok(summary) => eprintln!(
                                "[recorder] Auto-stopped: {} blocks saved → {}",
                                summary.recording.block_count,
                                summary.recording.raw_path.display()
                            ),
                            Err(e) => eprintln!("[recorder] Auto-stop finish error: {e}"),
                        }
                    }
                }
                AcqMode::Device => {
                    // Recorder thread will finalize on Terminate (sent by pipeline stop below)
                }
            }
            self.recording.state = RecordingState::Idle;
        }
        self.demo_started = false;
        // Dropping the handle stops both producer and recorder threads cleanly.
        self.live_pipeline = None;
    }

    /// Build LP 250 Hz filter chains (used by the fixed LFP ring).
    fn build_lfp_chains(channel_count: usize, sample_rate: f64) -> Vec<FilterChain> {
        (0..channel_count)
            .map(|_| {
                let mut chain = FilterChain::passthrough();
                if sample_rate > 500.0 {
                    chain.lp = Biquad::lowpass(250.0, sample_rate, Q_BUTTERWORTH);
                    chain.lp_enabled = true;
                }
                chain
            })
            .collect()
    }

    /// Build HP 300 Hz filter chains (used by the fixed AP / spike ring).
    fn build_ap_chains(channel_count: usize, sample_rate: f64) -> Vec<FilterChain> {
        (0..channel_count)
            .map(|_| {
                let mut chain = FilterChain::passthrough();
                if sample_rate > 600.0 {
                    chain.hp = Biquad::highpass(300.0, sample_rate, Q_BUTTERWORTH);
                    chain.hp_enabled = true;
                }
                chain
            })
            .collect()
    }

    /// Rebuild filter chains from the current FilterSettings.
    fn rebuild_filter_chains(&mut self, sample_rate: f64, channel_count: usize) {
        self.filter_chains.clear();
        for _ in 0..channel_count {
            let mut chain = FilterChain::passthrough();
            if self.filters.hp_enabled && self.filters.hp_cutoff_hz > 0.0 {
                chain.hp = Biquad::highpass(self.filters.hp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
                chain.hp_enabled = true;
            }
            if self.filters.lp_enabled && self.filters.lp_cutoff_hz < sample_rate / 2.0 {
                chain.lp = Biquad::lowpass(self.filters.lp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
                chain.lp_enabled = true;
            }
            if self.filters.notch_enabled {
                chain.notch = Biquad::notch(self.filters.notch_freq_hz(), sample_rate, Q_NOTCH);
                chain.notch_enabled = true;
            }
            self.filter_chains.push(chain);
        }
        self.filter_settings_snapshot = self.filters;
        // Re-filter existing history with new chains
        self.refilter_history();
    }

    /// Re-filter the entire block_history (called when filter settings change).
    fn refilter_history(&mut self) {
        self.filtered_history.clear();
        // Rebuild chains fresh for the re-filter pass
        let sample_rate = self.latest_block.as_ref().map(|b| b.sample_rate).unwrap_or(30000.0);
        let channel_count = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(16);
        let mut chains: Vec<FilterChain> = Vec::with_capacity(channel_count);
        for _ in 0..channel_count {
            let mut chain = FilterChain::passthrough();
            if self.filters.hp_enabled && self.filters.hp_cutoff_hz > 0.0 {
                chain.hp = Biquad::highpass(self.filters.hp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
                chain.hp_enabled = true;
            }
            if self.filters.lp_enabled && self.filters.lp_cutoff_hz < sample_rate / 2.0 {
                chain.lp = Biquad::lowpass(self.filters.lp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
                chain.lp_enabled = true;
            }
            if self.filters.notch_enabled {
                chain.notch = Biquad::notch(self.filters.notch_freq_hz(), sample_rate, Q_NOTCH);
                chain.notch_enabled = true;
            }
            chains.push(chain);
        }
        let car_enabled = self.filters.car_enabled;
        for block in self.block_history.iter() {
            let filtered = Self::filter_block_with_chains(block, &mut chains, car_enabled);
            self.filtered_history.push_back(filtered);
        }
        // Keep persistent chains in sync (their state now reflects all history)
        self.filter_chains = chains;

        // Rebuild display ring from the re-filtered history
        if self.disp_ring.channel_count != channel_count
            || (self.disp_ring.sample_rate - sample_rate).abs() > 1.0
        {
            self.disp_ring.reconfigure(channel_count, sample_rate);
        } else {
            self.disp_ring.reset();
        }
        let needs_filter = self.filters.any_filter_enabled() || self.filters.car_enabled;
        let source = if needs_filter {
            &self.filtered_history
        } else {
            &self.block_history
        };
        for block in source.iter() {
            self.disp_ring.push_block(block);
        }
    }

    /// Apply filter chains to a single block, producing a new filtered block.
    fn filter_block_with_chains(
        block: &SampleBlock,
        chains: &mut [FilterChain],
        car_enabled: bool,
    ) -> SampleBlock {
        let ch_count = block.channel_count;
        let spc = block.samples_per_channel;
        let mut data = block.data.clone();

        for s in 0..spc {
            // CAR: subtract mean across channels at this time step
            if car_enabled && ch_count > 0 {
                let base = s * ch_count;
                let mut sum: f64 = 0.0;
                for ch in 0..ch_count {
                    sum += data[base + ch] as f64;
                }
                let mean = sum / ch_count as f64;
                for ch in 0..ch_count {
                    data[base + ch] = (data[base + ch] as f64 - mean) as i16;
                }
            }
            // Per-channel biquad filter
            for ch in 0..ch_count.min(chains.len()) {
                let idx = s * ch_count + ch;
                let x = data[idx] as f64 / i16::MAX as f64;
                let y = chains[ch].process(x);
                data[idx] = (y * i16::MAX as f64).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
            }
        }

        SampleBlock {
            data,
            ..block.clone()
        }
    }

    /// Process a new incoming block: store raw, filter incrementally, store filtered,
    /// and push the display-ready version into the display ring buffer.
    fn ingest_block(&mut self, block: SampleBlock) {
        let ch_count = block.channel_count;
        let sample_rate = block.sample_rate;

        // Detect filter settings change → rebuild chains
        if self.filters != self.filter_settings_snapshot || self.filter_chains.len() != ch_count {
            self.rebuild_filter_chains(sample_rate, ch_count);
        }

        // Reconfigure all display rings if channel count or sample rate changed.
        let ring_needs_reconfigure = self.disp_ring.channel_count != ch_count
            || (self.disp_ring.sample_rate - sample_rate).abs() > 1.0;
        if ring_needs_reconfigure {
            self.disp_ring.reconfigure(ch_count, sample_rate);
            self.disp_ring_lfp.reconfigure(ch_count, sample_rate);
            self.disp_ring_ap.reconfigure(ch_count, sample_rate);
            // Rebuild fixed chains to match new channel count / sample rate.
            self.filter_chains_lfp = Self::build_lfp_chains(ch_count, sample_rate);
            self.filter_chains_ap  = Self::build_ap_chains(ch_count, sample_rate);
        } else {
            // Lazy-initialise fixed chains on first block.
            if self.filter_chains_lfp.len() != ch_count {
                self.filter_chains_lfp = Self::build_lfp_chains(ch_count, sample_rate);
            }
            if self.filter_chains_ap.len() != ch_count {
                self.filter_chains_ap = Self::build_ap_chains(ch_count, sample_rate);
            }
        }

        // Produce filtered version using persistent chains
        let needs_filter = self.filters.any_filter_enabled() || self.filters.car_enabled;
        let filtered = if needs_filter {
            Self::filter_block_with_chains(&block, &mut self.filter_chains, self.filters.car_enabled)
        } else {
            block.clone()
        };

        // Feed main display ring from the display-ready (filtered or raw) block.
        self.disp_ring.push_block(if needs_filter { &filtered } else { &block });

        // Feed fixed-filter rings (always incremental, never CAR).
        let lfp_block = Self::filter_block_with_chains(&block, &mut self.filter_chains_lfp, false);
        self.disp_ring_lfp.push_block(&lfp_block);

        let ap_block = Self::filter_block_with_chains(&block, &mut self.filter_chains_ap, false);
        self.disp_ring_ap.push_block(&ap_block);

        // Feed AP-filtered block to the spike snippet detector.
        if self.snippet_store.channel_count() != ch_count {
            self.snippet_store.reconfigure(ch_count, sample_rate);
        }
        self.snippet_store.process_block(&ap_block);

        // Feed raw block to the streaming recorder — Demo mode only.
        // Device mode recording is handled by the recorder thread in live_pipeline.
        if self.mode == AcqMode::Demo && self.recording.state == RecordingState::Recording {
            if let Some(ref mut rec) = self.active_recorder {
                match rec.write_block(&block) {
                    Ok(()) => {
                        self.recording.recorded_blocks =
                            self.recording.recorded_blocks.saturating_add(1);
                        self.recording.recorded_bytes = self
                            .recording
                            .recorded_bytes
                            .saturating_add(block.data.len() as u64 * 2);
                    }
                    Err(e) => {
                        eprintln!("[recorder] write_block error: {e}");
                    }
                }
            }
        }

        // Store raw + filtered history for pause/browse and re-filter
        self.block_history.push_back(block.clone());
        self.filtered_history.push_back(filtered);
        while self.block_history.len() > self.history_capacity {
            self.block_history.pop_front();
            self.filtered_history.pop_front();
        }

        self.latest_block = Some(block);
    }

    fn is_running(&self) -> bool {
        match self.mode {
            AcqMode::Demo => self.demo_started,
            AcqMode::Device => self.live_pipeline.is_some(),
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
            RecordingState::Armed => match self.mode {
                AcqMode::Device => {
                    // Device mode: tell the recorder thread to open the file.
                    if let Some(ref pipeline) = self.live_pipeline {
                        let path: std::path::PathBuf = self.recording.output_dir.clone().into();
                        let _ = pipeline.recorder_cmd_tx.send(RecorderCmd::Start(path));
                        self.recording.state = RecordingState::Recording;
                        self.recording.recorded_blocks = 0;
                        self.recording.recorded_bytes = 0;
                        self.recording_start_time = Some(Instant::now());
                        self.recording_error = None;
                    }
                }
                AcqMode::Demo => {
                    // Demo mode: open recorder in GUI thread (legacy path).
                    match StreamingRecorder::new(&self.recording.output_dir) {
                        Ok(rec) => {
                            self.active_recorder = Some(rec);
                            self.recording.state = RecordingState::Recording;
                            self.recording.recorded_blocks = 0;
                            self.recording.recorded_bytes = 0;
                            self.recording_start_time = Some(Instant::now());
                            self.recording_error = None;
                        }
                        Err(e) => {
                            self.recording_error = Some(format!("Failed to open output: {e}"));
                        }
                    }
                }
            },
            RecordingState::Recording => match self.mode {
                AcqMode::Device => {
                    // Device mode: tell the recorder thread to finalize.
                    // State will be updated to Idle via RecorderEvent::Stopped.
                    if let Some(ref pipeline) = self.live_pipeline {
                        let _ = pipeline.recorder_cmd_tx.send(RecorderCmd::Stop);
                        self.recording_start_time = None;
                        self.recorder_buffer_occupancy = 0.0;
                    }
                }
                AcqMode::Demo => {
                    // Demo mode: finalize in GUI thread.
                    if let Some(rec) = self.active_recorder.take() {
                        match rec.finish() {
                            Ok(summary) => {
                                eprintln!(
                                    "[recorder] Saved {} blocks ({} bytes) → {}",
                                    summary.recording.block_count,
                                    summary.recording.byte_count,
                                    summary.recording.raw_path.display()
                                );
                            }
                            Err(e) => {
                                self.recording_error =
                                    Some(format!("Finish error: {e}"));
                            }
                        }
                    }
                    self.recording.state = RecordingState::Idle;
                    self.recording_start_time = None;
                    self.recorder_buffer_occupancy = 0.0;
                }
            },
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
                    .live_pipeline
                    .as_ref()
                    .map(|p| p.start_time.elapsed().as_secs_f64())
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

        for _ in 0..generate {
            let block = self.demo.next_block();
            self.ingest_block(block);
            self.demo_blocks_generated += 1;
        }

        if let Some(ref block) = self.latest_block {
            let stats = crate::preview::compute_block_stats(
                block,
                self.demo_blocks_generated,
                elapsed_total,
            );
            self.latest_stats = Some(stats);
        }
    }

    /// Poll the live pipeline for new blocks and recorder events.
    fn tick_device(&mut self) {
        if self.live_pipeline.is_none() {
            return;
        }

        // ── Collect recorder events and preview blocks while holding the borrow ──
        // We must release the borrow before calling self.ingest_block() or
        // mutating other self fields, so collect into locals first.

        let mut recorder_events: Vec<RecorderEvent> = Vec::new();
        let mut preview_blocks: Vec<SampleBlock> = Vec::new();

        {
            let pipeline = self.live_pipeline.as_mut().unwrap();

            while let Ok(event) = pipeline.event_rx.try_recv() {
                recorder_events.push(event);
            }

            while let Ok(block) = pipeline.preview_rx.try_recv() {
                pipeline.total_blocks += 1;
                preview_blocks.push(block);
            }
        } // borrow released here

        // ── Process recorder events ──────────────────────────────────────────
        for event in recorder_events {
            match event {
                RecorderEvent::Started => {}
                RecorderEvent::Stopped { blocks, bytes } => {
                    self.recording.recorded_blocks = blocks;
                    self.recording.recorded_bytes = bytes;
                    self.recording.state = RecordingState::Idle;
                    self.recording_start_time = None;
                    self.recorder_buffer_occupancy = 0.0;
                }
                RecorderEvent::Error(e) => {
                    self.recording_error = Some(e);
                }
                RecorderEvent::BufferStatus { occupancy } => {
                    self.recorder_buffer_occupancy = occupancy;
                }
            }
        }

        // ── Ingest all preview blocks ─────────────────────────────────────────
        let last_block = preview_blocks.last().cloned();
        for block in preview_blocks {
            self.ingest_block(block);
        }

        // Update stats from the most-recent block received this frame.
        if let Some(ref block) = last_block {
            let pipeline = self.live_pipeline.as_ref().unwrap();
            let elapsed = pipeline.start_time.elapsed().as_secs_f64();
            self.latest_stats = Some(compute_block_stats(block, pipeline.total_blocks, elapsed));
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

        // Advance snippet ages each frame (drives fade-out animation).
        self.snippet_store.advance_frames();

        // Detect filter settings change (user toggled in UI) — re-filter history
        if self.filters != self.filter_settings_snapshot {
            let sr = self.latest_block.as_ref().map(|b| b.sample_rate).unwrap_or(30000.0);
            let ch = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(16);
            self.rebuild_filter_chains(sr, ch);
        }

        let elapsed_live = self.elapsed_seconds();
        // Use frozen elapsed for display when paused
        let elapsed = if self.display_paused {
            self.paused_elapsed
        } else {
            elapsed_live
        };

        // ── Sweep-mode window management ─────────────────────────
        // Advance sweep_start_ms when new data has filled the current window.
        // This keeps x_left / x_right FIXED within a sweep — the entire display
        // is stationary and only the cursor moves right, matching the SpikeGLX /
        // Intan RHX default display mode.  When the window fills, the display
        // resets to a new window (brief flash, once per window duration).
        if !self.display_paused && self.disp_ring.ready {
            let latest_ms = self.disp_ring.latest_time_ms();
            let window_ms = self.display.time_window_ms();
            if latest_ms >= self.sweep_start_ms + window_ms {
                // Snap to the most recent complete window boundary
                self.sweep_start_ms =
                    (latest_ms / window_ms).floor() * window_ms;
            }
        }

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

                    // Pause button — only shown when running or already paused
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
                        if theme::transport_button(ui, pause_label, pause_color, true) {
                            self.toggle_pause_display();
                        }
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
                let mut toggle_rec = false;

                // Compute elapsed recording seconds for the clock display.
                let rec_elapsed_secs = self
                    .recording_start_time
                    .map(|t| t.elapsed().as_secs_f64());

                let mut dismiss_error = false;
                panels::draw_left_panel(
                    ui,
                    self.is_running(),
                    &mut start,
                    &mut stop,
                    &mut toggle_rec,
                    &mut self.display,
                    &mut self.filters,
                    &mut self.recording,
                    self.latest_block.as_ref(),
                    rec_elapsed_secs,
                    self.recorder_buffer_occupancy,
                    self.recording_error.as_deref(),
                    &mut dismiss_error,
                );
                if dismiss_error {
                    self.recording_error = None;
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
            });

        // ── Multi-view tile canvas ──────────────────────────────
        //
        // The CentralPanel now hosts an egui_tiles Tree.  All waveform tiles
        // share the same sweep_start_ms time axis.  The tile tree is temporarily
        // taken out of self so that KvTileBehavior can hold field-level borrows
        // of self simultaneously.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_DARKEST)
                    .inner_margin(egui::Margin::symmetric(2, 2)),
            )
            .show(ctx, |ui| {
                let mut tree = self.tile_tree.take().expect("tile_tree always present");

                let elapsed_secs = self.elapsed_seconds();
                let mut pending_add: Option<AddViewRequest> = None;

                {
                    let mut behavior = KvTileBehavior {
                        disp_ring:     &self.disp_ring,
                        disp_ring_lfp: &self.disp_ring_lfp,
                        disp_ring_ap:  &self.disp_ring_ap,
                        latest_block:  self.latest_block.as_ref(),
                        display:       &mut self.display,
                        filters:       &self.filters,
                        display_paused: self.display_paused,
                        paused_elapsed: &mut self.paused_elapsed,
                        sweep_start_ms: self.sweep_start_ms,
                        elapsed_secs,
                        show_perf_overlay: self.show_perf_overlay,
                        render_ms_ema:     &mut self.render_ms_ema,
                        block_history_len: self.block_history.len(),
                        snippet_store: &self.snippet_store,
                        pending_add:   &mut pending_add,
                    };
                    tree.ui(&mut behavior, ui);
                }

                // Process any add-view request that came out of the tile UI.
                if let Some(req) = pending_add {
                    let visible = self.display.visible_channels;
                    let kind = match req {
                        AddViewRequest::Lfp => TileKind::new_lfp(visible),
                        AddViewRequest::Ap  => TileKind::new_ap(visible),
                        AddViewRequest::SpikeOverlay => TileKind::new_spike_overlay(),
                    };
                    multiview::add_view_to_tree(&mut tree, kind);
                }

                self.tile_tree = Some(tree);
            });

        // Request continuous repaints while running (or paused — for overlay)
        if self.is_running() || self.display_paused {
            ctx.request_repaint();
        }
    }
}

// Overlay helpers are now handled inside multiview::KvTileBehavior::draw_main_waveform().
