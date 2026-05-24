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

Last updated: 2026-05-24 (session 4)

The project is in the simulator-first foundation phase. The streaming pipeline, incremental integrity, benchmark runner, latency distribution, CPU/memory monitoring, and professional GUI with neural demo mode are now complete.

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
- Streaming recorder implemented:
  - `StreamingRecorder::new(output_dir)` opens `.kvraw` file
  - `write_block(&SampleBlock)` appends incrementally with per-block write latency tracking
  - `finish()` writes `recording.json` metadata, returns `StreamingRecordingSummary` with max write latency
  - validates device consistency across blocks (same as batch recorder)
  - `block_count()` accessor for progress monitoring
- Incremental integrity implemented:
  - `IncrementalIntegrity::new()` creates empty state
  - `push(&SampleBlock)` processes one block at a time, tracking packet gaps and timestamp discontinuities
  - `finish()` returns `IntegrityReport` identical to batch `check_blocks` output
  - no buffering — suitable for unbounded streaming runs
- Streaming pipeline implemented:
  - `run_streaming_pipeline(config, source)` in `kv-core::pipeline`
  - `StreamingPipelineConfig` with `output_dir` field
  - recorder consumer thread writes directly to disk via `StreamingRecorder` + `IncrementalIntegrity`
  - returns `StreamingPipelineResult` with recording summary, integrity report, timing, per-consumer status, and `max_write_latency_us`
- Real benchmark timing added:
  - `kv-acq simulator-pipeline` command writes `benchmark.json` with `measurement_kind: "measured"`
  - `kv-acq simulator-stream` command writes `benchmark.json` with `measurement_kind: "measured_streaming"`
  - wall-clock `duration_seconds` and `average_write_mb_s` from actual elapsed time
  - `max_buffer_occupancy` from recorder and preview consumer final status
  - `max_write_latency_ms` from per-block streaming write latency
  - clearly distinct from `simulator_estimate` used by old `simulator-record` command
- Benchmark runner implemented:
  - `kv-acq benchmark --preset smoke|recorder|stress-128|stress-256|endurance`
  - `kv-acq benchmark --duration SECONDS [--channels N] [--sample-rate F] [--samples-per-packet N]`
  - `blocks_for_duration(seconds, sample_rate, samples_per_packet)` computes block count from target duration
  - preset durations: smoke=10s, recorder=600s, stress-128=600s, stress-256=600s, endurance=7200s
  - stress-128 and stress-256 presets override channel count to 128 and 256 respectively
  - uses streaming pipeline under the hood for memory-efficient long runs
  - returns `BenchmarkResult` with computed block count and requested duration
- CLI extended:
  - `kv-acq simulator-pipeline --blocks N [--output DIR] [--drop-packet ID] [--recorder-capacity N] [--preview-capacity N]`
  - `kv-acq simulator-stream --blocks N [--output DIR] [--drop-packet ID] [--recorder-capacity N] [--preview-capacity N]`
  - `kv-acq benchmark --preset NAME | --duration SECONDS [--channels N] [--sample-rate F] [--output DIR]`
  - `CommandResult` enum for unified command dispatch with Record, Pipeline, Stream, and Benchmark variants
  - default recorder capacity: 2048 blocks, preview: 32 blocks
  - binary smoke tests for all four commands

- kv-gui professional interface implemented:
  - `kv-gui` crate with egui/eframe + egui_plot 0.31
  - Professional dark theme (Intan RHX / Blackrock Central style) in `theme.rs`
  - Demo mode with realistic neural signal generator (`demo.rs`):
    - 8 channel archetypes: Quiet, LFP, Spiking, Bursting, Noisy
    - Poisson-timed spike waveforms, LFP theta/gamma oscillations, pink noise, burst mode
    - Per-channel phase variation and amplitude randomization
    - Auto-starts on launch, generates blocks at real-time cadence
  - egui_plot waveform rendering (`waveform.rs`):
    - Per-channel Plot widgets in vertical ScrollArea
    - Color-coded channel bars and labels
    - Automatic downsampling (MAX_DISPLAY_SAMPLES=4096)
    - Zero-reference lines, configurable grid
  - Multi-panel layout (`app.rs`, `panels.rs`):
    - Top toolbar: brand, mode selector (Demo/Device), run status, version
    - Left panel: device info, acquisition start/stop, recording arm/record/stop, display settings (time/amplitude scale, visible channels, grid, labels, overlay)
    - Right panel: per-channel RMS and peak-to-peak statistics, data rate, block rate, elapsed time
    - Bottom status bar: connection/recording indicators, data rate, elapsed time, dropped blocks
    - Central waveform area fills remaining space
  - Device mode connects to SimulatorBackend via background thread (`preview.rs`)
  - History ring buffer (128 blocks) for scrolling waveform display
  - Real-time BlockStats computation (per-channel RMS/peak-to-peak, data rate, block rate)

