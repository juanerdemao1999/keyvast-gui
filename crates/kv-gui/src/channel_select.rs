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
    // Count valid indices without allocating a separate Vec.
    let valid_count = indices.iter().filter(|&&c| c < ch).count();
    let mut data = Vec::with_capacity(valid_count * spc);
    for s in 0..spc {
        for &c in indices {
            if c < ch {
                data.push(block.data[s * ch + c]);
            }
        }
    }
    SampleBlock {
        data,
        channel_count: valid_count,
        ..block.clone()
    }
}

// ── Channel filter (#9) ─────────────────────────────────────────────

/// Whether channel `ch` should be shown given the filter/jump box text.
///
/// Accepts comma-separated tokens, each either a single number (`5`) or an
/// inclusive range (`0-15`).  Empty filter shows everything.  If nothing
/// numeric parses, falls back to a substring match on the `chN` label.
fn channel_matches(ch: usize, filter: &str) -> bool {
    let f = filter.trim();
    if f.is_empty() {
        return true;
    }
    let mut any_numeric = false;
    for tok in f.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        if let Some((a, b)) = tok.split_once('-')
            && let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>())
        {
            any_numeric = true;
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            if ch >= lo && ch <= hi {
                return true;
            }
            continue;
        }
        if let Ok(n) = tok.parse::<usize>() {
            any_numeric = true;
            if ch == n {
                return true;
            }
            continue;
        }
    }
    if !any_numeric {
        return format!("ch{ch}").contains(&f.to_lowercase());
    }
    false
}

/// Draw a tiny min/max-normalized waveform of the latest block for one channel
/// (#10), giving an at-a-glance health check per row.
fn draw_row_sparkline(
    painter: &egui::Painter,
    rect: egui::Rect,
    block: &SampleBlock,
    ch: usize,
    color: egui::Color32,
) {
    let cc = block.channel_count;
    let n = block.samples_per_channel;
    if cc == 0 || n == 0 || ch >= cc {
        return;
    }
    let max_pts = 40usize;
    let stride = (n / max_pts).max(1);
    let mut samples: Vec<f32> = Vec::with_capacity(max_pts + 1);
    let mut i = 0;
    while i < n {
        samples.push(block.data[i * cc + ch] as f32);
        i += stride;
    }
    if samples.len() < 2 {
        return;
    }
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in &samples {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    let span = (hi - lo).max(1.0);
    let pad = 1.5;
    let h = rect.height() - 2.0 * pad;
    let last = (samples.len() - 1) as f32;
    let pts: Vec<egui::Pos2> = samples
        .iter()
        .enumerate()
        .map(|(k, &v)| {
            let x = rect.left() + rect.width() * (k as f32 / last);
            let t = (v - lo) / span;
            let y = rect.bottom() - pad - t * h;
            egui::pos2(x, y)
        })
        .collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0, color)));
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
        // Wrapped so the full set (All/None/Invert/Even/Odd) flows onto a second
        // line in the narrow side panel instead of clipping the last button.
        ui.add_enabled_ui(select.enabled && !rec_locked, |ui| {
            ui.horizontal_wrapped(|ui| {
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

        // Filter / jump box (#9) — quickly narrow a 64-channel list. Accepts a
        // number, a range (`0-15`) or comma-separated mix.
        let filter_id = ui.id().with("kv_chan_filter");
        let mut filter: String = ui
            .memory_mut(|m| m.data.get_temp::<String>(filter_id))
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Filter")
                    .size(theme::FONT_CAPTION)
                    .color(theme::TEXT_DIM),
            );
            ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .hint_text("e.g. 5  or  0-15")
                    .desired_width(104.0),
            );
            if !filter.is_empty()
                && ui
                    .small_button("\u{2715}")
                    .on_hover_text("Clear filter")
                    .clicked()
            {
                filter.clear();
            }
        });
        ui.memory_mut(|m| m.data.insert_temp(filter_id, filter.clone()));

        // Shared column widths so the pinned header lines up with scrolled rows.
        const W_CH: f32 = 62.0;
        const W_WAVE: f32 = 54.0;
        const W_TOG: f32 = 34.0;
        let header_cell = |ui: &mut egui::Ui, w: f32, text: &str| {
            ui.allocate_ui_with_layout(
                egui::vec2(w, 14.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new(text)
                            .size(theme::FONT_CAPTION)
                            .color(theme::TEXT_DIM),
                    );
                },
            );
        };

        // Pinned column header (#11) — stays put while the list scrolls.
        ui.horizontal(|ui| {
            header_cell(ui, W_CH, "Channel");
            header_cell(ui, W_WAVE, "Wave");
            header_cell(ui, W_TOG, "Disp");
            header_cell(ui, W_TOG, "Rec");
        });
        ui.separator();

        // Per-channel rows: [color · CHn] [sparkline] [Disp] [Rec].
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .min_scrolled_width(190.0)
            .show(ui, |ui| {
                ui.set_min_width(190.0);
                let mut shown = 0usize;
                for ch in 0..visible {
                    if !channel_matches(ch, &filter) {
                        continue;
                    }
                    shown += 1;
                    let color = theme::channel_color(ch);
                    let disp_on = display.channel_enabled[ch];
                    let label_color = if disp_on { color } else { theme::TEXT_DIM };

                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(W_CH, 16.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
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
                            },
                        );

                        ui.allocate_ui_with_layout(
                            egui::vec2(W_WAVE, 16.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(W_WAVE - 4.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                if let Some(b) = block {
                                    let spark = if disp_on { color } else { theme::TEXT_DIM };
                                    draw_row_sparkline(ui.painter(), rect, b, ch, spark);
                                }
                            },
                        );

                        ui.allocate_ui_with_layout(
                            egui::vec2(W_TOG, 16.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.checkbox(&mut display.channel_enabled[ch], "");
                            },
                        );

                        ui.allocate_ui_with_layout(
                            egui::vec2(W_TOG, 16.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add_enabled_ui(select.enabled && !rec_locked, |ui| {
                                    if select.selected.len() <= ch {
                                        select.selected.resize(ch + 1, true);
                                    }
                                    ui.checkbox(&mut select.selected[ch], "");
                                });
                            },
                        );
                    });
                }

                if shown == 0 {
                    ui.label(
                        egui::RichText::new("No channels match the filter")
                            .size(theme::FONT_CAPTION)
                            .color(theme::TEXT_DIM),
                    );
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
