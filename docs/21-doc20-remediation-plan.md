# 21 · Doc 20 Remediation Plan (Deep-Audit Fix Tracking)

This document plans how the 44 findings in `docs/20-deep-audit.md` are fixed and
how the design docs are kept in sync. It is the working tracker for the deep
audit: each batch is a single focused PR that references the `DA` numbers it
closes (e.g. `fix DA1, DA17`).

## How To Use

1. Pick the next unstarted batch (top-down = highest in-vivo risk first).
2. For `⏳ 待核实` findings, re-verify the mechanism against source before
   touching code; downgrade or drop the item in this file if the audit was wrong.
3. Implement the smallest correct fix, add a regression test, run the
   verification commands, and sync the listed design docs in the *same* PR.
4. Tick the batch box, record the PR link, and update `docs/15-dev-handoff.md`.

Status legend: `[ ]` not started · `[~]` in progress · `[x]` merged.

## Sequencing Principle

Order follows the audit's "最该先处理（在体致命）" list: data that is silently
lost or corrupted ranks above quality/QC issues, which rank above ergonomics.
Several batches map onto branches that earlier sessions already pushed; those
are reused (rebased onto current `main`) rather than reimplemented.

## Batches

### Batch P1 — Parser non-fatal resync + per-sample TTL preservation
- **Closes**: DA2 (Critical), DA39 (Low)
- **Scope**: `kv-rhd/src/parser.rs`, `backend.rs`, `kv-core/src/pipeline.rs`.
  Stop treating in-block timestamp jumps / `BadMagic` as fatal; forward-scan to
  re-sync on `RHYTHM_HEADER_MAGIC` (Open Ephys behavior) and report via
  integrity/`AcquisitionEvent` counts. Preserve and validate per-sample TTL.
- **Docs to sync**: `06-protocol-draft.md`, `08-integrity.md`.
- **Reuse branch**: `devin/1782401059-da2-da39-parser-resync-ttl`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P2 — Digital/aux persistence + channel provenance
- **Closes**: DA1 (Critical), DA17 (High)
- **Scope**: persist `ttl_in/out_per_sample`, `board_adc_data`, `aux_data`, and
  generate `TtlChanged` events; store channel-index vector, enabled channels,
  chip/stream/bitfile provenance in `KvrawMetadata`. Selective-save subsets must
  record which raw channels each column maps to.
- **Docs to sync**: `04-data-model.md`, `07-recording-format.md`.
- **Reuse branch**: `devin/1782402693-da1-da17-kvaux-sidecar`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P3 — Timestamp domain (u32 wrap, real export timestamps, host clock)
- **Closes**: DA5 (High), DA10 (High), DA16 (High), DA41 (Low)
- **Scope**: maintain a cumulative u64 sample clock / compare modulo 2^32; export
  using `block.timestamp_start` not a synthetic 0-based counter; capture host
  wall/monotonic time per block + acquisition-start wall clock; reset FPGA
  timestamp on lazy first start.
- **Docs to sync**: `04-data-model.md`, `07-recording-format.md`, `08-integrity.md`.
- **Reuse branch**: `devin/1782396897-da-timestamp-domain` (DA5/DA10/DA41; add DA16).
- **Status**: [ ]　**PR**: _tbd_

### Batch P4 — Acquisition data-loss detection
- **Closes**: DA3 (High), DA4 (High), DA13 (High, ⏳), DA35 (Medium, ⏳)
- **Scope**: read `num_words_in_fifo()` and warn/emit `BufferOverflow` on FPGA
  backlog; bounded retry + "acquisition stall" event instead of fatal
  `NotEnoughFifoWords`; wire real `crc_errors`/`buffer_overflows` into
  `IntegritySummary`; verify hardware-derived loss (timestamp continuity) rather
  than host `packet_id`.
- **Docs to sync**: `08-integrity.md`, `03-architecture.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P5 — Pipeline / CLI lifecycle (finalize on error, incremental flush)
- **Closes**: DA12 (High), DA14 (High)
- **Scope**: on streaming-error exit, signal the producer to stop, join it, and
  call `recorder.finish()` so `.kvraw` is finalized; carry a partial
  `RecordingSummary`. `rhd-smoke`/`simulator-record` stream to disk incrementally
  instead of buffering the whole run in memory.
- **Docs to sync**: `05-state-machine.md`, `07-recording-format.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P6 — Panic hardening / block validation
- **Closes**: DA11 (High), DA29 (Medium, ⏳), DA36 (Medium), DA34 (Medium, ⚠️)
- **Scope**: exporters call `validate()` and use checked indexing; `validate()`
  checks side-channel vector lengths + per-sample TTL mask; `spike_overlay`
  bounds-checks `block.data`; declare panic policy + isolate acquisition/record
  threads so a GUI panic cannot corrupt an in-progress recording.
- **Docs to sync**: `04-data-model.md`, `08-integrity.md`.
- **Reuse branch**: `devin/1782395644-da-panic-hardening` (DA11/DA29/DA36; add DA34).
- **Status**: [ ]　**PR**: _tbd_

### Batch P7 — Build / deploy hardening
- **Closes**: DA15 (High), DA33 (Medium, ⏳)
- **Scope**: `[profile.release] overflow-checks = true`; convert the doc-19 wrap
  sites to observable panics / `checked_*`; ensure `okFrontPanel.dll` transitive
  deps + VC++ runtime resolve (`SetDllDirectory`/`AddDllDirectory`, document
  prerequisites).