- Latency distribution implemented:
  - `LatencyDistribution` struct in kv-recorder with count, min, max, mean, p50, p95, p99 (all in microseconds)
  - `LatencyDistribution::from_samples(&[u64])` computes distribution from raw samples
  - `StreamingRecordingSummary` carries `latency_distribution: Option<LatencyDistribution>`
  - `StreamingPipelineResult` carries `latency_distribution: Option<LatencyDistribution>`
  - `BenchmarkSummary` extended with `p50_write_latency_ms`, `p95_write_latency_ms`, `p99_write_latency_ms`
  - `benchmark.json` output includes all three percentile fields

- CPU/memory monitoring implemented:
  - `kv-core::process_metrics` module
  - `ProcessMetricsCollector::start()` / `finish(wall_clock_seconds)` pattern
  - On Windows: uses `GetProcessTimes` for CPU%, `GetProcessMemoryInfo` for peak working set
  - On non-Windows: returns `None` (graceful degradation)
  - `BenchmarkSummary.cpu_percent_avg` and `memory_mb_max` populated during benchmark runs
  - `windows-sys` v0.59 added as a `cfg(windows)` dependency in kv-core

Not yet implemented:

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

Last verified: 2026-05-24 (session 4)

Commands run successfully:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo build --bin kv-gui
cargo clippy --workspace
```

All 75 tests pass. Clippy clean except dead-code warnings on intentionally reserved fields/constants in kv-gui (future use: `auto_scale`, `file_prefix`, `min`/`max` in ChannelStats, palette colors).

Current test count:

```text
8 passing tests in kv-buffer
19 passing tests in kv-cli (6 record + 3 pipeline + 3 stream + 7 benchmark)
15 passing tests in kv-core (4 acquisition + 10 pipeline + 1 process_metrics)
4 passing tests in kv-types
5 passing tests in kv-simulator
10 passing tests in kv-integrity (6 batch + 4 incremental)
14 passing tests in kv-recorder (9 batch + 4 streaming + 1 latency distribution)
0 tests in kv-gui (GUI requires visual verification)
75 total passing tests
```

## How To Resume

The full benchmark pipeline and professional GUI are feature-complete. The GUI has two working modes: Demo (auto-generating neural signals) and Device (SimulatorBackend via background thread). The next useful tasks, in priority order:

1. **Visual smoke test**: Run `cargo run --bin kv-gui` and verify the professional layout renders correctly: dark theme, multi-channel waveforms updating in real-time, left/right panels showing device info and channel statistics, bottom status bar showing data rate and elapsed time.

2. **Wire GUI to live pipeline**: The Device mode uses `PreviewState` which wraps `start_preview()`. This already runs a SimulatorBackend in a background thread. Next step is connecting it to a real `FanoutBlockBuffer` preview consumer during a CLI-driven acquisition, so the GUI can monitor a live recording session.

3. **Run longer benchmarks**: The smoke preset (10s) works. Ladder up to `--preset recorder` (10 min), then `--preset endurance` (2 hours). Inspect `benchmark.json` for write latency tail, buffer occupancy, CPU%, and peak memory.

4. **Benchmark regression tracking**: Save `benchmark.json` outputs from known-good runs and compare across commits to catch throughput or latency regressions early.

5. **kv-daemon**: Long-running acquisition service with IPC for GUI and CLI clients. This is the next major crate after the GUI is functional.

Recommended implementation boundary:

```text
kv-simulator -> produces SampleBlock
kv-integrity -> checks SampleBlock continuity and sample counts (batch or incremental)
kv-recorder -> writes validated SampleBlock data to kvraw plus metadata (batch or streaming)
kv-core -> orchestrates acquisition: run_fixed_blocks, run_threaded_pipeline, or run_streaming_pipeline
kv-buffer -> bounded FIFO + fan-out buffering with per-consumer overflow counters
kv-cli -> thin developer commands: simulator-record, simulator-pipeline, simulator-stream, benchmark
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
