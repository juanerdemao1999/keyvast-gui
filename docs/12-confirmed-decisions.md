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
