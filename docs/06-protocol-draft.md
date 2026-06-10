# Protocol Draft

This is a simulator protocol draft, not the final FPGA protocol.

The goal is to give the upper software layers realistic packet metadata before the bit file and transport details are confirmed.

## Packet Shape

Conceptual packet:

```text
packet_header
packet_id
timestamp
channel_count
sample_count
ttl_bits
payload
crc
```

## Fields

| Field | Type | Meaning |
| --- | --- | --- |
| `magic` | `u32` | Simulator packet marker. |
| `version` | `u16` | Simulator protocol version. |
| `header_size` | `u16` | Header length in bytes. |
| `packet_id` | `u64` | Monotonic packet counter. |
| `timestamp_start` | `u64` | Timestamp of first sample in packet. |
| `sample_rate` | `f64` | Samples per second per channel. |
| `channel_count` | `u16` | Number of channels in payload. |
| `samples_per_channel` | `u16` | Number of samples per channel in payload. |
| `ttl_bits` | `u32` | TTL state at packet start or edge summary. The first MVP uses 16 TTL lines inside this field. |
| `payload_bytes` | `u32` | Payload size. |
| `crc` | `u32` | Simulator checksum placeholder. |

## Payload Layout

Initial simulator payload uses little-endian signed 16-bit values:

```text
i16 sample0_ch0
i16 sample0_ch1
...
i16 sample0_chN
i16 sample1_ch0
...
```

Expected payload size:

```text
channel_count * samples_per_channel * 2
```

## Packet ID Rule

Packet IDs are monotonic and increase by one.

Missing packet example:

```text
100
101
103
```

Expected integrity output:

```text
missing packet: 102
```

## Timestamp Rule

For the simulator, `timestamp_start` is a sample counter.

If packet `N` has `samples_per_channel = 64`, then packet `N + 1` should start at:

```text
previous.timestamp_start + 64
```

Real hardware timestamp units are TBD.

## Open Hardware Questions

These are intentionally not fixed here:

- final binary packet header
- endian format from FPGA
- CRC algorithm
- timestamp tick frequency
- packet size
- sample packing format
- TTL edge encoding
- multi-device sync markers
- USB, Ethernet, or PCIe framing
