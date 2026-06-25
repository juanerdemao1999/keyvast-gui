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
use crate::spike_overlay::{self, SpikeChannel, SpikeSnippetStore};
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
    /// Spike waveform overlay — threshold-triggered snippet view.
    SpikeOverlay {
        /// Selected channels, each carrying its own vertical scale.
        channels: Vec<SpikeChannel>,
        /// Per-tile grid toggle.
        show_grid: bool,
        pre_ms: f32,
        post_ms: f32,
        max_snippets: usize,
    },
    /// FFT spectrum view for a selected channel.
    FftSpectrum,
    /// Live TTL digital-logic monitor (gate input visualisation).
    TtlMonitor,
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
        Self::LfpView {
            start_ch: 0,
            visible_count,
            scroll_accum_ch: 0.0,
        }
    }

    pub fn new_ap(visible_count: usize) -> Self {
        Self::ApView {
            start_ch: 0,
            visible_count,
            scroll_accum_ch: 0.0,
        }
    }

    pub fn new_spike_overlay() -> Self {
        Self::SpikeOverlay {
            channels: Vec::new(),
            show_grid: true,
            pre_ms: 1.0,
            post_ms: 2.0,
            max_snippets: 50,
        }
    }

    pub fn new_fft() -> Self {
        Self::FftSpectrum
    }

    pub fn new_ttl_monitor() -> Self {
        Self::TtlMonitor
    }
}

// ── Add-view request (set during top_bar_right_ui, processed after tree.ui) ─

pub enum AddViewRequest {
    Lfp,
    Ap,
    SpikeOverlay,
    Fft,
    Ttl,
}

// ── Behavior ─────────────────────────────────────────────────────────

