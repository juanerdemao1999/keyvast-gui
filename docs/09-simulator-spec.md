# Simulator Spec

The simulator is the first device backend and the primary test fixture for the software stack.

## Default Configuration

```text
device_id: simulator-0
channels: 64
sample_rate: 30000 Hz
sample_type: i16
samples_per_packet: TBD default
ttl_lines: 16
```

## Signal Components

The simulator should support these components:

- baseline noise
- LFP-like low-frequency oscillation
- spike-like transient events
- EMG-like burst interference
- TTL state changes
- packet counter
- timestamp counter

## Determinism

The simulator must support a fixed seed:

```bash
kv record --device simulator --seed 1234
```

The same seed and config should generate repeatable packet IDs, TTL events, packet loss, and signal patterns.

## Fault Injection

The simulator should be able to inject:

- random packet loss
- deterministic packet loss by packet ID
- timestamp jumps
- CRC failures
- noise bursts
- delayed packets

Example:

```bash
kv record --device simulator --drop-packet 102 --duration 10s
```

Expected result:

```text
integrity: missing packet 102
```

## Scaling Targets

The simulator should support:

- 64 channels x 30 kHz
- 128 channels x 30 kHz
- 256 channels x 30 kHz

Higher channel counts are benchmark targets, not first MVP acceptance requirements.

## Backend Contract

The simulator must map its packet output into `SampleBlock` so all upper layers can be built before real hardware exists.
