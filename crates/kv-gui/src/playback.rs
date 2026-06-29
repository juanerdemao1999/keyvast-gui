//! Offline .kvraw file playback for the Keyvast GUI.
//!
//! Provides file loading, waveform display, play/pause/seek/timeline
//! scrubbing for recorded data files.

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use kv_recorder::{KvrawMetadata, KvrawReader};
use kv_types::SampleBlock;

use crate::theme;

/// Maximum number of frames to render per display update.
const MAX_DISPLAY_FRAMES: usize = 30_000;

/// Playback state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// No file loaded.
    Idle,
    /// File loaded, paused.
    Paused,
    /// File loaded, playing.
    Playing,
}

/// State for the offline playback mode.
pub struct PlaybackManager {
    pub state: PlaybackState,
    pub file_path: Option<PathBuf>,
    reader: Option<KvrawReader>,
    metadata: Option<KvrawMetadata>,
    /// Current playback position in frames (the playhead).
    pub cursor_frame: u64,
    /// High-water mark of frames already streamed out by `tick`. Lags behind
    /// `cursor_frame` when the playhead jumps forward by more than one display
    /// window (high speed / long stall); successive ticks advance it
    /// contiguously so no inter-block samples are ever skipped (DA44).
    read_cursor: u64,
    /// Playback speed multiplier (1.0 = real-time).
    pub speed: f64,
    /// Wall-clock time of last play tick.
    last_tick: Instant,
    /// Cursor position of the last block emitted by `tick` — lets paused
    /// frames skip re-reading/re-ingesting the same data while still
    /// refreshing immediately after a seek/scrub.
    last_emitted_frame: Option<u64>,
    /// Error message from file loading.
    pub error: Option<String>,
}

impl Default for PlaybackManager {
    fn default() -> Self {
        Self {
            state: PlaybackState::Idle,
            file_path: None,
            reader: None,
            metadata: None,
            cursor_frame: 0,
            read_cursor: 0,
            speed: 1.0,
            last_tick: Instant::now(),
            last_emitted_frame: None,
            error: None,
        }
    }
}

impl std::fmt::Debug for PlaybackManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackManager")
            .field("state", &self.state)
            .field("file_path", &self.file_path)
            .field("cursor_frame", &self.cursor_frame)
            .field("speed", &self.speed)
            .finish()
    }
}

