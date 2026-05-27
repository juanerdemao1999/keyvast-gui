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

Last updated: 2026-05-27 (session 14 — endurance test PASSED)

The project is in the simulator-first foundation phase. The streaming pipeline, incremental integrity, benchmark runner, latency distribution, CPU/memory monitoring, and professional GUI with neural demo mode are now complete. The GUI was refactored following Intan RHX / Open Ephys patterns and now covers Tier-1, Tier-2 and Tier-3 features (visualization polish, interaction, signal-processing).

Tier-4 experiments (FFT spectrum, TTL overlay, config persistence) were reverted — the Tier-3 baseline is the stable version on `main`. New work happens on the `dev` branch.

### Session 14: 2-hour endurance test — MVP acceptance #11 PASSED

**Goal**: Verify MVP acceptance criterion #11 — "Run a two-hour continuous acquisition test without unbounded memory growth."

**Benchmark ladder results** (all presets, `--release` build):

| Preset | Duration | Wall Clock | Samples Written | Missing Pkts | Memory Peak | Avg Write MB/s |
|--------|----------|------------|-----------------|--------------|-------------|----------------|
| smoke | 10s | 0.10s | 19.2M | 0 | 4.3 MB | 381 |
| recorder | 10min | 4.84s | 1.15B | 0 | 9.5 MB | 476 |
| **endurance** | **2h** | **769s** | **13.8B** | **0** | **55.8 MB** | **35.9** |

**Acceptance criteria verification**:
- ✅ No unbounded memory growth: 55.8 MB peak for 26 GB written (bounded, proportional to buffer sizing)
- ✅ Zero missing packets (no fault injection): 0 / 3,375,000
- ✅ Zero timestamp discontinuities
- ✅ Zero recorder buffer drops: recorder_dropped_blocks = 0
- ✅ Data integrity: expected_samples == written_samples == 13,824,000,000
- ✅ Byte count: 27,648,000,000 = samples × 2 (i16)

**Latency distribution (endurance)**:
- P50: 0.015 ms, P95: 0.034 ms, P99: 0.051 ms
- Max: 67,180 ms (single outlier — Windows file system flush on 26 GB file)

**Note for real-time hardware**: The 67s max write stall won't cause data loss with the simulator (which runs faster than real-time), but real hardware at 30 kHz producing 3.84 MB/s would overflow a 5s recorder buffer during such a stall. Future mitigations: pre-allocated file, segmented writes, or deeper recorder buffer.

**Baseline files saved**: `benchmarks/baselines/{smoke,recorder,endurance}-baseline.json`

**Commit**: (pending)

### Session 13: Sweep mode stretch fix

**Problem**: After introducing sweep mode in session 12, data appeared to progressively "stretch/zoom out" as the sweep filled in.

**Root cause**: `stride2 = visible_ring_entries / max_points`. At sweep start, only ~7 ring entries are visible, so stride2=1 (high resolution). By sweep end, 37,500 entries are visible, stride2=18 (coarse). Early data was re-rendered at coarser resolution each frame, making it look stretched.

**Fix**: stride2 now computed from the full **window** capacity, not the currently-filled portion. For a 5s window at 30kHz: window_ring_entries = 37,500, stride2 = 18 (constant from first frame of sweep).

**Commit**: `6472ddd`

---

### Session 12: Sweep mode display + sampling phase fix

**Problems fixed**:
1. "Twitching/flickering" when scrolling
2. Sampling phase drift causing horizontal waveform jitter

**Root causes**:
- Continuous scroll mode changed x_left/x_right every frame → all 32k data points changed pixel positions each frame even when data was unchanged
- `collect_channel()` ri_start varied ±1-3 ring entries per frame; stride2=18 amplified this into visible phase drift

**Fix 1 — Sweep mode** (SpikeGLX / Intan RHX default):
- `x_left = sweep_start_ms` and `x_right = x_left + window_ms` stay **fixed** within one sweep
- A cursor line sweeps from x_left to x_right as new data arrives
- When cursor overflows, `sweep_start_ms` advances one window and display resets (brief flash, once every 5-20s)
- Between resets: completely stationary display — no scrolling motion at all

