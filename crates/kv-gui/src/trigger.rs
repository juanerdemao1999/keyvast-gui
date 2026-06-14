//! Gated recording control driven by a TTL input line.
//!
//! A hardware trigger line (typically a 3.3 V or 5 V logic level wired to one
//! of the TTL inputs) gates recording directly: while the selected bit is at
//! its active level the recorder runs, and when it returns to the idle level
//! recording stops. There is no separate "arm" step and no per-recording
//! Enable click — turning the gate on is enough, the signal does the rest.
//!
//! A companion [`TtlHistory`] buffers recent TTL transitions so the GUI can
//! draw a live digital-logic trace (see [`draw_ttl_monitor`]), letting the
//! operator confirm the qualifying signal in real time.

use std::collections::VecDeque;

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use kv_types::SampleBlock;

use crate::theme;

/// Action the caller should take after a block is processed by the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerAction {
    /// No change.
    None,
    /// Start recording now.
    StartRecording,
    /// Stop recording now.
    StopRecording,
}

/// TTL-gated recording configuration and runtime state.
///
/// The gate is a single opt-in switch: when [`enabled`](Self::enabled) is set,
/// recording follows the selected TTL bit automatically. No arming, edge, or
/// mode selection is required — the common case (record while a level is high)
/// is the only behaviour.
#[derive(Debug, Clone)]
pub struct TriggerConfig {
    /// Master switch: when true the TTL line gates recording automatically.
    pub enabled: bool,
    /// Which TTL bit to watch (0-based).
    pub bit_index: usize,
    /// Active level. `true` = record while the bit is **high** (3.3/5 V), which
    /// is the usual wiring; `false` = record while it is **low**.
    pub active_high: bool,
    /// Whether the gate currently holds recording open (runtime state).
    recording: bool,
    /// Last observed level of the watched bit (for the status readout).
    last_level: bool,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bit_index: 0,
            active_high: true,
            recording: false,
            last_level: false,
        }
    }
}

impl TriggerConfig {
    /// Level of the watched bit as of the most recent block.
    pub fn last_level(&self) -> bool {
        self.last_level
    }

    /// Whether the gate is currently holding recording open.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// Feed one block to the gate and return the recording action to take.
    ///
    /// The watched bit's level is always tracked (even when disabled) so the
    /// status readout stays live. When enabled, recording starts on the first
    /// block whose level is active and stops on the first block that is not.
    pub fn process_block(&mut self, block: &SampleBlock) -> TriggerAction {
        let level = ((block.ttl_bits >> self.bit_index) & 1) == 1;
        self.last_level = level;

        if !self.enabled {
            // If the gate is switched off mid-capture, release recording once.
            if self.recording {
                self.recording = false;
                return TriggerAction::StopRecording;
            }
            return TriggerAction::None;
        }

        let active = level == self.active_high;
        match (active, self.recording) {
            (true, false) => {
                self.recording = true;
                TriggerAction::StartRecording
            }
            (false, true) => {
                self.recording = false;
                TriggerAction::StopRecording
            }
            _ => TriggerAction::None,
        }
    }
}

/// Draw the gate configuration section in the GUI sidebar.
pub fn draw_trigger_section(ui: &mut egui::Ui, config: &mut TriggerConfig) {
    egui::CollapsingHeader::new(
        egui::RichText::new("TRIGGER / GATE")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.checkbox(
            &mut config.enabled,
            egui::RichText::new("Auto-record while TTL active").size(10.0),
        );
        ui.label(
            egui::RichText::new("Recording follows the TTL line — no manual arm/start.")
                .size(9.0)
                .color(theme::TEXT_DIM),
        );

        ui.add_space(2.0);

        // TTL bit selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("TTL bit")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            let mut bit = config.bit_index as i32;
            if ui
                .add(egui::DragValue::new(&mut bit).range(0..=31).speed(0.3))
                .changed()
            {
                config.bit_index = bit.max(0) as usize;
            }
        });

        // Active level selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Active")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.selectable_value(
                &mut config.active_high,
                true,
                egui::RichText::new("High (3.3/5 V)").size(10.0),
            );
            ui.selectable_value(
                &mut config.active_high,
                false,
                egui::RichText::new("Low (0 V)").size(10.0),
            );
        });

        // Live status readout
        ui.add_space(4.0);
        let level_txt = if config.last_level() { "HIGH" } else { "LOW" };
        let (status_color, status_text) = if !config.enabled {
            (theme::TEXT_DIM, "Gate off")
        } else if config.is_recording() {
            (theme::STATUS_RECORDING, "RECORDING (gate open)")
        } else {
            (theme::STATUS_ARMED, "Waiting for active level")
        };
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("TTL: {level_txt}"))
                    .size(10.0)
                    .color(if config.last_level() {
                        theme::STATUS_RECORDING
                    } else {
                        theme::TEXT_DIM
                    }),
            );
            ui.label(
                egui::RichText::new(status_text)
                    .size(10.0)
                    .color(status_color),
            );
        });
        ui.label(
            egui::RichText::new("Tip: Add View ▸ TTL Monitor to watch the line live.")
                .size(9.0)
                .color(theme::TEXT_DIM),
        );
    });
}

