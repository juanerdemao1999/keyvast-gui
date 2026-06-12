//! Audio monitor — real-time audio output for a selected channel.
//!
//! Buffers incoming samples and provides them to an audio output callback.
//! The actual platform audio output (WASAPI/CoreAudio/ALSA) is abstracted
//! behind a simple ring buffer interface so that this module works on all
//! platforms and can be connected to any audio backend (cpal, rodio, etc.).
//!
//! Since kv-gui currently targets Windows and doesn't include cpal in
//! dependencies, this module provides the state management, GUI controls,
//! and a `drain_audio_samples()` method for future audio backend integration.

use std::collections::VecDeque;

use eframe::egui;

use crate::disp_ring::DisplayRing;
use crate::theme;

/// Audio monitor configuration and runtime state.
#[derive(Debug, Clone)]
pub struct AudioMonitorState {
    /// Whether audio monitoring is enabled.
    pub enabled: bool,
    /// Which channel to listen to (physical index).
    pub channel: usize,
    /// Output volume (0.0 – 1.0).
    pub volume: f32,
    /// Audio output sample rate (typically 44100 or 48000).
    pub output_sample_rate: u32,
    /// Ring buffer of audio samples ready for output.
    /// Values are normalized to [-1.0, 1.0].
    audio_buffer: VecDeque<f32>,
    /// Maximum audio buffer size (samples) before dropping old data.
    buffer_capacity: usize,
    /// Decimation ratio (acquisition SR / audio SR).
    decimation: usize,
    /// Decimation accumulator (counts input samples, outputs when reaching decimation).
    decim_counter: usize,
}

/// Default audio buffer: ~200ms at 48kHz.
const DEFAULT_BUFFER_CAPACITY: usize = 48000 / 5;

impl Default for AudioMonitorState {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: 0,
            volume: 0.5,
            output_sample_rate: 44100,
            audio_buffer: VecDeque::with_capacity(DEFAULT_BUFFER_CAPACITY),
            buffer_capacity: DEFAULT_BUFFER_CAPACITY,
            decimation: 1,
            decim_counter: 0,
        }
    }
}

impl AudioMonitorState {
    /// Update the decimation ratio when acquisition sample rate changes.
    pub fn update_sample_rate(&mut self, acq_sample_rate: f64) {
        if acq_sample_rate > 0.0 && self.output_sample_rate > 0 {
            self.decimation =
                (acq_sample_rate / self.output_sample_rate as f64).round().max(1.0) as usize;
        }
    }

    /// Feed raw i16 samples from the selected channel.
    /// Decimates to the output sample rate and buffers for audio output.
    pub fn feed_samples(&mut self, samples: &[i16]) {
        if !self.enabled {
            return;
        }

        for &sample in samples {
            self.decim_counter += 1;
            if self.decim_counter >= self.decimation {
                self.decim_counter = 0;
                // Normalize i16 → [-1.0, 1.0] and apply volume
                let normalized = (sample as f32 / 32767.0) * self.volume;
                self.audio_buffer.push_back(normalized);
                // Drop old samples if buffer overflows
                if self.audio_buffer.len() > self.buffer_capacity {
                    self.audio_buffer.pop_front();
                }
            }
        }
    }

    /// Feed samples from the display ring (called each frame when enabled).
    #[allow(dead_code)] // pending cpal output integration
    pub fn feed_from_ring(&mut self, ring: &DisplayRing, acq_sample_rate: f64) {
        if !self.enabled || !ring.ready {
            return;
        }
        self.update_sample_rate(acq_sample_rate);
        // Get the most recent chunk from the ring for this channel
        let chunk_size = 128; // roughly one block
        let samples = ring.last_n_samples(self.channel, chunk_size);
        self.feed_samples(&samples);
    }

    /// Drain available audio samples for the output callback.
    /// Returns up to `max_samples` normalized [-1.0, 1.0] samples.
    #[allow(dead_code)] // pending cpal output integration
    pub fn drain_audio_samples(&mut self, max_samples: usize) -> Vec<f32> {
        let n = self.audio_buffer.len().min(max_samples);
        self.audio_buffer.drain(..n).collect()
    }

    /// Number of samples currently buffered.
    pub fn buffered_samples(&self) -> usize {
        self.audio_buffer.len()
    }

    /// Clear the audio buffer (e.g., on channel change or disable).
    pub fn clear_buffer(&mut self) {
        self.audio_buffer.clear();
        self.decim_counter = 0;
    }
}

