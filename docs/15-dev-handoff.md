# Development Handoff

This file is the persistent handoff note for AI sessions. It should let a new model resume work without relying on previous chat context.

## Handoff Rules

At the start of a session:

1. Read `AGENTS.md`, `README.md`, and this file.
2. Check `git status --short`.
3. Inspect the relevant crates or docs for the next task.
4. Run focused verification before making risky changes.

Before ending a session after meaningful work:

1. Update this file.
2. Record the latest status, verification commands, next steps, and blockers.
3. Put confirmed product or hardware decisions in `docs/12-confirmed-decisions.md`.
4. Put unresolved product or hardware questions in `docs/14-open-questions.md`.

## Current State

Last updated: 2026-05-22

The project is in the simulator-first foundation phase.

Implemented:

- Git repository initialized in `D:\1cases\51_keyvast_gui`.
- Rust toolchain installed and usable from normal PowerShell.
- Microsoft Visual Studio Build Tools C++ workload installed for the Rust MSVC linker.
- Root Rust workspace created.
- `kv-types` crate created.
- `kv-simulator` crate created.
- `kv-integrity` crate created.
- `kv-recorder` crate created.
- `kv-core` crate created.
- `kv-buffer` crate created.
- `kv-cli` crate created with `kv-acq` binary.
- Initial shared data model implemented:
  - `DeviceBackendKind`
  - `DeviceConfig`
  - `SampleBlock`
  - `SampleBlockError`
  - `AcquisitionState`
  - `AcquisitionEvent`
  - `DeviceStatus`
  - `IntegritySummary`
- Initial contract tests added for simulator defaults and `SampleBlock` validation.
- Initial simulator backend implemented:
  - `SimulatorConfig`
  - `SimulatorBackend`
  - deterministic seed support
  - default `SampleBlock` emission
  - monotonic packet IDs
  - simulator timestamp as first sample index in block
  - deterministic packet drop by packet ID
  - generated `i16` sample data with simple noise, low-frequency, and spike-like components
- Initial integrity checks implemented:
  - `check_blocks(&[SampleBlock])`
  - `IntegrityReport`
  - `PacketGap`
  - `TimestampDiscontinuity`
  - invalid block rejection before reporting
  - packet gap detection
  - simulator timestamp discontinuity detection
  - expected vs written sample counts
- Initial recorder implemented:
  - `write_recording(output_dir, &[SampleBlock])`
  - `write_integrity_summary(output_dir, &IntegritySummary)`
  - `write_log_file(output_dir, &[line])`
  - `write_events_csv(output_dir, &[AcquisitionEvent])`
  - `write_benchmark_summary(output_dir, &BenchmarkSummary)`
  - `BenchmarkSummary`
  - `RecordingSummary`
  - `RecorderError`
  - pre-write `SampleBlock` validation
  - consistency checks for one recording's device ID, sample rate, channel count, and samples per packet
  - `recording.kvraw` little-endian interleaved `i16` writing
  - minimal `recording.json` metadata writing
  - machine-readable `integrity.json` summary writing
  - human-readable `log.txt` writing
  - machine-readable `events.csv` writing
  - simulator/dev estimate `benchmark.json` writing
  - filesystem error surfacing
- Initial acquisition core implemented:
  - `AcquisitionSource` trait for backend-like block readers
  - `run_fixed_blocks(config, requested_blocks, source)`
  - `AcquisitionRun`
  - `AcquisitionRunSummary`
  - `AcquisitionRunError`
  - explicit state history for fixed runs
  - simulator-backed fixed block acquisition test coverage
  - backend read error path with `AcquisitionState::Error`
  - config validation before reading blocks
  - post-run integrity report generation
- Initial bounded block buffer implemented:
  - `BlockBuffer`
  - `BufferStatus`
  - `BufferError`
  - FIFO pop semantics
  - fixed-capacity overflow policy that drops the oldest block
  - pushed and dropped block counters
  - occupancy reporting
- Initial fan-out block buffer implemented:
  - `FanoutBlockBuffer`
  - `BufferConsumerId`
  - `FanoutBufferStatus`
  - `ConsumerBufferStatus`
  - named consumers such as `recorder` and `preview`
  - independent bounded queues per consumer
  - per-consumer pop cursors, occupancy, pushed, popped, and dropped counters
  - late consumers start from future pushed blocks only
  - slow preview consumers drop only their own oldest blocks without affecting recorder consumption
  - internally shared sample blocks to avoid copying raw sample vectors for every consumer
