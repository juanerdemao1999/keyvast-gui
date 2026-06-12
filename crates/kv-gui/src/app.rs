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
use kv_rhd::RhdHardwareOptions;
use kv_simulator::SimulatorConfig;
use kv_types::SampleBlock;

use kv_recorder::StreamingRecorder;

use crate::demo::DemoPreview;
use crate::disp_ring::DisplayRing;
use crate::dsp::{Biquad, FilterChain, Q_BUTTERWORTH, Q_NOTCH};
use crate::channel_map::{self, ChannelMapState};
use crate::channel_select::{self, ChannelSelectState};
use crate::config_persist::{self, ConfigPersistState, PersistentConfig};
use crate::fft_panel::{self, FftState};
use crate::impedance_panel::{self, ImpedanceMsg, ImpedanceState};
use crate::live_pipeline::{self, LivePipelineHandle, PipelineSource, RecorderCmd, RecorderEvent};
use crate::probe_map::{self, ProbeMapState};
use crate::remote_api::{self, RemoteApiState, RemoteApiHandle, RemoteCommand, RemoteResponse, AppStatus};
use crate::trigger::{self, TriggerConfig, TriggerAction};
use crate::multiview::{self, AddViewRequest, KvTileBehavior, TileKind};
use crate::spike_overlay::SpikeSnippetStore;
use crate::panels::{
    self, DeviceKind, DeviceSettings, DisplaySettings, FilterSettings, RecordingSettings,
    RecordingState,
};
use crate::playback::{self, PlaybackManager};
use crate::preview::{BlockStats, compute_block_stats};
use crate::theme;


