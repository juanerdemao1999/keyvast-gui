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

## Headstage bring-up gates every stream, delay and port on signal quality (DA8, DA24, DA25)

The RHD bring-up scan locates the headstage, picks a MISO sampling delay, and
commits it. Three gates were too weak and could pass a bad bring-up as good.

**DA8 — centering check only looked at stream 0.** After committing the chosen
delay, `acquire_headstage` confirms the amplifier data is DSP-centered (~0x8000)
rather than half-scale (~0x4000, the wrong-sampling-phase signature). It called
`amplifier_mean_raw_word(.., 0)` — stream 0 only. A 64-channel RHD2164 is
dual-MISO: channels 32–63 live on stream 1. A delay chosen from stream-0 data
could leave the upper half railed/half-scale while stream 0 looked centered, so
the upper 32 electrodes recorded corrupt 0x4000 data for the whole session while
the bring-up reported success. The check now loops `0..detected_streams` and
refuses (`HalfScaleAmplifierData`) if **any** stream is half-scale.

**DA25 — a chip-ID-verified delay was accepted even when mostly railed.** Delay
selection pushed a delay into the strong "chip-ID-verified" set whenever
register 63 matched, regardless of how much of the data was rail-pinned; the
railed fraction was logged but never gated per delay. A delay whose register 63
happens to read back correctly but whose amplifier data is mostly railed (wrong
phase / open line) could be committed. Delay acceptance now applies a railed gate
(`MAX_ACCEPT_RAILED_FRACTION = 0.5`) to the chip-ID path as well, falling back to
low-railed-only delays only when no clean chip-ID delay exists.

**DA24 — cross-port "best" selection was last-wins.** When several ports
responded in the same validation tier, the later-scanned port overwrote the
earlier one purely by scan order, so a reflection/cross-talk port appearing
after the real headstage could displace it. Port ranking is now by validation
tier first (chip-ID-verified beats fraction-only) then by signal quality (lower
railed fraction at the chosen delay); scan position is never the tiebreaker.

The decision logic is factored into pure functions (`choose_port_delay`,
`port_is_better`) over a `DelayProbe` summary so it is unit-tested without
hardware; the hardware I/O loop only feeds them per-delay probe results.
