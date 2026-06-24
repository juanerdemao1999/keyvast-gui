//! Channel mapping / sorting panel.
//!
//! Allows the user to reorder channels in the display, matching physical
//! probe layouts (e.g., NeuroNexus, Cambridge NeuroTech) or custom orderings.

use eframe::egui;

use crate::panels::DisplaySettings;
use crate::theme;

/// Predefined channel mapping presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMapPreset {
    /// Natural order: 0, 1, 2, ...
    Natural,
    /// Reverse order: N-1, N-2, ..., 0
    Reverse,
    /// Even channels first, then odd: 0, 2, 4, ..., 1, 3, 5, ...
    EvenOdd,
    /// Custom (user-defined).
    Custom,
}

impl ChannelMapPreset {
    fn label(self) -> &'static str {
        match self {
            Self::Natural => "Natural (0,1,2,...)",
            Self::Reverse => "Reverse",
            Self::EvenOdd => "Even/Odd",
            Self::Custom => "Custom",
        }
    }
}

const PRESETS: &[ChannelMapPreset] = &[
    ChannelMapPreset::Natural,
    ChannelMapPreset::Reverse,
    ChannelMapPreset::EvenOdd,
    ChannelMapPreset::Custom,
];

/// Channel mapping state (kept in app).
#[derive(Debug, Clone)]
pub struct ChannelMapState {
    pub preset: ChannelMapPreset,
    /// User-entered custom mapping string (e.g. "0,2,4,1,3,5").
    pub custom_text: String,
    /// Parse error for the custom text.
    pub parse_error: Option<String>,
}

impl Default for ChannelMapState {
    fn default() -> Self {
        Self {
            preset: ChannelMapPreset::Natural,
            custom_text: String::new(),
            parse_error: None,
        }
    }
}

/// Generate a channel order from a preset.
fn generate_order(preset: ChannelMapPreset, total_channels: usize) -> Vec<usize> {
    match preset {
        ChannelMapPreset::Natural => Vec::new(), // empty = identity
        ChannelMapPreset::Reverse => (0..total_channels).rev().collect(),
        ChannelMapPreset::EvenOdd => {
            let mut order: Vec<usize> = (0..total_channels).step_by(2).collect();
            order.extend((1..total_channels).step_by(2));
            order
        }
        ChannelMapPreset::Custom => Vec::new(), // handled separately
    }
}

/// Parse a custom mapping string like "0,2,4,1,3,5" into a channel order.
fn parse_custom_mapping(text: &str, total_channels: usize) -> Result<Vec<usize>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = text.split(',').map(|s| s.trim()).collect();
    let mut order = Vec::with_capacity(parts.len());
    let mut seen = std::collections::HashSet::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        let ch: usize = part
            .parse()
            .map_err(|_| format!("Invalid number at position {}: '{part}'", i + 1))?;
        if ch >= total_channels {
            return Err(format!(
                "Channel {ch} out of range (max {})",
                total_channels - 1
            ));
        }
        if !seen.insert(ch) {
            return Err(format!("Duplicate channel {ch} at position {}", i + 1));
        }
        order.push(ch);
    }
    Ok(order)
}

/// Draw the channel mapping section in the sidebar.
pub fn draw_channel_map_section(
    ui: &mut egui::Ui,
    state: &mut ChannelMapState,
    display: &mut DisplaySettings,
    total_channels: usize,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("CHANNEL MAP")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        let mut changed = false;

        // Preset selector
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Preset")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            egui::ComboBox::from_id_salt("ch_map_preset")
                .width(ui.available_width() - 4.0)
                .selected_text(
                    egui::RichText::new(state.preset.label())
                        .size(10.0)
                        .color(theme::TEXT_PRIMARY),
                )
                .show_ui(ui, |ui| {
                    for &preset in PRESETS {
                        if ui
                            .selectable_value(&mut state.preset, preset, preset.label())
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });
        });

        // Custom input (only when Custom preset is selected)
        if state.preset == ChannelMapPreset::Custom {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Order")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
                if ui
                    .text_edit_singleline(&mut state.custom_text)
                    .on_hover_text("Comma-separated channel indices, e.g. 0,2,4,1,3,5")
                    .changed()
                {
                    changed = true;
                }
            });

            if let Some(ref err) = state.parse_error {
                ui.colored_label(theme::ACCENT_RED, err);
            }
        }

        // Current mapping info
        if !display.channel_order.is_empty() {
            let preview: String = display
                .channel_order
                .iter()
                .take(12)
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let suffix = if display.channel_order.len() > 12 {
                ",..."
            } else {
                ""
            };
            ui.label(
                egui::RichText::new(format!("Map: {preview}{suffix}"))
                    .size(9.0)
                    .color(theme::TEXT_DIM),
            );
        }

        // Apply mapping when changed
        if changed {
            if state.preset == ChannelMapPreset::Custom {
                match parse_custom_mapping(&state.custom_text, total_channels) {
                    Ok(order) => {
                        display.channel_order = order;
                        state.parse_error = None;
                    }
                    Err(e) => {
                        state.parse_error = Some(e);
                    }
                }
            } else {
                display.channel_order = generate_order(state.preset, total_channels);
                state.parse_error = None;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_is_empty() {
        let order = generate_order(ChannelMapPreset::Natural, 8);
        assert!(order.is_empty());
    }

    #[test]
    fn reverse_order() {
        let order = generate_order(ChannelMapPreset::Reverse, 4);
        assert_eq!(order, vec![3, 2, 1, 0]);
    }

    #[test]
    fn even_odd_order() {
        let order = generate_order(ChannelMapPreset::EvenOdd, 6);
        assert_eq!(order, vec![0, 2, 4, 1, 3, 5]);
    }

    #[test]
    fn parse_valid_custom() {
        let order = parse_custom_mapping("3,1,0,2", 4).unwrap();
        assert_eq!(order, vec![3, 1, 0, 2]);
    }

    #[test]
    fn parse_out_of_range() {
        let err = parse_custom_mapping("0,5", 4).unwrap_err();
        assert!(err.contains("out of range"));
    }

    #[test]
    fn parse_empty_returns_empty() {
        let order = parse_custom_mapping("", 4).unwrap();
        assert!(order.is_empty());
    }
}
