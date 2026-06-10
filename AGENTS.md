# Keyvast GUI Project Instructions

This repository is for the Keyvast acquisition software platform. Current hardware bit files, register maps, packet formats, and transport choices are not final, so the project must develop a hardware-independent software base first.

## Core Operating Style

1. Plan before major implementation.
2. Keep changes small, testable, and reversible.
3. Prefer simulator-first development until real FPGA interfaces are confirmed.
4. Treat documentation, verification, and benchmark output as part of the product.
5. Do not hardcode FPGA register addresses, CRC algorithms, timestamp clocks, ADC conversion factors, USB endpoints, Ethernet packet layouts, or channel maps before hardware confirmation.

## Karpathy-Style Foundation

Use `docs/00-karpathy-skills.md` as the bottom-level development method for this project.

The practical translation for Keyvast is:

- Start with the smallest end-to-end loop that can run.
- Use synthetic data and simulators before waiting for hardware.
- Inspect real outputs, not only internal abstractions.
- Build evaluation and benchmark fixtures early.
- Prefer simple, observable systems over clever designs.
- Replace one boundary at a time when moving from simulator to real hardware.

## Hardware-Independence Rule

All upper layers must depend on stable internal contracts, not on a specific bit file:

```text
DeviceBackend -> kv-core -> kv-buffer -> kv-recorder / kv-gui / kv-daemon
```

The first backend is `SimulatorBackend`. Later hardware backends can be added as:

```text
UsbBackend
EthernetBackend
PcieBackend
RealFpgaBackend
```

Upper layers should continue to consume the same data model.

## Documentation Order

When project behavior is unclear, update or consult these documents first:

1. `docs/00-karpathy-skills.md`
2. `docs/01-documentation-roadmap.md`
3. `docs/02-mvp-scope.md`
4. `docs/03-architecture.md`
5. `docs/04-data-model.md`
6. `docs/05-state-machine.md`
7. `docs/06-protocol-draft.md`
8. `docs/07-recording-format.md`
9. `docs/08-integrity.md`
10. `docs/09-simulator-spec.md`
11. `docs/10-benchmark-plan.md`
12. `docs/11-local-api.md`
13. `docs/12-confirmed-decisions.md`
14. `docs/13-glossary.md`
15. `docs/14-open-questions.md`
16. `docs/15-dev-handoff.md`

## Development Handoff Rule

This is a large project. Do not rely on chat memory alone.

At the start of a new AI session:

1. Read this `AGENTS.md`.
2. Read `README.md`.
3. Read `docs/15-dev-handoff.md`.
4. Check `git status --short`.
5. Run or inspect the latest verification commands before making risky changes.

Before ending a session after meaningful work:

1. Update `docs/15-dev-handoff.md`.
2. Record what changed, what was verified, what is next, and any blockers.
3. Keep confirmed product or hardware decisions in `docs/12-confirmed-decisions.md`.
4. Keep unresolved questions in `docs/14-open-questions.md`.

## Verification Standard

After meaningful implementation changes:

- Run the smallest relevant test first.
- Then run broader checks when available: format, lint, typecheck, build, benchmark, or GUI smoke test.
- Review the diff before declaring completion.
- Report any checks that could not be run.

## Security And Reliability

- Never hardcode secrets, tokens, or credentials.
- Validate all external inputs, file paths, and API requests.
- Avoid silent error handling.
- Record acquisition errors, dropped packets, write failures, and buffer overflows.
- GUI failure must not stop acquisition or corrupt recording output.
