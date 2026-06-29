# Confirmed Decisions

This document records current project decisions confirmed by the user. If a later decision changes, update this file in the same change.

## Project

```text
Project folder name: 51_keyvast_gui
First target OS: Windows
Primary development language: Rust
First GUI direction: Rust native engineering GUI
Initial GUI candidate: egui
First product focus: in vivo electrophysiology
Sleep EEG/EMG product line: not first priority
```

## First MVP

```text
Channels: 64
Sample rate: 30 kHz
Sample value type: i16
Data layout: interleaved_by_sample
Recording format: kvraw
TTL lines: 16
```

## Hardware Direction

```text
Future connector: USB Type-C
Future transport: USB-based data transfer
First hardware module: Opal Kelly XEM7310-A75
First hardware protocol: Opal Kelly FrontPanel / Intan Rhythm USB3-style endpoints
First hardware bit file: keyvast_260607_with_UART.bit (provide the path at runtime via --bitfile / the GUI picker)
  Canonical candidate order lives in code: kv_rhd::RHD_BITFILE_CANDIDATES
  [keyvast_combined_download.bit, keyvast_260607_with_UART.bit, intan_rec_controller_7310.bit]
Host program should bundle the required FrontPanel runtime DLL for convenience
First live hardware channel target: up to two 32-channel RHD headstages
Register map: use Rhythm USB3 / FrontPanel endpoints unless the Keyvast bitfile changes them
Packet format: Rhythm USB3 data frames unless the Keyvast bitfile changes them
CRC algorithm: TBD
Timestamp clock: Rhythm USB3 32-bit sample timestamp for first hardware bring-up
ADC gain conversion: follow Open Ephys / Intan RHD convention for display, while preserving raw data
```

## Verification Ladder

Use this order:

```text
10-second smoke test
10-minute recorder test
2-hour endurance test
```

## API Direction

The user does not need to decide the Python / MATLAB integration mode now.

First phase:

```text
CLI + kvraw + metadata + events + integrity report
```

Later phase:

```text
kv-daemon local API
Python client
MATLAB client
Web GUI or external tools
```

## Rust Workspace Decision

Use a Rust workspace unless implementation reveals a strong reason not to.

Plain meaning: one project folder contains multiple smaller Rust packages, such as:

```text
crates/kv-types
crates/kv-simulator
crates/kv-core
crates/kv-cli
crates/kv-gui
```

This lets each part stay small, while still building as one project.

The folder can stay named `51_keyvast_gui`. Rust crate names should use normal package names such as `kv-types`, `kv-core`, and `kv-cli`.

## Build & Deployment Hardening

These are properties of the shipped (release) build, not just debug tests.

### Overflow checks in release (DA15)

`[profile.release]` sets `overflow-checks = true`. Field/delivery builds are
always release (the GUI alias runs `--release`), so without this an integer
wrap in register-bit, byte-offset/seek, or timestamp math would silently
corrupt data in vivo while debug tests panic — the classic "tested fine,
exploded on site". The real-time cost on hot paths is negligible.

Consequence for code: any arithmetic that is *meant* to wrap (e.g. a packet-id
or sample-timestamp counter rolling over) must use explicit `wrapping_*`;
counters that must not exceed a bound use `checked_*` / `saturating_*`. A plain
`+`/`*`/`<<` that overflows now panics observably instead of producing bad
data.

### FrontPanel DLL dependency resolution (DA33)

The Opal Kelly `okFrontPanel.dll` is loaded by absolute path, but it has its
own transitive dependencies (the Visual C++ runtime and Opal Kelly helper
DLLs). Plain `LoadLibrary` only searches the standard path, so a fresh
bring-up machine missing those runtimes fails to load with an opaque error.

The loader resolves the DLL to a fully qualified path and loads it with
`LOAD_WITH_ALTERED_SEARCH_PATH`, which puts the DLL's own directory at the
front of the dependency search order. Deployments should still bundle the
required FrontPanel/VC++ runtime alongside `okFrontPanel.dll` (see
"Host program should bundle the required FrontPanel runtime DLL" above) so the
dependencies are present in that directory.

## Sample-rate handling

### The programmed rate is the recorded rate (DA9)

The configured `sample_rate` is threaded all the way to the hardware: board
bring-up calls `set_sample_rate(config.sample_rate)` (the PLL M/D step table,
1000–30000 Hz) instead of a hardcoded 30 kHz, and the per-chip register set is
built with `Rhd2000Registers::new(sample_rate)` so MUX/ADC bias and the DSP
high-pass cutoff match the rate actually running. A rate outside the PLL step
table is rejected at configure time with `RhdReadError::UnsupportedSampleRate`
rather than silently falling back to 30 kHz.

