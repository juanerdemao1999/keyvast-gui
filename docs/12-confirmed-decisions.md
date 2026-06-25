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
