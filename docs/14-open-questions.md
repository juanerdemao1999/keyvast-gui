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
First hardware module is Opal Kelly XEM7310-A75.
First hardware transport uses FrontPanel over USB 3.0.
Host application should bundle the FrontPanel runtime DLL when packaging for Windows.
```

Unknown:

```text
Exact Windows installer/driver packaging steps
hotplug behavior
device serial number format
whether to auto-open the first XEM7310-A75 or require serial selection
```

Status:

```text
Needed for first hardware bring-up, but keep behind a hardware backend.
```

### 11. Real FPGA Packet Format

Known for first hardware bring-up:

```text
Use a Keyvast bitfile (default keyvast_260607_with_UART.bit; see kv_rhd::RHD_BITFILE_CANDIDATES). Pass the path at runtime via --bitfile or the GUI picker.
The FPGA design currently embeds an Intan Rhythm USB3-style data plane.
Expected FrontPanel data endpoint is BTPipeOut 0xA0.
Expected frame magic is 0xd7a22aaa38132a53.
Expected board id is 700 if the Rhythm endpoint map is unchanged.
```

Still unknown:

```text
whether the Keyvast bitfile changes any Rhythm endpoint behavior
CRC algorithm
TTL edge encoding
full physical channel mapping beyond the first two 32-channel headstages
```

Status:

```text
Use the Rhythm USB3 parser first, but keep it behind DeviceBackend.
```

### 12. ADC Conversion And Channel Map

Known:

```text
First live hardware target is up to two 32-channel RHD headstages.
For display scaling, follow the Open Ephys / Intan RHD convention.
Preserve raw data in recording unless a later product decision changes this.
```

Still unknown:

```text
physical channel ordering
reference channel behavior
```

Status:

```text
Needed for hardware bring-up and metadata, but not for the upper GUI contract.
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

## 2026-07-14 RHD Follow-ups

- **Hardware blocker:** `keyvast_260714_fifo.bit` intermittently emits corrupt/zero
  1024-byte BTPipe regions after otherwise valid acquisition. Failures reproduced in
  both GUI and CLI at strict frame boundaries (for example 256-frame blocks fail at
  samples 217/237; 128-frame blocks fail at sample 109). Host-side 2 KiB FIFO headroom,
  24 KiB chunking, 8 KiB chunking, and halving the logical block size did not eliminate
  it. Inspect the FPGA FIFO write/read CDC, circular-buffer wrap, `EP_READY`, overflow/
  underflow flags, and the semantics of WireOut `0x20`.
- Should FIFO counter layout become an explicit bitfile/backend capability? The verified
  `keyvast_260714_fifo.bit` build uses the full count at WireOut `0x20` and repurposes
  `0x26`, while the stock Intan Rhythm fallback historically used split LSW/MSW values.
- Make `flush_fifo()` fail closed on implausible or non-decreasing counts, propagate
  FrontPanel pipe errors, and enforce a short wall-clock timeout instead of relying on
  a 10,000-iteration cap. Throttle-override WireIn errors are now propagated, but flush
  PipeOut errors and no-progress detection remain.
- Represent Device startup as `Connecting -> Ready -> Streaming` in the GUI. The current
  `live_pipeline.is_some()` check displays `Connected/LIVE` before the RHD backend has
  opened or produced its first block, and synchronous thread join can freeze Stop if a
  driver call never returns.
- Reuse strict frame resynchronization for the final post-delay centering read, not only
  the per-delay scan analysis.
