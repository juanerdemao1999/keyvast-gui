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

Last updated: 2026-06-12 (Reliability: write-failure auto-stop, live progress, poison-safe locks, input validation; audio monitor removed)

### Session 26: Reliability + cleanup (stacked on P2 branch)

Branch `devin/1781290000-reliability`:

1. **Audio monitor removed** (product decision): `audio_monitor.rs` deleted,
   sidebar section, per-block feed in `ingest_block`, and the
   `audio_channel`/`audio_volume` config fields are gone. Old config files
   still load (unknown keys are ignored by the lenient parser).
2. **Write-failure auto-stop (B1)**: a failed `write_block` now stops and
   finalizes the recording immediately in both paths (Demo in `ingest_block`,
   Device in `recorder_loop`) instead of continuing to write into a possibly
   corrupt file. The error banner says the file may be incomplete.
3. **Live recording progress (B2)**: new `RecorderEvent::Progress { blocks,
   bytes }` sent ~5/s by the recorder thread; `StreamingRecorder` gained a
   `byte_count()` getter. The status panel now updates during Device
   recordings instead of only at Stop.
4. **Poison-safe locks (B3)**: all 7 `lock().unwrap()` (remote-API queues,
   client_count) replaced with `remote_api::lock_recover` —
   `unwrap_or_else(PoisonError::into_inner)` so a panicked worker can no
   longer take down the GUI thread.
5. **Remote-API input validation (B4)**: `validate_output_dir` rejects
   empty paths, NUL bytes, and `..` traversal before `StartRecording`
   accepts a client-supplied output_dir (unit tested).
6. **Playback scrub (A2)**: `tick()` now emits a block only when the cursor
   moved (`last_emitted_frame`), so paused frames stop re-ingesting the same
   data every frame while scrubbing still refreshes instantly; `tick` now
   routes through `read_block_at` (its dead_code allow removed).

**Verification:** `cargo test --workspace --exclude kv-gui` all pass;
clippy zero warnings on both halves; Windows-target check clean.
GUI smoke test still pending on Windows.

**Next:** merge chain #10 → #11 → #12 → this PR. Future: windows-latest CI
job for kv-gui tests, app.rs split, serde_json, rustfmt decision.

---

### Session 25: P2 — Engineering (CI workflow, clippy zero-warning, eprintln→log)

Branch `devin/1781284000-p2-engineering` (stacked on the P1 branch → `v2.0`):

1. **GitHub Actions CI** — `.github/workflows/ci.yml`, two jobs on every PR
   and pushes to `main`/`v2.0`:
   - `test`: `cargo test --workspace --exclude kv-gui`
   - `lint`: `cargo clippy --workspace --exclude kv-gui --all-targets -- -D warnings`
     plus `cargo clippy -p kv-gui --target x86_64-pc-windows-msvc -- -D warnings`
     (kv-gui only builds for Windows; checking against the MSVC target needs
     no linker). No `cargo fmt --check`: the codebase intentionally is not
     rustfmt-formatted (242 diff hunks) — decide separately before adding it.
2. **Clippy zero-warning** — `cargo clippy --fix` plus manual fixes (strip_prefix,
   needless_range_loop, clamp, `&PathBuf`→`&Path`, collapsed identical trigger
   branches). Intentionally-unused API (hardware bring-up constants/`RhdChipType`,
   audio-monitor plumbing, custom probe geometry, scrub-bar `read_block_at`,
   channel stats) carries targeted `#[allow(dead_code)]` with a reason comment.
   Removed `ChannelSelectState::filter_block_data` (superseded by
   `filter_block_channels`; test migrated).
3. **eprintln → `log` crate** — all `eprintln!` in `kv-rhd` (backend,
   frontpanel) and `kv-gui` (app) replaced with `log::info!/warn!/error!`;
   `kv-gui` main initializes `env_logger` (default level `info`, override via
   `RUST_LOG`). `kv-cli` keeps `println!` (CLI output, not logging).

**Verification:** clippy zero warnings on both halves; `cargo test --workspace
--exclude kv-gui` all pass; `cargo check -p kv-gui --target
x86_64-pc-windows-msvc` clean. GUI smoke test still pending on Windows.

**Next:** merge chain #10 → #11 → P2 PR; future: app.rs split, cpal audio
monitoring, rustfmt decision.

---

### Session 24: P1 — Performance (lazy band filters, history dedup, refilter debounce, LTO)

Branch `devin/1781280000-p1-perf` (stacked on the P0 branch → `v2.0`):

1. **Lazy LFP/AP band filtering** — `ingest_block` previously ran three full
   filter passes per block regardless of layout. Now the fixed LFP band is only
   computed when an `LfpView` tile exists (`lfp_tile_open()`), and the AP band
   only when an `ApView` or `SpikeOverlay` tile exists (`ap_band_needed()`,
   since the snippet detector consumes the AP block). Default single-tile
   layout: 3 passes → 1.
2. **filtered_history dedup** — when no user filter/CAR is active,
   `filtered_history` was a full clone of `block_history` (~80 MB at capacity).
   It now stays empty in that case; `refilter_history` already falls back to
   `block_history` as the display-ring source.
