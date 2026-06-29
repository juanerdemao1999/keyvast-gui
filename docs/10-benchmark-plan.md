# Benchmark Plan

Benchmarks verify that the acquisition stack can keep up with continuous data.

## Primary MVP Benchmark

```text
64 channels x 30 kHz x 16-bit x 2 hours
```

Required outputs:

- `recording.kvraw`
- `recording.json`
- `integrity.json`
- `benchmark.json`
- `log.txt`

## Short Development Benchmarks

Use shorter runs during development:

| Name | Config | Duration |
| --- | --- | --- |
| smoke | 64 channels x 30 kHz | 10 seconds |
| recorder | 64 channels x 30 kHz | 10 minutes |
| stress-128 | 128 channels x 30 kHz | 10 minutes |
| stress-256 | 256 channels x 30 kHz | 10 minutes |
| endurance | 64 channels x 30 kHz | 2 hours |

When neither `--blocks` nor `--duration` is given, the CLI now acquires
`DEFAULT_BLOCKS = 1000` blocks instead of a single block, so an unparameterized
run produces a meaningful, non-trivial recording (DA31).

## Metrics

Each benchmark should report:

- run duration
- channel count
- sample rate
- theoretical sample count
- actual written sample count
- missing packet count
- CRC error count
- timestamp discontinuities
- buffer high-water mark
- average write speed
- maximum write latency
- CPU average when available
- memory maximum when available

## Initial Simulator Output

The first `kv-acq simulator-record` benchmark output is an explicit simulator/dev estimate, not a real hardware timing result.

Initial `benchmark.json` fields:

- `measurement_kind: "simulator_estimate"`
- `duration_seconds` based on written samples, channel count, and sample rate
  (the *recorded* signal duration, derived from `written_samples`, not the
  requested run length — DA32)
- `requested_duration_seconds` — the duration the operator asked for (`null`
  when the run was bounded by `--blocks` rather than `--duration`), so a short
  or truncated run is distinguishable from its target (DA32)
- `channel_count`
- `sample_rate`
- `expected_samples`
- `written_samples`
- `missing_packets`
- `crc_errors`
- `timestamp_discontinuities`
- `byte_count`
- `average_write_mb_s` from bytes over estimated written-sample duration
- `max_write_latency_ms: null`
- `max_buffer_occupancy: null`
- `cpu_percent_avg: null`
- `memory_mb_max: null`

Later benchmark runners should replace nulls with measured values and label the measurement kind accordingly.

## GUI Impact Test

Run benchmark twice:

```text
GUI closed
GUI open with 64 channels displayed
```

Recording health must not depend on GUI refresh speed.

## Acceptance Thresholds

Initial thresholds:

- no unbounded memory growth
- no recorder backlog growth during normal disk conditions
- zero missing packets when fault injection is disabled
- injected packet loss must be detected exactly
- `recording.kvraw` byte count must match written sample count

Exact CPU and memory thresholds are TBD after the first implementation.