impl PlaybackManager {
    /// Load a .kvraw file.
    pub fn load_file(&mut self, path: PathBuf) {
        match KvrawReader::open(&path) {
            Ok(reader) => {
                let meta = reader.metadata().clone();
                if !meta.sample_rate.is_finite() || meta.sample_rate <= 0.0 {
                    self.error = Some(format!(
                        "Invalid sample_rate ({}) in file metadata",
                        meta.sample_rate
                    ));
                    self.state = PlaybackState::Idle;
                    return;
                }
                self.metadata = Some(meta);
                self.reader = Some(reader);
                self.file_path = Some(path);
                self.cursor_frame = 0;
                self.read_cursor = 0;
                self.last_emitted_frame = None;
                self.state = PlaybackState::Paused;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("{e}"));
                self.state = PlaybackState::Idle;
            }
        }
    }

    /// Close the current file.
    pub fn close(&mut self) {
        self.reader = None;
        self.metadata = None;
        self.file_path = None;
        self.cursor_frame = 0;
        self.read_cursor = 0;
        self.last_emitted_frame = None;
        self.state = PlaybackState::Idle;
        self.error = None;
    }

    pub fn play(&mut self) {
        if self.reader.is_some() {
            self.state = PlaybackState::Playing;
            self.last_tick = Instant::now();
        }
    }

    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
        }
    }

    pub fn toggle_play_pause(&mut self) {
        match self.state {
            PlaybackState::Playing => self.pause(),
            PlaybackState::Paused => self.play(),
            PlaybackState::Idle => {}
        }
    }

    pub fn seek_to(&mut self, frame: u64) {
        if let Some(ref meta) = self.metadata {
            self.cursor_frame = frame.min(meta.total_frames());
            // A seek is a jump, not continuous playback: collapse the read
            // high-water mark onto the new playhead so the next tick shows a
            // window at the target instead of streaming through everything the
            // user scrubbed past.
            self.read_cursor = self.cursor_frame;
            self.last_emitted_frame = None;
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.reader.is_some()
    }

    pub fn metadata(&self) -> Option<&KvrawMetadata> {
        self.metadata.as_ref()
    }

    pub fn total_frames(&self) -> u64 {
        self.metadata.as_ref().map_or(0, |m| m.total_frames())
    }

    /// Advance the playback cursor and return a SampleBlock for display.
    /// Returns None if there's no more data or not playing.
    pub fn tick(&mut self) -> Option<SampleBlock> {
        let meta = self.metadata.as_ref()?;
        self.reader.as_ref()?;
        let sample_rate = meta.sample_rate;
        let total_frames = meta.total_frames();
        let samples_per_channel = meta.samples_per_channel;

        if self.state == PlaybackState::Playing {
            let now = Instant::now();
            let dt = now.duration_since(self.last_tick).as_secs_f64();
            self.last_tick = now;

            let frames_to_advance = (dt * sample_rate * self.speed).round() as u64;
            self.cursor_frame = self
                .cursor_frame
                .saturating_add(frames_to_advance)
                .min(total_frames);

            // Auto-pause at end of file.
            if self.cursor_frame >= total_frames {
                self.state = PlaybackState::Paused;
            }
        }

        let block_frames = if samples_per_channel > 0 {
            samples_per_channel as u64
        } else {
            256
        };

        // DA44: stream every frame between the previously-read position and the
        // playhead instead of only the fixed block ending at the cursor. When
        // the playhead jumped forward by more than one display window (high
        // speed, or a long UI stall), read the gap in `MAX_DISPLAY_FRAMES`
        // chunks across successive ticks so no inter-block samples are skipped.
        if self.read_cursor < self.cursor_frame {
            let start = self.read_cursor;
            let remaining = self.cursor_frame - start;
            let frames_to_read = remaining.min(MAX_DISPLAY_FRAMES as u64) as usize;
            let block = self.read_block_at(start, frames_to_read)?;
            // Advance by the frames actually backed by file data so a short read
            // near EOF still terminates the catch-up rather than spinning.
            let advanced = (block.samples_per_channel as u64).max(1);
            self.read_cursor = start.saturating_add(advanced).min(self.cursor_frame);
            self.last_emitted_frame = Some(self.cursor_frame);
            return Some(block);
        }

        // Caught up to the playhead (paused, or finished draining): only emit a
        // fresh static window when the cursor actually moved since the last
        // emit (e.g. a seek/scrub) — re-reading the same data every frame is
        // wasted work.
        if self.last_emitted_frame == Some(self.cursor_frame) {
            return None;
        }

        let start = self.cursor_frame.saturating_sub(block_frames);
        let frames_to_read = (self.cursor_frame - start)
            .max(block_frames)
            .min(MAX_DISPLAY_FRAMES as u64) as usize;

        let block = self.read_block_at(start, frames_to_read)?;
        self.last_emitted_frame = Some(self.cursor_frame);
        Some(block)
    }

    /// Read a specific range of frames as a SampleBlock (used by `tick` and
    /// the seek/scrub path).
    pub fn read_block_at(&mut self, start_frame: u64, num_frames: usize) -> Option<SampleBlock> {
        let meta = self.metadata.as_ref()?;
        let reader = self.reader.as_mut()?;
        let ch = meta.channel_count;
        if ch == 0 {
            return None;
        }

        let data = reader.read_frames(start_frame, num_frames).ok()?;
        if data.is_empty() {
            return None;
        }

        let actual_frames = data.len() / ch;

        Some(SampleBlock {
            device_id: meta.device_id.clone(),
            stream_id: 0,
            packet_id: start_frame,
            timestamp_start: start_frame,
            sample_rate: meta.sample_rate,
            channel_count: ch,
            samples_per_channel: actual_frames,
            ttl_bits: 0,
            data,
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
            host_time_ns: None,
        })
    }
}

