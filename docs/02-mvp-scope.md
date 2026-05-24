# MVP Scope

The first MVP must work without real hardware.

## Goal

Build a hardware-independent acquisition platform that can use a simulator to run:

```text
64 channels x 30 kHz x 16-bit ADC x continuous acquisition
```

The software should acquire, buffer, display, record, and verify data using simulated device output.

First target operating system:

```text
Windows
```

First product focus:

```text
in vivo electrophysiology
```

## Non-Goals

The first MVP must not depend on:

- real FPGA bit file
- fixed register map
- fixed USB, Ethernet, or PCIe implementation
- final packet binary layout
- final CRC algorithm
- final timestamp frequency
- final ADC gain conversion
- firmware flashing or bit-file loading workflow

## Required Modules

| Module | MVP Responsibility |
| --- | --- |
| `kv-types` | Shared data structures and config. |
| `kv-core` | Acquisition state machine and backend orchestration. |
| `kv-simulator` | Simulated device data source. |
| `kv-buffer` | Non-blocking data buffering between producer and consumers. |
| `kv-recorder` | Stable raw recording output. |
| `kv-integrity` | Packet loss and continuity checks. |
| `kv-cli` | Run simulator acquisition, record, inspect, benchmark. |
| `kv-gui` | Rust native engineering UI, likely `egui` first, for waveform, TTL, buffer, and health display. |

## Acceptance Criteria

The MVP is accepted when it can:

1. Simulate 64 channels continuously at 30 kHz.
2. Emit 16-bit ADC samples.
3. Simulate 16-line TTL events.
4. Inject deterministic packet loss.
5. Detect missing packet IDs.
6. Write `recording.kvraw`.
7. Write metadata and event files.
8. Display 16 / 32 / 64 channel waveforms in a GUI prototype.
9. Report buffer occupancy and dropped packets.
10. Produce a benchmark report.
11. Run a two-hour continuous acquisition test without unbounded memory growth.
12. Keep recording healthy even if GUI refresh is slow.

## MVP Benchmark Ladder

Start small and increase duration:

```text
10-second smoke test
10-minute recorder test
2-hour endurance test
```

## First CLI Targets

```bash
kv sim start --channels 64 --sample-rate 30000
kv record --device simulator --duration 10m --output ./test-run
kv inspect ./test-run/recording.json
kv benchmark --channels 64 --sample-rate 30000 --duration 10m
kv replay ./test-run/recording.kvraw
```

## Success Definition

The first MVP is not just "data is flowing". It must answer:

```text
Was the data complete?
How much data was expected?
How much data was written?
Were packets dropped?
Did buffers overflow?
Was write speed sufficient?
```
