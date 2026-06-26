# GUI Visual Optimization Plan

Created: 2026-05-25 (session 8)
Branch: `dev`

This document tracks planned GUI visual/interaction improvements, ordered by
impact.  Each item has acceptance criteria so progress is verifiable.

---

## Round 1 — Visual Quality Core

### 1.1 Min-Max Decimation

**Problem**: Current decimation picks one sample per stride (stride-anchored
point-skipping).  When zoomed out (e.g. 20 s window at 30 kHz = 600 k
samples → 4096 display points), short transients like neural spikes are
almost certainly skipped.  The user sees flat noise with no spikes.

**Solution**: For each stride bucket, keep **two** points — the sample with
the minimum value and the sample with the maximum value — and emit them in
time order.  This guarantees that no local extrema are lost.

**Acceptance criteria**:
- [ ] Demo mode, 20 s window, HP 300 Hz: spikes are clearly visible as
      sharp deflections even when stride > 8.
- [ ] Performance: render time per frame ≤ 3 ms on 16 channels (measured via
      the F-overlay).
- [ ] Fast-path (no filter) and full-pipeline both use min-max.
- [ ] No visual flicker when scrolling.

**Reference**: Intan RHX uses min-max decimation for exactly this reason.
LTTB (Largest-Triangle-Three-Buckets) is an alternative but min-max is
simpler and sufficient for electrophysiology.

---

### 1.2 Persistent FilterChain State

**Problem**: Every frame, `collect_lines_filtered()` creates a fresh
`FilterChain` with zeroed state registers.  The leftmost ~10 ms of the
visible window shows a start-up transient (ringing), which is distracting
and incorrect once the user enables HP filtering.

**Solution**: Store one `FilterChain` per visible channel in the app state.
Feed samples to the chain in chronological order across frames.  Only reset
state when:
- The user changes filter parameters (cutoff, enable/disable).
- Acquisition restarts (history cleared).

**Acceptance criteria**:
- [ ] HP 300 Hz enabled: no visible ringing at the left edge of the window.
- [ ] Changing the HP cutoff via slider → trace smoothly transitions without
      persistent artifacts.
- [ ] Pausing and resuming does not introduce a transient.

---

## Round 2 — Interaction Polish

### 2.1 Voltage Scale Bar

**Problem**: The Y axis is in arbitrary normalized units.  The user cannot
determine actual signal amplitude from the display.

**Solution**: Draw a small vertical scale bar on the right side of the plot
(similar to Intan RHX / Open Ephys).  Label it with the current µV value
corresponding to the bar height.  The bar size should adapt to the
`amp_scale_uv` setting.

**Acceptance criteria**:
- [ ] A labeled scale bar is always visible on the waveform area.
- [ ] Changing the Amplitude combo updates the bar label.
- [ ] Bar is drawn in screen space (not affected by plot zoom/pan).

---

### 2.2 Dynamic Channel Spacing

**Problem**: Channel spacing is fixed at `CHANNEL_SPACING = 2.2`.  High-
amplitude signals overlap adjacent channels; low-amplitude signals waste
vertical space.

**Solution**: Allow the user to adjust spacing with:
- `+` / `-` keys (coarse step)
- A slider in the Display settings panel (fine control)

Spacing should be clamped between 1.0 (dense) and 6.0 (spread out).

**Acceptance criteria**:
- [ ] `+` / `-` keys visibly change lane height.
- [ ] Slider in Display section maps to channel spacing.
- [ ] Changing spacing does not cause Y-axis label jitter.

---

## Round 3 — Detail Refinements

### 3.1 Extended Color Palette (32+ channels)

**Problem**: Current palette has 16 colors.  When displaying 32 or 64
channels, colors repeat and neighboring channels become visually confusable.

**Solution**: Generate a perceptually-uniform HSL spiral with 64 entries
(hue rotation, stable saturation/lightness on dark background).  Fall back
to the handcrafted 16 colors for the first 16 channels.

**Acceptance criteria**:
- [ ] 64 channels displayed: no two adjacent channels share the same color.
- [ ] Colors remain clearly visible against `BG_DARKEST`.
- [ ] Original 16-color palette preserved for ≤ 16 ch display.

---

### 3.2 Drag-to-Browse When Paused

**Problem**: When the display is frozen (`P` key), the user cannot scroll
back to examine earlier data in the history buffer.

**Solution**: While paused, enable horizontal drag on the plot (set
`allow_drag([true, false])` on the Plot).  Constrain X panning to the
available history range `[0, paused_elapsed * 1000]`.

**Acceptance criteria**:
- [ ] Press P → display freezes at current time.
- [ ] Click-drag left → waveform scrolls back in time.
- [ ] Scrolling is clamped: cannot go past start of history or beyond the
      paused timestamp.
- [ ] Unpausing resumes live scrolling at the current acquisition time.

---

## Round 4 — Crash Resilience

### 4.1 Per-Frame Panic Isolation (DA34)

**Problem**: `eframe` drives `KvApp::update()` once per frame, and that frame
drives the live acquisition/recording pipeline directly.  A panic anywhere in
the render path — an out-of-range notch index, a non-power-of-two FFT length, a
stray `unwrap` — unwinds straight out of the event loop and the main thread
exits.  When that happens mid-experiment, the recorder thread is killed before
it can flush the `.kvraw` footer, so the in-progress recording is left
truncated and the experiment's data is lost.  The release profile also left the
panic strategy implicit, so a future `panic = "abort"` could silently defeat any
guard.

**Solution**:
- Declare `panic = "unwind"` explicitly in `[profile.release]` with a comment
  noting that the GUI guard depends on it (`abort` would bypass `catch_unwind`).
- Add a `panic_guard::guard_frame()` helper that runs one frame body inside
  `std::panic::catch_unwind`, returning `Ok(())` or `Err(message)` with a
  best-effort human-readable description of the payload.
- In the `eframe::App::update()` wrapper, run the real `render_frame()` through
  the guard.  On panic, latch `fatal_panic`, send `RecorderCmd::Terminate` so
  the recorder finalizes the active `.kvraw`, drop the live pipeline, and render
  a static recovery screen instead of re-entering the broken render path.

**Acceptance criteria**:
- [ ] Release profile declares `panic = "unwind"` explicitly.
- [ ] A panic in any panel during `update()` is caught; the process stays alive
      and shows a recovery screen rather than vanishing.
- [ ] On a caught panic while recording, `RecorderCmd::Terminate` is sent so the
      `.kvraw` footer is flushed and the file is a valid recording.
- [ ] Once latched, subsequent frames render the recovery screen without
      re-running the panicking path.

---

## Verification Commands

```bash
# Build
cargo build -p kv-gui

# Run GUI (demo auto-starts)
cargo run -p kv-gui

# Run tests (DSP + all workspace)
cargo test --workspace

# Clippy (should be 0 new warnings)
cargo clippy --workspace
```

## Completion Tracking

| Item | Status | Commit |
|------|--------|--------|
| 1.1 Min-max decimation | Pending | — |
| 1.2 Persistent filter state | Pending | — |
| 2.1 Voltage scale bar | Pending | — |
| 2.2 Dynamic channel spacing | Pending | — |
| 3.1 Extended palette | Pending | — |
| 3.2 Drag-to-browse | Pending | — |
| 4.1 Per-frame panic isolation (DA34) | Done | — |
