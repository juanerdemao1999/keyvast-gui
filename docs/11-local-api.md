# Local API Draft

The local daemon lets GUI, Python, MATLAB, and web clients control acquisition without touching hardware directly.

This API is a draft. It should be implemented after the simulator, core, buffer, recorder, and integrity path are stable.

Current decision: first make stable recording files and CLI tools. Live Python / MATLAB control can come later through this API or by reading recorded files.

## Principles

- GUI talks to `kv-daemon`, not directly to hardware.
- Acquisition can continue if GUI crashes.
- API responses must include current state and last error when useful.
- Streaming waveform data must not block recording.

## Device Endpoints

```http
GET /device/list
GET /device/status
POST /device/open
POST /device/close
```

## Acquisition Endpoints

```http
POST /acquisition/configure
POST /acquisition/start
POST /acquisition/stop
GET /acquisition/status
```

## Recording Endpoints

```http
POST /recording/start
POST /recording/stop
GET /recording/status
GET /recording/latest
```

## Streaming Endpoints

```http
GET /stream/waveform
GET /stream/events
```

The actual transport can be WebSocket, TCP, or local IPC. This is TBD.

## Status Shape

Conceptual response:

```json
{
  "state": "Acquiring",
  "device_id": "simulator-0",
  "backend": "simulator",
  "sample_rate": 30000.0,
  "channel_count": 64,
  "last_packet_id": 123456,
  "missing_packets": 0,
  "buffer_occupancy": 0.12,
  "recording": true,
  "last_error": null
}
```