**Fix 2 — Global alignment**: `collect_channel()` snaps `ri_start` to the global absolute-sample grid (`abs_idx % (stride2 * dwnsp) == 0`), eliminating per-frame phase jitter.

**Commits**: `8c98ab3`

---

### Session 11: Waveform rendering overhaul (stride + O(output) collection)

**Signal thickness fix**: Replaced min-max decimation with simple Nth-sample stride (SpikeGLX `draw1Analog` style). Min-max connected min and max as a line strip, causing zigzag / thick appearance. Stride emits 1 point per interval — thin consistent line at all zoom levels.

**Fluidity fix**:
- Binary search for first visible block: O(log N) vs O(N) across 10,000 history blocks per frame
- Arithmetic sample indexing within each block: compute first stride-aligned index and step directly, never iterate samples outside the window
- Overall: O(output_points) per channel, not O(input_samples)
- MAX_DISPLAY_POINTS reduced from 4096 → 2000

**Research basis**: Intan RHX `waveformdisplaymanager.cpp` and SpikeGLX `MGraph.cpp` source code. Both tools use 1-sample-per-display-unit rendering for normal mode; min-max / binMax is an explicit opt-in secondary mode in SpikeGLX.

**Commit**: `07ee295`

### Session 10: Incremental filtering + bug fixes

**Problem solved**: Enabling biquad/CAR filters caused frame drops because the entire visible window (5s × 30kHz × 16ch = 2.4M filter ops) was re-processed every frame.

**New architecture**:
- `app.rs` maintains `filtered_history: VecDeque<SampleBlock>` alongside `block_history`
- `filter_chains: Vec<FilterChain>` — persistent per-channel filter state (survives across frames)
- Filtering happens at ingest time (`ingest_block()`) — only new blocks are processed (O(new_block) per frame)
- When user changes filter settings, `rebuild_filter_chains()` detects the mismatch and re-filters the entire history once
- `waveform.rs` always uses the fast path (min-max decimation only) — no per-frame filtering logic
- Render code selects `filtered_history` or `block_history` based on whether any filter is enabled

**Bug fixes in same commit**:
- Gain/ch_spacing decoupling: gain formula now uses fixed `DEFAULT_CHANNEL_SPACING` constant (not `ch_spacing * 3.0`), so amplitude is independent of the channel spacing slider
- Scale bar label accuracy: bar shows 1/3 lane height = amp_scale/3 µV; label now correctly reflects the actual bar voltage

### Session 10 commits on `dev`

- `b9b622d` — Incremental filtering architecture + scale bar accuracy fix

### Session 9: GUI visual optimization (ALL COMPLETE)

See `docs/16-gui-optimization-plan.md` for the full plan with acceptance criteria.

All 3 rounds implemented:
1. Min-max decimation (preserve spikes when zoomed out) -- DONE
2. Filter warmup margin (eliminate left-edge transient) -- DONE
3. Voltage scale bar -- DONE
4. Dynamic channel spacing (+/- keys, slider) -- DONE
5. Extended color palette (32 distinct colors) -- DONE
6. Drag-to-browse when paused -- DONE

### Session 9 commits on `dev`

- `3a5b830` — Dynamic channel spacing: configurable via slider and +/- keys
- `8697e23` — Expand channel palette to 32 distinct colors
- `6c1ee6a` — Drag-to-browse history when display is paused

### Session 6 changes (waveform / UX polish)

