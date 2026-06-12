//! Gate/Trigger recording control.
//!
//! Monitors TTL input bits from `SampleBlock::ttl_bits` to automatically
//! start and stop recording based on configurable trigger conditions.
//!
//! Trigger modes:
//! - **Edge**: Start on rising/falling edge, stop on opposite edge
//! - **Level**: Record while the trigger line is high (or low)
//! - **Pulse count**: Start on N-th rising edge, stop after M edges

use eframe::egui;
use kv_types::SampleBlock;

use crate::theme;

/// Which TTL edge triggers an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerEdge {
    Rising,
    Falling,
}

/// Trigger mode defining when to start/stop recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// Record while the selected bit matches the active level.
    Level,
    /// Start on edge, stop on the opposite edge.
    EdgeToggle,
    /// Start on edge, stop after `duration_blocks` of recording.
    /// (0 = record until manual stop or next edge)
    EdgeTimed,
}

/// State of the trigger system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerState {
    /// Trigger is disabled / not armed.
    Disabled,
    /// Armed — waiting for trigger condition.
    Armed,
    /// Triggered — recording in progress.
    Triggered,
}

/// Full trigger configuration and runtime state.
#[derive(Debug, Clone)]
pub struct TriggerConfig {
    /// Whether trigger-based recording is enabled.
    pub enabled: bool,
    /// Which TTL bit to monitor (0-based).
    pub bit_index: usize,
    /// Edge type for triggering.
    pub edge: TriggerEdge,
    /// Trigger mode.
    pub mode: TriggerMode,
    /// For EdgeTimed mode: how many blocks to record after trigger (0 = until manual stop).
    pub timed_duration_blocks: u64,
    /// Current trigger state.
    pub state: TriggerState,
    /// Previous TTL value (for edge detection).
    prev_ttl: u32,
    /// Block counter since trigger fired (for timed mode).
    blocks_since_trigger: u64,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bit_index: 0,
            edge: TriggerEdge::Rising,
            mode: TriggerMode::EdgeToggle,
            timed_duration_blocks: 0,
            state: TriggerState::Disabled,
            prev_ttl: 0,
            blocks_since_trigger: 0,
        }
    }
}

/// Result of processing a block through the trigger system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerAction {
    /// No action needed.
    None,
    /// Start recording now.
    StartRecording,
    /// Stop recording now.
    StopRecording,
}

impl TriggerConfig {
    /// Arm the trigger (called when user clicks "Arm" in the GUI).
    pub fn arm(&mut self) {
        if self.enabled {
            self.state = TriggerState::Armed;
            self.blocks_since_trigger = 0;
        }
    }

    /// Disarm the trigger.
    pub fn disarm(&mut self) {
        self.state = TriggerState::Disabled;
        self.blocks_since_trigger = 0;
    }

    /// Process one sample block through the trigger system.
    /// Returns the action the caller should take (start/stop recording or nothing).
    pub fn process_block(&mut self, block: &SampleBlock) -> TriggerAction {
        if !self.enabled {
            return TriggerAction::None;
        }

        let current_bit = (block.ttl_bits >> self.bit_index) & 1;
        let prev_bit = (self.prev_ttl >> self.bit_index) & 1;
        self.prev_ttl = block.ttl_bits;

        let rising = prev_bit == 0 && current_bit == 1;
        let falling = prev_bit == 1 && current_bit == 0;

        let edge_detected = match self.edge {
            TriggerEdge::Rising => rising,
            TriggerEdge::Falling => falling,
        };

        let opposite_edge = match self.edge {
            TriggerEdge::Rising => falling,
            TriggerEdge::Falling => rising,
        };

        match self.state {
            TriggerState::Disabled => TriggerAction::None,

            TriggerState::Armed => {
                match self.mode {
                    TriggerMode::Level => {
                        let active = match self.edge {
                            TriggerEdge::Rising => current_bit == 1,
                            TriggerEdge::Falling => current_bit == 0,
                        };
                        if active {
                            self.state = TriggerState::Triggered;
                            self.blocks_since_trigger = 0;
                            TriggerAction::StartRecording
                        } else {
                            TriggerAction::None
                        }
                    }
                    TriggerMode::EdgeToggle | TriggerMode::EdgeTimed => {
                        if edge_detected {
                            self.state = TriggerState::Triggered;
                            self.blocks_since_trigger = 0;
                            TriggerAction::StartRecording
                        } else {
                            TriggerAction::None
                        }
                    }
                }
            }

            TriggerState::Triggered => {
                self.blocks_since_trigger += 1;

                match self.mode {
                    TriggerMode::Level => {
                        let active = match self.edge {
                            TriggerEdge::Rising => current_bit == 1,
                            TriggerEdge::Falling => current_bit == 0,
                        };
                        if !active {
                            self.state = TriggerState::Armed;
                            TriggerAction::StopRecording
                        } else {
                            TriggerAction::None
                        }
                    }
                    TriggerMode::EdgeToggle => {
                        if opposite_edge {
                            self.state = TriggerState::Armed;
                            TriggerAction::StopRecording
                        } else {
                            TriggerAction::None
                        }
                    }
                    TriggerMode::EdgeTimed => {
                        if self.timed_duration_blocks > 0
                            && self.blocks_since_trigger >= self.timed_duration_blocks
                        {
                            self.state = TriggerState::Armed;
                            TriggerAction::StopRecording
                        } else if opposite_edge {
                            self.state = TriggerState::Armed;
                            TriggerAction::StopRecording
                        } else {
                            TriggerAction::None
                        }
                    }
                }
            }
        }
    }
}

