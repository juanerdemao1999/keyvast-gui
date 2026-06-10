# Documentation Roadmap

This project starts with documentation because the hardware bit file is not confirmed. The documents define stable software contracts that should survive later FPGA changes.

## Phase 0: Foundation

These documents explain how the project works and how decisions are made.

| Document | Purpose |
| --- | --- |
| `00-karpathy-skills.md` | Bottom-level development method: simulator-first, measurable, simple, inspectable. |
| `01-documentation-roadmap.md` | Order of documents and how they relate. |
| `02-mvp-scope.md` | First hardware-independent MVP and acceptance criteria. |

## Phase 1: Stable Internal Contracts

These documents should be written before serious code.

| Document | Purpose |
| --- | --- |
| `03-architecture.md` | Module boundaries and data flow. |
| `04-data-model.md` | Shared internal types such as `SampleBlock`, events, config, status. |
| `05-state-machine.md` | Acquisition states and allowed transitions. |
| `06-protocol-draft.md` | Simulator packet draft and open FPGA questions. |
| `07-recording-format.md` | First stable on-disk format. |

## Phase 2: Verification And Simulation

These documents turn the architecture into testable behavior.

| Document | Purpose |
| --- | --- |
| `08-integrity.md` | Packet loss, CRC, timestamp, and buffer integrity checks. |
| `09-simulator-spec.md` | Simulator signal generation and fault injection. |
| `10-benchmark-plan.md` | Performance tests and acceptance thresholds. |

## Phase 3: External Control

These documents prepare the GUI and external integrations.

| Document | Purpose |
| --- | --- |
| `11-local-api.md` | Local daemon API for GUI, Python, MATLAB, or web clients. |
| `12-confirmed-decisions.md` | Current project decisions confirmed by the user. |
| `13-glossary.md` | Plain-language explanations for key technical terms. |
| `14-open-questions.md` | Unknowns separated by priority and whether they block implementation. |
| `15-dev-handoff.md` | Current implementation state, verification status, next steps, and AI handoff notes. |

## Working Rule

When implementation reveals a better contract, update the document first or in the same change. The docs are not decorative; they are the project memory.
