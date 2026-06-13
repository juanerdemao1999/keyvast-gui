//! Selective channel save — allows recording only a subset of channels.
//!
//! When enabled, the recorder writes only the selected channels to disk,
//! reducing file size for experiments where only certain brain regions are
//! of interest. The selection state is stored here and consulted by the
//! recording pipeline when writing blocks.

use eframe::egui;
use kv_types::SampleBlock;

use crate::theme;

// ── Channel selection state ─────────────────────────────────────────

/// Per-channel recording selection.
#[derive(Debug, Clone)]
pub struct ChannelSelectState {
    /// Master enable for selective saving. When false, all channels are saved.
    pub enabled: bool,
    /// Per-channel selection (true = save this channel).
    pub selected: Vec<bool>,
    /// Total channel count (synced from acquisition).
    pub channel_count: usize,
}

impl Default for ChannelSelectState {
    fn default() -> Self {
        Self {
            enabled: false,
            selected: vec![true; 64],
            channel_count: 16,
        }
    }
}

impl ChannelSelectState {
    /// Ensure selection vec matches current channel count.
    pub fn sync_channel_count(&mut self, count: usize) {
        if self.channel_count != count {
            self.channel_count = count;
            self.selected.resize(count, true);
        }
    }

    /// Number of channels selected for recording.
    pub fn selected_count(&self) -> usize {
        if !self.enabled {
            return self.channel_count;
        }
        self.selected
            .iter()
            .take(self.channel_count)
            .filter(|&&s| s)
            .count()
    }

    /// Get ordered list of selected channel indices.
    pub fn selected_indices(&self) -> Vec<usize> {
        if !self.enabled {
            return (0..self.channel_count).collect();
        }
        (0..self.channel_count)
            .filter(|&ch| self.selected.get(ch).copied().unwrap_or(true))
            .collect()
    }

    /// Check if a specific channel is selected for recording.
    #[allow(dead_code)] // selection API kept for upcoming probe-map integration
    pub fn is_selected(&self, ch: usize) -> bool {
        if !self.enabled {
            return true;
        }
        self.selected.get(ch).copied().unwrap_or(true)
    }

    /// Select all channels.
    pub fn select_all(&mut self) {
        for s in self.selected.iter_mut() {
            *s = true;
        }
    }

    /// Deselect all channels.
    pub fn deselect_all(&mut self) {
        for s in self.selected.iter_mut() {
            *s = false;
        }
    }

    /// Select a range of channels [start, end) inclusive start.
    #[allow(dead_code)] // selection API kept for upcoming probe-map integration
    pub fn select_range(&mut self, start: usize, end: usize) {
        for ch in start..end.min(self.channel_count) {
            if let Some(s) = self.selected.get_mut(ch) {
                *s = true;
            }
        }
    }

    /// Channel indices to write while recording, or `None` when every
    /// channel should be saved (selection disabled, empty, or complete).
    pub fn recording_selection(&self) -> Option<Vec<usize>> {
        if !self.enabled {
            return None;
        }
        let indices = self.selected_indices();
        if indices.is_empty() || indices.len() == self.channel_count {
            None
        } else {
            Some(indices)
        }
    }
}

/// Build a copy of `block` containing only the given channels, preserving
/// sample order. Indices outside the block's channel range are skipped.
pub fn filter_block_channels(block: &SampleBlock, indices: &[usize]) -> SampleBlock {
    let ch = block.channel_count;
    let spc = block.samples_per_channel;
    let valid: Vec<usize> = indices.iter().copied().filter(|&c| c < ch).collect();
    let mut data = Vec::with_capacity(valid.len() * spc);
    for s in 0..spc {
        for &c in &valid {
            data.push(block.data[s * ch + c]);
        }
    }
    SampleBlock {
        data,
        channel_count: valid.len(),
        ..block.clone()
    }
}

// ── GUI drawing ─────────────────────────────────────────────────────

