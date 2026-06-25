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
