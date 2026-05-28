//! Multi-view tile layout for kv-gui.
//!
//! Uses `egui_tiles` to provide a VS Code-style draggable, resizable tile canvas.
//!
//! ## Tile types
//!
//! | TileKind           | Ring used     | Filter     |
//! |--------------------|---------------|------------|
//! | `MainWaveform`     | `disp_ring`   | User-defined (FILTERS panel) |
//! | `LfpView`          | `disp_ring_lfp` | Fixed LP 250 Hz |
//! | `ApView`           | `disp_ring_ap`  | Fixed HP 300 Hz |
//! | `SpikeOverlay`     | —             | Phase 2    |
//!
//! ## Architecture notes
//!
//! - `KvApp` holds `tile_tree: Option<egui_tiles::Tree<TileKind>>`.
//! - In `KvApp::update()` the tree is temporarily taken out with `Option::take()`,
//!   a `KvTileBehavior` is constructed with references to the remaining `KvApp` fields,
//!   `tree.ui(&mut behavior, ui)` is called, and the tree is put back.
//! - All tiles share the same `sweep_start_ms` time axis.
//! - Channel scrolling (mouse wheel) is per-tile and changes `start_ch` inside the pane.

use eframe::egui;
use egui_tiles::UiResponse;

use crate::disp_ring::DisplayRing;
use crate::panels::{AMP_SCALES, DisplaySettings, FilterSettings, TIME_WINDOWS};
use crate::theme;
use crate::waveform;
use kv_types::SampleBlock;

// ── Tile kind ────────────────────────────────────────────────────────

/// The kind of content displayed in a tile pane.
pub enum TileKind {
    /// Main waveform tile using the user-configured filter ring.
    MainWaveform {
        start_ch: usize,
        visible_count: usize,
        /// Per-tile scroll accumulators (Y-amp, X-time, browse).
        scroll_accum_y: f32,
        scroll_accum_t: f32,
        scroll_accum_browse: f32,
    },
    /// LFP band view (fixed LP 250 Hz).
    LfpView {
        start_ch: usize,
        visible_count: usize,
        scroll_accum_ch: f32,
    },
    /// AP / spike band view (fixed HP 300 Hz).
    ApView {
        start_ch: usize,
        visible_count: usize,
        scroll_accum_ch: f32,
    },
    /// Spike waveform overlay (Phase 2 — placeholder for now).
    SpikeOverlay {
        channels: Vec<usize>,
        pre_ms: f32,
        post_ms: f32,
        max_snippets: usize,
    },
}

impl TileKind {
    pub fn new_main(visible_count: usize) -> Self {
        Self::MainWaveform {
            start_ch: 0,
            visible_count,
            scroll_accum_y: 0.0,
            scroll_accum_t: 0.0,
            scroll_accum_browse: 0.0,
        }
    }

    pub fn new_lfp(visible_count: usize) -> Self {
        Self::LfpView { start_ch: 0, visible_count, scroll_accum_ch: 0.0 }
    }

    pub fn new_ap(visible_count: usize) -> Self {
        Self::ApView { start_ch: 0, visible_count, scroll_accum_ch: 0.0 }
    }

    pub fn new_spike_overlay() -> Self {
        Self::SpikeOverlay {
            channels: Vec::new(),
            pre_ms: 1.0,
            post_ms: 2.0,
            max_snippets: 50,
        }
    }
}

// ── Add-view request (set during top_bar_right_ui, processed after tree.ui) ─

pub enum AddViewRequest {
    Lfp,
    Ap,
    SpikeOverlay,
}

// ── Behavior ─────────────────────────────────────────────────────────

/// Context passed to the tile behavior so each pane can render with live data.
pub struct KvTileBehavior<'a> {
    // Rings
    pub disp_ring:     &'a DisplayRing,
    pub disp_ring_lfp: &'a DisplayRing,
    pub disp_ring_ap:  &'a DisplayRing,
    // Latest block (channel count, sample rate)
    pub latest_block: Option<&'a SampleBlock>,
    // Shared display settings (amp scale, time window) — modified by main tile scroll
    pub display: &'a mut DisplaySettings,
    // Filter settings (used by main tile only)
    pub filters: &'a FilterSettings,
    // Sweep / time state
    pub display_paused: bool,
    pub paused_elapsed: &'a mut f64,
    pub sweep_start_ms: f64,
    pub elapsed_secs:   f64,
    // Perf overlay (main tile only)
    pub show_perf_overlay: bool,
    pub render_ms_ema:     &'a mut f64,
    pub block_history_len: usize,
    // Add-view request: set by top_bar_right_ui, consumed by app.rs after tree.ui()
    pub pending_add: &'a mut Option<AddViewRequest>,
}

