# Integrity Checks

Integrity checks tell users whether the recorded data is complete and trustworthy.

## Required Checks

| Check | MVP Requirement |
| --- | --- |
| Packet continuity | Detect missing packet IDs. |
| Timestamp continuity | Detect unexpected timestamp jumps. |
| Sample count | Compare expected and written samples. |
| Buffer overflow | Count blocks dropped by software buffering. |
| Recorder health | Detect write failures and incomplete flushes. |
| CRC | Simulator placeholder now; real algorithm TBD. |

## Packet Loss Detection

For every packet:

```text
expected_packet_id = previous_packet_id + 1
```

If observed packet ID is larger:

```text
missing_count = observed_packet_id - expected_packet_id
```

Record:

- expected packet ID
- observed packet ID
- missing count
- host time
- device timestamp
- buffer occupancy
- recorder write speed when available

## Timestamp Continuity

For simulator packets:

```text
expected_timestamp = previous.timestamp_start + previous.samples_per_channel
```

Timestamp rules for real FPGA are TBD.

FPGA timestamps are 32-bit hardware counters stored zero-extended in
`timestamp_start`; at 30 kHz they wrap roughly every 39.7 h. Continuity is
therefore compared in the 2^32 domain via `timestamps_contiguous(expected,
observed)` so a routine counter wrap is **not** misreported as a discontinuity
(DA5). The acquisition path resets the FPGA sample-timestamp counter on the
first start (`reset_fpga_timestamp`, pulsing `WIRE_IN_RESET_RUN`) so a lazily
started session begins from zero rather than a stale counter (DA41).

For the Rhythm hardware backend, a per-sample timestamp that does not increment
by one is a **recoverable** anomaly, not a fatal error (DA2): the parser resyncs
against the previous sample and tallies the event in `BlockParseReport`. A
corrupt frame magic is likewise tallied (`bad_magic_frames`) and decoded in
place. `backend.rs` emits one summary `log::warn!` per block that parsed with
non-zero counts so operators can see transient framing faults without losing the
session.

`SampleBlock::validate_against_ttl_lines()` validates not only the summary
`ttl_bits` but every per-sample TTL word against the configured `ttl_line_count`
mask, rejecting out-of-range words with `TtlPerSampleOutOfRange { sample_index, .. }`
(DA39).

## Sample Count

For a clean observed stream:

```text
observed_packets * channel_count * samples_per_channel
```

When packet gaps are detected, expected samples should include the implied missing packets so the report can answer how much data should have existed versus how much was actually observed or written.

Written samples should match the sum of blocks successfully written to disk.

## Reporting

Integrity output must be machine-readable and human-readable:

- `integrity.json` for tools
- `log.txt` for operators
- GUI counters for live acquisition

## Initial Rust Contract

The first Rust crate is `kv-integrity`.

Initial API:

```rust
check_blocks(&[SampleBlock]) -> Result<IntegrityReport, IntegrityError>
```

Initial report fields:

- `summary: IntegritySummary`
- `packet_gaps: Vec<PacketGap>`
- `timestamp_discontinuities: Vec<TimestampDiscontinuity>`

For simulator blocks, `expected_packets` means the packet range implied by observed packet IDs plus detected gaps. `written_samples` is the sum of validated observed sample values, and `expected_samples` includes samples implied by missing packets.

CRC, recorder health, host-time annotations, buffer occupancy, and write-speed annotations remain later additions.

## GUI Health Indicators

The GUI should show:

- dropped packet count
- CRC error count
- timestamp discontinuity count
- buffer occupancy
- recording status
- last error
