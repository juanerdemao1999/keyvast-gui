# Keyvast Acquisition Platform

This repository is starting as a hardware-independent acquisition software base for Keyvast.

Current project folder name:

```text
51_keyvast_gui
```

The FPGA bit file, register table, real transport, and final packet format are not confirmed yet, so the first goal is to build and document the software platform around a simulator.

## First Principle

Do not wait for hardware to build the upper software stack.

Build this first:

```text
simulated data -> acquisition core -> buffer -> recorder -> GUI -> benchmark
```

Add this later:

```text
real driver -> real packet parser -> register control -> hardware validation
```

## Start Here

Read these documents in order:

1. `AGENTS.md`
2. `docs/00-karpathy-skills.md`
3. `docs/01-documentation-roadmap.md`
4. `docs/02-mvp-scope.md`
5. `docs/03-architecture.md`
6. `docs/04-data-model.md`
7. `docs/05-state-machine.md`
8. `docs/06-protocol-draft.md`
9. `docs/07-recording-format.md`

Then continue with:

- `docs/08-integrity.md`
- `docs/09-simulator-spec.md`
- `docs/10-benchmark-plan.md`
- `docs/11-local-api.md`
- `docs/12-confirmed-decisions.md`
- `docs/13-glossary.md`
- `docs/14-open-questions.md`
- `docs/15-dev-handoff.md`

For day-to-day AI handoff, read `docs/15-dev-handoff.md` after `AGENTS.md` and this README. It records the latest implementation state, verification commands, next work, and blockers.

## MVP Target

The first MVP should run without real hardware:

```text
64 channels x 30 kHz x 16-bit ADC
continuous acquisition
real-time display
raw recording
packet-loss detection
benchmark report
```

Confirmed first-stage assumptions:

- First target OS: Windows.
- First GUI direction: Rust native engineering GUI, with `egui` as the initial candidate.
- First product focus: in vivo electrophysiology.
- First simulated device: 64 channels, 30 kHz, 16-bit signed integer samples.
- First TTL target: 16 TTL lines.
- Future physical connector: USB over Type-C connector.
- Live Python / MATLAB API: later phase; first make stable files and CLI.

## Design Rule

Upper layers must consume stable internal types such as `SampleBlock` and `DeviceStatus`. The first backend is `SimulatorBackend`; real USB, Ethernet, PCIe, or FPGA backends should be added later behind the same boundary.
