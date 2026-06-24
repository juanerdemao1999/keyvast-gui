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
    /// Current playback position in frames.
    pub cursor_frame: u64,
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
                self.metadata = Some(meta);
                self.reader = Some(reader);
                self.file_path = Some(path);
                self.cursor_frame = 0;
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
        if !sample_rate.is_finite() || sample_rate <= 0.0 {
            return None;
        }
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

        // While paused, only emit a fresh block when the cursor actually
        // moved (e.g. the user dragged the timeline scrubber) — re-reading
        // and re-ingesting the same data every frame is wasted work.
        if self.last_emitted_frame == Some(self.cursor_frame) {
            return None;
        }

        // Read up to one "block" of data (samples_per_channel or a reasonable chunk).
        let block_frames = if samples_per_channel > 0 {
            samples_per_channel
        } else {
            256
        };
        let start = self.cursor_frame.saturating_sub(block_frames as u64);
        let frames_to_read = ((self.cursor_frame - start) as usize)
            .max(block_frames)
            .min(MAX_DISPLAY_FRAMES);

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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-playback")
            .join(format!("{name}-{nanos}"))
    }

    fn make_block(packet_id: u64, ch: usize, spc: usize) -> SampleBlock {
        SampleBlock {
            device_id: "playback-test".to_string(),
            stream_id: 0,
            packet_id,
            timestamp_start: packet_id * spc as u64,
            sample_rate: 30_000.0,
            channel_count: ch,
            samples_per_channel: spc,
            ttl_bits: 0,
            data: vec![1000i16; ch * spc],
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        }
    }

    fn write_test_kvraw(dir: &std::path::Path, blocks: usize) -> PathBuf {
        fs::create_dir_all(dir).expect("mkdir");
        let mut rec = StreamingRecorder::new(dir).expect("recorder");
        for i in 0..blocks {
            rec.write_block(&make_block(i as u64, 4, 256))
                .expect("write");
        }
        rec.finish().expect("finish");
        dir.join("recording.kvraw")
    }

    #[test]
    fn tick_returns_none_when_idle() {
        let mut pm = PlaybackManager::default();
        assert_eq!(pm.state, PlaybackState::Idle);
        assert!(pm.tick().is_none());
    }

    #[test]
    fn tick_returns_block_after_load() {
        let dir = unique_dir("tick-load");
        let path = write_test_kvraw(&dir, 5);

        let mut pm = PlaybackManager::default();
        pm.load_file(path);
        assert_eq!(pm.state, PlaybackState::Paused);

        // First tick after load should emit a block (cursor at 0, never emitted)
        let block = pm.tick();
        assert!(block.is_some());

        // Second tick without cursor change should return None
        assert!(pm.tick().is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tick_paused_emits_after_seek() {
        let dir = unique_dir("tick-seek");
        let path = write_test_kvraw(&dir, 5);

        let mut pm = PlaybackManager::default();
        pm.load_file(path);

        // Consume initial emission
        pm.tick();

        // Seek to a new position
        pm.seek_to(500);
        assert_eq!(pm.cursor_frame, 500);

        // Tick should now emit a new block
        let block = pm.tick();
        assert!(block.is_some());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tick_returns_none_with_invalid_sample_rate() {
        let dir = unique_dir("tick-bad-sr");
        let path = write_test_kvraw(&dir, 2);

        let mut pm = PlaybackManager::default();
        pm.load_file(path);

        // Tamper with metadata to simulate invalid sample_rate
        if let Some(ref mut meta) = pm.metadata {
            meta.sample_rate = 0.0;
        }

        pm.play();
        assert!(pm.tick().is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn seek_clamps_to_total_frames() {
        let dir = unique_dir("seek-clamp");
        let path = write_test_kvraw(&dir, 3);

        let mut pm = PlaybackManager::default();
        pm.load_file(path);

        let total = pm.total_frames();
        pm.seek_to(total + 10_000);
        assert_eq!(pm.cursor_frame, total);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn play_pause_toggle_state_transitions() {
        let dir = unique_dir("toggle");
        let path = write_test_kvraw(&dir, 2);

        let mut pm = PlaybackManager::default();
        pm.load_file(path);
        assert_eq!(pm.state, PlaybackState::Paused);

        pm.toggle_play_pause();
        assert_eq!(pm.state, PlaybackState::Playing);

        pm.toggle_play_pause();
        assert_eq!(pm.state, PlaybackState::Paused);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn close_resets_to_idle() {
        let dir = unique_dir("close");
        let path = write_test_kvraw(&dir, 2);

        let mut pm = PlaybackManager::default();
        pm.load_file(path);
        assert!(pm.is_loaded());

        pm.close();
        assert!(!pm.is_loaded());
        assert_eq!(pm.state, PlaybackState::Idle);
        assert_eq!(pm.cursor_frame, 0);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_nonexistent_file_sets_error() {
        let mut pm = PlaybackManager::default();
        pm.load_file(PathBuf::from("/nonexistent/path/bad.kvraw"));
        assert_eq!(pm.state, PlaybackState::Idle);
        assert!(pm.error.is_some());
    }
}
