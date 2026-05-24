# Data Model

The data model is the stable internal contract between simulator, core, buffer, recorder, GUI, daemon, and future hardware backends.

## SampleBlock

`SampleBlock` represents one contiguous block of samples from one device stream.

```rust
pub struct SampleBlock {
    pub device_id: String,
    pub stream_id: u32,
    pub packet_id: u64,
    pub timestamp_start: u64,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub samples_per_channel: usize,
    pub ttl_bits: u32,
    pub data: Vec<i16>,
}
```

## Data Layout

The initial internal layout is interleaved by sample:

```text
sample0_ch0, sample0_ch1, ... sample0_ch63,
sample1_ch0, sample1_ch1, ... sample1_ch63,
...
```

For a block:

```text
data.len() == channel_count * samples_per_channel
```

This layout is friendly for recording raw time-contiguous data and can be converted for GUI or analysis.

## DeviceConfig

```rust
pub struct DeviceConfig {
    pub device_id: String,
    pub backend: DeviceBackendKind,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub samples_per_packet: usize,
    pub enabled_channels: Vec<usize>,
    pub ttl_enabled: bool,
    pub ttl_line_count: usize,
}
```

## DeviceBackendKind

```rust
pub enum DeviceBackendKind {
    Simulator,
    Usb,
    Ethernet,
    Pcie,
}
```

Only `Simulator` is required for the first MVP.

First MVP defaults:

```text
sample_rate: 30000
channel_count: 64
ttl_line_count: 16
sample type: i16
layout: interleaved_by_sample
```

## AcquisitionState

Defined in `docs/05-state-machine.md`.

## AcquisitionEvent

```rust
pub enum AcquisitionEvent {
    Started {
        timestamp_host_ms: u64,
    },
    Stopped {
        timestamp_host_ms: u64,
    },
    TtlChanged {
        timestamp_start: u64,
        ttl_bits: u32,
    },
    PacketMissing {
        expected_packet_id: u64,
        observed_packet_id: u64,
        missing_count: u64,
    },
    BufferOverflow {
        dropped_blocks: u64,
        buffer_occupancy: f64,
    },
    RecorderError {
        message: String,
    },
}
```

## DeviceStatus

```rust
pub struct DeviceStatus {
    pub device_id: String,
    pub backend: DeviceBackendKind,
    pub connected: bool,
    pub configured: bool,
    pub acquiring: bool,
    pub sample_rate: f64,
    pub channel_count: usize,
    pub packet_rate_hz: f64,
    pub last_packet_id: Option<u64>,
    pub ttl_bits: u32,
    pub last_error: Option<String>,
}
```

## IntegritySummary

```rust
pub struct IntegritySummary {
    pub expected_packets: u64,
    pub observed_packets: u64,
    pub missing_packets: u64,
    pub crc_errors: u64,
    pub timestamp_discontinuities: u64,
    pub buffer_overflows: u64,
    pub expected_samples: u64,
    pub written_samples: u64,
}
```

## Open Questions

- Final ADC conversion from raw value to microvolts: TBD.
- Channel physical mapping: TBD.
- Multi-device sync identifiers: TBD.
- Timestamp tick source: TBD.
