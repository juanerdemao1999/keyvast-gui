# Acquisition State Machine

The acquisition lifecycle must be explicit. Each state defines which operations are allowed.

## States

```text
Idle
  |
  v
DeviceConnected
  |
  v
Configured
  |
  v
Acquiring
  |
  v
Stopping
  |
  v
Stopped
```

Any state can transition to:

```text
Error
```

## State Definitions

| State | Meaning |
| --- | --- |
| `Idle` | No active device connection. |
| `DeviceConnected` | Backend is open and device identity is known. |
| `Configured` | Device has valid acquisition config. |
| `Acquiring` | Device is actively producing data. |
| `Stopping` | Stop requested; system is draining and finalizing. |
| `Stopped` | Acquisition ended cleanly; output can be inspected. |
| `Error` | Acquisition cannot continue without recovery. |

## Allowed Operations

| State | Allowed Operations |
| --- | --- |
| `Idle` | scan, open simulator, open device |
| `DeviceConnected` | read info, configure, close |
| `Configured` | start, reconfigure, close |
| `Acquiring` | stop, read status |
| `Stopping` | wait, force stop if supported |
| `Stopped` | inspect report, start new run, close |
| `Error` | inspect error, export log, reset, reconnect |

## Disallowed Operations

While `Acquiring`, the system must not allow changes to:

- sample rate
- channel count
- enabled channel list
- samples per packet
- output directory
- backend transport

These require stopping and starting a new acquisition.

## Error Transitions

Move to `Error` when:

- device read fails repeatedly
- recorder cannot write
- output disk is full
- packet format cannot be parsed
- buffer overflow exceeds configured policy
- backend disconnects
- timestamp discontinuity violates configured policy

## Stop Semantics

Stop should be graceful by default:

1. request backend stop
2. stop accepting new blocks
3. drain buffer to recorder
4. flush metadata and logs
5. write final integrity report
6. transition to `Stopped`

Forced stop may skip draining but must record that the run was not clean.

## GUI Rule

GUI commands must go through this state machine. GUI widgets should be disabled when their operation is not valid for the current state.

