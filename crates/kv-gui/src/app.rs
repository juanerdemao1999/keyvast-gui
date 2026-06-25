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
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use kv_rhd::RhdHardwareOptions;
use kv_simulator::SimulatorConfig;
use kv_types::SampleBlock;

use kv_recorder::StreamingRecorder;

use crate::channel_map::{self, ChannelMapState};
use crate::channel_select::{self, ChannelSelectState};
use crate::config_persist::{self, ConfigPersistState, PersistentConfig};
use crate::demo::DemoPreview;
use crate::disp_ring::DisplayRing;
use crate::dsp::{Biquad, FilterChain, Q_BUTTERWORTH, Q_NOTCH};
use crate::fft_panel::{self, FftState};
use crate::impedance_panel::{self, ImpedanceMsg, ImpedanceState};
use crate::live_pipeline::{self, LivePipelineHandle, PipelineSource, RecorderCmd, RecorderEvent};
use crate::multiview::{self, AddViewRequest, KvTileBehavior, TileKind};
use crate::panels::{
    self, DeviceKind, DeviceSettings, DisplaySettings, FilterSettings, RecordingSettings,
    RecordingState,
};
use crate::playback::{self, PlaybackManager};
use crate::preview::{BlockStats, compute_block_stats};
use crate::remote_api::{
    self, AppStatus, RemoteApiHandle, RemoteApiState, RemoteCommand, RemoteResponse,
};
use crate::spike_overlay::SpikeSnippetStore;
use crate::theme;
use crate::toast::Toasts;
use crate::trigger::{self, TriggerAction, TriggerConfig, TtlHistory};

/// How long filter settings must stay unchanged before the full history
/// is re-filtered (lets slider drags settle first).
const REFILTER_DEBOUNCE_MS: u64 = 150;

// ── Acquisition mode ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcqMode {
    Demo,
    Device,
}

// ── Data source (B1) ────────────────────────────────────────────────

/// The single, top-level data source the display is currently bound to.
///
/// Exactly one source feeds the waveform at a time, so the user always knows
/// where the on-screen signal comes from:
///   - `Demo`     — synthetic neural generator (no hardware).
///   - `Device`   — live acquisition (simulator or RHD backend).
///   - `Playback` — an offline `.kvraw` recording.
///
/// `Demo`/`Device` correspond to the live [`AcqMode`]; `Playback` drives the
/// [`PlaybackManager`] instead.  Live acquisition and playback are mutually
/// exclusive — selecting one stops the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSource {
    Demo,
    Device,
    Playback,
}

// ── Application state ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Acquire,
    Display,
    Tools,
}

pub struct KvApp {
    // Sidebar
    sidebar_tab: SidebarTab,
    // Mode
    mode: AcqMode,
    /// Top-level data source (Demo / Device / Playback). Drives which source
    /// feeds the display and enforces live-vs-playback mutual exclusion.
    data_source: DataSource,
    /// Whether the keyboard-shortcut help overlay is shown.
    show_help: bool,
    /// Transient toast notifications (top-right stack).
    toasts: Toasts,
    /// One-shot guard so the first-run welcome hint (B7) only fires once.
    welcomed: bool,
    // Demo
    demo: DemoPreview,
    demo_started: bool,
    demo_last_tick: Instant,
    demo_blocks_generated: u64,
    demo_start_time: Instant,
    // Device (live pipeline: producer thread + recorder thread + preview channel)
    live_pipeline: Option<LivePipelineHandle>,
    /// User-selected acquisition source (simulator or RHD hardware) + config.
    device: DeviceSettings,
    /// Latest acquisition-source error (device open / read failure).
    /// Surfaced as a dismissible banner; does not interrupt the GUI.
    device_error: Option<String>,
    // Shared view state
    block_history: VecDeque<SampleBlock>,
    /// Pre-filtered version of block_history. Only populated while a user
    /// filter (or CAR) is active — empty otherwise to avoid duplicating
    /// block_history in memory.
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
    /// Instant of the most recent (still unapplied) filter-settings change.
    /// Re-filtering the full history is debounced behind this so dragging a
    /// cutoff slider doesn't re-filter 10k blocks every frame.
    filter_change_pending_since: Option<Instant>,
    /// Filter settings as of the previous frame (debounce change detection).
    filters_last_frame: FilterSettings,
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
    // Phase 1 features
    impedance: ImpedanceState,
    /// Receiver for progress/results from a background impedance thread.
    impedance_rx: Option<std::sync::mpsc::Receiver<ImpedanceMsg>>,
    playback_mgr: PlaybackManager,
    // Phase 2 features
    fft: FftState,
    channel_map: ChannelMapState,
    // Phase 3 features
    trigger: TriggerConfig,
    /// Rolling TTL transition history feeding the live TTL monitor tile.
    ttl_history: TtlHistory,
    remote_api_state: RemoteApiState,
    remote_api_handle: Option<RemoteApiHandle>,
    /// Export format (for recording panel UI)
    export_format: kv_recorder::export_formats::ExportFormat,
    /// Receiver for the result of a background .kvraw export.
    export_rx: Option<std::sync::mpsc::Receiver<Result<std::path::PathBuf, String>>>,
    /// Outcome of the last export (output path or error message).
    export_status: Option<String>,
    /// Channel subset captured when a Demo-mode recording starts.
    record_channels: Option<Vec<usize>>,
    // Phase 4 features
    channel_select: ChannelSelectState,
    config_persist: ConfigPersistState,
    /// Global UI scale (pixels-per-point multiplier), persisted (#17).
    ui_scale: f32,
    /// Set once after the first frame restores the saved window size (#15).
    window_restored: bool,
    /// Window size persisted at startup, reused on the first frame so the
    /// config file is read once (in `new`) rather than again in `update`.
    restore_window_size: (f32, f32),
}

