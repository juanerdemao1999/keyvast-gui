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