- **Smooth wall-clock-driven scrolling** — viewport edges are computed from `elapsed_secs * 1000` instead of from data-derived bounds; data points keep absolute time positions from `block.timestamp_start`, so they never move once placed. Eliminated the discrete jumps caused by per-frame re-zeroing.
- **Anchored decimation** — points are filtered by `(timestamp_start + s) % stride == 0` so the same physical samples are picked every frame regardless of where they fall in the per-frame collected vector. Eliminated the visual "flicker" that array-position decimation produced.
- **Per-channel DC removal** — each visible channel's mean is subtracted before display so traces stay centered in their lane (industry standard).
- **Display freeze** — `P` toggles a paused viewport while acquisition and recording continue.  Captured `paused_elapsed` keeps the X bounds locked.
- **Mouse-wheel zoom** on the plot cycles through `TIME_WINDOWS` (1s/2s/5s/10s/20s); `[` and `]` do the same from the keyboard.
- **Performance overlay** — `F` toggles a small panel showing FPS, frame interval (EMA), render time (EMA), and history block count.  Uses 0.9/0.1 EMA so the readout is stable.
- **Hover highlight** — hovering over a channel draws it in white with extra width; non-hovered channels are dimmed; a tooltip shows `CHn  •  t = 12.34 ms`.
- **Smarter time axis** — ticks render as seconds when window ≥ 2s, ms otherwise.
- **Y-axis jitter fix** — replaced `include_y` + `.reset()` (which auto-fit each frame) with explicit `set_plot_bounds()` inside the draw closure.  Channel labels now stay still.
- **ComboBox visibility fix** — set `weak_bg_fill` on widget styles so the Time/Amp dropdowns no longer render as white-on-white.

### Quick keyboard / mouse reference

| Key       | Action                                  |
|-----------|-----------------------------------------|
| `Space`   | Toggle acquisition                       |
| `R`       | Toggle recording (Arm → Record → Stop)   |
| `G`       | Toggle grid                              |
| `P`       | Pause / resume display                   |
| `F`       | Toggle performance overlay               |
| `[` `]`   | Decrease / increase time window          |
| `+` `-`   | Increase / decrease channel spacing      |
| `1`–`9`   | Quick-set visible channels               |
| Wheel     | Increase / decrease time window          |
| Hover     | Highlight channel + tooltip              |
| Drag      | Browse history (when paused)             |

### Files most relevant to this session

- `crates/kv-gui/src/waveform.rs` — viewport, decimation, hover highlight, filter pipeline routing, spike threshold rendering
- `crates/kv-gui/src/app.rs` — pause state, perf metrics, scroll-wheel handling, overlays, FilterSettings wiring
- `crates/kv-gui/src/panels.rs` — `TIME_WINDOWS`, ComboBox styling, `FilterSettings` struct + Filters UI section
- `crates/kv-gui/src/dsp.rs` — Biquad IIR filters (HP/LP/Notch via RBJ cookbook, Direct Form II Transposed), FilterChain, 9 unit tests
- `crates/kv-gui/src/theme.rs` — `weak_bg_fill`, `transport_button` (no `add_enabled`)

### Tier-3 signal processing (added at end of session 6)

- **HP / LP / Notch biquad filters** — RBJ cookbook designs at user-selected cutoffs.  Defaults: HP 300 Hz (spike band), LP 250 Hz (LFP band), Notch 50 / 60 Hz selectable.  Q = 1/√2 (Butterworth) for HP/LP, Q = 30 for Notch.
- **Common Average Reference (CAR)** — at each time index, subtract the mean of all enabled visible channels from every channel.  Standard mu-mode noise removal in multi-channel arrays.
- **Spike threshold + crossing count** — per-channel σ (RMS) over the visible window, threshold at `−k·σ` (default k = 4), negative-going threshold crossings counted with a 1 ms refractory period.  Threshold line drawn dashed-red across each lane; crossing count painted at the right edge of each lane.
- **Display vs. recording**: filters are display-only; the recording stream remains raw, matching standard practice in Open Ephys / Intan RHX / Plexon.  A small caption in the FILTERS panel reminds users.
- **Performance routing**: when no filter / CAR / spike-detection is enabled, the renderer takes the original fast path (per-channel anchored decimation, no extra allocation).  The full pipeline (collect every raw sample → CAR → biquad chain → decimate) only runs when needed.


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