impl KvApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let now = Instant::now();
        let filters = FilterSettings::default();

        // Restore persisted settings at startup (#15/#17).  Missing or invalid
        // files fall back to defaults, so this never blocks launch.
        let saved = config_persist::load_or_default();
        let ui_scale = saved
            .ui_scale
            .clamp(config_persist::UI_SCALE_MIN, config_persist::UI_SCALE_MAX);
        let start_source = match saved.last_source.as_str() {
            "device" => DataSource::Device,
            "playback" => DataSource::Playback,
            _ => DataSource::Demo,
        };

        let mut app = Self {
            sidebar_tab: SidebarTab::Acquire,
            mode: AcqMode::Demo,
            data_source: start_source,
            show_help: false,
            toasts: Toasts::default(),
            welcomed: false,
            demo: DemoPreview::default_neural(),
            demo_started: false,
            demo_last_tick: now,
            demo_blocks_generated: 0,
            demo_start_time: now,
            live_pipeline: None,
            device: DeviceSettings::default(),
            device_error: None,
            // 20s at 30kHz / 64spc ≈ 9375 blocks; round up with margin
            block_history: VecDeque::with_capacity(10_000),
            filtered_history: VecDeque::with_capacity(10_000),
            history_capacity: 10_000,
            latest_block: None,
            latest_stats: None,
            filter_chains: Vec::new(),
            filter_settings_snapshot: filters,
            filter_change_pending_since: None,
            filters_last_frame: filters,
            disp_ring: DisplayRing::new(16, 30_000.0),
            disp_ring_lfp: DisplayRing::new(16, 30_000.0),
            disp_ring_ap: DisplayRing::new(16, 30_000.0).with_peak_hold(),
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
            impedance: ImpedanceState::default(),
            impedance_rx: None,
            playback_mgr: PlaybackManager::default(),
            fft: FftState::default(),
            channel_map: ChannelMapState::default(),
            trigger: TriggerConfig::default(),
            ttl_history: TtlHistory::new(),
            remote_api_state: RemoteApiState::default(),
            remote_api_handle: None,
            export_format: kv_recorder::export_formats::ExportFormat::KeyvastNative,
            export_rx: None,
            export_status: None,
            record_channels: None,
            channel_select: ChannelSelectState::default(),
            config_persist: ConfigPersistState::default(),
            ui_scale,
            window_restored: false,
            restore_window_size: (saved.window_width, saved.window_height),
        };

        // Apply the persisted display/filter/recording settings to live state.
        saved.apply_to(
            &mut app.display,
            &mut app.filters,
            &mut app.recording.output_dir,
            &mut app.recording.file_prefix,
            &mut app.remote_api_state.port,
        );
        app.filter_settings_snapshot = app.filters;
        app.filters_last_frame = app.filters;
        app.config_persist.loaded = true;

        app
    }

    /// Snapshot the current settings (including UI scale, window size and the
    /// active source) into a [`PersistentConfig`] for saving (#15/#17).
    fn capture_persistent(&self, ctx: &egui::Context) -> PersistentConfig {
        let mut cfg = PersistentConfig::capture_from(
            &self.display,
            &self.filters,
            &self.recording.output_dir,
            &self.recording.file_prefix,
            self.remote_api_state.port,
        );
        cfg.ui_scale = self.ui_scale;
        let size = ctx.screen_rect().size();
        cfg.window_width = size.x;
        cfg.window_height = size.y;
        cfg.last_source = match self.data_source {
            DataSource::Demo => "demo",
            DataSource::Device => "device",
            DataSource::Playback => "playback",
        }
        .to_string();
        cfg
    }

    /// Switch to Demo mode and start generating.
    fn start_demo(&mut self) {
        self.mode = AcqMode::Demo;
        self.data_source = DataSource::Demo;
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
        self.ttl_history.clear();
        self.snippet_store
            .reconfigure(self.demo.channel_count, self.demo.sample_rate);
        self.sweep_start_ms = 0.0;
        self.latest_block = None;
        self.latest_stats = None;
        // Stop any live pipeline that was running in Device mode
        self.live_pipeline = None;
    }

    /// Switch to Device mode: start the live pipeline (producer + recorder threads).
    ///
    /// The producer backend is chosen from `self.device` (simulator or RHD).
    /// If the RHD source is selected without a bitfile, no pipeline is started
    /// and a device error is recorded for the banner instead.
    fn start_device(&mut self) {
        self.mode = AcqMode::Device;
        self.data_source = DataSource::Device;
        self.demo_started = false;
        self.block_history.clear();
        self.filtered_history.clear();
        self.filter_chains.clear();
        self.filter_chains_lfp.clear();
        self.filter_chains_ap.clear();
        self.disp_ring.reset();
        self.disp_ring_lfp.reset();
        self.disp_ring_ap.reset();
        self.ttl_history.clear();
        // snippet_store will be reconfigured lazily on the first ingest_block()
        // when the actual channel count and sample rate are known from the device.
        self.sweep_start_ms = 0.0;
        self.latest_block = None;
        self.latest_stats = None;

        match self.build_pipeline_source() {
            Ok(source) => {
                self.device_error = None;
                self.live_pipeline = Some(live_pipeline::start_live_pipeline(source));
            }
            Err(message) => {
                self.device_error = Some(message);
                self.live_pipeline = None;
            }
        }
    }

    /// Build the live-pipeline data source from the current device settings.
    /// Returns an error message (for the banner) when the selection is
    /// incomplete, e.g. RHD chosen without a bitfile.
    fn build_pipeline_source(&self) -> Result<PipelineSource, String> {
        match self.device.kind {
            DeviceKind::Simulator => Ok(PipelineSource::Simulator(SimulatorConfig::default())),
            DeviceKind::Rhd => {
                let bitfile = self.device.rhd_bitfile.clone().ok_or_else(|| {
                    "Select an FPGA bitfile before starting RHD acquisition.".to_string()
                })?;
                let options = RhdHardwareOptions::new(bitfile, self.device.rhd_streams);
                Ok(PipelineSource::Rhd(Box::new(options)))
            }
        }
    }

    fn stop_all(&mut self) {
        // Finalize any active recording first.
        if self.recording.state == RecordingState::Recording {
            match self.mode {
                AcqMode::Demo => {
                    if let Some(rec) = self.active_recorder.take() {
                        match rec.finish() {
                            Ok(summary) => log::info!(
                                "Auto-stopped: {} blocks saved → {}",
                                summary.recording.block_count,
                                summary.recording.raw_path.display()
                            ),
                            Err(e) => {
                                self.recording_error = Some(format!("Auto-stop finish error: {e}"));
                            }
                        }
                    }
                    self.record_channels = None;
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

    /// Build a fresh Vec of filter chains from the current FilterSettings.
    fn build_filter_chains(
        filters: &FilterSettings,
        sample_rate: f64,
        channel_count: usize,
    ) -> Vec<FilterChain> {
        (0..channel_count)
            .map(|_| {
                let mut chain = FilterChain::passthrough();
                if filters.hp_enabled && filters.hp_cutoff_hz > 0.0 {
                    chain.hp = Biquad::highpass(filters.hp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
                    chain.hp_enabled = true;
                }
                if filters.lp_enabled && filters.lp_cutoff_hz < sample_rate / 2.0 {
                    chain.lp = Biquad::lowpass(filters.lp_cutoff_hz, sample_rate, Q_BUTTERWORTH);
                    chain.lp_enabled = true;
                }
                if filters.notch_enabled {
                    chain.notch = Biquad::notch(filters.notch_freq_hz(), sample_rate, Q_NOTCH);
                    chain.notch_enabled = true;
                }
                chain
            })
            .collect()
    }

    /// Rebuild filter chains from the current FilterSettings.
    fn rebuild_filter_chains(&mut self, sample_rate: f64, channel_count: usize) {
        self.filter_chains = Self::build_filter_chains(&self.filters, sample_rate, channel_count);
        self.filter_settings_snapshot = self.filters;
        // Re-filter existing history with new chains
        self.refilter_history();
    }

    /// Re-filter the entire block_history (called when filter settings change).
    fn refilter_history(&mut self) {
        self.filtered_history.clear();
        // Rebuild chains fresh for the re-filter pass
        let sample_rate = self
            .latest_block
            .as_ref()
            .map(|b| b.sample_rate)
            .unwrap_or(30000.0);
        let channel_count = self
            .latest_block
            .as_ref()
            .map(|b| b.channel_count)
            .unwrap_or(16);
        let mut chains = Self::build_filter_chains(&self.filters, sample_rate, channel_count);
        let needs_filter = self.filters.any_filter_enabled() || self.filters.car_enabled;
        if needs_filter {
            let car_enabled = self.filters.car_enabled;
            for block in self.block_history.iter() {
                let filtered = Self::filter_block_with_chains(block, &mut chains, car_enabled);
                self.filtered_history.push_back(filtered);
            }
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

        // Rebuild chains on channel-count change. Filter-settings changes are
        // handled (debounced) in update() so slider drags don't re-filter the
        // full history every frame.
        if self.filter_chains.len() != ch_count {
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
            self.filter_chains_ap = Self::build_ap_chains(ch_count, sample_rate);
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
            Some(Self::filter_block_with_chains(
                &block,
                &mut self.filter_chains,
                self.filters.car_enabled,
            ))
        } else {
            None
        };

        // Feed main display ring from the display-ready (filtered or raw) block.
        self.disp_ring
            .push_block(filtered.as_ref().unwrap_or(&block));

        // Feed fixed-filter rings (always incremental, never CAR) — but only
        // when a tile actually consumes them, so the default single-tile
        // layout pays for one filter pass instead of three.
        if self.lfp_tile_open() {
            let lfp_block =
                Self::filter_block_with_chains(&block, &mut self.filter_chains_lfp, false);
            self.disp_ring_lfp.push_block(&lfp_block);
        }

        // The AP band feeds both the AP tile and the spike snippet detector.
        if self.ap_band_needed() {
            let ap_block =
                Self::filter_block_with_chains(&block, &mut self.filter_chains_ap, false);
            self.disp_ring_ap.push_block(&ap_block);

            if self.snippet_store.channel_count() != ch_count {
                self.snippet_store.reconfigure(ch_count, sample_rate);
            }
            self.snippet_store.process_block(&ap_block);
        }

        // Phase 3: feed the TTL monitor and process the recording gate.
        self.ttl_history.push_block(&block);
        let trigger_action = self.trigger.process_block(&block);
        match trigger_action {
            TriggerAction::StartRecording => {
                if self.recording.state != RecordingState::Recording {
                    self.begin_recording();
                }
            }
            TriggerAction::StopRecording => {
                if self.recording.state == RecordingState::Recording {
                    self.stop_recording();
                }
            }
            TriggerAction::None => {}
        }

        // Feed the block to the streaming recorder — Demo mode only.
        // Device mode recording is handled by the recorder thread in live_pipeline.
        let mut demo_write_failed = false;
        if self.mode == AcqMode::Demo
            && self.recording.state == RecordingState::Recording
            && let Some(ref mut rec) = self.active_recorder
        {
            let (write_result, written_values) = match self.record_channels {
                Some(ref indices) => {
                    let filtered = channel_select::filter_block_channels(&block, indices);
                    let len = filtered.data.len() as u64;
                    (rec.write_block(&filtered), len)
                }
                None => (rec.write_block(&block), block.data.len() as u64),
            };
            match write_result {
                Ok(()) => {
                    self.recording.recorded_blocks =
                        self.recording.recorded_blocks.saturating_add(1);
                    self.recording.recorded_bytes = self
                        .recording
                        .recorded_bytes
                        .saturating_add(written_values * 2);
                }
                Err(e) => {
                    // A failed write means the file can no longer be
                    // trusted — stop instead of writing into a possibly
                    // corrupt recording.
                    self.recording_error = Some(format!(
                        "Recording write failed: {e} — recording stopped; file may be incomplete"
                    ));
                    log::error!("recording write failed, stopping: {e}");
                    demo_write_failed = true;
                }
            }
        }
        if demo_write_failed {
            self.stop_recording();
        }

        // Store raw + filtered history for pause/browse and re-filter.
        // filtered_history stays empty while no user filter is active — the
        // raw history serves both roles in that case.
        if let Some(filtered_block) = filtered {
            self.filtered_history.push_back(filtered_block);
        }
        self.latest_block = Some(block.clone());
        self.block_history.push_back(block);
        while self.block_history.len() > self.history_capacity {
            self.block_history.pop_front();
        }
        while self.filtered_history.len() > self.history_capacity {
            self.filtered_history.pop_front();
        }
    }

    /// True when an LFP tile exists in the layout.
    fn lfp_tile_open(&self) -> bool {
        self.tile_has_pane(|kind| matches!(kind, TileKind::LfpView { .. }))
    }

    /// True when an FFT spectrum tile exists in the layout.
    fn fft_tile_open(&self) -> bool {
        self.tile_has_pane(|kind| matches!(kind, TileKind::FftSpectrum))
    }

    /// True when the AP band must be computed: an AP tile or a spike-overlay
    /// tile (which consumes the snippet detector) exists in the layout.
    fn ap_band_needed(&self) -> bool {
        self.tile_has_pane(|kind| {
            matches!(
                kind,
                TileKind::ApView { .. } | TileKind::SpikeOverlay { .. }
            )
        })
    }

    fn tile_has_pane(&self, pred: impl Fn(&TileKind) -> bool) -> bool {
        self.tile_tree.as_ref().is_some_and(|tree| {
            tree.tiles.tiles().any(|tile| match tile {
                egui_tiles::Tile::Pane(kind) => pred(kind),
                egui_tiles::Tile::Container(_) => false,
            })
        })
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

    /// Switch the top-level data source (B1).  Live acquisition and playback
    /// are mutually exclusive: choosing `Playback` stops any running
    /// acquisition (finalizing a recording first), and choosing `Demo`/`Device`
    /// leaves playback paused in the background.
    fn select_source(&mut self, src: DataSource) {
        if self.data_source == src {
            return;
        }
        match src {
            DataSource::Demo => self.start_demo(),
            DataSource::Device => self.start_device(),
            DataSource::Playback => {
                self.stop_all();
                self.data_source = DataSource::Playback;
                self.latest_block = None;
                self.latest_stats = None;
            }
        }
    }

    /// Prompt for a `.kvraw` file, load it, and switch to Playback source.
    /// Stops any live acquisition first so the two never feed the display at
    /// once.  Surfaces success / failure via a toast.
    fn open_playback_file_dialog(&mut self) {
        let Some(path) = playback::pick_kvraw_file() else {
            return;
        };
        self.stop_all();
        self.playback_mgr.load_file(path);
        self.data_source = DataSource::Playback;
        self.latest_block = None;
        self.latest_stats = None;
        match self.playback_mgr.error.clone() {
            Some(e) => self.toasts.error(format!("Open failed: {e}")),
            None => {
                self.playback_mgr.play();
                let name = self
                    .playback_mgr
                    .file_path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("recording")
                    .to_string();
                self.toasts.success(format!("Loaded {name}"));
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
                } else {
                    self.toasts.warning("Start acquisition before recording");
                }
            }
            RecordingState::Armed => self.begin_recording(),
            RecordingState::Recording => self.stop_recording(),
        }
    }

    /// Start recording (used by the Armed → Recording transition, triggers,
    /// and the remote API). Captures the channel selection at start so a
    /// mid-recording selection change cannot change the file's layout.
    fn begin_recording(&mut self) {
        let channels = self.channel_select.recording_selection();
        match self.mode {
            AcqMode::Device => {
                if let Some(ref pipeline) = self.live_pipeline {
                    let path: std::path::PathBuf = self.recording.output_dir.clone().into();
                    let _ = pipeline
                        .recorder_cmd_tx
                        .send(RecorderCmd::Start { path, channels });
                    self.recording.state = RecordingState::Recording;
                    self.recording.recorded_blocks = 0;
                    self.recording.recorded_bytes = 0;
                    self.recording_start_time = Some(Instant::now());
                    self.recording_error = None;
                    self.toasts.info("Recording started");
                }
            }
            AcqMode::Demo => match StreamingRecorder::new(&self.recording.output_dir) {
                Ok(rec) => {
                    self.active_recorder = Some(rec);
                    self.record_channels = channels;
                    self.recording.state = RecordingState::Recording;
                    self.recording.recorded_blocks = 0;
                    self.recording.recorded_bytes = 0;
                    self.recording_start_time = Some(Instant::now());
                    self.recording_error = None;
                    self.toasts.info("Recording started");
                }
                Err(e) => {
                    let msg = format!("Failed to open output: {e}");
                    self.toasts.error(msg.clone());
                    self.recording_error = Some(msg);
                }
            },
        }
    }

    /// Stop recording (used by the toggle, triggers, and the remote API).
    fn stop_recording(&mut self) {
        match self.mode {
            AcqMode::Device => {
                // Recorder thread finalizes; state goes Idle via RecorderEvent::Stopped.
                if let Some(ref pipeline) = self.live_pipeline {
                    let _ = pipeline.recorder_cmd_tx.send(RecorderCmd::Stop);
                    self.recording_start_time = None;
                    self.recorder_buffer_occupancy = 0.0;
                } else {
                    // No live pipeline (e.g. it was already torn down): there is
                    // no recorder thread to emit RecorderEvent::Stopped, so reset
                    // the recording state here instead of leaving it stuck.
                    self.recording.state = RecordingState::Idle;
                    self.recording_start_time = None;
                    self.recorder_buffer_occupancy = 0.0;
                }
            }
            AcqMode::Demo => {
                if let Some(rec) = self.active_recorder.take() {
                    match rec.finish() {
                        Ok(summary) => {
                            log::info!(
                                "Saved {} blocks ({} bytes) → {}",
                                summary.recording.block_count,
                                summary.recording.byte_count,
                                summary.recording.raw_path.display()
                            );
                            self.toasts.success(format!(
                                "Saved {} blocks ({})",
                                summary.recording.block_count,
                                theme::format_bytes(summary.recording.byte_count),
                            ));
                        }
                        Err(e) => {
                            let msg = format!("Finish error: {e}");
                            self.toasts.error(msg.clone());
                            self.recording_error = Some(msg);
                        }
                    }
                }
                self.record_channels = None;
                self.recording.state = RecordingState::Idle;
                self.recording_start_time = None;
                self.recorder_buffer_occupancy = 0.0;
            }
        }
    }

    /// Launch a background impedance measurement on the RHD hardware.
    /// The test drives the SPI bus itself, so any running acquisition is
    /// stopped first and the device is reopened by the worker thread.
    fn start_impedance_test(&mut self) {
        if self.impedance.measuring {
            return;
        }
        if self.device.kind != DeviceKind::Rhd {
            self.impedance.error =
                Some("Impedance test requires the RHD hardware source".to_string());
            return;
        }
        let Some(bitfile) = self.device.rhd_bitfile.clone() else {
            self.impedance.error =
                Some("Select an FPGA bitfile in the DEVICE panel first".to_string());
            return;
        };
        self.stop_all();

        let streams = self.device.rhd_streams;
        let config = kv_rhd::ImpedanceTestConfig {
            frequency_hz: self.impedance.frequency_hz,
            num_periods: self.impedance.num_periods,
            channel_count: kv_rhd::CHANNELS_PER_STREAM * streams,
            ..Default::default()
        };
        let options = RhdHardwareOptions::new(bitfile, streams);

        let (tx, rx) = std::sync::mpsc::channel();
        self.impedance.measuring = true;
        self.impedance.error = None;
        self.impedance.progress = (0, config.channel_count);
        self.impedance_rx = Some(rx);

        std::thread::spawn(move || {
            let backend = match kv_rhd::RhdHardwareBackend::open(options) {
                Ok(backend) => backend,
                Err(e) => {
                    let _ = tx.send(ImpedanceMsg::Failed(format!("device open failed: {e}")));
                    return;
                }
            };
            let progress_tx = tx.clone();
            let progress = move |cur: usize, total: usize| {
                let _ = progress_tx.send(ImpedanceMsg::Progress(cur, total));
            };
            match backend.run_impedance_test(&config, Some(&progress)) {
                Ok(result) => {
                    let _ = tx.send(ImpedanceMsg::Done(result));
                }
                Err(e) => {
                    let _ = tx.send(ImpedanceMsg::Failed(e.to_string()));
                }
            }
        });
    }

    /// Drain progress/results from the background impedance thread.
    fn poll_impedance(&mut self) {
        let Some(rx) = self.impedance_rx.as_ref() else {
            return;
        };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(ImpedanceMsg::Progress(cur, total)) => {
                    self.impedance.progress = (cur, total);
                }
                Ok(ImpedanceMsg::Done(result)) => {
                    self.impedance.results = Some(result);
                    self.impedance.measuring = false;
                    finished = true;
                    break;
                }
                Ok(ImpedanceMsg::Failed(e)) => {
                    self.impedance.error = Some(e);
                    self.impedance.measuring = false;
                    finished = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.impedance.error = Some("impedance thread exited unexpectedly".to_string());
                    self.impedance.measuring = false;
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.impedance_rx = None;
        }
    }

    /// Convert a .kvraw recording to the selected export format on a
    /// background thread, writing the output next to the source file.
    fn start_export(&mut self, source: std::path::PathBuf) {
        let format = self.export_format;
        let (tx, rx) = std::sync::mpsc::channel();
        self.export_rx = Some(rx);
        self.export_status = None;
        std::thread::spawn(move || {
            let _ = tx.send(export_kvraw(&source, format));
        });
    }

    /// Drain the result of the background .kvraw export, if any.
    fn poll_export(&mut self) {
        let Some(rx) = self.export_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(path)) => {
                self.toasts.success(format!(
                    "Exported \u{2192} {}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
                ));
                self.export_status = Some(format!("Exported → {}", path.display()));
                self.export_rx = None;
            }
            Ok(Err(e)) => {
                self.toasts.error(format!("Export failed: {e}"));
                self.export_status = Some(format!("Export failed: {e}"));
                self.export_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.toasts.error("Export thread exited unexpectedly");
                self.export_status = Some("export thread exited unexpectedly".to_string());
                self.export_rx = None;
            }
        }
    }

    /// Handle a remote API command and return a JSON result or error.
    fn handle_remote_command(&mut self, cmd: &RemoteCommand) -> Result<String, String> {
        match cmd {
            RemoteCommand::Ping => Ok("\"pong\"".to_string()),
            RemoteCommand::GetStatus => {
                let status = AppStatus {
                    is_running: self.is_running(),
                    is_recording: self.recording.state == RecordingState::Recording,
                    elapsed_seconds: self.elapsed_seconds(),
                    channel_count: self
                        .latest_block
                        .as_ref()
                        .map(|b| b.channel_count)
                        .unwrap_or(0),
                    sample_rate: self
                        .latest_block
                        .as_ref()
                        .map(|b| b.sample_rate)
                        .unwrap_or(0.0),
                    display_mode: match self.display.display_mode {
                        crate::panels::DisplayMode::Sweep => "sweep".to_string(),
                        crate::panels::DisplayMode::Roll => "roll".to_string(),
                    },
                    recorded_blocks: self.recording.recorded_blocks,
                };
                Ok(remote_api::format_status_json(&status))
            }
            RemoteCommand::GetChannelCount => {
                let ch = self
                    .latest_block
                    .as_ref()
                    .map(|b| b.channel_count)
                    .unwrap_or(0);
                Ok(ch.to_string())
            }
            RemoteCommand::StartAcquisition { mode } => {
                if self.is_running() {
                    return Err("already running".to_string());
                }
                match mode.as_str() {
                    "demo" => {
                        self.start_demo();
                        Ok("true".to_string())
                    }
                    "device" => {
                        self.start_device();
                        Ok("true".to_string())
                    }
                    _ => Err(format!("unknown mode: {mode}")),
                }
            }
            RemoteCommand::StopAcquisition => {
                self.stop_all();
                Ok("true".to_string())
            }
            RemoteCommand::StartRecording { output_dir } => {
                if !self.is_running() {
                    return Err("acquisition not running".to_string());
                }
                if let Some(dir) = output_dir {
                    remote_api::validate_output_dir(dir)?;
                    self.recording.output_dir = dir.clone();
                }
                self.begin_recording();
                Ok("true".to_string())
            }
            RemoteCommand::StopRecording => {
                if self.recording.state != RecordingState::Recording {
                    return Err("not recording".to_string());
                }
                self.stop_recording();
                Ok("true".to_string())
            }
            RemoteCommand::SetDisplayMode { mode } => match mode.as_str() {
                "sweep" => {
                    self.display.display_mode = crate::panels::DisplayMode::Sweep;
                    Ok("true".to_string())
                }
                "roll" => {
                    self.display.display_mode = crate::panels::DisplayMode::Roll;
                    Ok("true".to_string())
                }
                _ => Err(format!("unknown display mode: {mode}")),
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
                0, // demo mode has no drops
            );
            self.latest_stats = Some(stats);
        }
    }

    /// Poll the live pipeline for new blocks and recorder events.
    fn tick_device(&mut self) {
        // ── Collect recorder events and preview blocks while holding the borrow ──
        // We must release the borrow before calling self.ingest_block() or
        // mutating other self fields, so collect into locals first.

        let mut recorder_events: Vec<RecorderEvent> = Vec::new();
        let mut preview_blocks: Vec<Arc<SampleBlock>> = Vec::new();

        {
            let Some(pipeline) = self.live_pipeline.as_mut() else {
                return;
            };

            while let Ok(event) = pipeline.event_rx.try_recv() {
                recorder_events.push(event);
            }

            while let Ok(block) = pipeline.preview_rx.try_recv() {
                pipeline.total_blocks += 1;
                // Detect dropped blocks via packet-ID discontinuity
                if let Some(expected) = pipeline.expected_next_packet_id
                    && block.packet_id > expected
                {
                    pipeline.dropped_blocks += block.packet_id - expected;
                }
                pipeline.expected_next_packet_id = Some(block.packet_id + 1);
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
                    self.toasts.success(format!(
                        "Saved {} blocks ({})",
                        blocks,
                        theme::format_bytes(bytes),
                    ));
                }
                RecorderEvent::Progress { blocks, bytes } => {
                    self.recording.recorded_blocks = blocks;
                    self.recording.recorded_bytes = bytes;
                }
                RecorderEvent::Error(e) => {
                    self.toasts.error(format!("Recorder error: {e}"));
                    self.recording_error = Some(e);
                }
                RecorderEvent::BufferStatus { occupancy } => {
                    self.recorder_buffer_occupancy = occupancy;
                }
                RecorderEvent::SourceError(e) => {
                    // The producer (device) thread has stopped. Surface the
                    // error and tear down the pipeline so the UI shows
                    // "Disconnected" and recording cannot continue silently.
                    self.toasts.error(format!("Device error: {e}"));
                    self.device_error = Some(e);
                    self.live_pipeline = None;
                    if self.recording.state != RecordingState::Idle {
                        self.recording.state = RecordingState::Idle;
                        self.recording_start_time = None;
                    }
                    self.recorder_buffer_occupancy = 0.0;
                }
            }
        }

        // ── Process remote API commands ──────────────────────────────────────
        let remote_cmds: Vec<(u64, RemoteCommand)> = self
            .remote_api_handle
            .as_ref()
            .map(|h| remote_api::lock_recover(&h.commands).drain(..).collect())
            .unwrap_or_default();
        if !remote_cmds.is_empty() {
            let mut responses: Vec<RemoteResponse> = Vec::new();
            for (id, cmd) in remote_cmds {
                let result = self.handle_remote_command(&cmd);
                responses.push(RemoteResponse { id, result });
            }
            if let Some(ref handle) = self.remote_api_handle {
                let mut resp_q = remote_api::lock_recover(&handle.responses);
                for r in responses {
                    resp_q.push_back(r);
                }
            }
        }

        // ── Ingest all preview blocks ─────────────────────────────────────────
        let last_block = preview_blocks.last().cloned();
        for block in preview_blocks {
            // Reuse the producer's allocation when the recorder has already
            // released its copy; otherwise fall back to a single deep clone.
            let block = Arc::try_unwrap(block).unwrap_or_else(|b| (*b).clone());
            self.ingest_block(block);
        }

        // Update stats from the most-recent block received this frame.
        // (live_pipeline may have just been torn down by a SourceError above.)
        if let (Some(block), Some(pipeline)) = (last_block.as_ref(), self.live_pipeline.as_ref()) {
            let elapsed = pipeline.start_time.elapsed().as_secs_f64();
            self.latest_stats = Some(compute_block_stats(
                block,
                pipeline.total_blocks,
                elapsed,
                pipeline.dropped_blocks,
            ));
        }
    }

    /// Handle keyboard shortcuts.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Only when no text field is focused
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }

        // Snapshot the source outside the input closure to avoid borrowing
        // self twice.
        let playback = self.data_source == DataSource::Playback;

        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                if playback {
                    self.playback_mgr.toggle_play_pause();
                } else {
                    self.toggle_acquisition();
                }
            }
            // Recording is a live-acquisition action only.
            if i.key_pressed(egui::Key::R) && !playback {
                self.toggle_recording();
            }
            // Toggle the shortcut cheat-sheet overlay.
            if i.key_pressed(egui::Key::Questionmark) || i.key_pressed(egui::Key::F1) {
                self.show_help = !self.show_help;
            }
            if i.key_pressed(egui::Key::Escape) {
                self.show_help = false;
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
                    (self.display.channel_spacing + panels::SPACING_STEP).min(panels::SPACING_MAX);
            }
            if i.key_pressed(egui::Key::Minus) {
                self.display.channel_spacing =
                    (self.display.channel_spacing - panels::SPACING_STEP).max(panels::SPACING_MIN);
            }
        });
    }

    /// Draw the keyboard-shortcut cheat-sheet overlay (B4).  Shown on demand
    /// via the toolbar `?` button or the `?` / F1 key; dismissed with Esc, a
    /// click outside, or the close button.
    ///
    /// Rendered as an [`egui::Modal`] so it sits on a dimmed backdrop — the busy
    /// running waveform behind it is darkened, keeping the shortcut list legible
    /// instead of competing with the moving traces.
    fn draw_help_overlay(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("kv_help_modal"))
            // Darken the running waveform clearly so the dialog reads as modal.
            .backdrop_color(egui::Color32::from_black_alpha(160))
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Keyboard shortcuts")
                            .size(theme::FONT_HEADING)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(egui::RichText::new("\u{2715}").size(theme::FONT_BODY))
                            .on_hover_text("Close (Esc)")
                            .clicked()
                        {
                            self.show_help = false;
                        }
                    });
                });
                ui.separator();
                ui.add_space(2.0);
                let rows = [
                    (
                        "Space",
                        "Start / stop acquisition (play / pause in Playback)",
                    ),
                    ("R", "Arm \u{2192} record \u{2192} stop recording"),
                    ("P", "Pause / resume the display (acquisition continues)"),
                    ("G", "Toggle the waveform grid"),
                    ("F", "Toggle the performance overlay"),
                    ("[  ]", "Decrease / increase the time window"),
                    (
                        "1 \u{2013} 9",
                        "Quick-set visible channel count (\u{00D7}4)",
                    ),
                    ("+  \u{2212}", "Increase / decrease channel spacing"),
                    ("?  /  F1", "Toggle this help (Esc to close)"),
                ];
                egui::Grid::new("kv_help_grid")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        for (key, desc) in rows {
                            ui.label(
                                egui::RichText::new(key)
                                    .size(theme::FONT_BODY)
                                    .strong()
                                    .monospace()
                                    .color(theme::ACCENT_BLUE),
                            );
                            ui.label(theme::body(desc));
                            ui.end_row();
                        }
                    });
            });
        if modal.should_close() {
            self.show_help = false;
        }
    }
}

