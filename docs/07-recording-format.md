# Recording Format

The first recording format should be boring, stable, and easy to inspect.

The recorder writes a directory, not a single complex container file.

## Output Directory

```text
run-YYYYMMDD-HHMMSS/
  recording.kvraw
  recording.kvaux
  recording.json
  events.csv
  integrity.json
  benchmark.json
  log.txt
```

The initial CLI uses this directory name automatically when `--output` is omitted. The timestamp is formatted from UTC using only the Rust standard library. Explicit `--output DIR` remains supported for deterministic tests and scripted runs.

## recording.kvraw

Raw continuous sample payload.

Initial layout:

```text
i16 little-endian interleaved samples
```

Ordering:

```text
sample0_ch0, sample0_ch1, ... sample0_chN,
sample1_ch0, sample1_ch1, ... sample1_chN,
...
```

No metadata should be required to parse the binary values, but metadata is required to interpret channel count, sample rate, and units.

## recording.kvaux

The `.kvraw` file holds only interleaved amplifier samples. Per-sample TTL
in/out words, board-ADC channels, and auxiliary-command channels are parsed off
the wire but cannot live in `.kvraw` without breaking its bare-`i16` contract,
so they are persisted in a companion `recording.kvaux` sidecar (DA1). The same
file also records the channel→electrode mapping the bare `.kvraw` header lacks
(DA17). The sidecar is always written, so the mapping is recoverable even when a
recording carries no side-channel streams.

Layout mirrors the `.kvraw` embedded-header convention:

```text
[0..8]      magic b"KVAUX1\0\0"
[8..12]     json_len: u32 LE
[12..8204]  json_block: 8192 B  (UTF-8 JSON, zero-padded)
[8204..]    per-block side-channel payload (fixed stride from SideChannelLayout)
```

JSON header shape:

```json
{
  "format": "kvaux",
  "format_version": 1,
  "device_id": "simulator-0",
  "backend": "simulator",
  "sample_rate": 30000.0,
  "channel_count": 64,
  "samples_per_block": 64,
  "block_count": 1000,
  "data_offset_bytes": 8204,
  "enabled_channels": [0, 1, 2, 3],
  "ttl_line_count": 16,
  "side_channels": {
    "per_block_streams": {
      "ttl_in_samples": 64,
      "ttl_out_samples": 64,
      "board_adc_channels": 8,
      "board_adc_samples": 64,
      "aux_streams": 4,
      "aux_channels_per_stream": 3,
      "aux_samples": 64
    }
  }
}
```

`enabled_channels` is the selective-save column→electrode mapping (empty means
all channels in natural order). The `side_channels.per_block_streams` block is
the `SideChannelLayout` established from the first block and enforced for every
subsequent block, so the payload is a fixed-stride sequence the reader indexes
without a per-block table.

## recording.json

Metadata example:

```json
{
  "format": "kvraw",
  "format_version": 1,
  "device_id": "simulator-0",
  "backend": "simulator",
  "sample_rate": 30000.0,
  "channel_count": 64,
  "sample_type": "i16",
  "endianness": "little",
  "layout": "interleaved_by_sample",
  "started_at": "TBD",
  "stopped_at": "TBD",
  "clean_stop": true
}
```

Initial `kv-recorder` metadata also writes:

- `samples_per_packet`
- `first_packet_id`
- `last_packet_id`
- `written_samples`

### Clock-domain fields (DA16)

Both the batch and streaming recorders also emit the first/last value of each
clock domain so offline tools can align the FPGA sample counter to wall-clock
time and estimate host↔FPGA drift:

- `fpga_timestamp_first`, `fpga_timestamp_last` — `SampleBlock::timestamp_start`
  (the FPGA hardware sample counter) of the first and last written block.
- `host_clock_first_ns`, `host_clock_last_ns` — `SampleBlock::host_time_ns`
  (host wall-clock, nanoseconds since the Unix epoch) of those same blocks, or
  `null` when the source did not stamp a wall-clock (synthetic/replayed data).

In the streaming (`KVRAW v2`) format the metadata is embedded in the file header
rather than a sidecar `recording.json`. The reserved JSON block is **1024 bytes**
(was 512 B): a fully-populated header with the DA16 clock-domain fields exceeds
512 B, and `finish()` would otherwise truncate it into invalid JSON. Readers
consume exactly `data_offset_bytes` (recorded in the header) before the sample
payload, so files written with either reserved size parse via their own offset.

The first implementation writes this fixed JSON shape with Rust standard library code only. If metadata grows more complex, introduce a proper structured JSON library instead of expanding ad hoc formatting.

## events.csv

CSV columns:

```text
host_time_ms,timestamp_start,event_type,value,message
```

Example rows:

```text
0,0,started,,
1024,30720,ttl_changed,1,
2050,61440,packet_missing,102,expected 102 observed 103
```

`ttl_changed` rows are derived from per-sample TTL transitions by
`ttl_change_events(&[SampleBlock])`, which emits a `TtlChanged` for the first
sample and every sample whose TTL-in word differs from its predecessor (DA1).
The fixed-block CLI paths (which retain blocks) feed these into `events.csv`;
streaming/benchmark paths skip them to avoid an unbounded event log.

## integrity.json

Integrity summary example:

```json
{
  "expected_packets": 1000,
  "observed_packets": 999,
  "missing_packets": 1,
  "crc_errors": 0,
  "timestamp_discontinuities": 0,
  "buffer_overflows": 0,
  "expected_samples": 1920000,
  "written_samples": 1918080
}
```

