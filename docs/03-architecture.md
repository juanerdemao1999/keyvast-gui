# Architecture

Keyvast should be built as a hardware-independent acquisition platform. Real FPGA details live behind the lowest backend layer.

## High-Level Data Flow

```text
AcquisitionSource (kv-simulator / kv-rhd / ...)
    |
    v
kv-core
    |
    v
kv-buffer
    |--------------------|--------------------|
    v                    v                    v
kv-recorder          kv-gui               kv-daemon / kv-stream
    |
    v
kv-integrity / benchmark reports
```

## Module Responsibilities

| Module | Responsibility |
| --- | --- |
| `kv-types` | Shared data model, config, events, status, errors. |
| `kv-core` | Acquisition lifecycle, state machine, backend orchestration. |
| `kv-simulator` | Hardware-free signal source and fault injector. |
| `kv-driver` | Real hardware backends after bit file confirmation. |
| `kv-buffer` | Thread-safe buffering and fan-out to consumers. |
| `kv-recorder` | Raw data and metadata writing. |
| `kv-integrity` | Packet, timestamp, CRC, and sample-count checks. |
| `kv-cli` | Development and operations command-line interface. |
| `kv-gui` | Rust native engineering GUI for real-time display and health monitoring. |
| `kv-daemon` | Local API service for GUI, Python, MATLAB, and web clients. |
| `kv-stream` | WebSocket, TCP, or local streaming layer. |

## Backend Boundary

Every data source plugs into `kv-core` through a single small trait,
`AcquisitionSource` (defined in `kv-core/src/lib.rs`):

```rust
pub trait AcquisitionSource {
    type Error: fmt::Display;

    fn read_block(&mut self) -> Result<SampleBlock, Self::Error>;
}

// Any `FnMut() -> Result<SampleBlock, E>` is also a source, so closures and
// adapters can be used without a dedicated type.
impl<F, E> AcquisitionSource for F
where
    F: FnMut() -> Result<SampleBlock, E>,
    E: fmt::Display,
{ /* ... */ }
```

The trait is intentionally minimal: it only pulls the next `SampleBlock`. The
device **lifecycle** (open / configure / start / stop) is not part of the
trait — each source performs its own setup in its constructor and tear-down on
`Drop`, while `kv-core`'s pipeline owns the run loop and integrity checks. This
keeps `kv-core` agnostic of any specific transport.

Current implementations:

```text
kv-simulator   — synthetic neural data (used directly or via an FnMut closure)
kv-rhd         — Opal Kelly / Intan RHD board over FrontPanel (Windows)
```

Future transports (USB / Ethernet / PCIe variants, other FPGA boards) implement
the same `AcquisitionSource` trait.

The currently expected physical connector is USB Type-C, but the exact USB protocol, endpoint layout, and packet framing are still TBD.

## Threading Principle

The acquisition thread should only receive data and push it into a buffer.

It should not:

- render GUI
- write complex file formats
- run expensive conversions
- block on network clients
- perform long logging operations

Consumers should run independently:

```text
device reader -> ring buffer -> recorder
                           -> GUI
                           -> network stream
                           -> benchmark collector
```

Initial `kv-buffer` fan-out contract:

- `BlockBuffer` remains a small single-consumer FIFO for focused buffering checks.
- `FanoutBlockBuffer` registers named consumers such as `recorder` and `preview`.
- Each consumer has its own bounded queue, cursor, occupancy, and dropped-block count.
- A slow preview consumer drops only its own oldest blocks; recorder consumption is not advanced or truncated by preview lag.
- Blocks are shared internally so fan-out does not copy the raw sample vector for every consumer.

## Failure Isolation

GUI failure should not stop recording.

Network clients should not slow acquisition.

Recorder errors must move acquisition into a visible error path and produce logs.

Buffer overflow must be counted, reported, and included in integrity output.

## Hardware-Specific Decisions

These must remain outside upper layers until confirmed:

- FPGA registers
- packet binary format
- CRC algorithm
- timestamp source and tick rate
- ADC gain conversion
- channel mapping
- physical transport
- firmware or bit-file loading process