impl eframe::App for KvApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        let frame_delta_ms = frame_start.duration_since(self.last_frame).as_secs_f64() * 1000.0;
        self.last_frame = frame_start;
        // EMA of frame interval (~250ms time constant at 60fps)
        self.frame_ms_ema = self.frame_ms_ema * 0.9 + frame_delta_ms * 0.1;

        // Apply theme once
        if !self.theme_applied {
            theme::apply(ctx);
            self.theme_applied = true;
        }

        // Apply the persisted UI scale every frame so slider changes take
        // effect live (#17).
        ctx.set_pixels_per_point(self.ui_scale);

        // Restore the saved window size on the first frame (#15).  Done here
        // rather than in NativeOptions so it stays in sync with the same
        // config the rest of the settings come from.
        if !self.window_restored {
            self.window_restored = true;
            let (sw, sh) = self.restore_window_size;
            let w = sw.clamp(640.0, 7680.0);
            let h = sh.clamp(480.0, 4320.0);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
        }

        // Auto-save the full config when the window is closing (#15).
        if ctx.input(|i| i.viewport().close_requested()) && self.config_persist.auto_save {
            let cfg = self.capture_persistent(ctx);
            let _ = config_persist::save_config(&self.config_persist.config_path, &cfg);
        }

        // Auto-start demo on first frame
        if !self.demo_started
            && self.data_source == DataSource::Demo
            && self.mode == AcqMode::Demo
            && self.latest_block.is_none()
        {
            self.start_demo();
        }

        // First-run guidance (B7): a single welcome toast points new users at
        // the source switcher and the shortcut help.
        if !self.welcomed {
            self.welcomed = true;
            self.toasts
                .info("Streaming demo data \u{2014} switch Source or press ? for shortcuts");
        }

        // Handle keyboard shortcuts
        self.handle_keys(ctx);

        // Tick the live source (acquisition runs regardless of display pause).
        // Skipped while playing back an offline recording so the two sources
        // never feed the display at once.
        if self.data_source != DataSource::Playback {
            match self.mode {
                AcqMode::Demo => self.tick_demo(),
                AcqMode::Device => self.tick_device(),
            }
        }

        // Drain background impedance-test and export results.
        self.poll_impedance();
        self.poll_export();

        // Tick playback only while it is the selected source.
        if self.data_source == DataSource::Playback
            && self.playback_mgr.is_loaded()
            && let Some(block) = self.playback_mgr.tick()
        {
            self.ingest_block(block);
        }

        // Advance snippet ages each frame (drives fade-out animation).
        self.snippet_store.advance_frames();

        // Refresh the FFT spectrum once per frame while an FFT view is open, so
        // the view is self-contained and no longer depends on the sidebar
        // section being expanded/enabled (#4a).
        if self.fft_tile_open() {
            let sr = self
                .latest_block
                .as_ref()
                .map(|b| b.sample_rate)
                .unwrap_or(30000.0);
            self.fft.update_from_ring(&self.disp_ring, sr);
        }

        // Detect filter settings change (user toggled in UI) — re-filter
        // history once the settings have been stable for the debounce window,
        // so dragging a cutoff slider doesn't re-filter 10k blocks per frame.
        if self.filters != self.filter_settings_snapshot {
            if self.filters != self.filters_last_frame || self.filter_change_pending_since.is_none()
            {
                self.filter_change_pending_since = Some(Instant::now());
            }
            let stable = self
                .filter_change_pending_since
                .is_some_and(|t| t.elapsed().as_millis() as u64 >= REFILTER_DEBOUNCE_MS);
            if stable {
                let sr = self
                    .latest_block
                    .as_ref()
                    .map(|b| b.sample_rate)
                    .unwrap_or(30000.0);
                let ch = self
                    .latest_block
                    .as_ref()
                    .map(|b| b.channel_count)
                    .unwrap_or(16);
                self.rebuild_filter_chains(sr, ch);
                self.filter_change_pending_since = None;
            }
        } else {
            self.filter_change_pending_since = None;
        }
        self.filters_last_frame = self.filters;

        let elapsed_live = self.elapsed_seconds();
        // Use frozen elapsed for display when paused
        let elapsed = if self.display_paused {
            self.paused_elapsed
        } else {
            elapsed_live
        };

        // ── Display-mode window management ─────────────────────────
        if !self.display_paused && self.disp_ring.ready {
            let latest_ms = self.disp_ring.latest_time_ms();
            let window_ms = self.display.time_window_ms();
            match self.display.display_mode {
                panels::DisplayMode::Sweep => {
                    // Fixed window, cursor sweeps right.  When the window fills,
                    // snap to the next window boundary (brief flash, once per window).
                    if latest_ms >= self.sweep_start_ms + window_ms {
                        self.sweep_start_ms = (latest_ms / window_ms).floor() * window_ms;
                    }
                }
                panels::DisplayMode::Roll => {
                    // Continuous scrolling: x_right = latest, x_left = latest - window.
                    self.sweep_start_ms = (latest_ms - window_ms).max(0.0);
                }
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

                    // Transport buttons.  In Playback mode the live transport
                    // is replaced by playback controls so only one source is
                    // ever driven from the toolbar.
                    let running = self.is_running();
                    let mut open_playback = false;
                    if self.data_source == DataSource::Playback {
                        let loaded = self.playback_mgr.is_loaded();
                        let playing = self.playback_mgr.state == playback::PlaybackState::Playing;
                        if theme::transport_button_sized(
                            ui,
                            if playing { " Pause " } else { "  Play  " },
                            if playing {
                                theme::TEXT_SECONDARY
                            } else {
                                theme::BTN_PLAY
                            },
                            loaded,
                            "Play / pause the recording (Space)",
                            88.0,
                        ) {
                            self.playback_mgr.toggle_play_pause();
                        }
                        if theme::transport_button_tip(
                            ui,
                            " Restart ",
                            theme::BG_WIDGET,
                            loaded,
                            "Jump back to the start of the recording",
                        ) {
                            self.playback_mgr.seek_to(0);
                        }
                        ui.add_space(8.0);
                        if loaded {
                            let name = self
                                .playback_mgr
                                .file_path
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .unwrap_or("recording")
                                .to_string();
                            ui.label(theme::caption(format!("\u{25B6} {name}")));
                        } else if theme::transport_button_tip(
                            ui,
                            " Open\u{2026} ",
                            theme::BTN_PLAY,
                            true,
                            "Open a .kvraw recording to play back",
                        ) {
                            open_playback = true;
                        }
                    } else {
                        if theme::transport_button_sized(
                            ui,
                            if running { "  Stop  " } else { "  Start  " },
                            if running {
                                theme::BTN_STOP
                            } else {
                                theme::BTN_PLAY
                            },
                            true,
                            "Start / stop acquisition (Space)",
                            88.0,
                        ) {
                            self.toggle_acquisition();
                        }

                        // Record button — always clickable when running.
                        let rec_label = match self.recording.state {
                            RecordingState::Idle => " Record ",
                            RecordingState::Armed => " ARMED ",
                            RecordingState::Recording => " STOP REC ",
                        };
                        let rec_color = match self.recording.state {
                            RecordingState::Idle => theme::BTN_RECORD,
                            RecordingState::Armed => theme::ACCENT_YELLOW,
                            RecordingState::Recording => theme::BTN_RECORD_ACTIVE,
                        };
                        let rec_enabled = running || self.recording.state != RecordingState::Idle;
                        let rec_tip = match self.recording.state {
                            RecordingState::Idle => "Arm recording (R)",
                            RecordingState::Armed => "Begin recording (R)",
                            RecordingState::Recording => "Stop recording (R)",
                        };
                        // Fixed width fits the widest label ("STOP REC") so
                        // arming / recording never shoves the toolbar sideways.
                        if theme::transport_button_sized(
                            ui,
                            rec_label,
                            rec_color,
                            rec_enabled,
                            rec_tip,
                            104.0,
                        ) {
                            self.toggle_recording();
                        }

                        // Pause button — only shown when running or already paused.
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
                            if theme::transport_button_sized(
                                ui,
                                pause_label,
                                pause_color,
                                true,
                                "Freeze / resume the display; acquisition continues (P)",
                                88.0,
                            ) {
                                self.toggle_pause_display();
                            }
                        }
                    }
                    if open_playback {
                        self.open_playback_file_dialog();
                    }

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // Data-source selector (B1): one switch for Demo / Device /
                    // Playback so it is always clear where the signal comes from.
                    ui.label(
                        egui::RichText::new("Source:")
                            .size(theme::FONT_BODY)
                            .color(theme::TEXT_DIM),
                    );
                    let device_tip = match self.device.kind {
                        DeviceKind::Simulator => "Live acquisition \u{2014} Simulator backend",
                        DeviceKind::Rhd => {
                            "Live acquisition \u{2014} RHD hardware (set bitfile in DEVICE panel)"
                        }
                    };
                    // Segmented control: the three sources sit flush inside one
                    // recessed frame so they read as a single "pick one" switch
                    // rather than three independent buttons.
                    let mut pick: Option<DataSource> = None;
                    egui::Frame::new()
                        .fill(theme::BG_DARKEST)
                        .corner_radius(egui::CornerRadius::same(5))
                        .inner_margin(egui::Margin::same(2))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (src, label, tip) in [
                                (
                                    DataSource::Demo,
                                    "Demo",
                                    "Synthetic neural data \u{2014} no hardware",
                                ),
                                (DataSource::Device, "Device", device_tip),
                                (
                                    DataSource::Playback,
                                    "Playback",
                                    "Replay a saved .kvraw recording",
                                ),
                            ] {
                                let selected = self.data_source == src;
                                if ui
                                    .add_sized(
                                        [66.0, 22.0],
                                        egui::SelectableLabel::new(
                                            selected,
                                            egui::RichText::new(label).size(theme::FONT_HEADING),
                                        ),
                                    )
                                    .on_hover_text(tip)
                                    .clicked()
                                    && !selected
                                {
                                    pick = Some(src);
                                }
                            }
                        });
                    if let Some(src) = pick {
                        self.select_source(src);
                    }

                    ui.add_space(8.0);
                    if ui
                        .button(
                            egui::RichText::new(" ? ")
                                .size(theme::FONT_HEADING)
                                .strong(),
                        )
                        .on_hover_text("Keyboard shortcuts")
                        .clicked()
                    {
                        self.show_help = !self.show_help;
                    }

                    // Right-aligned: live status pill + clock + version
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("v0.2.0")
                                .size(9.0)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(10.0);

                        // Acquisition clock — colored by state.
                        let clock_color = if self.recording.state == RecordingState::Recording {
                            theme::ACCENT_RED
                        } else if running {
                            theme::ACCENT_YELLOW
                        } else {
                            theme::TEXT_DIM
                        };
                        ui.label(
                            egui::RichText::new(theme::format_clock(elapsed))
                                .size(15.0)
                                .monospace()
                                .strong()
                                .color(clock_color),
                        );

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);

                        // At-a-glance state pill: REC / ARMED / LIVE / IDLE.
                        let (dot, label, color) = match self.recording.state {
                            RecordingState::Recording => {
                                (theme::STATUS_RECORDING, "REC", theme::ACCENT_RED)
                            }
                            RecordingState::Armed => {
                                (theme::STATUS_ARMED, "ARMED", theme::ACCENT_YELLOW)
                            }
                            RecordingState::Idle if running => {
                                (theme::STATUS_CONNECTED, "LIVE", theme::ACCENT_GREEN)
                            }
                            RecordingState::Idle => {
                                (theme::STATUS_IDLE, "IDLE", theme::TEXT_SECONDARY)
                            }
                        };
                        // In a right-to-left layout, add the label first so the
                        // status dot lands to its left, reading "● LABEL".
                        // Fixed-width label keeps the dot from hopping as the
                        // state text (REC / ARMED / LIVE / IDLE) changes width.
                        ui.add_sized(
                            [44.0, 16.0],
                            egui::Label::new(
                                egui::RichText::new(label).size(12.0).strong().color(color),
                            ),
                        );
                        ui.add_space(5.0);
                        theme::status_dot(ui, dot);
                    });
                });
            });

        // ── Device error banner ─────────────────────────────────
        // Surfaced when the acquisition source fails to open or read.
        // Dismissible; the GUI and any other mode keep running regardless.
        // Borrow the message instead of cloning it every frame; record the
        // dismiss action in a local and apply it after the panel closure ends.
        let mut dismiss_device_error = false;
        if let Some(err) = self.device_error.as_ref() {
            egui::TopBottomPanel::top("device_error_banner")
                .frame(
                    egui::Frame::new()
                        .fill(theme::ACCENT_RED)
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("\u{26A0} Device error: {err}"))
                                .size(12.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Dismiss").clicked() {
                                dismiss_device_error = true;
                            }
                        });
                    });
                });
        }
        if dismiss_device_error {
            self.device_error = None;
        }

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

                // Placeholder copy that matches the active source: in Playback the
                // user loads a file (no Start button), so "Press Start" would be
                // misleading.
                let empty_hint: &str = match self.data_source {
                    // Playback shows an actionable Open button below (#18), so
                    // the subtitle just sets context rather than pointing away.
                    DataSource::Playback => "No recording loaded yet",
                    _ => "Press Start to begin acquisition",
                };

                {
                    let mut behavior = KvTileBehavior {
                        disp_ring: &self.disp_ring,
                        disp_ring_lfp: &self.disp_ring_lfp,
                        disp_ring_ap: &self.disp_ring_ap,
                        latest_block: self.latest_block.as_ref(),
                        display: &mut self.display,
                        filters: &self.filters,
                        display_paused: self.display_paused,
                        paused_elapsed: &mut self.paused_elapsed,
                        sweep_start_ms: self.sweep_start_ms,
                        elapsed_secs,
                        show_perf_overlay: self.show_perf_overlay,
                        render_ms_ema: &mut self.render_ms_ema,
                        block_history_len: self.block_history.len(),
                        snippet_store: &mut self.snippet_store,
                        fft: &self.fft,
                        trigger: &self.trigger,
                        ttl_history: &self.ttl_history,
                        pending_add: &mut pending_add,
                        empty_hint,
                    };
                    tree.ui(&mut behavior, ui);
                }

                // Process any add-view request that came out of the tile UI.
                if let Some(req) = pending_add {
                    let visible = self.display.visible_channels;
                    let kind = match req {
                        AddViewRequest::Lfp => TileKind::new_lfp(visible),
                        AddViewRequest::Ap => TileKind::new_ap(visible),
                        AddViewRequest::SpikeOverlay => TileKind::new_spike_overlay(),
                        AddViewRequest::Fft => TileKind::new_fft(),
                        AddViewRequest::Ttl => TileKind::new_ttl_monitor(),
                    };
                    multiview::add_view_to_tree(&mut tree, kind);
                }

                self.tile_tree = Some(tree);
            });

        // ── Actionable empty-state for Playback (#18) ───────────
        // When no recording is loaded yet, drop an Open button (with an icon)
        // right on the canvas so the user doesn't have to hunt the toolbar.
        if self.data_source == DataSource::Playback && !self.playback_mgr.is_loaded() {
            let mut open_clicked = false;
            egui::Area::new(egui::Id::new("kv_playback_empty_cta"))
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 44.0))
                .interactable(true)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("\u{1F4C1}")
                                .size(34.0)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(4.0);
                        if theme::primary_button(ui, "Open .kvraw\u{2026}", true) {
                            open_clicked = true;
                        }
                    });
                });
            if open_clicked {
                self.open_playback_file_dialog();
            }
        }

        // ── Shortcut help overlay (B4) ──────────────────────────
        self.draw_help_overlay(ctx);

        // ── Toast notifications (B5) ────────────────────────────
        self.toasts.show(ctx);

        // Request continuous repaints while running (or paused — for overlay)
        // and while background impedance/export work needs progress updates.
        if self.is_running()
            || self.display_paused
            || self.impedance_rx.is_some()
            || self.export_rx.is_some()
            || self.filter_change_pending_since.is_some()
        {
            ctx.request_repaint();
        }
    }
}