- **Docs to sync**: `15-dev-handoff.md`, `12-confirmed-decisions.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P8 — Configurable hardware sample rate
- **Closes**: DA9 (High), DA40 (Low)
- **Scope**: thread `DeviceConfig.sample_rate` into `configure()` →
  `set_sample_rate()` validated against the supported table; stamp the *actually
  programmed* rate onto `SampleBlock`/`KvrawMetadata`; compute cable delay from
  the real rate.
- **Docs to sync**: `04-data-model.md`, `06-protocol-draft.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P9 — Bring-up correctness
- **Closes**: DA8 (High), DA24 (Medium), DA25 (Medium)
- **Scope**: half-scale centering gate loops over all detected streams; best-port
  selection scores by railed-ratio/amplitude instead of last-wins; per-delay
  gating includes railed ratio, not just chip-ID.
- **Docs to sync**: `03-architecture.md` (bring-up section), `14-open-questions.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P10 — Impedance correctness
- **Closes**: DA6 (High), DA7 (High), DA26 (Medium, ⚠️), DA27 (Medium, ⏳), DA28 (Medium)
- **Scope**: enforce block-pipe length alignment; port Intan
  `approximateSaturationVoltage` rail rejection + empirical frequency
  calibration; iterate cap-scale selection; error (not clamp) on
  `period > MAX_COMMAND_LENGTH`; write real bandwidth/DSP params in `.rhd` header.
- **Docs to sync**: `06-protocol-draft.md`, `07-recording-format.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P11 — CLI defaults / semantics
- **Closes**: DA31 (Medium), DA32 (Medium), DA42 (Low, ⏳)
- **Scope**: require explicit `--blocks`/`--duration` (or sane default) instead of
  1; benchmark records true signal duration + `requested_duration`;
  `register_value()` returns real reg6 and a correct out-of-range error type.
- **Docs to sync**: `10-benchmark-plan.md`.
- **Reuse branch**: `devin/1782396066-da-cli-safety`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P12 — Recording disk-space guard
- **Closes**: DA18 (High)
- **Scope**: `begin_recording` pre-checks free space vs threshold / estimated
  session size; poll during recording; clean auto-stop + toast near the safe
  watermark.
- **Docs to sync**: `05-state-machine.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P13 — GUI signal fidelity
- **Closes**: DA19 (High), DA20 (High), DA21 (High), DA22 (High), DA37 (Medium)
- **Scope**: apply `display_to_physical` across display/FFT/spike/selective-save;
  render Rec rows for all channels (or clamp selection to visible); rail/SAT
  badge before DC removal; CAR over an explicit non-railed reference subset in
  f64; detect spikes on a minimally-decimated AP stream.
- **Docs to sync**: `03-architecture.md` (GUI section).
- **Status**: [ ]　**PR**: _tbd_

### Batch P14 — Trigger fidelity
- **Closes**: DA23 (High), DA38 (Medium)
- **Scope**: walk `block.ttl_in_per_sample` (fall back to `ttl_bits`) keeping
  `prev_ttl` across sample boundaries; reset trigger state + `prev_ttl` on
  start/stop/source change. Depends on P1/P2 preserving per-sample TTL.
- **Docs to sync**: `05-state-machine.md`.
- **Status**: [ ]　**PR**: _tbd_

### Batch P15 — Remaining type/integrity/playback
- **Closes**: DA30 (Medium, ⏳), DA43 (Low, ⏳), DA44 (Low)
- **Scope**: type-level `DeviceConfig::validate()` called by all backends;
  account for packets lost before the first observed block; playback reads the
  full `[prev_cursor, cursor)` span at high speed instead of a fixed block.
- **Docs to sync**: `09-simulator-spec.md`, `08-integrity.md`.
- **Status**: [ ]　**PR**: _tbd_

## Tracking Table

| Batch | DA items | Severity peak | Status |
|---|---|---|---|
| P1 | DA2, DA39 | 🔴 Critical | [ ] |
| P2 | DA1, DA17 | 🔴 Critical | [ ] |
| P3 | DA5, DA10, DA16, DA41 | 🟠 High | [ ] |
| P4 | DA3, DA4, DA13, DA35 | 🟠 High | [ ] |
| P5 | DA12, DA14 | 🟠 High | [ ] |
| P6 | DA11, DA29, DA36, DA34 | 🟠 High | [ ] |
| P7 | DA15, DA33 | 🟠 High | [ ] |
| P8 | DA9, DA40 | 🟠 High | [ ] |
| P9 | DA8, DA24, DA25 | 🟠 High | [ ] |
| P10 | DA6, DA7, DA26, DA27, DA28 | 🟠 High | [ ] |
| P11 | DA31, DA32, DA42 | 🟡 Medium | [ ] |
| P12 | DA18 | 🟠 High | [ ] |
| P13 | DA19, DA20, DA21, DA22, DA37 | 🟠 High | [ ] |
| P14 | DA23, DA38 | 🟠 High | [ ] |
| P15 | DA30, DA43, DA44 | 🟡 Medium | [ ] |

All 44 findings are covered exactly once across P1–P15.