// ── TTL history (for the live monitor view) ──────────────────────────

/// One recorded TTL transition: the time it occurred and the full word.
#[derive(Debug, Clone, Copy)]
struct TtlEntry {
    t_ms: f64,
    bits: u32,
}

/// Rolling buffer of TTL transitions used to render the live monitor trace.
///
/// Only *changes* are stored (the line is piecewise-constant between them), so
/// the buffer stays tiny even at high block rates. The most recent transition
/// before the visible window is retained so the trace always has a known
/// starting level.
pub struct TtlHistory {
    buf: VecDeque<TtlEntry>,
    latest_ms: f64,
    window_ms: f64,
}

impl Default for TtlHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl TtlHistory {
    /// Maximum transitions retained before the oldest are dropped.
    const MAX_ENTRIES: usize = 4096;

    pub fn new() -> Self {
        Self {
            buf: VecDeque::new(),
            latest_ms: 0.0,
            window_ms: 10_000.0,
        }
    }

    /// Visible time window in milliseconds.
    pub fn window_ms(&self) -> f64 {
        self.window_ms
    }

    /// Timestamp (ms) of the most recently ingested sample.
    pub fn latest_ms(&self) -> f64 {
        self.latest_ms
    }

    /// Clear all history (called when the stream resets).
    pub fn clear(&mut self) {
        self.buf.clear();
        self.latest_ms = 0.0;
    }

    /// Ingest one block, recording any TTL transitions it contains.
    ///
    /// Per-sample TTL is used when present (finer edges); otherwise the
    /// block-level [`SampleBlock::ttl_bits`] word is used.
    pub fn push_block(&mut self, block: &SampleBlock) {
        let sr = block.sample_rate.max(1.0);
        let start = block.timestamp_start as f64;
        if let Some(per) = block.ttl_in_per_sample.as_ref() {
            for (i, &word) in per.iter().enumerate() {
                let t_ms = (start + i as f64) * 1000.0 / sr;
                self.push_word(t_ms, word);
            }
            // Advance latest to the block end even if the last samples repeated.
            self.latest_ms = (start + per.len().saturating_sub(1) as f64) * 1000.0 / sr;
        } else {
            let t_ms = start * 1000.0 / sr;
            self.push_word(t_ms, block.ttl_bits);
            self.latest_ms = (start + block.samples_per_channel.saturating_sub(1) as f64) * 1000.0
                / sr;
        }
        self.prune();
    }

    fn push_word(&mut self, t_ms: f64, bits: u32) {
        if self.buf.back().map(|e| e.bits) != Some(bits) {
            self.buf.push_back(TtlEntry { t_ms, bits });
        }
        if t_ms > self.latest_ms {
            self.latest_ms = t_ms;
        }
    }

    fn prune(&mut self) {
        let cutoff = self.latest_ms - self.window_ms * 1.5;
        // Keep one entry before the cutoff so the leading level is known.
        while self.buf.len() > 2 && self.buf[1].t_ms < cutoff {
            self.buf.pop_front();
        }
        while self.buf.len() > Self::MAX_ENTRIES {
            self.buf.pop_front();
        }
    }

    /// Level of `bit` at time `t_ms` (last transition at or before it).
    fn level_at(&self, bit: usize, t_ms: f64) -> bool {
        let mut level = self.buf.front().map(|e| bit_set(e.bits, bit)).unwrap_or(false);
        for e in &self.buf {
            if e.t_ms > t_ms {
                break;
            }
            level = bit_set(e.bits, bit);
        }
        level
    }

    /// Build a square-step trace for `bit` over `[t_left, t_right]`, with x in
    /// seconds relative to the right edge (0 = now, negative = past).
    fn step_points(&self, bit: usize, t_left: f64, t_right: f64) -> Vec<[f64; 2]> {
        let rel = |t: f64| (t - t_right) / 1000.0;
        let mut pts: Vec<[f64; 2]> = Vec::new();
        let mut level = self.level_at(bit, t_left);
        pts.push([rel(t_left), level as i32 as f64]);
        for e in &self.buf {
            if e.t_ms <= t_left {
                continue;
            }
            if e.t_ms > t_right {
                break;
            }
            let lvl = bit_set(e.bits, bit);
            if lvl != level {
                pts.push([rel(e.t_ms), level as i32 as f64]);
                pts.push([rel(e.t_ms), lvl as i32 as f64]);
                level = lvl;
            }
        }
        pts.push([rel(t_right), level as i32 as f64]);
        pts
    }
}

#[inline]
fn bit_set(bits: u32, bit: usize) -> bool {
    ((bits >> bit) & 1) == 1
}

