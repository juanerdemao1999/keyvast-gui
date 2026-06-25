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
