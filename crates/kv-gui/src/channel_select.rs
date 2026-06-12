//! Selective channel save — allows recording only a subset of channels.
//!
//! When enabled, the recorder writes only the selected channels to disk,
//! reducing file size for experiments where only certain brain regions are
//! of interest. The selection state is stored here and consulted by the
//! recording pipeline when writing blocks.

use eframe::egui;

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
        self.selected.iter().take(self.channel_count).filter(|&&s| s).count()
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
    pub fn select_range(&mut self, start: usize, end: usize) {
        for ch in start..end.min(self.channel_count) {
            if let Some(s) = self.selected.get_mut(ch) {
                *s = true;
            }
        }
    }

    /// Filter a block's data to only include selected channels.
    /// Returns (filtered_data, filtered_channel_count).
    pub fn filter_block_data(&self, data: &[i16], channel_count: usize, samples_per_channel: usize) -> (Vec<i16>, usize) {
        if !self.enabled {
            return (data.to_vec(), channel_count);
        }

        let indices = self.selected_indices();
        let out_ch = indices.len();
        if out_ch == 0 || out_ch == channel_count {
            return (data.to_vec(), channel_count);
        }

        let mut out = Vec::with_capacity(out_ch * samples_per_channel);
        for s in 0..samples_per_channel {
            for &ch in &indices {
                out.push(data[s * channel_count + ch]);
            }
        }
        (out, out_ch)
    }
}

// ── GUI drawing ─────────────────────────────────────────────────────

/// Draw the channel selection section in the sidebar.
pub fn draw_channel_select_section(
    ui: &mut egui::Ui,
    state: &mut ChannelSelectState,
) {
    egui::CollapsingHeader::new(
        egui::RichText::new("CHANNEL SELECTION (REC)")
            .size(11.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.checkbox(
            &mut state.enabled,
            egui::RichText::new("Save selected channels only").size(10.0),
        );

        if !state.enabled {
            ui.label(
                egui::RichText::new("All channels will be recorded")
                    .size(9.0)
                    .italics()
                    .color(theme::TEXT_DIM),
            );
            return;
        }

        // Summary
        let count = state.selected_count();
        let total = state.channel_count;
        ui.label(
            egui::RichText::new(format!("{count}/{total} channels selected"))
                .size(10.0)
                .color(if count == 0 { theme::ACCENT_RED } else { theme::ACCENT_GREEN }),
        );

        // Quick actions
        ui.horizontal(|ui| {
            if ui.small_button("All").clicked() {
                state.select_all();
            }
            if ui.small_button("None").clicked() {
                state.deselect_all();
            }
            if ui.small_button("Even").clicked() {
                for ch in 0..state.channel_count {
                    if let Some(s) = state.selected.get_mut(ch) {
                        *s = ch % 2 == 0;
                    }
                }
            }
            if ui.small_button("Odd").clicked() {
                for ch in 0..state.channel_count {
                    if let Some(s) = state.selected.get_mut(ch) {
                        *s = ch % 2 == 1;
                    }
                }
            }
        });

        // Channel grid (compact checkboxes)
        ui.add_space(2.0);
        let cols = 4;
        egui::Grid::new("ch_select_grid")
            .num_columns(cols)
            .spacing(egui::vec2(2.0, 1.0))
            .show(ui, |ui| {
                for ch in 0..state.channel_count {
                    let label = format!("{ch:2}");
                    ui.checkbox(
                        &mut state.selected[ch],
                        egui::RichText::new(label).size(9.0).monospace(),
                    );
                    if (ch + 1) % cols == 0 {
                        ui.end_row();
                    }
                }
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
    fn filter_block_data_extracts_channels() {
        let mut s = ChannelSelectState::default();
        s.enabled = true;
        s.channel_count = 4;
        s.selected = vec![true, false, true, false];
        // 2 samples × 4 channels: [s0c0, s0c1, s0c2, s0c3, s1c0, s1c1, s1c2, s1c3]
        let data: Vec<i16> = vec![10, 20, 30, 40, 50, 60, 70, 80];
        let (filtered, ch_count) = s.filter_block_data(&data, 4, 2);
        assert_eq!(ch_count, 2);
        // Should have: [s0c0, s0c2, s1c0, s1c2]
        assert_eq!(filtered, vec![10, 30, 50, 70]);
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
