# Karpathy Skills For Keyvast

This document defines the bottom-level development method for the Keyvast acquisition platform. It is inspired by a practical, empirical engineering style: build the smallest running loop, look directly at outputs, measure behavior, and scale only after the simple case works.

It is not a hardware specification. It is the way this project should think.

## Why This Matters

The FPGA bit file and real communication protocol are not confirmed yet. That uncertainty should not block software development.

The project should first build a hardware-independent acquisition platform:

```text
simulated device -> acquisition core -> buffer -> recorder -> GUI -> benchmark
```

When hardware is ready, only the lowest backend should change:

```text
SimulatorBackend -> RealFpgaBackend
```

## Skill 1: Build The Smallest Running Loop

Always prefer a tiny end-to-end path over a large unfinished architecture.

First useful loop:

```text
kv-simulator emits SampleBlock
kv-core accepts SampleBlock
kv-recorder writes kvraw
kv-integrity checks packet continuity
kv-cli prints summary
```

GUI, daemon, network streaming, and real drivers can be added after this loop is stable.

## Skill 2: Treat Simulated Data As A First-Class Dataset

The simulator is not a temporary toy. It is the software test fixture for the whole system.

It should generate:

- 64 / 128 / 256 channel data
- 30 kHz sample rate
- 16-bit ADC values
- TTL events
- packet counters
- timestamps
- white noise
- LFP-like low frequency signals
- spike-like events
- EMG-like interference
- deterministic seeds
- intentional packet loss

Every major module should be testable with simulator output.

## Skill 3: Look At The Actual Outputs

Do not trust an internal success flag alone.

For every stage, inspect a real artifact:

- simulator: sample block dump or replay plot
- recorder: file size, metadata, event CSV, log entries
- integrity: missing packet report
- GUI: screenshot or visual smoke test
- benchmark: measured throughput and latency

The project should make it easy to answer: "What did the system actually produce?"

## Skill 4: Make Evaluation Early

Benchmark and integrity checks are product features, not afterthoughts.

Each acquisition run should be able to report:

- expected sample count
- actual sample count
- missing packets
- buffer high-water mark
- write throughput
- maximum write latency
- run duration
- channel count
- sample rate
- CPU and memory metrics when available

The first MVP is not done until it can report whether data is complete.

## Skill 5: Overfit The Simple Case Before Scaling

Start with one stable target:

```text
64 channels x 30 kHz x 16-bit x continuous recording
```

Only after this is correct and observable should the project scale to:

```text
128 channels
256 channels
multi-device synchronization
network streaming
real FPGA backend
```

## Skill 6: Keep Interfaces Boring And Explicit

Prefer plain data structures and clear state transitions.

Good project contracts:

- `SampleBlock`
- `DeviceConfig`
- `DeviceStatus`
- `DeviceBackend`
- `AcquisitionState`
- `RecordingManifest`
- `IntegrityReport`

Avoid clever hidden coupling between GUI, recorder, and hardware.

## Skill 7: Replace One Boundary At A Time

When real hardware arrives, do not rewrite upper layers.

The migration path should be:

1. Keep simulator tests passing.
2. Add a real backend behind `DeviceBackend`.
3. Map real packets into `SampleBlock`.
4. Compare simulator and hardware acquisition reports.
5. Only then add hardware-specific controls.

## Skill 8: Prefer Observability Over Guessing

The acquisition system should continuously expose:

- current state
- packet rate
- sample rate
- dropped packet count
- buffer occupancy
- write speed
- write latency
- TTL state
- backend health
- last error

If a user sees "recording", they should also see whether recording is healthy.

## Skill 9: Document Assumptions As TBD

Unconfirmed hardware facts must be marked as TBD, not disguised as decisions.

Examples:

- FPGA register map: TBD
- real packet binary layout: TBD
- CRC algorithm: TBD
- timestamp clock frequency: TBD
- ADC gain conversion: TBD
- physical transport: TBD
- firmware loading process: TBD

Temporary simulator decisions are allowed, but must be labeled as simulator protocol.

## Skill 10: Tight Feedback Loop

For every feature, define the shortest check that proves it works.

Examples:

- data model: construct and validate one `SampleBlock`
- simulator: emit 10 packets with deterministic packet IDs
- integrity: detect a missing packet ID
- recorder: write 1 second of kvraw and verify expected byte count
- buffer: push faster than consumer and observe overflow behavior
- GUI: run with simulator and verify waveform updates

The development loop is:

```text
make small change -> run focused check -> inspect artifact -> fix -> repeat
```

