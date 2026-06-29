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

mod acquisition;
mod export;
mod recording;
mod render;

/// How long filter settings must stay unchanged before the full history
/// is re-filtered (lets slider drags settle first).
const REFILTER_DEBOUNCE_MS: u64 = 150;

/// How often the recording disk-space guard samples free space (DA18).
const DISK_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Minimum spacing between low-disk-space warning toasts.
const DISK_WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// Channels whose absolute sample is at or above this (~0.998 of full scale)
/// are treated as rail-pinned/saturated and excluded from CAR.
const CAR_RAIL_EXCLUDE_I16: u16 = 32_700;

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
    /// Cumulative blocks dropped by fanout-buffer overflow this session.
    recorder_dropped_blocks: u64,
    /// Latest error from the recorder thread (None = no error / dismissed).
    recording_error: Option<String>,
    /// Last time the recording disk-space guard sampled free space.
    last_disk_check: Option<Instant>,
    /// Last time a low-disk-space warning toast was shown.
    last_disk_warn: Option<Instant>,
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
            recorder_dropped_blocks: 0,
            recording_error: None,
            last_disk_check: None,
            last_disk_warn: None,
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

        // Warn as recording storage gets low and auto-stop before it fills.
        self.poll_recording_disk_space();

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

        self.render_toolbar(ctx, elapsed);
        self.render_status_bar(ctx, elapsed);
        self.render_sidebar(ctx);
        self.render_central(ctx);

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

    #[test]
    fn car_reference_mean_excludes_railed_channels() {
        let mean = KvApp::car_reference_mean(&[90, 100, 110, i16::MAX]);
        assert!((mean - 100.0).abs() < 1e-9, "got {mean}");

        let all_railed = KvApp::car_reference_mean(&[i16::MAX, i16::MAX]);
        assert!((all_railed - i16::MAX as f64).abs() < 1e-9);
    }

    #[test]
    fn car_does_not_inject_railed_channel_artifact() {
        let block = block_interleaved(4, 1, vec![100, 100, 100, i16::MAX]);
        let mut chains = vec![FilterChain::passthrough(); 4];
        let out = KvApp::filter_block_with_chains(&block, &mut chains, true);
        assert_eq!(&out.data[0..3], &[0, 0, 0]);
    }
}

// Overlay helpers are now handled inside multiview::KvTileBehavior::draw_main_waveform().