- kv-gui professional interface implemented (v0.2.0, refactored session 5):
  - `kv-gui` crate with egui/eframe + egui_plot 0.31
  - Professional dark theme (Intan RHX / Open Ephys style) in `theme.rs`
    - 16-color channel palette, transport button colors, toolbar background
    - `transport_button()` reusable widget, `format_clock()` helper
  - Demo mode with realistic neural signal generator (`demo.rs`):
    - 8 channel archetypes: Quiet, LFP, Spiking, Bursting, Noisy
    - Poisson-timed spike waveforms, LFP theta/gamma oscillations, pink noise, burst mode
    - Per-channel phase variation and amplitude randomization
    - Auto-starts on launch, generates blocks at real-time cadence
  - Single-plot multi-channel waveform display (`waveform.rs`, rewritten session 5):
    - **Single `egui_plot::Plot` with all channels as stacked waterfall traces**
      (replaces N separate Plot widgets — much faster, professional look)
    - Per-channel vertical offset with Y-axis channel labels
    - Per-channel coloring from 16-color palette
    - Zero-reference lines for each channel baseline
    - Horizontal pan/zoom on time axis
    - Automatic downsampling (MAX_DISPLAY_SAMPLES=4096)
  - Professional toolbar layout (`app.rs`, rewritten session 5):
    - Prominent Start/Stop and Record transport buttons with color states
    - Real-time acquisition clock (yellow=acquiring, red=recording)
    - Mode selector (Demo/Device)
  - Collapsible sidebar (`panels.rs`, rewritten session 5):
    - DEVICE: connection status, device info (CollapsingHeader)
    - ACQUISITION: transport controls with status indicator
    - DISPLAY: channel count slider, time/amplitude ComboBox, grid/label toggles
    - CHANNELS: per-channel enable/disable checkboxes with colored bars, All/None toggle
    - RECORDING: arm/record/stop workflow with directory selector
  - Status bar: ACQ/IDLE, recording state, clock, device info, data rate, block rate, drops
  - Keyboard shortcuts: Space=start/stop, R=record cycle, G=grid, 1-9=quick channel count
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

Last verified: 2026-05-27 (session 14)

Commands run successfully:

```powershell
cargo build --release --bin kv-acq
kv-acq benchmark --preset smoke    # 0 missing, 4.3 MB mem
kv-acq benchmark --preset recorder # 0 missing, 9.5 MB mem
kv-acq benchmark --preset endurance # 0 missing, 55.8 MB mem, 26 GB written
```

All benchmark presets pass with zero data loss. The full 2-hour endurance test completes MVP acceptance criterion #11.

Current test count:

```text
8 passing tests in kv-buffer
19 passing tests in kv-cli (6 record + 3 pipeline + 3 stream + 7 benchmark)
15 passing tests in kv-core (4 acquisition + 10 pipeline + 1 process_metrics)
4 passing tests in kv-types
5 passing tests in kv-simulator
10 passing tests in kv-integrity (6 batch + 4 incremental)
14 passing tests in kv-recorder (9 batch + 4 streaming + 1 latency distribution)
9 passing tests in kv-gui::dsp (Biquad / FilterChain frequency response)
84 total passing tests
```

## How To Resume

The full benchmark pipeline and professional GUI are feature-complete. The GUI was refactored in session 5 (layout) and session 6 (smooth scrolling, freeze, perf overlay, hover, decimation fix) to match Tier-1 features of Intan RHX / Open Ephys. Tier-3 signal processing (filters, CAR, spike detection) is the next planned GUI work. The next useful tasks, in priority order:

1. **Visual smoke test**: Run `gui.bat` and verify all the new interactions: smooth scroll (no flicker), `P` to freeze, `F` for perf overlay, scroll-wheel changes time window, hover highlights a channel and shows tooltip.

2. **Tier-4 GUI / analysis features** (planned, not started):
   - Filter parameter persistence between sessions (config file)
   - Real-time FFT / spectrogram inset on hovered channel
   - Channel grouping (probe layout view) for high channel counts
   - Multi-window split: zoomed window + overview window
   - Spike sorting (online): per-channel waveform clustering, ISI histogram
   - Event marker stream + TTL overlay

3. **Wire GUI to live pipeline**: Device mode uses `PreviewState` which wraps `start_preview()`. Already runs a SimulatorBackend in a background thread. Next step is connecting it to a real `FanoutBlockBuffer` preview consumer during a CLI-driven acquisition.

4. **Run longer benchmarks**: The smoke preset (10s) works. Ladder up to `--preset recorder` (10 min), then `--preset endurance` (2 hours). Inspect `benchmark.json`.

5. **Benchmark regression tracking**: Save `benchmark.json` outputs from known-good runs and compare across commits.

6. **kv-daemon**: Long-running acquisition service with IPC for GUI and CLI clients.

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
