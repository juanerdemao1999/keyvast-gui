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

### 3.3 Gap-Free Offline Playback Streaming (DA44)

**Problem**: `PlaybackManager::tick()` advanced `cursor_frame` by
`dt * sample_rate * speed` each frame, then read a single fixed block ending
at the cursor. When the playhead jumped forward by more than one display
window — high playback speed, or a long UI stall — every frame between the
previous and new cursor position was silently skipped and never streamed out.

**Solution**: Track a `read_cursor` high-water mark of frames already emitted.
Each tick drains the `[read_cursor, cursor_frame)` range contiguously, reading
at most `MAX_DISPLAY_FRAMES` (30,000) per tick and advancing `read_cursor` by
the frames actually backed by file data, so successive ticks catch up without
ever skipping inter-block samples. A seek/scrub collapses
`read_cursor = cursor_frame` so a deliberate jump is treated as a fresh static
window rather than a continuous stream to drain.

**Acceptance criteria**:
- [ ] High-speed play streams every frame in order with no gaps.
- [ ] Streaming resumes from the previous cursor, not a trailing block ending
      at the new cursor.
- [ ] A seek does not replay the skipped-over range.

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
