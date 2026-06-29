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
