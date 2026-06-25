# Recording Format

The first recording format should be boring, stable, and easy to inspect.

The recorder writes a directory, not a single complex container file.

## Output Directory

```text
run-YYYYMMDD-HHMMSS/
  recording.kvraw
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