/// Context passed to the tile behavior so each pane can render with live data.
pub struct KvTileBehavior<'a> {
    // Rings
    pub disp_ring: &'a DisplayRing,
    pub disp_ring_lfp: &'a DisplayRing,
    pub disp_ring_ap: &'a DisplayRing,
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
    pub elapsed_secs: f64,
    // Perf overlay (main tile only)
    pub show_perf_overlay: bool,
    pub render_ms_ema: &'a mut f64,
    pub block_history_len: usize,
    // Spike snippet store — mutable so the tile UI can update detection params.
    pub snippet_store: &'a mut SpikeSnippetStore,
    // FFT state
    pub fft: &'a crate::fft_panel::FftState,
    // TTL gate state + history (for the TTL monitor tile)
    pub trigger: &'a crate::trigger::TriggerConfig,
    pub ttl_history: &'a crate::trigger::TtlHistory,
    // Add-view request: set by top_bar_right_ui, consumed by app.rs after tree.ui()
    pub pending_add: &'a mut Option<AddViewRequest>,
    // Source-aware subtitle for the "No Data" placeholder (e.g. "Press Start…"
    // vs. "Open a .kvraw recording…") so the empty view matches the active source.
    pub empty_hint: &'a str,
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
            } => self.draw_main_waveform(
                ui,
                *start_ch,
                scroll_accum_y,
                scroll_accum_t,
                scroll_accum_browse,
            ),

            TileKind::LfpView {
                start_ch,
                visible_count,
                scroll_accum_ch,
            } => {
                self.draw_band_view(
                    ui,
                    self.disp_ring_lfp,
                    start_ch,
                    *visible_count,
                    scroll_accum_ch,
                    "LFP",
                );
                UiResponse::None
            }

            TileKind::ApView {
                start_ch,
                visible_count,
                scroll_accum_ch,
            } => {
                self.draw_band_view(
                    ui,
                    self.disp_ring_ap,
                    start_ch,
                    *visible_count,
                    scroll_accum_ch,
                    "Spike AP",
                );
                UiResponse::None
            }

            TileKind::SpikeOverlay {
                channels,
                show_grid,
                pre_ms,
                post_ms,
                max_snippets,
            } => {
                self.draw_spike_overlay_pane(
                    ui,
                    _tile_id,
                    channels,
                    show_grid,
                    pre_ms,
                    post_ms,
                    max_snippets,
                );
                UiResponse::None
            }

            TileKind::FftSpectrum => {
                let sr = self.latest_block.map(|b| b.sample_rate).unwrap_or(30000.0);
                crate::fft_panel::draw_fft_plot(ui, self.fft, sr);
                UiResponse::None
            }

            TileKind::TtlMonitor => {
                crate::trigger::draw_ttl_monitor(ui, self.ttl_history, self.trigger);
                UiResponse::None
            }
        }
    }

    fn tab_title_for_pane(&mut self, pane: &TileKind) -> egui::WidgetText {
        match pane {
            TileKind::MainWaveform {
                start_ch,
                visible_count,
                ..
            } => format!("Waveform  CH{}–{}", start_ch, start_ch + visible_count).into(),
            TileKind::LfpView {
                start_ch,
                visible_count,
                ..
            } => format!("LFP  CH{}–{}", start_ch, start_ch + visible_count).into(),
            TileKind::ApView {
                start_ch,
                visible_count,
                ..
            } => format!("Spike AP  CH{}–{}", start_ch, start_ch + visible_count).into(),
            TileKind::SpikeOverlay { channels, .. } => {
                format!("Spike Overlay  ({} ch)", channels.len()).into()
            }
            TileKind::FftSpectrum => "FFT Spectrum".into(),
            TileKind::TtlMonitor => "TTL Monitor".into(),
        }
    }

    fn is_tab_closable(
        &self,
        _tiles: &egui_tiles::Tiles<TileKind>,
        _tile_id: egui_tiles::TileId,
    ) -> bool {
        // All tiles are closable; main waveform can also be closed (user can re-add)
        true
    }

    fn on_tab_close(
        &mut self,
        _tiles: &mut egui_tiles::Tiles<TileKind>,
        _tile_id: egui_tiles::TileId,
    ) -> bool {
        true
    }

    /// Minimum pane size — prevents tiles from being resized to an unusable sliver.
    fn min_size(&self) -> f32 {
        80.0
    }

    /// Keep the tab bar even when there is only one pane so the
    /// "+ Add View" button in `top_bar_right_ui` is always visible.
    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: true,
            ..Default::default()
        }
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
            self.add_view_menu_items(ui);
        });
    }
}

// ── Per-tile render helpers ───────────────────────────────────────────

const Y_STRIP_W: f32 = 55.0;
const X_STRIP_H: f32 = 28.0;
const SCROLL_STEP_PX: f32 = 30.0;

impl<'a> KvTileBehavior<'a> {
    /// The shared "Add View" menu items, used by both the tab-bar button and
    /// the right-click context menu on a tile (B6).
    fn add_view_menu_items(&mut self, ui: &mut egui::Ui) {
        if ui
            .button(egui::RichText::new("LFP view  (LP 250 Hz)").size(11.0))
            .clicked()
        {
            *self.pending_add = Some(AddViewRequest::Lfp);
            ui.close_menu();
        }
        if ui
            .button(egui::RichText::new("Spike view  (HP 300 Hz)").size(11.0))
            .clicked()
        {
            *self.pending_add = Some(AddViewRequest::Ap);
            ui.close_menu();
        }
        if ui
            .button(egui::RichText::new("Spike Overlay").size(11.0))
            .clicked()
        {
            *self.pending_add = Some(AddViewRequest::SpikeOverlay);
            ui.close_menu();
        }
        if ui
            .button(egui::RichText::new("FFT Spectrum").size(11.0))
            .clicked()
        {
            *self.pending_add = Some(AddViewRequest::Fft);
            ui.close_menu();
        }
        if ui
            .button(egui::RichText::new("TTL Monitor").size(11.0))
            .clicked()
        {
            *self.pending_add = Some(AddViewRequest::Ttl);
            ui.close_menu();
        }
    }

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

