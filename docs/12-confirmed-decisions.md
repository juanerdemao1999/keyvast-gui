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

## TTL gate sees every sample and resets per session (DA23, DA38)

**DA23 — sample-accurate gating.** `process_block` watched only the block-level
`block.ttl_bits` word: `current = (ttl_bits >> bit) & 1`, evaluated once per
block. An RHD block is 256 samples ≈ 8.53 ms at 30 kHz, so any pulse shorter
than a block, and any rise+fall pair inside one block, was invisible; two rising
edges in a block counted as one. Optogenetic / electrical-stim / behavioural TTLs
are routinely sub-millisecond to a few ms, so triggers were quantized to the
~8.5 ms block boundary and pulses were dropped — trial alignment and stim logs
silently diverged.

The gate now scans `block.ttl_in_per_sample` when present (falling back to
`ttl_bits` only when it is absent), tracking the level across sample and block
boundaries. Because the recorder is block-granular it records **any block that
contains at least one active sample** and releases on the first fully-idle
block, so a sub-block pulse is captured instead of missed. `last_level` now
reflects the final sample for an accurate live readout. (Depends on DA1 making
the parser actually retain per-sample TTL.)

**DA38 — per-session reset.** `self.trigger` was only ever touched by
`ingest_block` and the sidebar UI; `start_demo` / `start_device` / `stop_all`
(and therefore `select_source`, which routes through them) never cleared it.
Stopping while the gate held `recording = true` leaked that state into the next
session: the first block could fire a false stop, or a stale level could swallow
the first real edge — so the opening stimulus of a new trial/animal was lost
with no error. A new `TriggerConfig::reset()` clears `recording` and
`last_level` (preserving `enabled` / `bit_index` / `active_high`) and is now
called from `start_demo`, `start_device`, and `stop_all`. The gate is
level-based, so the first block of a fresh session correctly evaluates the
current line level instead of depending on a remembered edge.