impl<'a> egui_tiles::Behavior<TileKind> for KvTileBehavior<'a> {
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut TileKind,
    ) -> UiResponse {
        match pane {
            TileKind::MainWaveform {
                start_ch,
                visible_count: _,
                scroll_accum_y,
                scroll_accum_t,
                scroll_accum_browse,
            } => self.draw_main_waveform(ui, *start_ch, scroll_accum_y, scroll_accum_t, scroll_accum_browse),

            TileKind::LfpView { start_ch, visible_count, scroll_accum_ch } => {
                self.draw_band_view(ui, self.disp_ring_lfp, start_ch, *visible_count, scroll_accum_ch, "LFP");
                UiResponse::None
            }

            TileKind::ApView { start_ch, visible_count, scroll_accum_ch } => {
                self.draw_band_view(ui, self.disp_ring_ap, start_ch, *visible_count, scroll_accum_ch, "Spike AP");
                UiResponse::None
            }

            TileKind::SpikeOverlay { .. } => {
                // Phase 2 placeholder
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new("Spike Overlay — coming in Phase 2")
                            .size(13.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                UiResponse::None
            }
        }
    }

    fn tab_title_for_pane(&mut self, pane: &TileKind) -> egui::WidgetText {
        match pane {
            TileKind::MainWaveform { start_ch, visible_count, .. } =>
                format!("Waveform  CH{}–{}", start_ch, start_ch + visible_count).into(),
            TileKind::LfpView { start_ch, visible_count, .. } =>
                format!("LFP  CH{}–{}", start_ch, start_ch + visible_count).into(),
            TileKind::ApView { start_ch, visible_count, .. } =>
                format!("Spike AP  CH{}–{}", start_ch, start_ch + visible_count).into(),
            TileKind::SpikeOverlay { channels, .. } =>
                format!("Spike Overlay  ({} ch)", channels.len()).into(),
        }
    }

    fn is_tab_closable(&self, _tiles: &egui_tiles::Tiles<TileKind>, _tile_id: egui_tiles::TileId) -> bool {
        // All tiles are closable; main waveform can also be closed (user can re-add)
        true
    }

    fn on_tab_close(&mut self, _tiles: &mut egui_tiles::Tiles<TileKind>, _tile_id: egui_tiles::TileId) -> bool {
        true
    }

    /// "+ Add View" button in the tab bar right area.
    fn top_bar_right_ui(
        &mut self,
        _tiles: &egui_tiles::Tiles<TileKind>,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        _tabs: &egui_tiles::Tabs,
        _scroll_offset: &mut f32,
    ) {
        ui.add_space(4.0);
        ui.menu_button(egui::RichText::new("＋ Add View").size(11.0), |ui| {
            if ui.button(egui::RichText::new("LFP view  (LP 250 Hz)").size(11.0)).clicked() {
                *self.pending_add = Some(AddViewRequest::Lfp);
                ui.close_menu();
            }
            if ui.button(egui::RichText::new("Spike view  (HP 300 Hz)").size(11.0)).clicked() {
                *self.pending_add = Some(AddViewRequest::Ap);
                ui.close_menu();
            }
            if ui.button(egui::RichText::new("Spike Overlay").size(11.0)).clicked() {
                *self.pending_add = Some(AddViewRequest::SpikeOverlay);
                ui.close_menu();
            }
        });
    }
}

// ── Per-tile render helpers ───────────────────────────────────────────

const Y_STRIP_W: f32 = 55.0;
const X_STRIP_H: f32 = 28.0;
const SCROLL_STEP_PX: f32 = 30.0;