        // `click` is always part of the sense so the right-click "Add View"
        // context menu (B6) works in both live and paused modes; paused mode
        // additionally needs drag for browsing the buffer.
        let sense = if self.display_paused {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::click()
        };
        let scroll_response = ui.interact(tile_rect, egui::Id::new("main_waveform_drag"), sense);

        scroll_response.context_menu(|ui| {
            ui.label(
                egui::RichText::new("Add view")
                    .size(theme::FONT_CAPTION)
                    .color(theme::TEXT_DIM),
            );
            self.add_view_menu_items(ui);
        });

        let raw_scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let cursor_pos = ui.input(|i| i.pointer.hover_pos());

        // Route raw scroll into the correct accumulator (zone-aware)
        if raw_scroll.abs() > 0.5
            && let Some(pos) = cursor_pos
            && tile_rect.contains(pos)
        {
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
            self.empty_hint,
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
        if raw_scroll.abs() > 0.5
            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
            && tile_rect.contains(pos)
        {
            *scroll_accum_ch += raw_scroll;
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

        // Override visible_channels with this tile's visible_count for the
        // draw call, then restore it. Temporarily mutating the shared settings
        // avoids cloning the whole struct (two Vecs) every frame per tile.
        let prev_visible = self.display.visible_channels;
        self.display.visible_channels = visible_count;
        waveform::draw_waveform_area(
            ui,
            ring,
            self.latest_block,
            *start_ch,
            self.display,
            self.filters,
            sweep_left_ms,
            self.empty_hint,
        );
        self.display.visible_channels = prev_visible;

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

    /// Spike Overlay pane: channel selector, parameter controls, and snippet plot.
    #[allow(clippy::too_many_arguments)]
    fn draw_spike_overlay_pane(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        channels: &mut Vec<SpikeChannel>,
        show_grid: &mut bool,
        _pre_ms: &mut f32,
        _post_ms: &mut f32,
        _max_snippets: &mut usize,
    ) {
        let total_ch = self.snippet_store.channel_count();
        let tile_salt = tile_id.0 as usize;

        // ── Config strip ─────────────────────────────────────────────
        // Read current store values into locals so we can borrow snippet_store
        // mutably for controls then immutably for rendering.
        let mut sigma = self.snippet_store.sigma;
        let mut pre_ms_val = self.snippet_store.pre_ms();
        let mut post_ms_val = self.snippet_store.post_ms();
        let mut max_snips = self.snippet_store.max_snippets;

        let mut params_changed = false;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("σ").size(11.0).color(theme::TEXT_DIM))
                .on_hover_text("Detection threshold multiplier (−σ × per-channel RMS)");
            if ui
                .add(
                    egui::DragValue::new(&mut sigma)
                        .speed(0.1)
                        .range(0.5_f32..=20.0)
                        .suffix("σ"),
                )
                .changed()
            {
                params_changed = true;
            }

            ui.separator();

            ui.label(egui::RichText::new("pre").size(10.0).color(theme::TEXT_DIM));
            if ui
                .add(
                    egui::DragValue::new(&mut pre_ms_val)
                        .speed(0.05)
                        .range(0.1_f32..=5.0)
                        .suffix(" ms"),
                )
                .changed()
            {
                params_changed = true;
            }

            ui.label(
                egui::RichText::new("post")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
            if ui
                .add(
                    egui::DragValue::new(&mut post_ms_val)
                        .speed(0.05)
                        .range(0.1_f32..=10.0)
                        .suffix(" ms"),
                )
                .changed()
            {
                params_changed = true;
            }

            ui.separator();
            ui.label(egui::RichText::new("max").size(10.0).color(theme::TEXT_DIM));
            if ui
                .add(
                    egui::DragValue::new(&mut max_snips)
                        .speed(1)
                        .range(5_usize..=200),
                )
                .changed()
            {
                params_changed = true;
            }

            ui.separator();
            ui.checkbox(
                show_grid,
                egui::RichText::new("Grid")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            )
            .on_hover_text("Toggle this tile's grid");
        });

        // Apply any parameter changes back to the store.
        if params_changed {
            self.snippet_store.sigma = sigma;
            self.snippet_store.set_window_ms(pre_ms_val, post_ms_val);
            self.snippet_store.max_snippets = max_snips;
        }

        ui.separator();

        // ── Channel selector (collapsible) ───────────────────────────
        egui::CollapsingHeader::new(
            egui::RichText::new(format!("Channels  ({})", channels.len()))
                .size(10.0)
                .color(theme::TEXT_DIM),
        )
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for ch in 0..total_ch {
                    let selected = channels.iter().any(|c| c.ch == ch);
                    let label =
                        egui::RichText::new(format!("CH{ch}"))
                            .size(10.0)
                            .color(if selected {
                                theme::channel_color(ch)
                            } else {
                                theme::TEXT_DIM
                            });
                    if ui
                        .selectable_label(selected, label)
                        .on_hover_text(format!("Toggle CH{ch}"))
                        .clicked()
                    {
                        if selected {
                            channels.retain(|c| c.ch != ch);
                        } else {
                            channels.push(SpikeChannel::new(ch));
                            channels.sort_by_key(|c| c.ch);
                        }
                    }
                }
            });
        });

        // ── Per-channel Y scale ──────────────────────────────────────
        // One magnification control per selected channel — lets low-amplitude
        // channels be enlarged independently (the per-channel "Y range").
        if !channels.is_empty() {
            egui::CollapsingHeader::new(
                egui::RichText::new("Y scale  (per channel)")
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            )
            .default_open(true)
            .show(ui, |ui| {
                for sc in channels.iter_mut() {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("CH{}", sc.ch))
                                .size(10.0)
                                .color(theme::channel_color(sc.ch)),
                        );
                        ui.add(
                            egui::DragValue::new(&mut sc.y_scale)
                                .speed(0.05)
                                .range(0.1_f32..=20.0)
                                .suffix("×"),
                        )
                        .on_hover_text("Vertical magnification for this channel");
                    });
                }
            });
        }

        ui.separator();

        // ── Snippet plot ─────────────────────────────────────────────
        // Renderer needs &mut to refresh each snippet's cached geometry.
        spike_overlay::draw_spike_overlay(ui, self.snippet_store, channels, *show_grid, tile_salt);
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