/// Read an entire .kvraw file and export it in the requested format.
/// Returns the output path on success.
fn export_kvraw(
    source: &std::path::Path,
    format: kv_recorder::export_formats::ExportFormat,
) -> Result<std::path::PathBuf, String> {
    use std::cell::RefCell;
    use std::rc::Rc;

    use kv_recorder::KvrawReader;
    use kv_recorder::export_formats::{self, ExportFormat, ExportHeader};

    // Native format needs no conversion — just copy the .kvraw alongside.
    if format.is_native() {
        let output = source.with_extension("copy.kvraw");
        std::fs::copy(source, &output).map_err(|e| e.to_string())?;
        return Ok(output);
    }

    let mut reader = KvrawReader::open(source).map_err(|e| e.to_string())?;
    let meta = reader.metadata().clone();
    if meta.channel_count == 0 {
        return Err("kvraw file has no channels".to_string());
    }
    let total_frames = reader.total_frames();
    if total_frames == 0 {
        return Err("no data to export".to_string());
    }

    let header = ExportHeader {
        sample_rate: meta.sample_rate,
        channel_count: meta.channel_count,
    };
    let notes = format!("exported from {}", source.display());

    // Stream blocks straight from disk into the exporter. Reading happens lazily
    // inside the iterator so the whole recording is never held in memory; a read
    // failure is captured and surfaced after the exporter returns.
    const FRAMES_PER_CHUNK: usize = 30_000;
    let read_err: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let read_err_sink = Rc::clone(&read_err);
    let channel_count = meta.channel_count;
    let device_id = meta.device_id.clone();
    let sample_rate = meta.sample_rate;
    let mut frame: u64 = 0;
    let mut packet_id: u64 = 0;
    let blocks = std::iter::from_fn(move || {
        if frame >= total_frames {
            return None;
        }
        let want = FRAMES_PER_CHUNK.min((total_frames - frame) as usize);
        match reader.read_frames(frame, want) {
            Ok(data) => {
                if data.is_empty() {
                    return None;
                }
                let frames_read = data.len() / channel_count;
                let block = SampleBlock {
                    device_id: device_id.clone(),
                    stream_id: 0,
                    packet_id,
                    timestamp_start: frame,
                    sample_rate,
                    channel_count,
                    samples_per_channel: frames_read,
                    ttl_bits: 0,
                    data,
                    aux_data: None,
                    board_adc_data: None,
                    ttl_in_per_sample: None,
                    ttl_out_per_sample: None,
                };
                packet_id += 1;
                frame += frames_read as u64;
                Some(block)
            }
            Err(e) => {
                *read_err_sink.borrow_mut() = Some(e.to_string());
                None
            }
        }
    });

    let result = match format {
        // Native is short-circuited above before any frames are read.
        ExportFormat::KeyvastNative => unreachable!("native format handled before frame read"),
        ExportFormat::IntanRhd => {
            let output = source.with_extension(format.extension());
            export_formats::export_intan_rhd_streaming(&output, header, blocks, &notes)
        }
        ExportFormat::FlatBinary => {
            // Flat binary writes recording.bin + recording.meta.json into a directory.
            let output_dir = source.with_extension("export");
            export_formats::export_flat_binary_streaming(&output_dir, header, blocks, &notes)
        }
    };

    if let Some(e) = read_err.borrow_mut().take() {
        return Err(e);
    }
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::FilterSettings;
    use kv_types::SampleBlock;

    fn block_interleaved(
        channel_count: usize,
        samples_per_channel: usize,
        data: Vec<i16>,
    ) -> SampleBlock {
        assert_eq!(data.len(), channel_count * samples_per_channel);
        SampleBlock {
            device_id: "test".to_string(),
            stream_id: 0,
            packet_id: 0,
            timestamp_start: 0,
            sample_rate: 30_000.0,
            channel_count,
            samples_per_channel,
            ttl_bits: 0,
            data,
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        }
    }

    #[test]
    fn build_filter_chains_sets_one_passthrough_chain_per_channel_when_disabled() {
        let filters = FilterSettings::default();
        let chains = KvApp::build_filter_chains(&filters, 30_000.0, 4);
        assert_eq!(chains.len(), 4);
        for chain in &chains {
            assert!(!chain.hp_enabled);
            assert!(!chain.lp_enabled);
            assert!(!chain.notch_enabled);
        }
    }

    #[test]
    fn build_filter_chains_enables_requested_stages() {
        let filters = FilterSettings {
            hp_enabled: true,
            hp_cutoff_hz: 300.0,
            lp_enabled: true,
            lp_cutoff_hz: 5_000.0,
            notch_enabled: true,
            ..FilterSettings::default()
        };
        let chains = KvApp::build_filter_chains(&filters, 30_000.0, 2);
        assert_eq!(chains.len(), 2);
        for chain in &chains {
            assert!(chain.hp_enabled);
            assert!(chain.lp_enabled);
            assert!(chain.notch_enabled);
        }
    }

    #[test]
    fn filter_block_with_chains_is_identity_when_car_off_and_chains_passthrough() {
        // 2 channels, 2 samples, interleaved by sample: [s0c0, s0c1, s1c0, s1c1].
        let block = block_interleaved(2, 2, vec![100, -200, 300, -400]);
        let mut chains = vec![FilterChain::passthrough(); 2];
        let out = KvApp::filter_block_with_chains(&block, &mut chains, false);
        assert_eq!(out.data, block.data);
    }

    #[test]
    fn filter_block_with_chains_subtracts_common_average_when_car_on() {
        // Per time step the mean across channels is removed.
        // s0 = [10, 20] -> mean 15 -> [-5, 5]; s1 = [30, 50] -> mean 40 -> [-10, 10].
        let block = block_interleaved(2, 2, vec![10, 20, 30, 50]);
        let mut chains = vec![FilterChain::passthrough(); 2];
        let out = KvApp::filter_block_with_chains(&block, &mut chains, true);
        assert_eq!(out.data, vec![-5, 5, -10, 10]);
        // Metadata is preserved; only the samples change.
        assert_eq!(out.channel_count, block.channel_count);
        assert_eq!(out.samples_per_channel, block.samples_per_channel);
    }
}

// Overlay helpers are now handled inside multiview::KvTileBehavior::draw_main_waveform().
