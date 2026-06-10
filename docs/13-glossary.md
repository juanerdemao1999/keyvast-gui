# Glossary

This document explains project terms in plain language.

## Rust Workspace

A Rust workspace means one big project folder contains multiple small Rust packages.

For this project, that would look like:

```text
51_keyvast_gui/
  Cargo.toml
  crates/
    kv-types/
    kv-simulator/
    kv-core/
    kv-buffer/
    kv-recorder/
    kv-cli/
    kv-gui/
```

Why use it:

- `kv-types` only defines shared data structures.
- `kv-simulator` only pretends to be a device.
- `kv-recorder` only writes files.
- `kv-gui` only shows the interface.

This avoids putting all code into one huge file or one huge package.

## i16

`i16` means a signed 16-bit integer.

Plain meaning:

```text
one sample is a small whole number
range: -32768 to 32767
size: 2 bytes
```

Why it fits electrophysiology acquisition:

- ADC raw data is commonly stored as 16-bit values.
- 64 channels x 30 kHz x 2 bytes is manageable.
- It is easy to write directly to disk without conversion.

Example:

```text
-123
0
456
12000
```

These are raw values, not yet converted to microvolts.

## interleaved_by_sample

This describes how multi-channel samples are arranged in memory or on disk.

For 4 channels, interleaved by sample means:

```text
time0_ch0, time0_ch1, time0_ch2, time0_ch3,
time1_ch0, time1_ch1, time1_ch2, time1_ch3,
time2_ch0, time2_ch1, time2_ch2, time2_ch3
```

For 64 channels, each time point stores channel 0 through channel 63 together.

Why use it first:

- It matches how many acquisition packets naturally arrive.
- It is simple to write continuously.
- It keeps all channels from the same time point next to each other.

## kvraw

`kvraw` is the first simple Keyvast raw data file format.

Plain meaning:

```text
recording.kvraw = raw binary sample values
recording.json = metadata explaining how to read the raw values
events.csv = TTL and event records
integrity.json = packet loss and data integrity report
```

Why not write a complex format first:

- Acquisition should prioritize stable writing.
- Complex formats can be exported later.
- If something goes wrong, raw files are easier to inspect and recover.

## TTL

TTL lines are digital on/off signals used for synchronization.

Examples:

- stimulus start
- camera frame marker
- behavior event
- external trigger

First MVP decision:

```text
16 TTL lines
```

That means the software can track 16 independent digital signals.

## Python / MATLAB API

There are two possible ways for Python or MATLAB to use the system.

First, simple offline reading:

```text
Rust records kvraw files
Python or MATLAB reads the files later
```

Later, live control through a local service:

```text
Python -> kv-daemon -> Rust acquisition core
GUI -> kv-daemon -> Rust acquisition core
```

Current decision:

```text
do files and CLI first
do live API later
```

