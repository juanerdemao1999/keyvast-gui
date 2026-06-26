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