/// Insert a new tile as a **split** so it is immediately visible — placed
/// side-by-side with the existing views, with no manual drag required.
///
/// Repeated adds append to the same horizontal split so panes tile evenly
/// (left-to-right) instead of nesting deeper on each add.
/// `simplification_options.all_panes_must_have_tabs` still gives every pane its
/// own tab bar (and therefore its own "＋ Add View" button), so a view can be
/// added from any pane.  Users can still drag a tab to any edge to re-arrange
/// (e.g. stack vertically) at will.
pub fn add_view_to_tree(tree: &mut egui_tiles::Tree<TileKind>, kind: TileKind) {
    let new_id = tree.tiles.insert_pane(kind);

    let root_id = match tree.root {
        Some(id) => id,
        None => {
            // Empty tree — this pane becomes the only root.
            tree.root = Some(tree.tiles.insert_tab_tile(vec![new_id]));
            return;
        }
    };

    // If the root is already a horizontal split, append to it so all added views
    // tile evenly (avoids an ever-deepening nest on each add).
    if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(lin))) =
        tree.tiles.get_mut(root_id)
        && lin.dir == egui_tiles::LinearDir::Horizontal
    {
        lin.add_child(new_id);
        return;
    }

    // Otherwise wrap the current root + new pane in a new horizontal split so
    // they appear side-by-side immediately.
    let new_root = tree.tiles.insert_horizontal_tile(vec![root_id, new_id]);
    tree.root = Some(new_root);
}