/// Draw the playback panel in the left sidebar.
pub fn draw_playback_section(
    ui: &mut egui::Ui,
    playback: &mut PlaybackManager,
    open_file: &mut bool,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("PLAYBACK")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        // File info / open button
        if let Some(ref path) = playback.file_path {
            let filename = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            ui.horizontal(|ui| {
                theme::status_dot(ui, theme::STATUS_CONNECTED);
                ui.label(
                    egui::RichText::new(&filename)
                        .size(10.0)
                        .color(theme::TEXT_PRIMARY),
                );
            });

            if let Some(meta) = playback.metadata() {
                ui.label(
                    egui::RichText::new(format!(
                        "{}ch / {:.0} Hz / {:.1}s",
                        meta.channel_count,
                        meta.sample_rate,
                        meta.duration_seconds(),
                    ))
                    .size(10.0)
                    .color(theme::TEXT_DIM),
                );
            }

            ui.add_space(4.0);

            // Transport controls
            ui.horizontal(|ui| {
                // Play/Pause
                let play_label = match playback.state {
                    PlaybackState::Playing => "Pause",
                    _ => "Play",
                };
                if ui.button(play_label).clicked() {
                    playback.toggle_play_pause();
                }

                // Stop (rewind to start)
                if ui.button("Rewind").clicked() {
                    playback.seek_to(0);
                    playback.pause();
                }

                // Close
                if ui.button("Close").clicked() {
                    playback.close();
                }
            });

            ui.add_space(2.0);

            // Speed control
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Speed")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
                ui.add(
                    egui::DragValue::new(&mut playback.speed)
                        .range(0.1..=10.0)
                        .speed(0.1)
                        .suffix("x"),
                );
            });

            // Timeline scrubber
            let total = playback.total_frames();
            if total > 0 {
                ui.add_space(4.0);

                let mut frame = playback.cursor_frame as f64;
                let response = ui.add(
                    egui::Slider::new(&mut frame, 0.0..=total as f64)
                        .text("")
                        .show_value(false),
                );
                if response.changed() {
                    playback.seek_to(frame as u64);
                }

                // Time label
                if let Some(meta) = playback.metadata() {
                    let time_s = playback.cursor_frame as f64 / meta.sample_rate;
                    let total_s = meta.duration_seconds();
                    ui.label(
                        egui::RichText::new(format!("{:.2}s / {:.2}s", time_s, total_s,))
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                }
            }
        } else {
            // No file loaded
            if ui.button("Open .kvraw file...").clicked() {
                *open_file = true;
            }

            if let Some(ref err) = playback.error {
                ui.colored_label(theme::ACCENT_RED, err);
            }
        }
    });
}

