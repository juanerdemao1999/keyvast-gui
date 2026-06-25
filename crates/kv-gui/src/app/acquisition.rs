use super::*;

impl KvApp {
    /// Switch to Demo mode and start generating.
    pub(crate) fn start_demo(&mut self) {
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
    pub(crate) fn start_device(&mut self) {
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
                self.recorder_dropped_blocks = 0;
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
    pub(crate) fn build_pipeline_source(&self) -> Result<PipelineSource, String> {
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

    pub(crate) fn stop_all(&mut self) {
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
    pub(crate) fn build_lfp_chains(channel_count: usize, sample_rate: f64) -> Vec<FilterChain> {
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
    pub(crate) fn build_ap_chains(channel_count: usize, sample_rate: f64) -> Vec<FilterChain> {
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
    pub(crate) fn build_filter_chains(
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
    pub(crate) fn rebuild_filter_chains(&mut self, sample_rate: f64, channel_count: usize) {
        self.filter_chains = Self::build_filter_chains(&self.filters, sample_rate, channel_count);
        self.filter_settings_snapshot = self.filters;
        // Re-filter existing history with new chains
        self.refilter_history();
    }

    /// Re-filter the entire block_history (called when filter settings change).
    pub(crate) fn refilter_history(&mut self) {
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
    pub(crate) fn filter_block_with_chains(
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
    pub(crate) fn ingest_block(&mut self, block: SampleBlock) {
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
    pub(crate) fn lfp_tile_open(&self) -> bool {
        self.tile_has_pane(|kind| matches!(kind, TileKind::LfpView { .. }))
    }

    /// True when an FFT spectrum tile exists in the layout.
    pub(crate) fn fft_tile_open(&self) -> bool {
        self.tile_has_pane(|kind| matches!(kind, TileKind::FftSpectrum))
    }

    /// True when the AP band must be computed: an AP tile or a spike-overlay
    /// tile (which consumes the snippet detector) exists in the layout.
    pub(crate) fn ap_band_needed(&self) -> bool {
        self.tile_has_pane(|kind| {
            matches!(
                kind,
                TileKind::ApView { .. } | TileKind::SpikeOverlay { .. }
            )
        })
    }

    pub(crate) fn tile_has_pane(&self, pred: impl Fn(&TileKind) -> bool) -> bool {
        self.tile_tree.as_ref().is_some_and(|tree| {
            tree.tiles.tiles().any(|tile| match tile {
                egui_tiles::Tile::Pane(kind) => pred(kind),
                egui_tiles::Tile::Container(_) => false,
            })
        })
    }

    pub(crate) fn is_running(&self) -> bool {
        match self.mode {
            AcqMode::Demo => self.demo_started,
            AcqMode::Device => self.live_pipeline.is_some(),
        }
    }

    pub(crate) fn toggle_acquisition(&mut self) {
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
    pub(crate) fn select_source(&mut self, src: DataSource) {
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
    pub(crate) fn open_playback_file_dialog(&mut self) {
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
    pub(crate) fn toggle_pause_display(&mut self) {
        if self.display_paused {
            self.display_paused = false;
        } else {
            self.paused_elapsed = self.elapsed_seconds();
            self.display_paused = true;
        }
    }

    /// Launch a background impedance measurement on the RHD hardware.
    /// The test drives the SPI bus itself, so any running acquisition is
    /// stopped first and the device is reopened by the worker thread.
    pub(crate) fn start_impedance_test(&mut self) {
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
    pub(crate) fn poll_impedance(&mut self) {
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

    /// Handle a remote API command and return a JSON result or error.
    pub(crate) fn handle_remote_command(&mut self, cmd: &RemoteCommand) -> Result<String, String> {
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
    pub(crate) fn elapsed_seconds(&self) -> f64 {
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
    pub(crate) fn tick_demo(&mut self) {
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
    pub(crate) fn tick_device(&mut self) {
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
                RecorderEvent::BufferOverflow {
                    dropped_blocks,
                    occupancy,
                } => {
                    self.recorder_dropped_blocks = dropped_blocks;
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
                responses.push(RemoteResponse::new(id, result));
            }
            if let Some(ref handle) = self.remote_api_handle {
                let mut resp_q = remote_api::lock_recover(&handle.responses);
                for r in responses {
                    remote_api::push_response_capped(&mut resp_q, r);
                }
            }
        }

        // Sweep orphaned responses whose client already timed out, so the
        // response queue cannot grow without bound if clients disconnect.
        if let Some(ref handle) = self.remote_api_handle {
            let mut resp_q = remote_api::lock_recover(&handle.responses);
            remote_api::sweep_stale_responses(&mut resp_q, Instant::now());
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
}