/// Draw the trigger configuration section in the GUI sidebar.
pub fn draw_trigger_section(ui: &mut egui::Ui, config: &mut TriggerConfig) {
    egui::CollapsingHeader::new(
        egui::RichText::new("TRIGGER / GATE")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut config.enabled,
                egui::RichText::new("Enable trigger").size(10.0),
            );
        });

        if !config.enabled {
            return;
        }

        // TTL bit selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("TTL bit")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            let mut bit = config.bit_index as i32;
            if ui
                .add(egui::DragValue::new(&mut bit).range(0..=15).speed(0.3))
                .changed()
            {
                config.bit_index = bit.max(0) as usize;
            }
        });

        // Edge selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Edge")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            ui.selectable_value(
                &mut config.edge,
                TriggerEdge::Rising,
                egui::RichText::new("Rising ↑").size(10.0),
            );
            ui.selectable_value(
                &mut config.edge,
                TriggerEdge::Falling,
                egui::RichText::new("Falling ↓").size(10.0),
            );
        });

        // Mode selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Mode")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            egui::ComboBox::from_id_salt("trigger_mode")
                .width(120.0)
                .selected_text(match config.mode {
                    TriggerMode::Level => "Level (gate)",
                    TriggerMode::EdgeToggle => "Edge toggle",
                    TriggerMode::EdgeTimed => "Edge + timed",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut config.mode, TriggerMode::Level, "Level (gate)");
                    ui.selectable_value(&mut config.mode, TriggerMode::EdgeToggle, "Edge toggle");
                    ui.selectable_value(&mut config.mode, TriggerMode::EdgeTimed, "Edge + timed");
                });
        });

        // Duration for timed mode
        if config.mode == TriggerMode::EdgeTimed {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Duration")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
                let mut dur = config.timed_duration_blocks as i64;
                if ui
                    .add(
                        egui::DragValue::new(&mut dur)
                            .range(0..=100000)
                            .speed(10.0)
                            .suffix(" blocks"),
                    )
                    .changed()
                {
                    config.timed_duration_blocks = dur.max(0) as u64;
                }
            });
        }

        // Status and arm/disarm buttons
        ui.add_space(4.0);
        let (status_color, status_text) = match config.state {
            TriggerState::Disabled => (theme::TEXT_DIM, "Idle"),
            TriggerState::Armed => (theme::STATUS_ARMED, "Armed — waiting"),
            TriggerState::Triggered => (theme::STATUS_RECORDING, "TRIGGERED"),
        };
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(status_text)
                    .size(10.0)
                    .color(status_color),
            );

            if config.state == TriggerState::Disabled || config.state == TriggerState::Armed {
                if ui
                    .small_button(egui::RichText::new("Arm").size(9.0))
                    .clicked()
                {
                    config.arm();
                }
            }
            if config.state != TriggerState::Disabled {
                if ui
                    .small_button(egui::RichText::new("Disarm").size(9.0))
                    .clicked()
                {
                    config.disarm();
                }
            }
        });
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
    fn rising_edge_triggers_start() {
        let mut config = TriggerConfig {
            enabled: true,
            bit_index: 0,
            edge: TriggerEdge::Rising,
            mode: TriggerMode::EdgeToggle,
            state: TriggerState::Armed,
            ..Default::default()
        };

        // No edge yet (stays at 0)
        assert_eq!(config.process_block(&make_block(0)), TriggerAction::None);
        // Rising edge
        assert_eq!(config.process_block(&make_block(1)), TriggerAction::StartRecording);
        assert_eq!(config.state, TriggerState::Triggered);
    }

    #[test]
    fn falling_edge_stops_recording() {
        let mut config = TriggerConfig {
            enabled: true,
            bit_index: 0,
            edge: TriggerEdge::Rising,
            mode: TriggerMode::EdgeToggle,
            state: TriggerState::Triggered,
            prev_ttl: 1,
            ..Default::default()
        };

        // Falling edge (opposite of rising)
        assert_eq!(config.process_block(&make_block(0)), TriggerAction::StopRecording);
        assert_eq!(config.state, TriggerState::Armed);
    }

    #[test]
    fn level_mode_gates_on_high() {
        let mut config = TriggerConfig {
            enabled: true,
            bit_index: 2,
            edge: TriggerEdge::Rising,
            mode: TriggerMode::Level,
            state: TriggerState::Armed,
            ..Default::default()
        };

        // Bit 2 low — no trigger
        assert_eq!(config.process_block(&make_block(0b000)), TriggerAction::None);
        // Bit 2 high — trigger
        assert_eq!(config.process_block(&make_block(0b100)), TriggerAction::StartRecording);
        // Still high — no action
        assert_eq!(config.process_block(&make_block(0b100)), TriggerAction::None);
        // Goes low — stop
        assert_eq!(config.process_block(&make_block(0b000)), TriggerAction::StopRecording);
    }

    #[test]
    fn timed_mode_stops_after_duration() {
        let mut config = TriggerConfig {
            enabled: true,
            bit_index: 0,
            edge: TriggerEdge::Rising,
            mode: TriggerMode::EdgeTimed,
            timed_duration_blocks: 3,
            state: TriggerState::Armed,
            ..Default::default()
        };

        // Trigger
        assert_eq!(config.process_block(&make_block(1)), TriggerAction::StartRecording);
        // Count blocks (stays high, no opposite edge)
        assert_eq!(config.process_block(&make_block(1)), TriggerAction::None);
        assert_eq!(config.process_block(&make_block(1)), TriggerAction::None);
        // Third block after trigger → stop
        assert_eq!(config.process_block(&make_block(1)), TriggerAction::StopRecording);
    }

    #[test]
    fn disabled_trigger_does_nothing() {
        let mut config = TriggerConfig::default();
        assert_eq!(config.process_block(&make_block(1)), TriggerAction::None);
    }
}