3. **Refilter debounce** — filter-settings changes now wait
   `REFILTER_DEBOUNCE_MS` (150 ms) of stability before `rebuild_filter_chains`
   re-filters the full 10k-block history, so dragging a cutoff slider no
   longer re-filters everything every frame. `ingest_block` only rebuilds
   chains on channel-count change; a repaint is requested while a change is
   pending so the debounce expires without user input.
4. **Release LTO** — workspace `[profile.release]` now sets `lto = "thin"`,
   `codegen-units = 1` for cross-crate inlining of filter/ring hot paths.

**Verification:** `cargo test --workspace --exclude kv-gui` all pass,
`cargo check -p kv-gui --target x86_64-pc-windows-msvc` clean (pre-existing
warnings only). GUI smoke test still pending on Windows.

**Next:** P2 engineering — CI workflow, clippy cleanup, eprintln→log.

---

### Session 23: P0 — Wire unfinished features (impedance / export / selective save / demo errors)

A full v2.0 audit found four features with UI but no backing implementation.
This session wires them up (branch `devin/1781275384-p0-wire-features` → `v2.0`):

1. **Impedance measurement wired** — `RhdHardwareBackend::run_impedance_test()`
   public delegate added in `kv-rhd/src/backend.rs`. In the GUI, the Run button
   now calls `KvApp::start_impedance_test()`: validates RHD source + bitfile,
   stops acquisition (test needs exclusive SPI access), spawns a worker thread
   that opens the device and runs the test, streaming `ImpedanceMsg`
   (Progress/Done/Failed) over an mpsc channel polled each frame
   (`poll_impedance`). Button gating changed from `acquiring` to
   "RHD source + bitfile selected".
2. **Export formats wired** — new "Export .kvraw…" button in the EXPORT FORMAT
   section. `export_kvraw()` (app.rs) reads a .kvraw via `KvrawReader` in 1 s
   chunks, rebuilds `SampleBlock`s, and calls `export_intan_rhd` (writes
   `<stem>.rhd` next to the source) or `export_flat_binary` (writes
   `<stem>.export/recording.bin` + meta). Runs on a worker thread; result
   surfaced in the panel via `poll_export`.