## benchmark.json

Benchmark summary example:

```json
{
  "measurement_kind": "simulator_estimate",
  "duration_seconds": 600.0,
  "channel_count": 64,
  "sample_rate": 30000.0,
  "expected_samples": 1152000000,
  "written_samples": 1152000000,
  "missing_packets": 0,
  "crc_errors": 0,
  "timestamp_discontinuities": 0,
  "byte_count": 2304000000,
  "average_write_mb_s": 3.84,
  "max_write_latency_ms": 12.5,
  "max_buffer_occupancy": 0.42,
  "cpu_percent_avg": null,
  "memory_mb_max": null
}
```

## log.txt

Human-readable operational log:

```text
[INFO] acquisition started
[WARN] missing packet expected=102 observed=103
[INFO] recorder flushed
[INFO] acquisition stopped cleanly
```

## Failure Handling and Finalization

A recording that fails partway through must never silently discard the data
acquired before the failure. In-vivo acquisitions are unreproducible, so the
recorder always finalizes whatever was captured.

### Streaming pipeline (DA12)

`run_streaming_pipeline` writes each block through a `StreamingRecorder` as it
arrives. The acquisition (producer) runs on its own thread; the recorder
consumer drains it on the caller's thread. On any error — a recorder write
failure, an integrity-check error, or a producer-reported acquisition error —
the pipeline:

1. sets a shared `stop_requested` flag and notifies the producer, so it stops
   reading hardware into a buffer no one is draining (no orphaned acquisition
   thread);
2. joins the producer thread;
3. calls `recorder.finish()` so `recording.kvraw` is flushed and its header is
   rewritten with the final block/sample counts instead of being left
   truncated.

A producer failure surfaces as `PipelineError::ProducerFailed { message,
blocks_acquired }`, where `blocks_acquired` reports how many blocks reached
disk before the failure.

### Fixed-block CLI paths (DA14)

The `simulator-record` and `rhd-smoke` commands acquire a bounded number of
blocks before writing. If a backend read fails mid-run, the blocks already
acquired are carried out of acquisition (rather than dropped) and a **partial
recording is finalized** — `recording.kvraw`, `recording.json`,
`integrity.json`, `events.csv`, `benchmark.json`, and `log.txt` are all written
from the partial data. The log is prefixed with:

```text
[WARN] acquisition ended early: backend read failed; partial recording finalized
```

The command still returns the underlying read error to the caller, but the
captured signal is preserved on disk. For long-running hardware acquisitions
prefer the streaming commands (`simulator-stream`, `benchmark`), which flush to
disk incrementally and keep memory bounded.

## Later Export Formats

Do not write these during acquisition in the first MVP:

- NWB
- MATLAB `.mat`
- SpikeInterface-compatible derived formats

Instead, write stable `kvraw` first, then convert offline.

Offline exporters preserve the real per-sample timestamps: the `.rhd` exporter
emits `block.timestamp_start.wrapping_add(sample_index)` (truncated to the
32-bit hardware domain) rather than a synthetic 0-based counter, and trailing
padding continues `last_ts.wrapping_add(i + 1)` so exported timestamps match the
acquired FPGA counter (DA10).

## Initial Rust Contract

The first Rust crate is `kv-recorder`.

Initial API:

```rust
write_recording(output_dir, &[SampleBlock]) -> Result<RecordingSummary, RecorderError>
```

Initial behavior:

- Validate all `SampleBlock` values before creating output files.
- Reject inconsistent device ID, sample rate, channel count, or samples per packet inside one recording.
- Write `recording.kvraw` as little-endian interleaved `i16` samples.
- Write minimal `recording.json` metadata.
- Write `integrity.json` from the acquisition integrity summary.
- Write `events.csv` from acquisition events such as start, stop, and missing packets.
- Write `log.txt` as a human-readable operator log.
- Write `benchmark.json` with an initial simulator/dev estimate.
- Return filesystem errors to the caller.

Real wall-clock write latency, buffer high-water marks, CPU, and memory metrics remain later additions.

## Intan `.rhd` Export Header (DA28)

The `.rhd` exporter (`kv-recorder::export_formats`) writes an Intan v2.0 header
ahead of the amplifier data. The filter fields in that header describe the
**analog front end**, not the digitiser, and downstream Intan tools (RHX,
NeuroScope) re-filter or estimate noise from them. Writing the digitiser Nyquist
frequency (`sample_rate / 2`) as the "upper bandwidth" therefore misreports the
hardware: a 30 kS/s capture claimed a 15 kHz analog corner when the headstage is
actually configured for 7.5 kHz.

The header now carries the **actual** amplifier configuration the application
programs into every RHD2000 headstage (`Rhd2000Registers::open_ephys_default`):

| Field | Value | Source |
|-------|-------|--------|
| DSP enabled | `1` (on) | `enable_dsp(true)` |
| DSP cutoff | `1.0 Hz` | `set_dsp_cutoff_freq(1.0)` |
| Lower bandwidth | `1.0 Hz` | `set_lower_bandwidth(1.0)` |
| Upper bandwidth | `7_500.0 Hz`, capped at Nyquist | `set_upper_bandwidth(7_500.0)` |
| Impedance test frequency | `1_000.0 Hz` | impedance run default |

These defaults live in `RhdFilterConfig::HARDWARE_DEFAULT` and are carried on
`ExportHeader::filter`, so a future caller that records the real per-recording
configuration can override them without touching the writer. The reported upper
bandwidth is clamped to Nyquist so a low-rate export never advertises a passband
wider than it can represent.
