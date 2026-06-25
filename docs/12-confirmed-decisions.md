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