Because the hardware now runs exactly the configured rate, the `sample_rate`
stamped into each `SampleBlock` and the `.kvraw` metadata is the true
acquisition rate — there is no longer a path where the file claims a rate the
ADC never ran at. The `rhd-smoke` command exposes `--sample-rate <hz>`
(default 30000) as the user path to select it; non-finite or non-positive
values are rejected during argument parsing.

### Cable-delay timing tracks the configured rate (DA40)

`set_cable_length_meters` computes the MISO sampling delay from
`t_step = 1 / (2800 * sample_rate)`. The per-channel SPI clock scales with the
sample rate, so this now uses the configured rate passed in from `configure`
rather than the `DEFAULT_RHD_SAMPLE_RATE` constant; otherwise the headstage
cable delay would be mis-compensated at any rate other than 30 kHz, degrading
the MISO sampling phase.

## Recording disk-space guard (DA18)

Free space used to be display-only (the headroom line in the recording panel):
nothing enforced it, so an unattended overnight/behavioral session (~3.7 MB/s at
30 kHz × 64 ch ≈ 13 GB/h) could silently fill the volume and truncate the
in-progress `.kvraw` as an opaque write error — a worst-case loss of hours of
non-reproducible animal data. The guard now actively enforces headroom.

The policy lives in `kv-gui/src/diskspace.rs` as pure, unit-tested functions
over plain byte counts, with the GUI supplying the live free-space query and the
toast/auto-stop side effects:

- **Pre-flight (`evaluate_start`)** — `begin_recording` queries free space
  before opening the file and refuses to start below
  `RECORDING_MIN_START_FREE_BYTES` (2 GB), surfacing an error toast and dropping
  an `Armed`/triggered start back to `Idle` instead of beginning a recording
  that is already doomed. An unqueryable volume (non-Windows, or a failed query)
  is *not* blocked — the existing write-error path still covers a genuinely full
  disk.
- **In-progress polling (`evaluate_recording`)** — while recording, the update
  loop samples free space every `DISK_CHECK_INTERVAL` (2 s). Below
  `RECORDING_WARN_FREE_BYTES` (5 GB) it warns via a rate-limited toast (one per
  `DISK_WARN_INTERVAL`, 20 s); at or below `RECORDING_STOP_FREE_BYTES` (1 GB) it
  triggers a **clean** auto-stop through the normal `stop_recording` path, so the
  recorder finalizes the file (metadata + flush) rather than leaving a truncated
  one. The 5 GB → 1 GB band gives ~18 min of warning at the 13 GB/h reference
  rate.

Thresholds are decimal GB to match the headroom indicator, and the `None`
(unqueryable) case is treated as healthy so an unmonitorable volume never forces
a spurious stop.

## Selective-save channel scope (DA20)

The unified CHANNELS panel exposes a per-channel **Rec** toggle that drives the
recording subset (`ChannelSelectState::recording_selection`). It previously
rendered Rec rows only for the on-screen window
(`visible = visible_channels.min(channel_count)`), but `selected` is sized to
the full `channel_count` and defaults to `true`. With the default 16-wide
window on a 64-channel headstage, enabling "Record subset only" and ticking just
CH0–3 still wrote CH16–63 to disk, while the summary `Record n/visible` clamped
the count with `.min(visible)` and hid the surplus — *what you see was not what
you saved*, and every offline channel→site mapping on the subset file was off.

`visible_channels` controls how many lanes the **waveform** draws; it is not a
recording scope. The two are now decoupled: the CHANNELS panel lists **every
acquired channel** (`0..channel_count`) for both the Disp and Rec columns, so
the recording subset is fully controllable, and the counts summary reports the
true totals over `channel_count` with no `.min(visible)` masking. The scroll
area + filter box keep the full list manageable, and `is_channel_enabled`
already tolerates out-of-range indices, so widening the display-enable vector to
`channel_count` is safe.

## Common-average reference excludes railed channels (DA22)

The display CAR (`filter_block_with_chains`, enabled via `filters.car_enabled`)
subtracted, per time step, the mean of **all** channels and stored the result
with `(sample - mean) as i16` — a plain truncation toward zero. Two problems:

- **No bad-channel exclusion.** A single saturated/dead electrode pinned to the
  ADC rail (≈±32767) dominates the mean; CAR then injects that channel's
  inverted artifact (scaled `1/N`) into every other channel, so a clean array is
  displayed as uniformly noisy and the operator chases a non-existent fault or
  discards the session.
- **Truncation bias.** `as i16` rounds toward zero every sample, a small
  systematic DC bias on top of the reference subtraction.