3. **Selective channel save wired** — `ChannelSelectState::recording_selection()`
   returns the channel subset (None = save all) and is captured at recording
   start (so mid-recording changes can't corrupt the file layout). Device mode:
   `RecorderCmd::Start { path, channels }` carries it to the recorder thread.
   Demo mode: `KvApp::record_channels`. Both paths filter via the new
   `channel_select::filter_block_channels()` before `write_block`.
4. **Demo recording errors surfaced** — demo-mode `write_block` and auto-stop
   `finish` failures now set `recording_error` (red banner) instead of
   eprintln-only. Also deduplicated `toggle_recording` to call
   `begin_recording`/`stop_recording`.

**Verification:** `cargo test --workspace --exclude kv-gui` (92 tests pass),
`cargo check -p kv-gui --target x86_64-pc-windows-msvc` clean (pre-existing
dead-code warnings only), kv-gui test code compiles (link step impossible on
Linux — GUI smoke test and on-hardware impedance run still pending on the
Windows machine).

**Next:** P1 performance (lazy LFP/AP rings, dedup filtered_history, refilter
debounce, release LTO), then P2 engineering (CI workflow, clippy cleanup,
logging). Each as its own PR.

---

Previous state (2026-06-12, merged all phases: Phase 1–4 into v2.0):

The project has progressed through 7 PRs on `v2.0`:
- PR #2: Signal quality fixes (chip ID validation, FIFO MSB)
- PR #3: Open Ephys alignment (11 fixes)
- PR #4: Code audit bug fixes + architecture improvements
- PR #5: Phase 1 — impedance measurement + offline .kvraw playback
- PR #6: Phase 2 — Roll mode, channel colors, FFT spectrum, channel mapping
- PR #7: Phase 3 — Recording format export, Gate/Trigger, Audio monitor, Remote API
- PR #8: Phase 4 — Probe Map, selective channel save, config persistence

None of the PRs have been merged yet (v2.0 still has only the initial commit).

### Phase 4: Advanced Features

**New modules added:**

1. **`kv-gui/src/probe_map.rs`** — 2D probe layout visualization. Presets: LinearSingle, LinearDual, Tetrode, Grid4x8, Custom. Per-channel RMS → color mapping (blue→cyan→yellow→red). Floating window with zoom/pan, color bar legend, hover tooltips. Tests: 5.

2. **`kv-gui/src/channel_select.rs`** — Selective channel recording. Per-channel checkboxes, quick actions (All/None/Even/Odd), range selection. `filter_block_data()` extracts only selected channels from interleaved data. Tests: 6.

3. **`kv-gui/src/config_persist.rs`** — JSON config file (keyvast_config.json next to exe). Saves/loads: display settings, filter config, recording paths, audio monitor, remote API port, probe geometry. Manual JSON serialization (no serde). Auto-save option. Tests: 4.

**Integration in `app.rs`:**
- ProbeMapState, ChannelSelectState, ConfigPersistState fields added to KvApp
- Probe map activity updated from display ring each frame
- Channel select syncs to acquisition channel count
- Config save/load buttons trigger capture_from/apply_to
- Probe map drawn as floating egui::Window when visible

**Build verification:**
- `cargo check -p kv-rhd` ✓
- `cargo check -p kv-gui --target x86_64-pc-windows-msvc` ✓ (warnings only)
- `cargo test --workspace --exclude kv-gui` ✓ (92 tests pass)

**All planned phases complete.** Future enhancements could include:
- Spike sorting (online clustering)
- LFP spectral decomposition (theta/gamma bands)
- Multi-probe support
- Network streaming (LSL integration)
- Plugin system for custom analysis

---

### Phase 3: Recording & Integration Features

**New modules added:**

1. **`kv-recorder/src/export_formats.rs`** — Export to Intan .rhd (magic 0xC6912702, v2.0 header with Qt QStrings, 128-sample data blocks) and flat binary (.bin + .meta.json). Tests: roundtrip, empty-blocks error, file creation.

2. **`kv-gui/src/trigger.rs`** — Gate/Trigger recording control. TriggerEdge (Rising/Falling), TriggerMode (Level/EdgeToggle/EdgeTimed), TriggerState (Disabled/Armed/Triggered). `process_block()` returns TriggerAction (None/StartRecording/StopRecording). Tests: 5 scenarios.

3. **`kv-gui/src/audio_monitor.rs`** — Audio buffer-only interface (ready for cpal integration). Decimates 30kHz → configurable output rate, volume control, ring buffer with overflow handling. Tests: 5 scenarios.

4. **`kv-gui/src/remote_api.rs`** — TCP server + JSON-RPC 2.0 (newline-delimited). Commands: Ping, GetStatus, GetChannelCount, StartAcquisition, StopAcquisition, StartRecording, StopRecording, SetDisplayMode. Default port 4444. Tests: 6 parse/format tests.

**Integration in `app.rs`:**
- TriggerConfig, AudioMonitorState, RemoteApiState, RemoteApiHandle fields added to KvApp
- Trigger actions processed in ingest_block → calls begin_recording/stop_recording
- Audio monitor fed from block data each ingest
- Remote command queue polled each frame, responses sent back
- Export format selector UI in sidebar (CollapsingHeader)
- Remote API start/stop logic tied to enabled toggle

**Build verification:**
- `RUSTUP_TOOLCHAIN=nightly cargo check -p kv-recorder` ✓
- `RUSTUP_TOOLCHAIN=nightly cargo check -p kv-gui --target x86_64-pc-windows-msvc` ✓ (warnings only)
- `RUSTUP_TOOLCHAIN=nightly cargo test --workspace --exclude kv-gui` ✓ (92 tests pass)

**Next (Phase 4):**
- Probe Map visualization
- Selective channel save
- Config persistence (save/load session settings)

---

### Session 22: Phase 1 — Impedance measurement + offline .kvraw playback

**Goal**: Implement the two highest-priority features from the cross-GUI comparison:
(1) impedance measurement, (2) offline .kvraw file reader + playback mode.

**PR #5** (`devin/1781111069-phase1-impedance-playback` → `v2.0`):

**Phase 1.1 — Impedance Measurement:**

1. **`commands.rs`**: Added `ZcheckScale` enum (100fF/1pF/10pF with capacitance values),
   `MAX_COMMAND_LENGTH = 1024`, `sample_rate()` getter, zcheck configuration methods
   (`enable_zcheck`, `set_zcheck_scale`, `set_zcheck_polarity`, `set_zcheck_channel`),
   and `create_command_list_zcheck_dac(frequency, amplitude)` that generates WRITE(6, x)
   sine wave commands for the on-chip impedance DAC.

2. **`impedance.rs` (new)**: `ChannelImpedance`, `ImpedanceTestConfig`, `ImpedanceResult`
   types. `compute_impedance()` performs single-bin DFT at the test frequency to extract
   magnitude/phase. `auto_select_scale()` picks the best capacitor scale based on
   impedance range. Quality labels/colors for GUI display. 5 unit tests.

3. **`backend.rs`**: Full `run_impedance_test()` implementation:
   - Uploads DC waveform to AuxCmd1 Bank 0, sine wave to AuxCmd1 Bank 1
   - Uploads register configs with zcheck enabled + 3 cap scales to AuxCmd3 Banks 2/3/4
   - For each channel: sets zcheck_select, switches banks, runs acquisition, reads data
   - Computes impedance via DFT, auto-selects best cap scale, re-measures if needed
   - Restores normal operation (DC DAC, non-zcheck config) after test
   - `extract_channel_from_raw()` helper extracts single-channel i16 from raw frame data

4. **`impedance_panel.rs` (new GUI)**: Impedance panel in left sidebar with:
   - Test frequency and periods configuration
   - Progress bar during measurement
   - Results table with per-channel magnitude (Ω/kΩ/MΩ), phase, and quality color coding

**Phase 1.2 — Offline .kvraw Playback:**

5. **`kv-recorder/src/lib.rs`**: Added `KvrawReader` and `KvrawMetadata` types.
   Reader opens .kvraw files, parses embedded JSON header (v2) or companion .json (v1),
   supports random-access `read_frames(start, count)` and `read_channels()`.
   Minimal JSON parser avoids serde dependency.

6. **`playback.rs` (new GUI)**: `PlaybackManager` state machine (Idle/Paused/Playing).
   GUI panel with: file open dialog, transport controls (play/pause/rewind/close),
   speed control (0.1x–10x), timeline scrubber slider, time position display.
   Playback feeds SampleBlocks into the existing `ingest_block()` pipeline for display.

**Build**: `cargo check -p kv-rhd` + `-p kv-gui --target x86_64-pc-windows-msvc` clean.
All tests pass (impedance: 5, recorder reader round-trip: 1, plus all 89 existing).

**Next steps (Phase 2, pending user approval):**
- Roll mode display
- Channel colors/grouping
- FFT / spectrum analysis
- Channel mapping/sorting

---

### Session 21: Cross-GUI comparison + code audit bug fixes

**Goal**: (1) Horizontal comparison with Intan RHX, SpikeGLX, Open Ephys GUI, and
NeuroScope2 to identify feature gaps. (2) Full codebase audit to find bugs and
architecture issues.

**PR #4** (`devin/1781109497-bugfix-and-improvements` → PR#3 branch):
https://github.com/juanerdemao1999/keyvast-gui/pull/4

**Bug fixes (5):**

1. **B1**: `start_demo()` / `start_device()` snippet_store channel count was hardcoded
   to 16 — now uses actual channel count from demo config or lazy-init from first block.
2. **B2**: `dropped_blocks` was always 0 — now tracks packet-ID discontinuity in
   `LivePipelineHandle` and passes to `compute_block_stats`.
3. **B3**: `streaming_metadata_json()` hardcoded `"backend": "simulator"` — now infers
   from device_id prefix (demo/rhd-hardware/simulator).
4. **B4/B12**: `default_bitfile_path()` and `default_frontpanel_dll_path()` used
   `env!("CARGO_MANIFEST_DIR")` (compile-time only) — now searches exe dir → cwd →
   debug-only compile path → bare name fallback.
5. **B5**: `MAX_CHANNEL_TOGGLES=64` renamed to `INITIAL_CHANNEL_TOGGLES` to clarify
   the vec grows dynamically.

**Architecture improvements (6):**

6. **D1**: Preview channel bounded (1024 slots) with `try_send` — prevents OOM when
   GUI falls behind; skips preview frames rather than stalling acquisition.
7. **D2**: Recorder thread replaced 1ms sleep polling with condvar notification from
   producer, matching kv-core/pipeline.rs pattern.
8. **D3/D6**: Reduced block clones in `ingest_block()` — filtered is now `Option<SampleBlock>`,
   avoiding a full clone when no user filter is active.
9. **D4**: Extracted duplicate filter chain construction into `build_filter_chains()`.
10. **D10**: Demo spike waveform improved from 1-sample biphasic to realistic 5-sample
    template (onset → trough → overshoot → AHP → recovery).
11. **D11**: Simulator `spike_component()` now generates spikes on all channels with
    channel-dependent rarity (was limited to first 8 channels).

**Cross-GUI comparison report**: Sent to user as attachment. Key findings:
- 🔴 Missing: impedance measurement, offline file viewer/playback
- 🟡 Missing: roll mode, channel colors, audio monitor, remote API, multi-format
  recording, gate/trigger, channel mapping, probe map, spectrogram, channel subset save

**Build**: All 89 tests pass. `cargo check` clean on all crates.

**Next steps (from comparison report, pending user approval):**
- Implement impedance measurement (RHD DAC waveform + amplitude/phase readout)
- Implement KVRAW offline reader + playback mode
- Add roll mode display option
- Add recording format export (Intan .rhd / Binary)
- Remote control API (TCP + JSON-RPC)

---

### Session 20: Comprehensive Open Ephys RHD plugin alignment (11 fixes)

**Goal**: Deep code-level comparison of keyvast-gui against Open Ephys rhythm-plugins
to find and fix all remaining issues and gaps.

**PR #3** (`devin/1781107079-openephys-alignment` → `v2.0`):
https://github.com/juanerdemao1999/keyvast-gui/pull/3

Includes PR#2 cherry-pick plus 11 new fixes/improvements.

**Critical fixes:**

1. **Reverted Reg 3/6 writes in AuxCmd3** — PR#2 incorrectly added `reg_write(3)`
   and `reg_write(6)` to `create_command_list_register_config`. Open Ephys
   intentionally skips these (Reg 3 = temp sensor / dig out via AuxCmd1/2;
   Reg 6 = impedance DAC). AuxCmd3 would overwrite AuxCmd2's temp sensor bits.

2. **Added `set_data_source()` stream→port MUX** — `WireInDataStreamSel1234`
   (0x12) / `WireInDataStreamSel5678` (0x13) now explicitly configured.
   Previously relied on FPGA power-on defaults.

3. **Split `setMaxTimeStep` into LSB (0x01) + MSB (0x02)** — was writing full
   32-bit to single WireIn.

**FIFO / USB3:**

4. `flush_fifo()` now sets USB3 throttle override bit (WireInResetRun bit 16),
   uses 256 KB bulk reads, then smaller aligned reads, and clears override.

**Parser improvements:**

5. Auxiliary data (temp, VDD, aux ADC) parsed into `SampleBlock::aux_data`
6. Board ADC (8 ch) parsed into `SampleBlock::board_adc_data`
7. Per-sample TTL in/out tracked (`ttl_in_per_sample`, `ttl_out_per_sample`)
8. `SampleBlock` extended with optional fields; all constructors backward-compat

**New capabilities:**

9. `set_cable_delay_port()` for per-port MISO delay
10. `set_sample_rate(f64)` with all 18 Open Ephys PLL M/D pairs (1kHz–30kHz)
11. `RhdChipType` enum for RHD2132/2216/2164 identification

**Stubs added:** impedance testing, DAC threshold, LED control, external fast settle.

**Build**: `cargo check -p kv-rhd` + `-p kv-gui --target x86_64-pc-windows-msvc`
clean. All 89 tests pass (workspace minus kv-gui).

**What was NOT changed (verified correct):** frame header magic validation, timestamp
continuity, ADC conversion (offset-binary → signed), PLL M/D for 30kHz, DSP cutoff
frequency selection, bandwidth register DAC solving, ADC calibration sequence, frame
length formula, AuxCmd1/2 command lists.

**Next steps:**
- Hardware retest after merging PR#3
- If `setDataSource` MUX mapping is wrong (custom bitfile may use hardcoded mapping),
  the user can disable `set_default_data_sources()` or adjust the mapping
- Wire `RhdChipType` detection into the scan to auto-adjust channel count
- Implement full impedance testing (requires dedicated AuxCmd bank with DAC waveform)
- Wire multi-sample-rate into the GUI (currently hardcoded 30kHz)
- Fill in DAC/LED/external-fast-settle stubs when hardware is available

---

### Session 19: Signal quality analysis + PR#2 (5 fixes)

**Goal**: Diagnose "signal too large, all noise" when using keyvast-gui with the
XEM7310 FPGA + RHD amplifier (works fine in Open Ephys RHD plugin).

**PR #2** (`devin/1781105421-fix-signal-quality` → `v2.0`):
https://github.com/juanerdemao1999/keyvast-gui/pull/2

5 fixes: chip ID validation in port scan, FIFO MSB read (0x26), register config
Reg 3/6 (later found to be wrong — reverted in session 20), display scaling with
0.195 µV/count, flush_fifo loop improvement.

---

### Session 18: RHD chip register configuration + ADC calibration (flat-signal fix)

**Symptom**: With the GUI on the RHD source, the board connected, data streamed
at the true 30 kHz (115 blk/s, 1.89 MB/s, 0 drops, no errors) — but every
channel was flat at ~0 µV. User confirmed the same bitfile + headstage produce
real signal in the Open Ephys RHD plugin, so the gap was purely software.

**Root cause**: the backend only configured the Rhythm *data plane* (sample
rate, streams, cable delay). It never uploaded the RHD2000 *chip* register
configuration or ran ADC self-calibration, so the amplifiers sat
unconfigured and the ADC emitted mid-scale (offset-binary 32768 → signed 0).

**Fix** (in `kv-rhd`):

- New `commands.rs` — faithful port of Intan `Rhd2000RegistersUsb3`:
  `Rhd2000Registers` (register state + defaults + sample-rate bias), DSP-cutoff
  selection, RH1/RH2/RL bandwidth DAC solving, `register_value` bit packing,
  16-bit MOSI command encoding (`create_rhd2000_command`), and the three
  128-command lists (register config w/ optional calibrate, temp sensor,
  dig-out). Verified line-by-line against the Open Ephys source.
- `backend.rs` — new FrontPanel endpoints (CmdRam 0x05-0x07, AuxCmdBank1/2/3
  0x08-0x0a, AuxCmdLength/Loop 0x0b/0x0c) and `upload_command_list` /
  `select_aux_command_bank(_all_ports)` / `select_aux_command_length`.
  `configure()` now runs `initialize_rhd_chips()`: upload AuxCmd1 (dig-out),
  AuxCmd2 (temp), AuxCmd3 bank0 (config+calibrate) / bank1 (config) / bank2
  (fast-settle); select calibrate bank; non-continuous 256-step run; read &
  discard; then switch to bank1 for normal acquisition.
- Also fixed the per-block re-trigger: `RhdHardwareBackend` now has an
  `acquisition_started` flag and calls `start_continuous_acquisition()` once
  on the first `read_block`, instead of re-running SPI every block.

**Build**: `cargo build -p kv-rhd` and `-p kv-gui` both clean (only the 4
pre-existing kv-gui dead-code warnings). Mirror note: builds go through the
Tsinghua (TUNA) crates.io mirror configured in `.cargo/config.toml`.

**Status**: awaiting on-hardware retest. Expectation: a few extra seconds at
Start (register upload + ADC calibration), then real waveforms. If still flat,
suspect MISO/cable-delay timing or a keyvast-bitfile endpoint difference, not
the chip config.

---

### Session 17: RHD backend wired into GUI Device mode

**Goal**: Let the GUI acquire from the real RHD / Opal Kelly board, not only the simulator.

**What changed** (all in `crates/kv-gui`):

- `Cargo.toml` — added `kv-rhd` path dependency.
- `live_pipeline.rs` — new `PipelineSource` enum (`Simulator(SimulatorConfig)` | `Rhd(Box<RhdHardwareOptions>)`). `start_live_pipeline` now takes a `PipelineSource`. `producer_loop` opens the chosen backend behind an internal `ActiveSource` adapter: the simulator keeps its sleep pacing, hardware blocks inside `read_block()` (no artificial pacing). New `RecorderEvent::SourceError(String)` reports device open/read failures back to the GUI.
- `panels.rs` — new `DeviceKind` enum + `DeviceSettings { kind, rhd_bitfile, rhd_streams }`. DEVICE panel gains a Simulator/RHD source selector, a bitfile picker (`rfd`, `.bit` filter), and a 1/2-headstage selector; all disabled while acquiring. `default_bitfile_path()` best-effort pre-fills `keyvast_260607_with_UART.bit` if found next to the workspace (returns None otherwise — nothing hard-coded into acquisition).
- `app.rs` — `KvApp` gains `device: DeviceSettings` + `device_error`. `start_device()` builds the source via `build_pipeline_source()` (RHD requires a bitfile, otherwise a banner error and no start). `tick_device` handles `SourceError` → banner + pipeline teardown. New red dismissible device-error banner. The old `live_pipeline.as_ref().unwrap()` in `tick_device` is now a safe `if let` (a `SourceError` can drop the pipeline mid-frame).

Everything downstream (fanout buffer, recorder thread, preview channel, display rings) is unchanged — the hardware-independent boundary holds, and `ingest_block` already adapts to each block's `channel_count` / `sample_rate`.

**Default behaviour preserved**: Device mode still defaults to `DeviceKind::Simulator`, so the GUI runs with no hardware and there is no regression. RHD is opt-in from the DEVICE panel.

**Build status**: `cargo build -p kv-gui` compiles cleanly (debug). The only new warning introduced was a stray `RHD_MIN_STREAMS` const, since removed; the remaining 4 warnings are pre-existing dead-code fields (`demo.rs`, `panels.rs`, `preview.rs`). `cargo test --workspace` not yet run.

**Network note for future sessions**: crates.io is unreachable directly from this machine (curl timeouts on download). `.cargo/config.toml` now replaces `crates-io` with the Tsinghua (TUNA) sparse mirror — `rsproxy.cn` and `mirrors.ustc.edu.cn` both timed out from here, TUNA responded in ~3.5 s. If builds start failing with download timeouts again, check/rotate that mirror.

**Hardware test steps**: build, run `kv-gui`, open the DEVICE panel → Source = RHD, pick `keyvast_260607_with_UART.bit`, Headstages = 2, then press Start (or Space). Watch for either live waveforms or the red device-error banner (which surfaces FrontPanel open / board-id / FIFO errors verbatim).

**Backend caveats still open (in `kv-rhd`, unchanged this session — surfaced to the user, not yet fixed)**:

- No RHD chip register / SPI command upload. The backend assumes the keyvast MicroBlaze firmware configures the RHD chips. UNVERIFIED — if amplifiers output nothing or garbage, this is the first suspect.
- `read_raw_block` re-triggers `SPI_START` every block in continuous run mode (the CLI `rhd-smoke` does the same), which is suspect vs standard Rhythm semantics.
- Synchronous poll read (`wait_for_fifo_words`, 1 s timeout); a non-blocking / continuous read may be needed if 30 kHz can't keep up.

**Next**: compile + smoke-test on the hardware machine; if data is wrong, investigate the two backend caveats above.

---

### Session 16: RHD / Opal Kelly hardware discovery

**Goal**: Understand how to connect the Keyvast GUI to the new bitfile and Open Ephys-compatible RHD acquisition path.

**External reference downloaded**:

```text
D:\11111\1case\104_keyvast_gui\external\rhd-recording-controller
```

This repository contains the Open Ephys RHD Recording Controller plugin and the useful Intan Rhythm USB3 API files:

```text
Source/rhythm-api/rhd2000evalboardusb3.*
Source/rhythm-api/rhd2000datablockusb3.*
Source/rhythm-api/rhd2000registersusb3.*
Source/rhythm-api/okFrontPanelDLL.h
Resources/okFrontPanel.dll
Resources/intan_rec_controller_7310.bit
```

**User-confirmed hardware decisions**:

```text
Opal Kelly board: XEM7310-A75
Keyvast bitfile to use: D:\11111\1case\104_keyvast_gui\keyvast_260607_with_UART.bit
Windows package should bundle the FrontPanel runtime DLL where possible
First live acquisition target: up to two 32-channel RHD headstages
Display scaling should follow Open Ephys / Intan RHD conventions
```

**Important findings**:

- `keyvast_top.sv` combines the Intan Rhythm data plane with the MicroBlaze control plane.
- The Rhythm data plane uses FrontPanel endpoints matching the Intan/Open Ephys USB3 path.
- Expected endpoint highlights: WireIn `0x00` reset/run, WireIn `0x03` sample clock M/D, WireIn `0x14` data stream enable, WireOut `0x20` FIFO words, WireOut `0x22` SPI running, WireOut `0x23` TTL in, WireOut `0x3e` board id, WireOut `0x3f` board version, BTPipeOut `0xA0` data.
- Expected data frame magic: `0xd7a22aaa38132a53`.
- Open Ephys converts amplifier words for display as `(uint16 - 32768) * 0.195`, i.e. unsigned offset-binary ADC words to microvolts. Preserve raw samples in recording unless changed later.

**Recommended next implementation boundary**:

Add a real hardware backend crate such as `kv-driver` or `kv-rhd` that implements the existing `kv-core::AcquisitionSource` contract and returns `SampleBlock`. Start with a CLI smoke command before wiring the GUI:

```text
kv-acq rhd-smoke --bitfile D:\11111\1case\104_keyvast_gui\keyvast_260607_with_UART.bit --blocks 10
```

The smoke command should open the first XEM7310-A75, upload the bitfile, verify FrontPanel + board id, initialize Rhythm, enable streams for the first two 32-channel headstages, read from BTPipeOut `0xA0`, validate magic/timestamps, and write existing `kvraw` output.

---

### Session 15: Multi-view tile display — planning complete, implementation starting

**Goal**: Replace single `CentralPanel` waveform with a draggable `egui_tiles` layout
supporting LFP (LP 250 Hz), AP/Spike (HP 300 Hz), and Spike Overlay (threshold snippet)
tiles alongside the existing main waveform.

**Design doc**: `docs/17-multiview-plan.md`

**Status**: Phase 1 starting — egui_tiles skeleton + LFP/AP rings.

---

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

### Session 17: RHD / Opal Kelly Bring-Up Start

Implemented first RHD hardware integration layer:

- New crate: `crates/kv-rhd`
  - Rhythm USB3 constants and block-size helpers
  - Rhythm raw USB frame parser into existing `SampleBlock`
  - RHD offset-binary ADC conversion: `u16 - 32768` into signed `i16`
  - Display scale helper: `0.195 uV/count`, matching Open Ephys / Intan RHD convention
  - Windows FrontPanel runtime loader using bundled `okFrontPanel.dll`
  - RHD command RAM generation for AuxCmd1/AuxCmd2/AuxCmd3
  - FrontPanel board path: configure bitfile, verify board id 700, set 30 kHz data clock, set MISO delay, enable 1-2 streams, upload command RAM, run ADC calibration, then read BTPipeOut `0xA0`

- New bundled runtime asset:
  - `third_party/opalkelly/windows-x64/okFrontPanel.dll`
  - Copied from downloaded Open Ephys RHD plugin resources for local packaging convenience.

- New CLI command:
  - `kv-acq rhd-smoke`
  - Hardware mode: downloads `D:\11111\1case\104_keyvast_gui\keyvast_260607_with_UART.bit` by default and reads Rhythm USB blocks.
  - Offline mode: `kv-acq rhd-smoke --raw-input capture.bin` parses raw Rhythm USB bytes and writes normal `recording.kvraw`, `integrity.json`, `events.csv`, `log.txt`, and `benchmark.json`.

- Build/network follow-up:
  - `kv-gui` now disables `eframe` default features and uses the `glow` renderer only, avoiding the unnecessary `egui-wgpu` dependency for the current Windows GUI.
  - `.cargo/config.toml` and `gui.bat` now set sparse registry protocol plus longer Cargo HTTP retry/timeout settings for slow crates.io downloads.

Current limitations:

- Not yet verified on a live XEM7310-A75 in this environment.
- Physical channel ordering still needs confirmation against the actual two-headstage wiring.
- The first implementation assumes Rhythm default stream mapping: stream 0 then stream 1, each with 32 channels.

Why Open Ephys does more setup:

- The bitfile exposes the USB data transport, FIFO, timing, and SPI scheduler.
- The RHD headstage chips still need register writes over SPI for bandwidth, DSP offset removal, amplifier power-up, aux inputs, fast settle, and ADC calibration.
- Open Ephys therefore uploads command lists into FPGA command RAM, selects command banks per SPI port, runs a short non-continuous calibration pass, then switches to the no-calibration bank for normal acquisition.

Not yet implemented:

- Live-board validation and final physical channel-map confirmation
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

Last verified: 2026-06-08 (session 17 partial, Rust toolchain unavailable)

Verification attempted:

```powershell
cargo test -p kv-rhd
cargo test -p kv-cli rhd_smoke_raw_input_writes_rhd_backend_metadata
cmd /c where cargo
cmd /c where rustc
```

Result:

```text
cargo and rustc were not found in the current PowerShell PATH.
Common install locations such as %USERPROFILE%\.cargo\bin and C:\Program Files were also checked without finding cargo.exe.
```

The RHD code, command RAM initialization, CLI path, and offline raw-input smoke test were added, but Rust tests could not be executed in this environment until the Rust toolchain is installed or available on PATH.

Previous full benchmark verification: 2026-05-27 (session 14)

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

The full benchmark pipeline is complete (all 12 MVP acceptance criteria met including 2h
endurance).  The GUI dev branch includes recording health monitoring (clock, buffer
water-mark, error banner) and a live pipeline connecting GUI display to the same data
source as the recorder.

**Active work: multi-view tile display** — see `docs/17-multiview-plan.md` for the full
design.  Implementation is in Phase 1.

### Phase 1 checklist (current)

- [ ] Verify `egui_tiles 0.10` + `eframe 0.31` compile compatibility
- [ ] Add `egui_tiles` to `kv-gui/Cargo.toml`
- [ ] Add `disp_ring_lfp`, `disp_ring_ap`, `filter_chains_lfp`, `filter_chains_ap` to `KvApp`
- [ ] Update `ingest_block()` to push to all three rings
- [ ] New `multiview.rs`: `TileKind` enum + `KvTileBehavior : egui_tiles::Behavior`
- [ ] Replace `CentralPanel` render block with `tile_tree.ui(...)`
- [ ] Startup tree: single `MainWaveform` node
- [ ] "+ Add View" dropdown: insert LFP / AP / Spike Overlay tiles
- [ ] Per-tile channel scroll (mouse wheel adjusts `start_ch`)
- [ ] `cargo test --workspace` all pass

### Remaining backlog (after multi-view)

1. **Experiment metadata** — animal ID, session, probe type, brain region → `recording.json`
2. **per-channel RMS** in CHANNELS panel (code exists, never shown)
3. **TTL digital track overlay**
4. **Config persistence** — filter/display settings survive restart
5. **kv-daemon** — background acquisition service
6. **kv inspect / kv replay** CLI commands

Recommended implementation boundary:

```text
kv-simulator -> produces SampleBlock
kv-integrity -> checks SampleBlock continuity and sample counts (batch or incremental)
kv-recorder -> writes validated SampleBlock data to kvraw plus metadata (batch or streaming)
kv-core -> orchestrates acquisition: run_fixed_blocks, run_threaded_pipeline, or run_streaming_pipeline
kv-buffer -> bounded FIFO + fan-out buffering with per-consumer overflow counters
kv-rhd -> Rhythm USB3 parser + Opal Kelly FrontPanel hardware smoke backend
kv-cli -> thin developer commands: simulator-record, simulator-pipeline, simulator-stream, benchmark, rhd-smoke
```

Keep real hardware details behind `kv-rhd` / backend boundaries. Do not let GUI panels parse FrontPanel packets directly.

## Open Decisions To Ask Eventually

These do not block the next core step:

1. Final CLI binary name: `kv-acq`, `kv`, or `keyvast-acq`.
2. Whether `64 samples per channel per packet` is acceptable long term.
3. Whether TTL should remain `SampleBlock.ttl_bits` plus timestamped events.
4. Recorder buffer defaults: 5 seconds for recorder, 1 second for GUI preview.
5. Recording folder format: `run-YYYYMMDD-HHMMSS`.

### Session: Phase 2 — display features (Roll mode, channel colors, FFT, channel mapping)

**Date**: 2026-06-10
**Branch**: `devin/1781113191-phase2-display-features` (base: `v2.0`)

**What changed**:

1. **Roll mode display** (`panels.rs`, `app.rs`) — `DisplayMode::Sweep | Roll` enum. In Roll mode, `sweep_start_ms = (latest_ms - window_ms).max(0.0)` every frame. Sweep cursor only drawn in Sweep mode. UI toggle in DISPLAY panel.

2. **Channel colors/grouping** (`panels.rs`, `waveform.rs`) — `CHANNEL_GROUP_COLORS` 8-color palette. `channel_color()` cycles colors based on `channels_per_group`. Waveform renderer uses group color when `color_by_group` enabled.

3. **FFT spectrum analysis** (`fft_panel.rs` — new) — `FftState` with configurable FFT size (256–4096), frequency range, log scale. Hand-written radix-2 Cooley-Tukey FFT. PSD computed from `DisplayRing::last_n_samples()`. Plot with 50/60 Hz markers. Available as sidebar section + tile view.

4. **Channel mapping/sorting** (`channel_map.rs` — new) — `ChannelMapPreset::Natural|Reverse|EvenOdd|Custom`. `display_to_physical()` mapping. Custom comma-separated input with validation. Sidebar UI.

5. **Tile system extended** (`multiview.rs`) — `TileKind::FftSpectrum` variant. FFT tile in "+ Add View" menu. `KvTileBehavior` carries `fft: &FftState`.

6. **DisplayRing extension** (`disp_ring.rs`) — `last_n_samples(ch, n)` extracts recent samples as i16 for FFT.

**Verification**:
- `cargo check -p kv-rhd` — pass
- `cargo check -p kv-gui --target x86_64-pc-windows-msvc` — pass (24 warnings, all pre-existing dead-code)
- `cargo test --workspace --exclude kv-gui` — 85 tests pass (includes 2 FFT tests + 6 channel map tests)

**Next steps**:
- Phase 3 features when user is ready (recording format export, gate/trigger, audio monitor, remote API)
- Hardware testing of all Phase 2 features on Windows with XEM7310

## Notes For Future Agents

- Keep hardware independence strict.
- Prefer small TDD steps.
- Update this handoff before stopping.
- If chat context gets large, summarize the current phase here before compacting or switching models.