/// Draw the live TTL digital-logic monitor tile.
pub fn draw_ttl_monitor(ui: &mut egui::Ui, history: &TtlHistory, config: &TriggerConfig) {
    let bit = config.bit_index;

    if history.buf.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new("Waiting for TTL signal…")
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
        });
        return;
    }

    let level_now = history.level_at(bit, history.latest_ms());
    let active = level_now == config.active_high;

    // Header strip: bit, current level, gate status.
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("TTL bit {bit}"))
                .size(11.0)
                .strong()
                .color(theme::TEXT_SECONDARY),
        );
        ui.label(
            egui::RichText::new(if level_now { "HIGH" } else { "LOW" })
                .size(11.0)
                .strong()
                .color(if level_now {
                    theme::STATUS_RECORDING
                } else {
                    theme::TEXT_DIM
                }),
        );
        if config.enabled {
            let (c, t) = if active {
                (theme::STATUS_RECORDING, "● recording")
            } else {
                (theme::STATUS_ARMED, "waiting")
            };
            ui.label(egui::RichText::new(t).size(10.0).color(c));
        } else {
            ui.label(
                egui::RichText::new("gate off")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
        }
    });

    let window_ms = history.window_ms();
    let t_right = history.latest_ms();
    let t_left = t_right - window_ms;
    let pts = history.step_points(bit, t_left, t_right);
    let window_s = window_ms / 1000.0;

    let line_color = if active && config.enabled {
        theme::STATUS_RECORDING
    } else {
        theme::ACCENT_CYAN
    };

    Plot::new("ttl_monitor_plot")
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .show_grid([false, true])
        .x_axis_label("seconds")
        .y_axis_formatter(|mark, _| match mark.value.round() as i64 {
            0 => "LOW".to_owned(),
            1 => "HIGH".to_owned(),
            _ => String::new(),
        })
        .show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
                [-window_s, -0.15],
                [0.0, 1.25],
            ));
            plot_ui.line(
                Line::new(PlotPoints::from(pts))
                    .color(line_color)
                    .fill(0.0)
                    .fill_alpha(0.18)
                    .width(1.5),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_block(ttl_bits: u32) -> SampleBlock {
        SampleBlock {
            device_id: "test".to_string(),
            stream_id: 0,
            packet_id: 0,
            timestamp_start: 0,
            sample_rate: 30000.0,
            channel_count: 4,
            samples_per_channel: 64,
            ttl_bits,
            data: vec![0i16; 256],
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        }
    }

    #[test]
    fn disabled_gate_does_nothing() {
        let mut cfg = TriggerConfig::default();
        assert_eq!(cfg.process_block(&make_block(1)), TriggerAction::None);
        assert!(cfg.last_level());
    }

    #[test]
    fn high_level_starts_then_low_stops() {
        let mut cfg = TriggerConfig {
            enabled: true,
            bit_index: 0,
            active_high: true,
            ..Default::default()
        };
        // Low: nothing
        assert_eq!(cfg.process_block(&make_block(0b0)), TriggerAction::None);
        // High: start, idempotent while held
        assert_eq!(cfg.process_block(&make_block(0b1)), TriggerAction::StartRecording);
        assert_eq!(cfg.process_block(&make_block(0b1)), TriggerAction::None);
        assert!(cfg.is_recording());
        // Low: stop
        assert_eq!(cfg.process_block(&make_block(0b0)), TriggerAction::StopRecording);
        assert!(!cfg.is_recording());
    }

    #[test]
    fn active_low_inverts_the_gate() {
        let mut cfg = TriggerConfig {
            enabled: true,
            bit_index: 2,
            active_high: false,
            ..Default::default()
        };
        // Bit high → inactive (active-low)
        assert_eq!(cfg.process_block(&make_block(0b100)), TriggerAction::None);
        // Bit low → active → start
        assert_eq!(cfg.process_block(&make_block(0b000)), TriggerAction::StartRecording);
        // Bit high again → stop
        assert_eq!(cfg.process_block(&make_block(0b100)), TriggerAction::StopRecording);
    }

    #[test]
    fn disabling_mid_capture_releases_recording() {
        let mut cfg = TriggerConfig {
            enabled: true,
            bit_index: 0,
            active_high: true,
            ..Default::default()
        };
        assert_eq!(cfg.process_block(&make_block(1)), TriggerAction::StartRecording);
        cfg.enabled = false;
        assert_eq!(cfg.process_block(&make_block(1)), TriggerAction::StopRecording);
        assert_eq!(cfg.process_block(&make_block(1)), TriggerAction::None);
    }

    #[test]
    fn history_records_transitions_only() {
        let mut h = TtlHistory::new();
        let mut blk = make_block(0);
        blk.timestamp_start = 0;
        h.push_block(&blk);
        let mut blk2 = make_block(0);
        blk2.timestamp_start = 64;
        h.push_block(&blk2); // no change → no new entry
        assert_eq!(h.buf.len(), 1);
        let mut blk3 = make_block(1);
        blk3.timestamp_start = 128;
        h.push_block(&blk3); // change → new entry
        assert_eq!(h.buf.len(), 2);
        assert!(h.level_at(0, h.latest_ms()));
    }
}