/// Draw the audio monitor section in the GUI sidebar.
pub fn draw_audio_monitor_section(
    ui: &mut egui::Ui,
    state: &mut AudioMonitorState,
    total_channels: usize,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("AUDIO MONITOR")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.horizontal(|ui| {
            let prev_enabled = state.enabled;
            ui.checkbox(
                &mut state.enabled,
                egui::RichText::new("Enable").size(10.0),
            );
            if !state.enabled && prev_enabled {
                state.clear_buffer();
            }
        });

        if !state.enabled {
            return;
        }

        // Channel selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Channel")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            let mut ch = state.channel as i32;
            let max_ch = total_channels.saturating_sub(1) as i32;
            let prev_ch = ch;
            if ui
                .add(egui::DragValue::new(&mut ch).range(0..=max_ch).speed(0.3))
                .changed()
            {
                state.channel = ch.max(0) as usize;
                if ch != prev_ch {
                    state.clear_buffer();
                }
            }
        });

        // Volume slider
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Volume")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::Slider::new(&mut state.volume, 0.0..=1.0)
                    .step_by(0.01)
                    .show_value(false),
            );
            ui.label(
                egui::RichText::new(format!("{:.0}%", state.volume * 100.0))
                    .size(9.0)
                    .color(theme::TEXT_DIM),
            );
        });

        // Output sample rate
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Out SR")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            egui::ComboBox::from_id_salt("audio_sr")
                .width(80.0)
                .selected_text(format!("{}", state.output_sample_rate))
                .show_ui(ui, |ui| {
                    for &sr in &[22050u32, 44100, 48000] {
                        ui.selectable_value(
                            &mut state.output_sample_rate,
                            sr,
                            format!("{sr} Hz"),
                        );
                    }
                });
        });

        // Buffer status
        let buffered = state.buffered_samples();
        let capacity = state.buffer_capacity;
        let fill_pct = (buffered as f64 / capacity as f64) * 100.0;
        ui.label(
            egui::RichText::new(format!(
                "Buffer: {buffered}/{capacity} ({fill_pct:.0}%)"
            ))
            .size(9.0)
            .color(if fill_pct > 80.0 {
                theme::ACCENT_RED
            } else {
                theme::TEXT_DIM
            }),
        );

        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Note: connect audio backend (cpal) for output")
                .size(9.0)
                .italics()
                .color(theme::TEXT_DIM),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_and_drain_samples() {
        let mut state = AudioMonitorState {
            enabled: true,
            volume: 1.0,
            decimation: 1,
            ..Default::default()
        };

        let samples: Vec<i16> = (0..100).map(|i| (i * 100) as i16).collect();
        state.feed_samples(&samples);

        assert_eq!(state.buffered_samples(), 100);
        let drained = state.drain_audio_samples(50);
        assert_eq!(drained.len(), 50);
        assert_eq!(state.buffered_samples(), 50);
    }

    #[test]
    fn decimation_reduces_output() {
        let mut state = AudioMonitorState {
            enabled: true,
            volume: 1.0,
            decimation: 3,
            ..Default::default()
        };

        let samples: Vec<i16> = vec![1000i16; 90];
        state.feed_samples(&samples);

        // 90 input / 3 decimation = 30 output
        assert_eq!(state.buffered_samples(), 30);
    }

    #[test]
    fn disabled_does_not_buffer() {
        let mut state = AudioMonitorState::default(); // enabled = false
        state.feed_samples(&[1000i16; 50]);
        assert_eq!(state.buffered_samples(), 0);
    }

    #[test]
    fn buffer_overflow_drops_old() {
        let mut state = AudioMonitorState {
            enabled: true,
            volume: 1.0,
            decimation: 1,
            buffer_capacity: 10,
            ..Default::default()
        };

        state.feed_samples(&[100i16; 15]);
        // Buffer should cap at 10
        assert_eq!(state.buffered_samples(), 10);
    }

    #[test]
    fn volume_scales_output() {
        let mut state = AudioMonitorState {
            enabled: true,
            volume: 0.5,
            decimation: 1,
            ..Default::default()
        };

        state.feed_samples(&[32767i16; 1]);
        let out = state.drain_audio_samples(1);
        // 32767/32767 * 0.5 = 0.5
        assert!((out[0] - 0.5).abs() < 0.001);
    }
}