impl<'a> KvTileBehavior<'a> {
    /// Main waveform tile: zone-aware scroll + full waveform rendering.
    fn draw_main_waveform(
        &mut self,
        ui: &mut egui::Ui,
        start_ch: usize,
        scroll_accum_y: &mut f32,
        scroll_accum_t: &mut f32,
        scroll_accum_browse: &mut f32,
    ) -> UiResponse {
        let tile_rect = ui.max_rect();

        let sense = if self.display_paused {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::hover()
        };
        let scroll_response =
            ui.interact(tile_rect, egui::Id::new("main_waveform_drag"), sense);

        let raw_scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let cursor_pos = ui.input(|i| i.pointer.hover_pos());

        // Route raw scroll into the correct accumulator (zone-aware)
        if raw_scroll.abs() > 0.5 {
            if let Some(pos) = cursor_pos {
                if tile_rect.contains(pos) {
                    let in_y_strip = pos.x < tile_rect.left() + Y_STRIP_W;
                    let in_x_strip = pos.y > tile_rect.bottom() - X_STRIP_H;
                    if in_y_strip && !in_x_strip {
                        *scroll_accum_y += raw_scroll;
                    } else if in_x_strip {
                        *scroll_accum_t += raw_scroll;
                    } else if self.display_paused {
                        *scroll_accum_browse += raw_scroll;
                    }
                }
            }
        }

        // Y-axis accumulator → amplitude scale
        {
            let max_idx = AMP_SCALES.len() - 1;
            while *scroll_accum_y >= SCROLL_STEP_PX {
                *scroll_accum_y -= SCROLL_STEP_PX;
                self.display.amp_scale_idx = self.display.amp_scale_idx.saturating_sub(1);
            }
            while *scroll_accum_y <= -SCROLL_STEP_PX {
                *scroll_accum_y += SCROLL_STEP_PX;
                self.display.amp_scale_idx = (self.display.amp_scale_idx + 1).min(max_idx);
            }
        }

        // X-axis accumulator → time window
        {
            let max_idx = TIME_WINDOWS.len() - 1;
            while *scroll_accum_t >= SCROLL_STEP_PX {
                *scroll_accum_t -= SCROLL_STEP_PX;
                self.display.time_scale_idx = self.display.time_scale_idx.saturating_sub(1);
            }
            while *scroll_accum_t <= -SCROLL_STEP_PX {
                *scroll_accum_t += SCROLL_STEP_PX;
                self.display.time_scale_idx = (self.display.time_scale_idx + 1).min(max_idx);
            }
        }

        // Browse accumulator → paused time position
        if self.display_paused {
            let window_ms = self.display.time_window_ms();
            let step_s = window_ms * (self.display.browse_step_pct / 100.0) / 1000.0;
            let live = self.elapsed_secs;
            while *scroll_accum_browse >= SCROLL_STEP_PX {
                *scroll_accum_browse -= SCROLL_STEP_PX;
                *self.paused_elapsed = (*self.paused_elapsed + step_s).min(live);
            }
            while *scroll_accum_browse <= -SCROLL_STEP_PX {
                *scroll_accum_browse += SCROLL_STEP_PX;
                *self.paused_elapsed = (*self.paused_elapsed - step_s).max(0.0);
            }
        } else {
            *scroll_accum_browse = 0.0;
        }

        // Drag-to-browse when paused
        if self.display_paused && scroll_response.dragged() {
            let drag_px = scroll_response.drag_delta().x;
            let plot_width = tile_rect.width().max(1.0);
            let time_window_ms = self.display.time_window_ms();
            let dt_ms = (drag_px as f64 / plot_width as f64) * time_window_ms;
            *self.paused_elapsed = (*self.paused_elapsed - dt_ms / 1000.0).max(0.0);
            let live = self.elapsed_secs;
            if *self.paused_elapsed > live {
                *self.paused_elapsed = live;
            }
        }

        // Compute sweep / paused left edge
        let sweep_left_ms = if self.display_paused {
            let window_ms = self.display.time_window_ms();
            (*self.paused_elapsed * 1000.0 - window_ms).max(0.0)
        } else {
            self.sweep_start_ms
        };

        let render_start = std::time::Instant::now();
        waveform::draw_waveform_area(
            ui,
            self.disp_ring,
            self.latest_block,
            start_ch,
            self.display,
            self.filters,
            sweep_left_ms,
        );
        let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
        *self.render_ms_ema = *self.render_ms_ema * 0.9 + render_ms * 0.1;

        // Paused overlay
        if self.display_paused {
            let painter = ui.painter();
            painter.text(
                tile_rect.center_top() + egui::vec2(0.0, 18.0),
                egui::Align2::CENTER_CENTER,
                "  PAUSED  (press P to resume)  ",
                egui::FontId::proportional(13.0),
                theme::ACCENT_YELLOW,
            );
        }

        // Performance overlay
        if self.show_perf_overlay {
            draw_perf_overlay_in_rect(ui, tile_rect, self.render_ms_ema, self.block_history_len);
        }

        UiResponse::None
    }