/// Unified channel panel (B3).
///
/// Merges the former display-channel list and recording-channel selector into
/// a single place, with one row per channel exposing both a **Disp** toggle
/// (whether the channel is drawn) and a **Rec** toggle (whether it is written
/// to disk).  The Rec column is only interactive when "Record subset only" is
/// enabled, and is locked entirely while a recording is in progress (`rec_locked`,
/// B2) since the on-disk channel set is fixed when recording starts.
pub fn draw_unified_channels(
    ui: &mut egui::Ui,
    display: &mut crate::panels::DisplaySettings,
    select: &mut ChannelSelectState,
    block: Option<&SampleBlock>,
    rec_locked: bool,
) {
    let ch_count = block.map(|b| b.channel_count).unwrap_or(0);
    let visible = display.visible_channels.min(ch_count);
    select.sync_channel_count(ch_count);

    egui::CollapsingHeader::new(
        egui::RichText::new(format!("CHANNELS ({visible})"))
            .size(theme::FONT_HEADING)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        if visible == 0 {
            ui.label(
                egui::RichText::new("No channels")
                    .size(theme::FONT_BODY)
                    .color(theme::TEXT_DIM),
            );
            return;
        }

        // Ensure the display-enable vector covers every visible channel.
        while display.channel_enabled.len() < visible {
            display.channel_enabled.push(true);
        }

        // Recording-subset master toggle.
        ui.add_enabled_ui(!rec_locked, |ui| {
            ui.checkbox(
                &mut select.enabled,
                egui::RichText::new("Record subset only").size(theme::FONT_BODY),
            )
            .on_hover_text("When off, every channel is recorded regardless of the Rec column");
        });

        // Counts summary.
        let disp_on = (0..visible)
            .filter(|&i| display.channel_enabled.get(i).copied().unwrap_or(true))
            .count();
        let rec_on = select.selected_count().min(visible);
        ui.label(
            egui::RichText::new(format!(
                "Display {disp_on}/{visible}  \u{00B7}  Record {rec_on}/{visible}"
            ))
            .size(theme::FONT_CAPTION)
            .color(theme::TEXT_DIM),
        );

        // Bulk actions — Display row.
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Disp")
                    .size(theme::FONT_CAPTION)
                    .color(theme::TEXT_DIM),
            );
            if ui.small_button("All").clicked() {
                for i in 0..visible {
                    display.channel_enabled[i] = true;
                }
            }
            if ui.small_button("None").clicked() {
                for i in 0..visible {
                    display.channel_enabled[i] = false;
                }
            }
            if ui.small_button("Invert").clicked() {
                for i in 0..visible {
                    display.channel_enabled[i] = !display.channel_enabled[i];
                }
            }
        });

        // Bulk actions — Record row (only when selecting a subset, unlocked).
        ui.add_enabled_ui(select.enabled && !rec_locked, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Rec ")
                        .size(theme::FONT_CAPTION)
                        .color(theme::TEXT_DIM),
                );
                if ui.small_button("All").clicked() {
                    select.select_all();
                }
                if ui.small_button("None").clicked() {
                    select.deselect_all();
                }
                if ui.small_button("Invert").clicked() {
                    for ch in 0..select.channel_count {
                        if let Some(s) = select.selected.get_mut(ch) {
                            *s = !*s;
                        }
                    }
                }
                if ui.small_button("Even").clicked() {
                    for ch in 0..select.channel_count {
                        if let Some(s) = select.selected.get_mut(ch) {
                            *s = ch % 2 == 0;
                        }
                    }
                }
                if ui.small_button("Odd").clicked() {
                    for ch in 0..select.channel_count {
                        if let Some(s) = select.selected.get_mut(ch) {
                            *s = ch % 2 == 1;
                        }
                    }
                }
            });
        });

        if rec_locked {
            ui.label(
                egui::RichText::new("\u{1F512} Record selection locked while recording")
                    .size(theme::FONT_CAPTION)
                    .color(theme::TEXT_DIM),
            );
        }

        ui.add_space(2.0);

        // Per-channel rows: [color · CHn] [Disp] [Rec].
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .min_scrolled_width(170.0)
            .show(ui, |ui| {
                ui.set_min_width(170.0);
                egui::Grid::new("kv_unified_chan_grid")
                    .num_columns(3)
                    .spacing(egui::vec2(10.0, 2.0))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("Channel")
                                .size(theme::FONT_CAPTION)
                                .color(theme::TEXT_DIM),
                        );
                        ui.label(
                            egui::RichText::new("Disp")
                                .size(theme::FONT_CAPTION)
                                .color(theme::TEXT_DIM),
                        );
                        ui.label(
                            egui::RichText::new("Rec")
                                .size(theme::FONT_CAPTION)
                                .color(theme::TEXT_DIM),
                        );
                        ui.end_row();

                        for ch in 0..visible {
                            let color = theme::channel_color(ch);
                            let disp_on = display.channel_enabled[ch];
                            let label_color = if disp_on { color } else { theme::TEXT_DIM };

                            ui.horizontal(|ui| {
                                let (bar_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(3.0, 12.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(bar_rect, 0.0, color);
                                ui.label(
                                    egui::RichText::new(format!("CH{ch}"))
                                        .size(theme::FONT_BODY)
                                        .monospace()
                                        .color(label_color),
                                );
                            });

                            ui.checkbox(&mut display.channel_enabled[ch], "");

                            ui.add_enabled_ui(select.enabled && !rec_locked, |ui| {
                                if select.selected.len() <= ch {
                                    select.selected.resize(ch + 1, true);
                                }
                                ui.checkbox(&mut select.selected[ch], "");
                            });

                            ui.end_row();
                        }
                    });
            });
    });
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_selects_all() {
        let s = ChannelSelectState::default();
        assert_eq!(s.selected_count(), 16); // channel_count=16
        assert_eq!(s.selected_indices(), (0..16).collect::<Vec<_>>());
    }

    #[test]
    fn disabled_returns_all() {
        let mut s = ChannelSelectState::default();
        s.enabled = false;
        s.selected[0] = false;
        // Disabled ignores selection state
        assert_eq!(s.selected_count(), 16);
        assert!(s.is_selected(0));
    }

    #[test]
    fn select_subset() {
        let mut s = ChannelSelectState::default();
        s.enabled = true;
        s.channel_count = 8;
        s.selected = vec![false; 8];
        s.selected[1] = true;
        s.selected[3] = true;
        s.selected[5] = true;
        assert_eq!(s.selected_count(), 3);
        assert_eq!(s.selected_indices(), vec![1, 3, 5]);
    }

    #[test]
    fn filter_block_channels_extracts_channels() {
        // 2 samples × 4 channels: [s0c0, s0c1, s0c2, s0c3, s1c0, s1c1, s1c2, s1c3]
        let block = SampleBlock {
            device_id: "test".to_string(),
            stream_id: 0,
            packet_id: 0,
            timestamp_start: 0,
            sample_rate: 30_000.0,
            channel_count: 4,
            samples_per_channel: 2,
            ttl_bits: 0,
            data: vec![10, 20, 30, 40, 50, 60, 70, 80],
            aux_data: None,
            board_adc_data: None,
            ttl_in_per_sample: None,
            ttl_out_per_sample: None,
        };
        let filtered = filter_block_channels(&block, &[0, 2]);
        assert_eq!(filtered.channel_count, 2);
        // Should have: [s0c0, s0c2, s1c0, s1c2]
        assert_eq!(filtered.data, vec![10, 30, 50, 70]);
    }

    #[test]
    fn select_range_works() {
        let mut s = ChannelSelectState::default();
        s.enabled = true;
        s.channel_count = 8;
        s.selected = vec![false; 8];
        s.select_range(2, 5);
        assert_eq!(s.selected_indices(), vec![2, 3, 4]);
    }

    #[test]
    fn sync_channel_count_resizes() {
        let mut s = ChannelSelectState::default();
        s.sync_channel_count(32);
        assert_eq!(s.channel_count, 32);
        assert_eq!(s.selected.len(), 32);
        assert!(s.selected[31]); // new channels default to selected
    }
}