/// File picker dialog for .kvraw files.
pub fn pick_kvraw_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("KVRAW recording", &["kvraw"])
        .pick_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_recorder::StreamingRecorder;
    use kv_types::SampleBlock;
    use std::sync::atomic::{AtomicU32, Ordering};

    const CH: usize = 4;
    const SPC: usize = 8;

    fn unique_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("kv_playback_test_{tag}_{pid}_{n}"))
    }

    fn block(packet_id: u64) -> SampleBlock {
        block_n(packet_id, SPC)
    }

    fn block_n(packet_id: u64, spc: usize) -> SampleBlock {
        SampleBlock {
            device_id: "test".to_string(),
            stream_id: 0,
            packet_id,
            timestamp_start: packet_id * spc as u64,
            sample_rate: 30_000.0,
            channel_count: CH,
            samples_per_channel: spc,
            ttl_bits: 0,
            data: (0..CH * spc).map(|i| i as i16).collect(),
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
            host_time_ns: None,
        }
    }

    /// Write a 2-block recording.kvraw into a fresh temp dir and return its path.
    fn write_recording(tag: &str) -> PathBuf {
        let dir = unique_dir(tag);
        let mut rec = StreamingRecorder::new(&dir).expect("create recorder");
        rec.write_block(&block(0)).expect("write block 0");
        rec.write_block(&block(1)).expect("write block 1");
        rec.finish().expect("finish recording");
        dir.join("recording.kvraw")
    }

    /// Write an `n_blocks`-block recording with `spc` frames per block.
    fn write_recording_frames(tag: &str, spc: usize, n_blocks: u64) -> PathBuf {
        let dir = unique_dir(tag);
        let mut rec = StreamingRecorder::new(&dir).expect("create recorder");
        for i in 0..n_blocks {
            rec.write_block(&block_n(i, spc)).expect("write block");
        }
        rec.finish().expect("finish recording");
        dir.join("recording.kvraw")
    }

    #[test]
    fn tick_emits_a_block_then_none_until_cursor_moves() {
        let path = write_recording("tick");
        let mut pb = PlaybackManager::default();
        pb.load_file(path);
        assert!(pb.is_loaded());
        assert_eq!(pb.state, PlaybackState::Paused);

        // First tick reads the block at the current cursor position.
        let first = pb.tick().expect("first tick yields a block");
        assert_eq!(first.channel_count, CH);

        // Cursor has not moved, so a paused tick produces no fresh block.
        assert!(pb.tick().is_none());

        // Seeking moves the cursor, so the next tick emits again.
        pb.seek_to(10);
        assert_eq!(pb.cursor_frame, 10);
        assert!(pb.tick().is_some());
    }

    #[test]
    fn seek_to_clamps_to_total_frames() {
        let path = write_recording("seek");
        let mut pb = PlaybackManager::default();
        pb.load_file(path);
        // 2 blocks * 8 samples = 16 frames total.
        pb.seek_to(9_999);
        assert_eq!(pb.cursor_frame, (2 * SPC) as u64);
    }

    #[test]
    fn tick_returns_none_without_a_loaded_file() {
        // L45: with no reader/metadata the cursor cannot advance and no block
        // can be read, so a tick is a no-op even if the state is forced.
        let mut pb = PlaybackManager::default();
        assert!(!pb.is_loaded());
        assert_eq!(pb.state, PlaybackState::Idle);
        assert!(pb.tick().is_none());

        // play() only takes effect with a reader, so the state stays Idle and
        // tick keeps returning None.
        pb.play();
        assert_eq!(pb.state, PlaybackState::Idle);
        assert!(pb.tick().is_none());
    }

    #[test]
    fn playing_tick_auto_pauses_at_end_of_file() {
        // L45: once the cursor reaches the final frame a playing tick clamps to
        // total_frames and flips the state back to Paused.
        let path = write_recording("autopause");
        let mut pb = PlaybackManager::default();
        pb.load_file(path);
        let total = pb.total_frames();
        assert_eq!(total, (2 * SPC) as u64);

        pb.seek_to(total);
        pb.play();
        assert_eq!(pb.state, PlaybackState::Playing);

        let _ = pb.tick();
        assert_eq!(pb.cursor_frame, total);
        assert_eq!(pb.state, PlaybackState::Paused);
    }

    #[test]
    fn toggle_play_pause_is_a_no_op_until_a_file_loads() {
        // L51: the transport state machine ignores toggles while Idle, then
        // alternates Playing/Paused once a recording is loaded.
        let mut pb = PlaybackManager::default();
        pb.toggle_play_pause();
        assert_eq!(pb.state, PlaybackState::Idle);

        let path = write_recording("toggle");
        pb.load_file(path);
        assert_eq!(pb.state, PlaybackState::Paused);

        pb.toggle_play_pause();
        assert_eq!(pb.state, PlaybackState::Playing);
        pb.toggle_play_pause();
        assert_eq!(pb.state, PlaybackState::Paused);
    }

    /// DA44: when the playhead jumps forward by more than one display window,
    /// the data between the previous and the new cursor must be streamed in
    /// full (chunked across ticks) — never skipped.
    #[test]
    fn high_speed_play_streams_every_frame_without_skipping() {
        // 40 blocks * 1000 frames = 40_000 frames (> MAX_DISPLAY_FRAMES), so a
        // single jump to the end must be drained over several ticks.
        let path = write_recording_frames("da44_drain", 1000, 40);
        let mut pb = PlaybackManager::default();
        pb.load_file(path);
        let total = pb.total_frames();
        assert_eq!(total, 40_000);
        assert!(total > MAX_DISPLAY_FRAMES as u64);

        // Simulate a high-speed advance that moves the playhead far past one
        // display window in a single tick.
        pb.cursor_frame = total;

        let mut covered = 0u64;
        let mut ticks = 0;
        while covered < total {
            let block = pb.tick().expect("tick yields a block while draining");
            // Each block is contiguous with the previous one: no inter-block gap.
            assert_eq!(block.packet_id, covered, "blocks must be contiguous");
            assert!(
                block.samples_per_channel as u64 <= MAX_DISPLAY_FRAMES as u64,
                "each chunk is capped at one display window"
            );
            covered += block.samples_per_channel as u64;
            ticks += 1;
            assert!(ticks < 100, "draining must terminate");
        }

        assert_eq!(covered, total, "every frame up to the cursor was read");
        assert!(
            ticks >= 2,
            "a >window jump must chunk across multiple ticks"
        );

        // Fully drained: a further tick yields nothing until the cursor moves.
        assert!(pb.tick().is_none());
    }

    /// DA44: the very first block streamed after a large jump starts at the
    /// previous read position (0 here), not at `cursor - block_frames`, which
    /// was the old frame-skipping behaviour.
    #[test]
    fn high_speed_play_starts_from_previous_cursor_not_a_trailing_block() {
        let path = write_recording_frames("da44_start", 8, 50); // 400 frames
        let mut pb = PlaybackManager::default();
        pb.load_file(path);
        let total = pb.total_frames();
        assert_eq!(total, 400);

        pb.cursor_frame = total; // jump to the end in one go

        let block = pb.tick().expect("first drain tick yields a block");
        assert_eq!(
            block.packet_id, 0,
            "streaming begins at the previous cursor"
        );
        // 400 < MAX_DISPLAY_FRAMES, so the whole range is covered at once.
        assert_eq!(block.samples_per_channel as u64, total);
    }
}
