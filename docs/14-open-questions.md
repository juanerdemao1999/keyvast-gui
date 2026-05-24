# Open Questions

This document records what is still unclear. The goal is to separate real unknowns from confirmed decisions so the project can keep moving without accidentally hardcoding future hardware assumptions.

## Current Summary

Nothing here blocks continuing documentation or starting the simulator-first Rust workspace.

The first implementation can proceed with these assumptions:

```text
Windows
Rust workspace
simulator first
64 channels
30 kHz
i16 samples
interleaved_by_sample layout
16 TTL lines
kvraw recording
egui-style Rust native debug GUI later
```

The questions below should be answered gradually.

## Tier 1: Decide Before Or During First Code Skeleton

These affect early code shape, but we can choose conservative defaults if the user does not know yet.

### 1. samples_per_packet

Question:

```text
How many samples per channel should one simulated packet contain?
```

Recommended first default:

```text
64 samples per channel per packet
```

Why:

- At 30 kHz, 64 samples is about 2.13 ms of data.
- Packet rate is about 468.75 packets per second.
- It is small enough for real-time display and large enough to avoid excessive overhead.

Status:

```text
Open, but not blocking. Use 64 unless changed.
```

### 2. TTL Representation

Question:

```text
Should TTL be represented as current state, edge events, or both?
```

Recommended first design:

```text
SampleBlock.ttl_bits = TTL state at block start
events.csv = TTL changes as timestamped events
```

Why:

- GUI can show the current TTL state easily.
- Recorder can preserve exact TTL changes as events.
- This avoids stuffing too much detail into every sample.

Status:

```text
Open, but not blocking. Use both state and events.
```

### 3. Timestamp Meaning

Question:

```text
For the simulator, should timestamp_start be sample count or clock tick?
```

Recommended first design:

```text
timestamp_start = sample index of the first sample in the block
```

Example:

```text
packet 0 timestamp_start = 0
packet 1 timestamp_start = 64
packet 2 timestamp_start = 128
```

Why:

- Simple to verify.
- Independent of hardware clock.
- Easy to convert to seconds: `timestamp_start / sample_rate`.

Status:

```text
Open for real hardware, but simulator should use sample count.
```

### 4. CLI Binary Name

Question:

```text
Should the command be named kv, kv-acq, or keyvast-acq?
```

Recommended first name:

```text
kv-acq
```

Why:

- Clearer than `kv`.
- Shorter than `keyvast-acq`.
- Does not imply this is only a GUI.

Status:

```text
Open. Needs user preference before CLI docs become final.
```

### 5. Output Folder Naming

Question:

```text
How should recording runs be named?
```

Recommended first format:

```text
run-YYYYMMDD-HHMMSS
```

Example:

```text
run-20260522-153000
```

Status:

```text
Open as a product decision, but implemented as the first CLI default.
Current CLI behavior: omit --output to create run-YYYYMMDD-HHMMSS using a UTC timestamp.
```

## Tier 2: Needed Before Serious Simulator And Recorder Work

These do not block the workspace skeleton, but they should be settled before benchmark work becomes meaningful.

### 6. Simulator Signal Amplitudes

Question:

```text
What ADC count ranges should simulated noise, spikes, LFP, and EMG use?
```

Recommended first defaults:

```text
baseline noise: small random signal
LFP: slow sine-like component
spike: short negative-positive transient
EMG interference: burst noise
```

Exact amplitudes:

```text
TBD
```

Status:

```text
Open, but can start with plausible synthetic values.
```

### 7. Spike Simulation Density

Question:

```text
How many channels should contain simulated spikes, and how often?
```

Recommended first default:

```text
8 active spike channels out of 64
random low-rate spike events
deterministic with seed
```

Status:

```text
Open.
```

### 8. Buffer Capacity

Question:

```text
How many seconds of data should the ring buffer hold before overflow?
```

Recommended first default:

```text
5 seconds for recorder path
1 second or less for GUI preview path
```

Why:

- Recorder needs protection from short disk stalls.
- GUI should drop old preview frames rather than delaying acquisition.

Status:

```text
Open, but can use defaults.
```

### 9. Recorder Flush Policy

Question:

```text
How often should the recorder flush metadata and logs?
```

Recommended first design:

```text
write raw data continuously
flush metadata/events periodically and on stop
always write final integrity report on clean stop
```

Status:

```text
Open.
```

## Tier 3: Hardware Questions For Later

These should not block current development.

### 10. USB Details

Known:

```text
Future connector is USB Type-C.
Transfer is USB-based.
```

Unknown:

```text
USB speed
USB class or vendor protocol
Windows driver path
endpoint layout
packet framing
hotplug behavior
device serial number format
firmware or bit-file version reporting
```

Status:

```text
Not needed for simulator-first work.
```

### 11. Real FPGA Packet Format

Unknown:

```text
real header layout
packet counter width
timestamp tick frequency
CRC algorithm
sample packing
endianness
TTL edge encoding
channel mapping
```

Status:

```text
Not needed now. Keep behind DeviceBackend.
```

### 12. ADC Conversion And Channel Map

Unknown:

```text
ADC gain
raw-to-microvolt conversion
physical channel ordering
headstage mapping
reference channel behavior
```

Status:

```text
Not needed for raw simulator MVP.
```

## Tier 4: Product Features To Delay

These are valuable, but deciding them now can distract from the acquisition core.

Delay:

- real-time spike sorting
- NWB writing during acquisition
- DeepLabCut integration
- video synchronization
- closed-loop stimulation
- multi-device synchronization
- full commercial UI styling
- Python / MATLAB live control
- Open Ephys plugin compatibility

First get this stable:

```text
simulator -> core -> buffer -> recorder -> integrity -> CLI report
```

## Questions For The User

Only these need user attention soon:

1. Should the CLI command be `kv-acq`, `kv`, or `keyvast-acq`?
2. Is `64 samples per channel per packet` acceptable as the first default?
3. Is it acceptable to record TTL as both current state and timestamped change events?
4. Is it acceptable to start with a 5-second recorder buffer and a 1-second GUI preview buffer?
5. Is `run-YYYYMMDD-HHMMSS` acceptable as the first recording folder format?