    /// LFP / AP band view: simpler rendering — no zone-aware scroll, channel scroll only.
    fn draw_band_view(
        &mut self,
        ui: &mut egui::Ui,
        ring: &DisplayRing,
        start_ch: &mut usize,
        visible_count: usize,
        scroll_accum_ch: &mut f32,
        label: &str,
    ) {
        let tile_rect = ui.max_rect();
        let total_ch = self.latest_block.map(|b| b.channel_count).unwrap_or(64);

        // Mouse wheel scrolls channels in this tile
        let raw_scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if raw_scroll.abs() > 0.5 {
            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                if tile_rect.contains(pos) {
                    *scroll_accum_ch += raw_scroll;
                }
            }
        }
        while *scroll_accum_ch >= SCROLL_STEP_PX {
            *scroll_accum_ch -= SCROLL_STEP_PX;
            *start_ch = start_ch.saturating_sub(1);
        }
        while *scroll_accum_ch <= -SCROLL_STEP_PX {
            *scroll_accum_ch += SCROLL_STEP_PX;
            let max_start = total_ch.saturating_sub(visible_count);
            *start_ch = (*start_ch + 1).min(max_start);
        }

        let sweep_left_ms = if self.display_paused {
            let window_ms = self.display.time_window_ms();
            (*self.paused_elapsed * 1000.0 - window_ms).max(0.0)
        } else {
            self.sweep_start_ms
        };

        // Use a temporary display settings that overrides visible_channels with
        // this tile's visible_count so draw_waveform_area renders the right number.
        let mut tile_display = self.display.clone();
        tile_display.visible_channels = visible_count;

        waveform::draw_waveform_area(
            ui,
            ring,
            self.latest_block,
            *start_ch,
            &tile_display,
            self.filters,
            sweep_left_ms,
        );

        // Tile type badge (top-left corner)
        let painter = ui.painter();
        painter.text(
            tile_rect.left_top() + egui::vec2(6.0, 6.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
    }
}

// ── Perf overlay helper (standalone fn, no self) ─────────────────────

fn draw_perf_overlay_in_rect(
    ui: &egui::Ui,
    rect: egui::Rect,
    render_ms_ema: &f64,
    history_blocks: usize,
) {
    let painter = ui.painter();
    let pos = rect.right_top() + egui::vec2(-8.0, 8.0);
    let lines = [
        format!("Render {:>5.1} ms", render_ms_ema),
        format!("Hist   {:>5} blk", history_blocks),
    ];
    let mut y = 0.0f32;
    for line in &lines {
        painter.text(
            pos + egui::vec2(0.0, y),
            egui::Align2::RIGHT_TOP,
            line,
            egui::FontId::monospace(10.0),
            theme::TEXT_DIM,
        );
        y += 14.0;
    }
}

// ── Tree construction helper ─────────────────────────────────────────

/// Create the initial tree with a single main waveform pane.
pub fn make_initial_tree(visible_channels: usize) -> egui_tiles::Tree<TileKind> {
    let mut tiles = egui_tiles::Tiles::default();
    let main_id = tiles.insert_pane(TileKind::new_main(visible_channels));
    let root = tiles.insert_tab_tile(vec![main_id]);
    egui_tiles::Tree::new("kv_tiles", root, tiles)
}

/// Insert a new tile into the tree, adding it as a new tab in the root container.
/// Falls back to creating a horizontal split if the root is not a tab container.
pub fn add_view_to_tree(tree: &mut egui_tiles::Tree<TileKind>, kind: TileKind) {
    let new_id = tree.tiles.insert_pane(kind);
    let root_id = match tree.root() {
        Some(id) => id,
        None => {
            // Empty tree — set this pane as the only root
            let _ = tree.tiles.insert_tab_tile(vec![new_id]);
            return;
        }
    };

    // Try to insert as a new tab in the root tabs container
    if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(tabs))) =
        tree.tiles.get_mut(root_id)
    {
        tabs.add_child(new_id);
        tabs.set_active(new_id);
    } else {
        // Root is not a tab container — wrap both in a new horizontal split
        let new_root = tree.tiles.insert_horizontal_tile(vec![root_id, new_id]);
        // Replace the root — egui_tiles Tree doesn't expose set_root directly,
        // so we rebuild the tree with the same tiles but a new root.
        let tiles = std::mem::take(&mut tree.tiles);
        *tree = egui_tiles::Tree::new("kv_tiles", new_root, tiles);
    }
}