/// How long filter settings must stay unchanged before the full history
/// is re-filtered (lets slider drags settle first).
const REFILTER_DEBOUNCE_MS: u64 = 150;

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
    probe_map: ProbeMapState,
    channel_select: ChannelSelectState,
    config_persist: ConfigPersistState,
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
            impedance: ImpedanceState::default(),
            impedance_rx: None,
            playback_mgr: PlaybackManager::default(),
            fft: FftState::default(),
            channel_map: ChannelMapState::default(),
            trigger: TriggerConfig::default(),
            remote_api_state: RemoteApiState::default(),
            remote_api_handle: None,
            export_format: kv_recorder::export_formats::ExportFormat::IntanRhd,
            export_rx: None,
            export_status: None,
            record_channels: None,
            probe_map: ProbeMapState::default(),
            channel_select: ChannelSelectState::default(),
            config_persist: ConfigPersistState::default(),
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
        self.snippet_store.reconfigure(self.demo.channel_count, self.demo.sample_rate);
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
        self.demo_started = false;
        self.block_history.clear();
        self.filtered_history.clear();
        self.filter_chains.clear();
        self.filter_chains_lfp.clear();
        self.filter_chains_ap.clear();
        self.disp_ring.reset();
        self.disp_ring_lfp.reset();
        self.disp_ring_ap.reset();
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
                                self.recording_error =
                                    Some(format!("Auto-stop finish error: {e}"));
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
        let sample_rate = self.latest_block.as_ref().map(|b| b.sample_rate).unwrap_or(30000.0);
        let channel_count = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(16);
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
            Some(Self::filter_block_with_chains(&block, &mut self.filter_chains, self.filters.car_enabled))
        } else {
            None
        };

        // Feed main display ring from the display-ready (filtered or raw) block.
        self.disp_ring.push_block(filtered.as_ref().unwrap_or(&block));

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

        // Phase 3: Process trigger/gate logic
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
        if self.mode == AcqMode::Demo && self.recording.state == RecordingState::Recording
            && let Some(ref mut rec) = self.active_recorder {
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

    /// True when the AP band must be computed: an AP tile or a spike-overlay
    /// tile (which consumes the snippet detector) exists in the layout.
    fn ap_band_needed(&self) -> bool {
        self.tile_has_pane(|kind| {
            matches!(kind, TileKind::ApView { .. } | TileKind::SpikeOverlay { .. })
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
                }
            }
            AcqMode::Demo => {
                match StreamingRecorder::new(&self.recording.output_dir) {
                    Ok(rec) => {
                        self.active_recorder = Some(rec);
                        self.record_channels = channels;
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
                        }
                        Err(e) => {
                            self.recording_error = Some(format!("Finish error: {e}"));
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
                    self.impedance.error =
                        Some("impedance thread exited unexpectedly".to_string());
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
                self.export_status = Some(format!("Exported → {}", path.display()));
                self.export_rx = None;
            }
            Ok(Err(e)) => {
                self.export_status = Some(format!("Export failed: {e}"));
                self.export_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
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
                    channel_count: self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(0),
                    sample_rate: self.latest_block.as_ref().map(|b| b.sample_rate).unwrap_or(0.0),
                    display_mode: match self.display.display_mode {
                        crate::panels::DisplayMode::Sweep => "sweep".to_string(),
                        crate::panels::DisplayMode::Roll => "roll".to_string(),
                    },
                    recorded_blocks: self.recording.recorded_blocks,
                };
                Ok(remote_api::format_status_json(&status))
            }
            RemoteCommand::GetChannelCount => {
                let ch = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(0);
                Ok(ch.to_string())
            }
            RemoteCommand::StartAcquisition { mode } => {
                if self.is_running() {
                    return Err("already running".to_string());
                }
                match mode.as_str() {
                    "demo" => { self.start_demo(); Ok("true".to_string()) }
                    "device" => { self.start_device(); Ok("true".to_string()) }
                    _ => Err(format!("unknown mode: {mode}"))
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
            RemoteCommand::SetDisplayMode { mode } => {
                match mode.as_str() {
                    "sweep" => {
                        self.display.display_mode = crate::panels::DisplayMode::Sweep;
                        Ok("true".to_string())
                    }
                    "roll" => {
                        self.display.display_mode = crate::panels::DisplayMode::Roll;
                        Ok("true".to_string())
                    }
                    _ => Err(format!("unknown display mode: {mode}"))
                }
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
                // Detect dropped blocks via packet-ID discontinuity
                if let Some(expected) = pipeline.expected_next_packet_id
                    && block.packet_id > expected {
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
                }
                RecorderEvent::Progress { blocks, bytes } => {
                    self.recording.recorded_blocks = blocks;
                    self.recording.recorded_bytes = bytes;
                }
                RecorderEvent::Error(e) => {
                    self.recording_error = Some(e);
                }
                RecorderEvent::BufferStatus { occupancy } => {
                    self.recorder_buffer_occupancy = occupancy;
                }
                RecorderEvent::SourceError(e) => {
                    // The producer (device) thread has stopped. Surface the
                    // error and tear down the pipeline so the UI shows
                    // "Disconnected" and recording cannot continue silently.
                    self.device_error = Some(e);
                    self.live_pipeline = None;
                    if self.recording.state == RecordingState::Recording {
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

        // Drain background impedance-test and export results.
        self.poll_impedance();
        self.poll_export();

        // Tick playback if active.
        if self.playback_mgr.is_loaded()
            && let Some(block) = self.playback_mgr.tick() {
                self.ingest_block(block);
            }

        // Advance snippet ages each frame (drives fade-out animation).
        self.snippet_store.advance_frames();

        // Detect filter settings change (user toggled in UI) — re-filter
        // history once the settings have been stable for the debounce window,
        // so dragging a cutoff slider doesn't re-filter 10k blocks per frame.
        if self.filters != self.filter_settings_snapshot {
            if self.filters != self.filters_last_frame
                || self.filter_change_pending_since.is_none()
            {
                self.filter_change_pending_since = Some(Instant::now());
            }
            let stable = self
                .filter_change_pending_since
                .is_some_and(|t| t.elapsed().as_millis() as u64 >= REFILTER_DEBOUNCE_MS);
            if stable {
                let sr = self.latest_block.as_ref().map(|b| b.sample_rate).unwrap_or(30000.0);
                let ch = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(16);
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
                        self.sweep_start_ms =
                            (latest_ms / window_ms).floor() * window_ms;
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
                        .on_hover_text(match self.device.kind {
                            DeviceKind::Simulator => "Live acquisition \u{2014} Simulator backend",
                            DeviceKind::Rhd => {
                                "Live acquisition \u{2014} RHD hardware (set bitfile in DEVICE panel)"
                            }
                        })
                        .clicked()
                        && demo_selected
                    {
                        self.start_device();
                    }

                    // Right-aligned: live status pill + clock + version
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new("v0.2.0")
                                    .size(9.0)
                                    .color(theme::TEXT_DIM),
                            );
                            ui.add_space(10.0);

                            // Acquisition clock — colored by state.
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
                            ui.label(
                                egui::RichText::new(label).size(12.0).strong().color(color),
                            );
                            ui.add_space(5.0);
                            theme::status_dot(ui, dot);
                        },
                    );
                });
            });

        // ── Device error banner ─────────────────────────────────
        // Surfaced when the acquisition source fails to open or read.
        // Dismissible; the GUI and any other mode keep running regardless.
        if let Some(err) = self.device_error.clone() {
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
                                self.device_error = None;
                            }
                        });
                    });
                });
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

                // Compute elapsed recording seconds for the clock display.
                let rec_elapsed_secs = self
                    .recording_start_time
                    .map(|t| t.elapsed().as_secs_f64());

                let mut dismiss_error = false;
                panels::draw_left_panel(
                    ui,
                    self.is_running(),
                    &mut self.device,
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

                // Impedance panel
                ui.add_space(4.0);
                let mut start_impedance = false;
                let can_measure =
                    self.device.kind == DeviceKind::Rhd && self.device.rhd_bitfile.is_some();
                impedance_panel::draw_impedance_section(
                    ui,
                    &mut self.impedance,
                    can_measure,
                    &mut start_impedance,
                );
                if start_impedance {
                    self.start_impedance_test();
                }

                // Playback panel
                ui.add_space(4.0);
                let mut open_playback_file = false;
                playback::draw_playback_section(
                    ui,
                    &mut self.playback_mgr,
                    &mut open_playback_file,
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
                if toggle_rec {
                    self.toggle_recording();
                }

                // Handle playback file open (outside borrow scope)
                if open_playback_file
                    && let Some(path) = playback::pick_kvraw_file() {
                        self.playback_mgr.load_file(path);
                    }

                // FFT spectrum panel
                ui.add_space(4.0);
                let total_ch = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(16);
                let sr = self.latest_block.as_ref().map(|b| b.sample_rate).unwrap_or(30000.0);
                fft_panel::draw_fft_section(
                    ui,
                    &mut self.fft,
                    &self.disp_ring,
                    sr,
                    total_ch,
                );

                // Channel mapping panel
                ui.add_space(4.0);
                channel_map::draw_channel_map_section(
                    ui,
                    &mut self.channel_map,
                    &mut self.display,
                    total_ch,
                );

                // Phase 3: Trigger/Gate
                ui.add_space(4.0);
                trigger::draw_trigger_section(ui, &mut self.trigger);

                // Phase 3: Remote API
                ui.add_space(4.0);
                let prev_enabled = self.remote_api_state.enabled;
                remote_api::draw_remote_api_section(ui, &mut self.remote_api_state);
                // Start/stop server based on enabled toggle
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

                // Phase 3: Export format selector (below recording)
                ui.add_space(4.0);
                egui::CollapsingHeader::new(
                    egui::RichText::new("EXPORT FORMAT")
                        .size(11.0)
                        .strong()
                        .color(theme::TEXT_SECONDARY),
                )
                .default_open(false)
                .show(ui, |ui| {
                    use kv_recorder::export_formats::ExportFormat;
                    ui.horizontal(|ui| {
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
                    if ui
                        .add_enabled(
                            !exporting,
                            egui::Button::new(
                                egui::RichText::new("Export .kvraw…").size(10.0),
                            ),
                        )
                        .on_hover_text("Convert a .kvraw recording to the selected format")
                        .clicked()
                        && let Some(path) = playback::pick_kvraw_file() {
                            self.start_export(path);
                        }
                    if exporting {
                        ui.label(
                            egui::RichText::new("Exporting…")
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
                });

                // Phase 4: Probe Map config
                ui.add_space(4.0);
                probe_map::draw_probe_map_section(ui, &mut self.probe_map, total_ch);

                // Phase 4: Selective channel save
                ui.add_space(4.0);
                self.channel_select.sync_channel_count(total_ch);
                channel_select::draw_channel_select_section(
                    ui,
                    &mut self.channel_select,
                );

                // Phase 4: Config persistence
                ui.add_space(4.0);
                let mut save_clicked = false;
                let mut load_clicked = false;
                config_persist::draw_config_section(
                    ui,
                    &mut self.config_persist,
                    &mut save_clicked,
                    &mut load_clicked,
                );
                if save_clicked {
                    let cfg = PersistentConfig::capture_from(
                        &self.display,
                        &self.filters,
                        &self.recording.output_dir,
                        &self.recording.file_prefix,
                        self.remote_api_state.port,
                        self.probe_map.geometry.label(),
                        self.probe_map.site_radius,
                    );
                    match config_persist::save_config(&self.config_persist.config_path, &cfg) {
                        Ok(()) => {
                            self.config_persist.status_message = Some("Saved".to_string());
                        }
                        Err(e) => {
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
                            self.config_persist.status_message = Some("Loaded".to_string());
                            self.config_persist.loaded = true;
                        }
                        Err(e) => {
                            self.config_persist.status_message = Some(e);
                        }
                    }
                }
            });

        // ── Probe Map window (floating) ─────────────────────────
        if self.probe_map.visible {
            let map_ch = self.latest_block.as_ref().map(|b| b.channel_count).unwrap_or(16);
            self.probe_map.update_activity(&self.disp_ring, map_ch);
            probe_map::draw_probe_map_window(ctx, &self.probe_map, map_ch);
        }

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
                        snippet_store: &mut self.snippet_store,
                        fft:           &self.fft,
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
                        AddViewRequest::Fft => TileKind::new_fft(),
                    };
                    multiview::add_view_to_tree(&mut tree, kind);
                }

                self.tile_tree = Some(tree);
            });

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
    use kv_recorder::KvrawReader;
    use kv_recorder::export_formats::{self, ExportFormat};

    let mut reader = KvrawReader::open(source).map_err(|e| e.to_string())?;
    let meta = reader.metadata().clone();
    if meta.channel_count == 0 {
        return Err("kvraw file has no channels".to_string());
    }
    let total_frames = reader.total_frames();

    // Read in ~1 s chunks; the exporters re-chunk internally as needed.
    const FRAMES_PER_CHUNK: usize = 30_000;
    let mut blocks: Vec<SampleBlock> = Vec::new();
    let mut frame: u64 = 0;
    let mut packet_id: u64 = 0;
    while frame < total_frames {
        let want = FRAMES_PER_CHUNK.min((total_frames - frame) as usize);
        let data = reader.read_frames(frame, want).map_err(|e| e.to_string())?;
        if data.is_empty() {
            break;
        }
        let frames_read = data.len() / meta.channel_count;
        blocks.push(SampleBlock {
            device_id: meta.device_id.clone(),
            stream_id: 0,
            packet_id,
            timestamp_start: frame,
            sample_rate: meta.sample_rate,
            channel_count: meta.channel_count,
            samples_per_channel: frames_read,
            ttl_bits: 0,
            data,
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        });
        packet_id += 1;
        frame += frames_read as u64;
    }
    if blocks.is_empty() {
        return Err("no data to export".to_string());
    }

    let notes = format!("exported from {}", source.display());
    match format {
        ExportFormat::IntanRhd => {
            let output = source.with_extension(format.extension());
            export_formats::export_intan_rhd(&output, &blocks, &notes)
        }
        ExportFormat::FlatBinary => {
            // Flat binary writes recording.bin + recording.meta.json into a directory.
            let output_dir = source.with_extension("export");
            export_formats::export_flat_binary(&output_dir, &blocks, &notes)
        }
    }
    .map_err(|e| e.to_string())
}

// Overlay helpers are now handled inside multiview::KvTileBehavior::draw_main_waveform().
