use super::super::*;

impl KvApp {
    pub(crate) fn render_central(&mut self, ctx: &egui::Context) {
        // ── Multi-view tile canvas ──────────────────────────────
        //
        // The CentralPanel now hosts an egui_tiles Tree.  All waveform tiles
        // share the same sweep_start_ms time axis.  The tile tree is temporarily
        // taken out of self so that KvTileBehavior can hold field-level borrows
        // of self simultaneously.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_DARKEST)
                    .inner_margin(egui::Margin::symmetric(2, 2)),
            )
            .show(ctx, |ui| {
                let mut tree = self.tile_tree.take().expect("tile_tree always present");

                let elapsed_secs = self.elapsed_seconds();
                let mut pending_add: Option<AddViewRequest> = None;

                // Placeholder copy that matches the active source: in Playback the
                // user loads a file (no Start button), so "Press Start" would be
                // misleading.
                let empty_hint: &str = match self.data_source {
                    // Playback shows an actionable Open button below (#18), so
                    // the subtitle just sets context rather than pointing away.
                    DataSource::Playback => "No recording loaded yet",
                    _ => "Press Start to begin acquisition",
                };

                {
                    let mut behavior = KvTileBehavior {
                        disp_ring: &self.disp_ring,
                        disp_ring_lfp: &self.disp_ring_lfp,
                        disp_ring_ap: &self.disp_ring_ap,
                        latest_block: self.latest_block.as_ref(),
                        display: &mut self.display,
                        filters: &self.filters,
                        display_paused: self.display_paused,
                        paused_elapsed: &mut self.paused_elapsed,
                        sweep_start_ms: self.sweep_start_ms,
                        elapsed_secs,
                        show_perf_overlay: self.show_perf_overlay,
                        render_ms_ema: &mut self.render_ms_ema,
                        block_history_len: self.block_history.len(),
                        snippet_store: &mut self.snippet_store,
                        fft: &self.fft,
                        trigger: &self.trigger,
                        ttl_history: &self.ttl_history,
                        pending_add: &mut pending_add,
                        empty_hint,
                    };
                    tree.ui(&mut behavior, ui);
                }

                // Process any add-view request that came out of the tile UI.
                if let Some(req) = pending_add {
                    let visible = self.display.visible_channels;
                    let kind = match req {
                        AddViewRequest::Lfp => TileKind::new_lfp(visible),
                        AddViewRequest::Ap => TileKind::new_ap(visible),
                        AddViewRequest::SpikeOverlay => TileKind::new_spike_overlay(),
                        AddViewRequest::Fft => TileKind::new_fft(),
                        AddViewRequest::Ttl => TileKind::new_ttl_monitor(),
                    };
                    multiview::add_view_to_tree(&mut tree, kind);
                }

                self.tile_tree = Some(tree);
            });

        // ── Actionable empty-state for Playback (#18) ───────────
        // When no recording is loaded yet, drop an Open button (with an icon)
        // right on the canvas so the user doesn't have to hunt the toolbar.
        if self.data_source == DataSource::Playback && !self.playback_mgr.is_loaded() {
            let mut open_clicked = false;
            egui::Area::new(egui::Id::new("kv_playback_empty_cta"))
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 44.0))
                .interactable(true)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("\u{1F4C1}")
                                .size(34.0)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(4.0);
                        if theme::primary_button(ui, "Open .kvraw\u{2026}", true) {
                            open_clicked = true;
                        }
                    });
                });
            if open_clicked {
                self.open_playback_file_dialog();
            }
        }
    }
}
