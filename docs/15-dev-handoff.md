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

Last updated: 2026-05-24

The project is in the simulator-first foundation phase. The threaded fan-out pipeline is now wired end-to-end.

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

- Threaded fan-out pipeline implemented:
  - `kv-core::pipeline` module with `run_threaded_pipeline`
  - `PipelineConfig`, `PipelineResult`, `PipelineTiming`, `PipelineError`
  - dedicated producer thread reading from `AcquisitionSource`, pushing into `FanoutBlockBuffer`
  - main thread draining recorder consumer into `Vec<SampleBlock>`
  - independent preview consumer drained in the same loop (drops old blocks without blocking recorder)
  - `Arc<Mutex<SharedState>>` + `Condvar` for thread synchronization
  - producer error propagation via `PipelineError::ProducerFailed`
  - wall-clock timing via `std::time::Instant`
  - first-block latency measurement
  - post-run integrity check on recorded blocks
  - per-consumer final status reporting (pushed, popped, dropped)
- Real benchmark timing added:
  - `kv-acq simulator-pipeline` command writes `benchmark.json` with `measurement_kind: "measured"`
  - wall-clock `duration_seconds` and `average_write_mb_s` from actual elapsed time
  - `max_buffer_occupancy` from recorder and preview consumer final status
  - clearly distinct from `simulator_estimate` used by old `simulator-record` command
- CLI extended:
  - `kv-acq simulator-pipeline --blocks N [--output DIR] [--drop-packet ID] [--recorder-capacity N] [--preview-capacity N]`
  - `CommandResult` enum for unified command dispatch
  - default recorder capacity: 2048 blocks, preview: 32 blocks
  - binary smoke test for the new command

Not yet implemented:

- `kv-gui`
- Benchmark runner (dedicated endurance/stress runner, separate from CLI one-shot commands)
- Fine-grained benchmark timing metrics: per-block write latency, CPU, memory
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

Last verified: 2026-05-24

Commands run successfully:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

All 51 integration tests pass. The threaded pipeline tests verify producer/consumer threading, independent recorder/preview consumption, wall-clock timing, producer error propagation, and packet gap detection through the pipeline. The CLI binary smoke test for `simulator-pipeline` confirms the binary produces `measurement_kind=measured` output and writes all expected files.

Current test count:

```text
8 passing integration tests in kv-buffer
9 passing integration tests in kv-cli (6 original + 3 pipeline)
10 passing integration tests in kv-core (4 original + 6 pipeline)
4 passing integration tests in kv-types
5 passing integration tests in kv-simulator
6 passing integration tests in kv-integrity
9 passing integration tests in kv-recorder
51 total passing integration tests
```

## How To Resume

The threaded fan-out pipeline and real benchmark timing are now complete. The next useful tasks, in priority order:

1. **Benchmark runner**: A dedicated endurance/stress test runner that exercises `simulator-pipeline` for configurable durations (10-second, 10-minute, 2-hour ladder from `docs/12-confirmed-decisions.md`). Collect per-block write latency distributions, peak buffer occupancy over time, and aggregate throughput.

2. **kv-gui scaffold**: Create the `kv-gui` crate with a minimal `egui` window that connects to the pipeline's preview consumer. Start with a simple channel trace or status display. The pipeline's `preview` consumer is already wired; it just needs a real consumer.

3. **Streaming recorder**: Currently the recorder writes all blocks at the end via `write_recording(&[SampleBlock])`. For long acquisitions, the recorder consumer should write blocks incrementally as they arrive (append to `recording.kvraw`, periodically flush metadata). This is needed before the 10-minute and 2-hour endurance tests become meaningful.

Recommended implementation boundary:

```text
kv-simulator -> produces SampleBlock
kv-integrity -> checks SampleBlock continuity and sample counts
kv-recorder -> writes validated SampleBlock data to kvraw plus metadata
kv-core -> orchestrates acquisition: run_fixed_blocks (synchronous) or run_threaded_pipeline (threaded fan-out)
kv-buffer -> bounded FIFO + fan-out buffering with per-consumer overflow counters
kv-cli -> thin developer commands: simulator-record (synchronous) and simulator-pipeline (threaded)
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
