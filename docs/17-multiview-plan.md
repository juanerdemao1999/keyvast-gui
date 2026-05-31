# Multi-View Display Plan

This document records the design, rationale, implementation progress, and reference sources
for the multi-window tile display system in kv-gui.

---

## Background

The current GUI uses a single `egui::CentralPanel` with one `egui_plot::Plot` showing all
channels with a single shared filter setting.  For in vivo electrophysiology research this
is insufficient: researchers routinely need to inspect the same electrode signal at different
frequency bands simultaneously (LFP 1–250 Hz, AP 300–6000 Hz) and to examine individual
spike waveforms through a threshold-triggered overlay.

Reference tools and how they handle this:

| Tool | Multi-view approach |
|------|---------------------|
| **SpikeGLX** (C++/Qt, Janelia) | Separate AP and LFP "streams" rendered in independent graphs; fixed layout with horizontal split; sweep mode (our `disp_ring` is already modelled on SpikeGLX's `WrapBuffer`) |
| **Open Ephys GUI** (C++/JUCE) | Plugin-based: `LFP Viewer` and `Spike Viewer` are separate plugins placed in a fixed layout; filters applied display-only; raw data recorded |
| **Intan RHX** (C++/Qt) | MDI-style floating windows: `Spike Scope` shows single-channel threshold snippets; amplitude scale and channel spacing are independent (already matches our design) |
| **Rust/egui ecosystem** | `egui::Window` for free-floating overlays; `egui_tiles` (the egui team's official tile-layout crate) for VS Code–style draggable split panes |

Decision: use **`egui_tiles`** for the tile canvas and **`egui::Window`** is not used for
this feature (tiles are preferable for synchronized time-axis views side by side).

---

## Design Goals

1. Startup shows only the existing main waveform tile.
2. User can add LFP / AP / Spike Overlay tiles via a **"+ Add View"** button; tiles are
   inserted into the `egui_tiles` tree and can be dragged, resized, or closed.
3. All waveform tiles share the same **time axis** (`sweep_start_ms`) — they scroll in sync.
4. Each tile has its own **start channel + visible channel count**, scrollable with the
   mouse wheel.
5. Filter presets are **fixed**: LFP = LP 250 Hz, AP = HP 300 Hz — no per-tile filter UI
   needed for these two.  The main waveform tile keeps using the existing user-configurable
   FILTERS panel.
6. Spike Overlay tile supports **any number of user-selected channels**, one row per channel,
   snippets overlaid and fading out by age.  Snippet parameters (pre ms, post ms,
   max snippets) are configurable per tile.
7. Recording and data integrity are **not affected** by display changes — all new rings are
   display-only.

---

## Architecture

### Tile Kind Enum

```rust
pub enum TileKind {
    /// Existing main waveform — uses user-configurable FILTERS settings.
    MainWaveform { start_ch: usize, visible_count: usize },

    /// Fixed LP 250 Hz low-pass — shows LFP band.
    LfpView { start_ch: usize, visible_count: usize },

    /// Fixed HP 300 Hz high-pass — shows AP / spike band.
    ApView  { start_ch: usize, visible_count: usize },

    /// Threshold-triggered snippet overlay for selected channels.
    SpikeOverlay {
        channels:     Vec<usize>,
        pre_ms:       f32,
        post_ms:      f32,
        max_snippets: usize,
    },
}
```

### Data Layer: Three Fixed DisplayRings

```
disp_ring           existing   user filter    main waveform tile
disp_ring_lfp       new        LP 250 Hz      LFP tile
disp_ring_ap        new        HP 300 Hz      AP tile + spike detection
```

All three rings are fed in `ingest_block()`.  Incremental filtering (O(new_block) per
frame) is extended to cover all three filter chains.

Memory estimate:
```
3 rings × 64 ch × 120 s × (30 000 / 4) samples/s × 4 B = 216 MB
```
This is acceptable for a desktop acquisition workstation.

### Spike Snippet Data Structure

```rust
/// One threshold-crossing waveform snippet.
pub struct SpikeSnippet {
    /// Normalised AP-filtered samples (pre_samples + post_samples total).
    pub samples: Vec<f32>,
    /// Frame-age since capture; used to compute fade alpha.
    pub age_frames: u32,
}

/// Per-channel snippet accumulator.
pub struct ChannelSnippetBuf {
    pub snippets:             VecDeque<SpikeSnippet>,
    /// Remaining samples of refractory silence after a detection.
    refractory_remaining:     usize,
    pub max_snippets:         usize,
    pub pre_samples:          usize,
    pub post_samples:         usize,
    /// Threshold in normalised units (negative-going).
    pub threshold:            f32,
    /// Partial snippet being assembled across block boundaries.
    pending:                  Option<PendingSnippet>,
}
```

Detection runs in `ingest_block()` on the AP ring data.  `age_frames` is incremented each
render frame; snippets older than `SNIPPET_FADE_FRAMES` are discarded.

### Rendering Dispatch

```
egui_tiles Behavior::ui()
  ├── TileKind::MainWaveform  → waveform::draw_waveform_area(&disp_ring,     ...)
  ├── TileKind::LfpView       → waveform::draw_waveform_area(&disp_ring_lfp, ...)
  ├── TileKind::ApView        → waveform::draw_waveform_area(&disp_ring_ap,  ...)
  └── TileKind::SpikeOverlay  → spike_overlay::draw_spike_overlay(stores, tile)
```

`draw_waveform_area()` already accepts `ring: &DisplayRing` — the three waveform tiles
reuse it without modification beyond a `start_ch` offset parameter.

### Time Synchronisation

All tiles read `sweep_start_ms` from `KvApp`.  There is no per-tile time state for
waveform tiles.  Spike Overlay is time-independent (shows recent snippets regardless of
current sweep position).

---

## Implementation Phases

### Phase 1 — Tile skeleton + LFP/AP rings  (target: 2 days)

**Scope:**
- Add `egui_tiles = "0.12"` dependency (`0.12.0` binds `egui 0.31.1` — verified compatible with `eframe 0.31`).
- Add `disp_ring_lfp`, `disp_ring_ap` and matching `filter_chains_lfp`,
  `filter_chains_ap` to `KvApp`.
- `ingest_block()` pushes to all three rings.
- New module `multiview.rs`: `TileKind` enum + `KvTileBehavior` implementing
  `egui_tiles::Behavior`.
- Replace `egui::CentralPanel` render block in `app.rs` with
  `self.tile_tree.ui(&mut behavior, ui)`.
- Startup tree: single `TileKind::MainWaveform` node (no change to UX yet).
- Add **"+ Add View"** dropdown button to toolbar; clicking inserts a new tile.
- Per-tile channel scroll: mouse wheel over a tile adjusts `start_ch`.

**Acceptance criteria:**
- `cargo build --bin kv-gui` succeeds.
- All 84 tests pass.
- Existing main waveform display is pixel-identical to before.
- LFP and AP tiles can be added and show plausible filtered data.
- Tiles can be dragged and resized.

**Status:** not started

---

### Phase 2 — Spike Overlay tile  (target: 2 days)

**Scope:**
- New file `crates/kv-gui/src/spike_overlay.rs`.
- `ChannelSnippetBuf` struct + threshold crossing detection in `ingest_block()`.
- `draw_spike_overlay()`: one row per selected channel, snippets overlaid,
  alpha = `1.0 - age_frames / FADE_FRAMES`.
- Tile-internal config widgets: channel list checkboxes, pre/post sliders, max-count
  spinbox, threshold sigma input.
- Wire into Phase 1 tile dispatch.

**Acceptance criteria:**
- Spike Overlay tile can be added from the "+ Add View" menu.
- Channels can be checked/unchecked inside the tile.
- Snippets accumulate and fade correctly.
- No measurable FPS drop vs baseline.

**Status:** not started

---

### Phase 3 — Polish  (target: 0.5 day)

- Tile title bar: show type label + channel range + close button.
- Minimum tile size guard (prevent tiles collapsing to < 80 px height).
- Persist tile layout to `~/.config/keyvast/layout.json` (optional; only if
  config persistence is implemented first).

**Status:** not started

---

## Reference Projects

### Rust / egui

| Project | Relevance | Notes |
|---------|-----------|-------|
| **egui_tiles** (emilk) | Direct dependency | `egui_tiles::Tree<Pane>` + `Behavior` trait; `SimplificationOptions` controls auto-merge; see `demo/` in the repo |
| **eframe demos** | egui::Window patterns | Shows how to open/close/reuse floating windows alongside panels |
| **Rerun.io** (rerun-rs) | Production egui app | Uses egui_tiles for its multi-panel layout; open source, good reference for large egui apps |

### Electrophysiology

| Project | What to borrow |
|---------|---------------|
| **SpikeGLX** (C++/Qt) | Dual-stream (AP + LFP) ring buffer architecture; sweep mode cursor; per-channel gain/colour rendering |
| **Intan RHX** (C++/Qt) | Spike Scope snippet overlay design; gain/spacing independence (already implemented) |
| **Open Ephys GUI** (C++/JUCE) | Plugin system idea for future extensibility |
| **phy** (Python/vispy) | Spike waveform overlay rendering patterns; template view colour coding |

### Key URLs

- egui_tiles docs: https://docs.rs/egui_tiles
- egui_tiles demo: https://www.egui.rs/#tiles
- SpikeGLX source: https://github.com/billkarsh/SpikeGLX  (CimAcq, MGraph, ShankMap)
- Intan RHX source: https://github.com/Intan-Technologies/IntanRHX  (spikescopedialog.cpp)
- Rerun source: https://github.com/rerun-io/rerun  (re_viewer crate)

---

## Progress Log

| Date | Phase | What was done | Commit |
|------|-------|---------------|--------|
| 2026-05-28 | Planning | Created this document; design finalised | (pending) |
| 2026-05-28 | Phase 1 | Verified `egui_tiles 0.12.0` + `eframe 0.31` compatibility; added dependency | cba005b |
| 2026-05-28 | Phase 1 | Implemented: 3× DisplayRing, LFP/AP FilterChains, `multiview.rs` (TileKind + KvTileBehavior), egui_tiles CentralPanel replacement, "+ Add View" menu, per-tile channel scroll; all 84 tests pass | a29a654 |
| 2026-05-28 | Phase 2 | Implemented: `spike_overlay.rs` (SpikeSnippetStore, per-channel ChannelBuf, threshold detection, snippet accumulation, fade-out renderer); SpikeOverlay pane_ui (channel selector + param display + egui_plot rendering); all tests pass | bc0c260 |
| 2026-05-28 | Phase 3 | Interactive σ/pre/post/max controls; min_size(80px) guard; preview.rs dead code removed (PreviewState/start_preview/PreviewHandle → deleted, BlockStats/ChannelStats kept); all tests pass | (pending) |

---

## Open Questions

1. ~~Does `egui_tiles 0.10` compile cleanly against `eframe 0.31` / `egui 0.31`?~~
   → **Resolved**: `egui_tiles 0.12.0` binds `egui 0.31.1`; compiles cleanly.
   `0.10` binds egui 0.29, `0.15` binds egui 0.34 — `0.12` is the correct match.

2. Should the LFP and AP filter cutoffs (250 Hz / 300 Hz) be user-adjustable in a later
   version, or stay hard-coded?
   → Currently hard-coded as per user decision.  Revisit after first working version.

3. Should the per-tile channel scroll position be synchronised across tiles of the same
   type, or always independent?
   → Currently: always independent (simpler).

4. Refractory period for spike detection: 1 ms (30 samples) default?
   → Use same value as the existing spike threshold detection in `waveform.rs`.
