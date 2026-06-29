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

## Rhythm USB Block Parsing (Hardware Backend)

The `kv-rhd` backend parses fixed-length Rhythm USB data blocks. Each frame is
self-positioned (fixed stride, leading `RHYTHM_HEADER_MAGIC`), so a single
transient framing fault must **not** tear down acquisition. The parser is
therefore non-fatal on recoverable anomalies (DA2):

- A frame whose header word does not equal `RHYTHM_HEADER_MAGIC` is decoded in
  place; the parser logs a warning and increments `bad_magic_frames`.
- A sample timestamp that is not exactly one greater than the previous sample
  is resynced against the *previous* sample (`prev.wrapping_add(1)`), not the
  block's first sample, so one jump is counted once instead of cascading. The
  count lands in `timestamp_discontinuities`.

Both counts are returned per block as a `BlockParseReport`:

```rust
pub struct BlockParseReport { pub bad_magic_frames: u32, pub timestamp_discontinuities: u32 }
pub fn parse_rhythm_data_block_reporting(..) -> Result<ParsedRhythmBlock, RhythmParseError>;
// parse_rhythm_data_block(..) is the unchanged wrapper that discards the report.
```

Only structural errors that make a frame undecodable (truncated buffer, invalid
config) remain hard `RhythmParseError`s.

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