The reference mean is now computed by `car_reference_mean`, which accumulates in
f64 and **skips channels whose `|sample| >= CAR_RAIL_EXCLUDE_I16` (32 700,
≈0.998 full scale)**. If every channel is railed it falls back to the full mean
so the reference is never undefined. The per-sample subtraction now `.round()`s
once before clamping to the `i16` range instead of truncating.

Scope note: this excludes *saturated* channels automatically. A user-selectable
reference subset (e.g. restrict CAR to a chosen group, exclude display-disabled
channels) remains future work; the rail exclusion already removes the dominant
in-vivo failure mode (one dead electrode poisoning the whole array).

## Saturation / railing indicator (DA21)

`finalize_channel` removes each channel's window mean before applying gain. A
channel pinned to the ADC rail (a saturated amp or dead/floating electrode sits
at ≈±1.0 normalized, i.e. ±32767) therefore drops to ≈0 after mean subtraction
and renders as a flat line on the baseline — visually identical to a quiet,
healthy channel. The single most valuable bring-up check (spotting saturation,
floating grounds, and bad electrodes) was defeated, and the display actively
disguised the fault.

`collect_from_ring` now runs `lane_is_saturated` on the **raw normalized points,
before** DC removal: a lane is flagged when at least `SAT_FRACTION` (0.5) of its
window samples are at or beyond `SAT_LEVEL` (0.98 of full scale). A railed lane
sits near 1.0 for ~100% of the window; a healthy channel with the occasional
large transient stays far below the fraction. Flagged lanes are drawn in the
warning color and tagged with a left-edge **`SAT`** badge (opposite the
right-edge spike counts so the two never overlap). The check operates on the
already-decimated display points, so it adds no measurable per-frame cost.

## Channel Map actually reorders the display (DA19)

The CHANNEL MAP panel let the user pick a preset or custom order (e.g.
`0,2,4,1,3,5`) and wrote it into `DisplaySettings::channel_order`, but the only
consumer was a preview label. `display_to_physical` — the one function that
turns a display lane into a physical channel — was marked `#[allow(dead_code)]`
with no call sites, and `waveform.rs::collect_from_ring` read the ring with
`phys_ch = start_ch + disp_pos` directly. The panel reported "map applied" while
every consumer still used the raw acquisition order: depth profiles and
electrode-site assignments were silently wrong with positive UI feedback.

The waveform display path now maps each lane's **display index**
(`start_ch + lane`) through `display_to_physical` before touching the ring:
trace read/draw, the inline per-lane spike detection, the colored lane chips and
axis labels, the zero-grid enable check, and hover all follow a non-identity
map. The hover readout recovers its lane from the cursor's plot-Y instead of
`hovered_ch - start_ch`, since under a reorder the physical channel is no longer
`start_ch + lane`. Identity (`channel_order` empty) is unchanged:
`display_to_physical(i) == i`.

Scope: the FFT panel selects its channel by **physical** index by design
(`FftPanelState::selected_channel`, "Which channel to analyze (physical
index)"), so it is unaffected by display reordering and stays unambiguous.
Recording likewise operates in **physical** index space — the Rec subset
(DA20) and the channel→site provenance map (DA17) are keyed by physical
channel, so the `.kvraw` stays in acquisition order with an explicit map rather
than being silently permuted by a display preference.

## Spike badge counts are zoom-independent (DA37)

The waveform spike-count badge ran detection on the points returned by
`ring.collect_channel`, which are decimated **twice**: once by `RING_DWNSP`
(=4) at ingestion and again by the render-time `stride2 = window / max_points`.
On a wide window (e.g. 60 s) each display point spans hundreds of raw samples, so
a ~1 ms spike lands between points and the refractory was computed from
`sample_rate / RING_DWNSP`, ignoring stride2 entirely. The result: the count
changed with the **zoom level** rather than the firing rate, and the sigma was
derived from decimated (often LFP-dominated) data — so an operator using the
badge for activity confirmation or probe localization could read a false
"silent here" or a phantom rate.

Detection moved into a pure `detect_spikes(pts, window_secs, sigma_mult)` that
derives its sample rate from the **actual** point density
(`pts.len() / window_secs`), i.e. after both decimation stages. The 1 ms
refractory is therefore expressed in true milliseconds regardless of zoom, and
when the effective rate falls below `SPIKE_MIN_DETECT_HZ` (1000 Hz, ≈1 point per
millisecond) the function returns `None` and the badge is **suppressed** instead
of reporting an aliased number. The `sample_rate` argument to `collect_from_ring`
is now unused and was removed.

This is the pragmatic half of the audit's fix (gate the badge to resolvable
windows + make the detection rate explicit). A dedicated minimally-decimated
AP-band / snippet stream for sorting-grade detection at any zoom remains future
work; the display ring is a render structure, not a spike-sorting source.