- Initial simulator recording command implemented:
  - `kv-acq simulator-record --blocks N --output DIR`
  - `kv-acq simulator-record --blocks N` defaulting to `run-YYYYMMDD-HHMMSS`
  - optional `--drop-packet PACKET_ID` fault injection
  - `run_simulator_recording(SimulatorRecordingOptions)`
  - `run_directory_name_utc(SystemTime)` helper for deterministic run folder names
  - simulator acquisition through `kv-core`
  - recording output through `kv-recorder`
  - `integrity.json` writing from the acquisition integrity summary
  - `log.txt` writing with start, warning, flush, and stop lines
  - `events.csv` writing with started, stopped, and packet_missing rows
  - `benchmark.json` writing with simulator_estimate metrics
  - returned acquisition, recording, and integrity summaries
  - binary smoke test that writes `recording.kvraw`, `recording.json`, `integrity.json`, `log.txt`, `events.csv`, and `benchmark.json`

Not yet implemented:

- `kv-gui`
- Benchmark runner
- Real benchmark timing metrics for write latency, buffer high-water mark, CPU, and memory
- Threaded fan-out integration between acquisition, recorder, GUI preview, daemon, and benchmark consumers
- `kv-daemon`

## Current Defaults In Use

These are recommended defaults from `docs/14-open-questions.md`; they are not final hardware decisions.

- Simulator device ID: `simulator-0`
- Channels: `64`
- Sample rate: `30000.0`
- Sample type: `i16`
- Samples per packet: `64`
- TTL lines: `16`
- Layout: `interleaved_by_sample`
- Simulator timestamp meaning: first sample index in the block
- Recording folder format: `run-YYYYMMDD-HHMMSS`
- Recommended CLI binary name: `kv-acq`

## Last Verification

Last verified: 2026-05-22

Commands run successfully:

```powershell
cargo fmt --all -- --check
cargo test -p kv-buffer
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The focused TDD pass for `kv-buffer` first failed because `FanoutBlockBuffer` and `BufferConsumerId` did not exist, then passed after implementation. The CLI manual smoke commands from the previous session were not rerun because this was a buffer-only change; the workspace integration tests still exercised `kv-acq simulator-record` including the binary smoke path.

Previous manual smoke context: the earlier smoke run wrote `recording.kvraw`, `recording.json`, `integrity.json`, `log.txt`, `events.csv`, and `benchmark.json`. With `--drop-packet 1`, the integrity summary reported `expected_packets: 4`, `observed_packets: 3`, `missing_packets: 1`, `timestamp_discontinuities: 1`, `expected_samples: 16384`, and `written_samples: 12288`. The log included warnings for `missing packet expected=1 observed=2 missing=1` and `timestamp discontinuity packet=2 expected=64 observed=128`. The events CSV included started, packet_missing, and stopped rows. The benchmark JSON reported `measurement_kind: simulator_estimate`, `duration_seconds: 0.006400`, `byte_count: 24576`, and `average_write_mb_s: 3.840000`.

Current test count:

```text
8 passing integration tests in kv-buffer
6 passing integration tests in kv-cli
4 passing integration tests in kv-core
4 passing integration tests in kv-types
5 passing integration tests in kv-simulator
6 passing integration tests in kv-integrity
9 passing integration tests in kv-recorder
42 total passing integration tests
```

## How To Resume

The next useful task is to wire `FanoutBlockBuffer` into a small simulator acquisition pipeline, or add a small real benchmark timing layer around recording writes. Keep explicit simulator-first boundaries.

Recommended first test cases:

1. For fan-out integration, verify simulator blocks can be pushed once and consumed independently by recorder and GUI-preview consumers.
2. Verify slow preview consumption reports preview dropped blocks without causing recorder packet loss.
3. For benchmark timing, measure wall-clock elapsed time around writing and keep simulator_estimate distinct from measured timing.
4. Do not block on GUI or real device transport.

Recommended implementation boundary:

```text
kv-simulator -> produces SampleBlock
kv-integrity -> checks SampleBlock continuity and sample counts
kv-recorder -> writes validated SampleBlock data to kvraw plus metadata
kv-core -> orchestrates fixed-size acquisition runs over backend-like sources
kv-buffer -> bounded FIFO buffering with observable overflow counters
kv-cli -> thin developer command that exercises simulator -> core -> recorder
```

Do not add real FPGA packet format, USB details, CRC algorithm, ADC conversion, or channel mapping yet.

## Open Decisions To Ask Eventually

These do not block the next core step:

1. Final CLI binary name: `kv-acq`, `kv`, or `keyvast-acq`.
2. Whether `64 samples per channel per packet` is acceptable long term.
3. Whether TTL should remain `SampleBlock.ttl_bits` plus timestamped events.
4. Recorder buffer defaults: 5 seconds for recorder, 1 second for GUI preview.
5. Recording folder format: `run-YYYYMMDD-HHMMSS`.

## Notes For Future Agents

- Keep hardware independence strict.
- Prefer small TDD steps.
- Update this handoff before stopping.
- If chat context gets large, summarize the current phase here before compacting or switching models.
