use super::*;

impl KvApp {
    pub(crate) fn toggle_recording(&mut self) {
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
    pub(crate) fn begin_recording(&mut self) {
        let free = crate::diskspace::free_bytes(&self.recording.output_dir);
        if let crate::diskspace::StartDecision::Block { free_bytes } =
            crate::diskspace::evaluate_start(free, None)
        {
            let msg = format!(
                "Not enough free disk space to start recording: {} free (need at least {})",
                theme::format_bytes(free_bytes),
                theme::format_bytes(crate::diskspace::RECORDING_MIN_START_FREE_BYTES),
            );
            log::warn!("{msg}");
            self.toasts.error(msg.clone());
            self.recording_error = Some(msg);
            if self.recording.state == RecordingState::Armed {
                self.recording.state = RecordingState::Idle;
            }
            return;
        }
        self.last_disk_check = None;
        self.last_disk_warn = None;

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

    /// Periodically sample free disk while recording and cleanly stop before
    /// the volume fills, so files finalize instead of truncating.
    pub(crate) fn poll_recording_disk_space(&mut self) {
        if self.recording.state != RecordingState::Recording {
            self.last_disk_check = None;
            return;
        }

        let now = Instant::now();
        if let Some(last) = self.last_disk_check
            && now.duration_since(last) < DISK_CHECK_INTERVAL
        {
            return;
        }
        self.last_disk_check = Some(now);

        let free = crate::diskspace::free_bytes(&self.recording.output_dir);
        match crate::diskspace::evaluate_recording(free) {
            crate::diskspace::RecordingDiskStatus::Ok => {}
            crate::diskspace::RecordingDiskStatus::Low { free_bytes } => {
                let warn_due = self
                    .last_disk_warn
                    .map(|t| now.duration_since(t) >= DISK_WARN_INTERVAL)
                    .unwrap_or(true);
                if warn_due {
                    self.last_disk_warn = Some(now);
                    self.toasts.warning(format!(
                        "Low disk space: {} free; recording will auto-stop near {}",
                        theme::format_bytes(free_bytes),
                        theme::format_bytes(crate::diskspace::RECORDING_STOP_FREE_BYTES),
                    ));
                }
            }
            crate::diskspace::RecordingDiskStatus::Critical { free_bytes } => {
                let msg = format!(
                    "Disk nearly full ({} free); recording stopped and finalized to avoid a truncated file",
                    theme::format_bytes(free_bytes),
                );
                log::warn!("{msg}");
                self.toasts.error(msg.clone());
                self.recording_error = Some(msg);
                self.stop_recording();
            }
        }
    }

    /// Stop recording (used by the toggle, triggers, and the remote API).
    pub(crate) fn stop_recording(&mut self) {
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
}
