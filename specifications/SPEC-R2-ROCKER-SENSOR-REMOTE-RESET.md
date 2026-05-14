# SPEC-R2-ROCKER-SENSOR-REMOTE-RESET: Remote Sensor Reset

**Version:** 0.1 Draft
**Date:** 2026-05-14
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-DASHBOARD

---

## 1. Introduction

Sensors in the production rig will be housed in sealed enclosures — the
hardware reset button on the DevKitC is not accessible once the
sensor is in service. When a sensor falls into a degraded state (sim
fallback per `SPEC-R2-ROCKER-SENSOR-HEALTH`, hung sender, ADXL355
silent), the operator needs a way to power-cycle it from the
dashboard without opening the enclosure.

This specification mirrors the existing **Phase 9-light OTA push**
pattern (per `SPEC-R2-ROCKER-DASHBOARD` and
`r2-esp::ota_tcp`): a thin TCP listener on the sensor, a dashboard
HTTP endpoint, a button on the device card.

### 1.1 Scope

In scope:

* Firmware TCP listener that accepts a single reset command and calls
  `esp_restart()`.
* Dashboard HTTP endpoint that pushes the reset command to a sensor by
  address.
* A "Reset Sensor" button on each device card in the WASM viewer.

Out of scope:

* TG-signed reset commands — deferred to Phase 5c (#24) alongside
  TG-signed OTA. The unauthenticated path here is consistent with
  the existing unauthenticated OTA push (Phase 9-light); both are
  acceptable while the deployment is single-trust-group and
  operator-supervised.
* Selective subsystem restarts (only-sender, only-BLE). The reset is a
  full `esp_restart()` reboot. Subsystem restarts are a future
  refinement if needed.
* BLE-based reset (works when WiFi is dead). Out of scope here; the
  sensor is reachable via WiFi by definition once it has appeared on
  the dashboard.

### 1.2 Terminology

RFC 2119 terms apply.

* **Reset** — a full software reboot of the sensor via
  `esp_restart()`. Equivalent to pressing the physical RST button.
  The OTA-rollback gate is reset; the firmware re-runs
  `Adxl355::new()` (so a sim-fallback sensor MAY recover to real
  source on the next boot if the underlying cause was a transient
  init race).

---

## 2. Wire protocol

### 2.1 Port and framing

The reset listener runs on **TCP port 21044** (one above the OTA port
21043). The choice mirrors the OTA listener architecturally — separate
port for separate purpose, no multiplexing onto the streaming socket.

| Step | Direction | Bytes | Meaning |
|---|---|---|---|
| 1 | client → sensor | 1 (`0x10`) | `CMD_RESET` command byte |
| 2 | sensor → client | 1 + 2 + N | `status(1) + len_le(2) + message` — same response shape as OTA |
| 3 | sensor | — | Sleep 100 ms, then `esp_restart()` |

The 100 ms sleep MUST happen after the status response is written so
the dashboard sees the OK before the TCP connection is forcibly closed
by the reboot.

`status` values:

| Value | Meaning |
|---|---|
| `0x00` | OK — reboot scheduled |
| `0x01` | Error — reboot not scheduled (reserved for future use; not emitted by v0.1) |

The command byte `0x10` is distinct from OTA's `CMD_START = 0x01` and
`CMD_QUERY = 0x02` so the same listener implementation pattern (read
one byte, dispatch) is unambiguous if the two ever share a port in
future.

### 2.2 Connection lifetime

The reset listener serves connections **sequentially** (one at a
time), same as the OTA listener. Each connection:

1. Accepts the connection.
2. Reads one command byte with a 5 s timeout. Unknown bytes → log + close.
3. On `CMD_RESET`: log the peer address, write the OK response, sleep
   100 ms, call `esp_restart()`.

A reset command in flight MUST NOT race with an OTA in progress
(`r2_esp::ota_tcp::ota_in_progress()`). If OTA is active, the reset
SHOULD return status `0x01` with message "OTA in progress" and **not**
reboot. (v0.1 MAY simply log + skip the gate and let `esp_restart`
abort the OTA — the bootloader rollback will recover the old image.
v0.2 adds the explicit gate.)

---

## 3. Dashboard

### 3.1 HTTP endpoint

`POST /api/sensor/{addr}/reset`

Body: none.

`addr` MAY be `ip` or `ip:port` (the streaming-socket port is stripped;
21044 is always used).

Response: JSON

```json
{ "ok": true|false, "status_byte": 0x00, "message": "..." }
```

Mirrors `POST /api/ota/{addr}` JSON shape. Errors return HTTP 502 with
`{"ok": false, "error": "..."}`.

### 3.2 WebSocket broadcast

The dashboard MUST broadcast on `/ws/status`:

```json
{ "type": "reset", "phase": "requested|applied|error", "target": "10.42.0.103:21044", "message": "..." }
```

Phases:

* `requested` — sent immediately on receipt of the POST, before the
  TCP roundtrip.
* `applied` — sent after the sensor returns status 0x00.
* `error` — sent on any failure (connect, read, status non-zero).

These mirror the OTA `type: ota` broadcasts.

### 3.3 UI

Each device card MUST show a **Reset Sensor** button alongside the
existing **Update Firmware** button (same action row, styled as a
secondary action — amber outline rather than the primary-action style
of Update Firmware).

Click behaviour: single click fires the POST. Button shows transient
state (`↻ Resetting…` while waiting, then `✓ Reset` or `✗ Failed`,
returning to `↻ Reset Sensor` after 4 s) — mirrors
`requestFirmwareUpdate` in the WASM viewer.

No modal confirmation in v0.1 — the reset is recoverable (a healthy
sensor reboots and reappears in seconds) and the button label is
unambiguous. A later revision MAY add a confirm dialog if accidental
presses become a real problem during measurement runs.

### 3.4 Text vocabulary

Per `feedback_ui_no_protocol_jargon`:

| Allowed | Forbidden |
|---|---|
| "Reset Sensor" | "Restart device"/"Reboot" — confusing with browser-side reset; sensor is the right noun |
| "↻ Resetting…" | "Sending CMD_RESET" |
| "✓ Reset" | "Reboot scheduled" |

---

## 4. Implementation notes (non-normative)

* Firmware: new module `crates/r2-esp/src/reset_tcp.rs` mirroring
  `ota_tcp.rs` structure. Listener spawned from the carrier
  firmware's `main.rs` in the WiFi-up branch alongside
  `ota_tcp::start_listener()`.
* Dashboard: `reset_push_handler` in `dashboard/src/main.rs`
  paralleling `ota_push_handler`. Same router-level `.route()`
  wiring. New helper `push_reset(target)` similar to
  `push_firmware(target, body, sha)`.
* UI: `requestReset(addr)` JS function in `webapp/index.html`. New
  CSS class `btn-reset` (amber outline). Button placed in the existing
  `.device-actions` row.
* Both carrier firmware trees implement this; the spec is
  carrier-agnostic.

---

## 5. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-14 | 0.1 | Initial draft. TCP listener on port 21044, CMD_RESET=0x10, mirrors OTA pattern. |
