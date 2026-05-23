# SPEC-R2-ROCKER-SENSOR-IDENTIFY: Identify Sensor

**Version:** 0.1 Draft
**Date:** 2026-05-22
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-DASHBOARD, SPEC-R2-ROCKER-WIRE

---

## 1. Introduction

A production rig may host many sensors (the design target is up to a
few dozen). When an operator needs to physically locate one — to
swap a battery, re-seat a probe, or replace a unit — the dashboard
shows which device needs attention by alias, but the operator still
has to walk to the rack and pick the right enclosure out of an
indistinguishable cluster.

The **Identify** feature lets the operator point the dashboard at a
specific sensor and have that sensor's RGB LED switch to a solid
white "find me" state until the operator dismisses it (or a 60-second
watchdog auto-clears it, so a forgotten Identify doesn't drain the
LiPo).

This spec describes a single command, an LED overlay, and a button.
It deliberately reuses the existing R2-WIRE inbound dispatch channel
rather than adding a new TCP listener — the command is small
(one CBOR map) and frequency is operator-driven (one click), so the
existing streaming socket is sufficient.

### 1.1 Scope

In scope:

* A new R2-WIRE event hash `r2.dash.identify_set` flowing
  dashboard → sensor.
* A new LED state `Identify` (solid white) that overrides every
  other LED state except none (it is the highest priority overlay).
* A 60-second sensor-side watchdog: if no `identify_set off`
  arrives within 60 s of `identify_set on`, the sensor MUST revert
  to its prior LED state.
* A new operator-plane cmd event `r2.dash.cmd.identify` on `/r2`
  (WIRE row 34) that the controller forwards to the sensor on its
  streaming socket as `r2.dash.identify_set`.
* A toggle button on each Devices-tab device card that turns
  Identify on/off, with a matching dashboard-side 60-second
  auto-revert.

Out of scope:

* Authenticated identify commands — deferred alongside Phase 5c
  TG-signed commands (#24). The unauthenticated channel here is
  consistent with the existing unauthenticated reset and OTA
  commands; all three become signed together.
* "Identify all sensors" / fleet-wide Identify. v0.1 is one-sensor
  at a time. (Multiple cards toggled at once still works, but no
  bulk button is exposed — Identify is a *locate the one I want*
  operator aid, not an attention-getting alarm.)
* Custom Identify colours / patterns per sensor. Solid white is the
  one allowed Identify visual.

### 1.2 Terminology

RFC 2119 terms apply.

* **Identify state** — the LED state in which the WS2812 emits
  solid white at full duty cycle.
* **Identify on / off** — the boolean payload of
  `r2.dash.identify_set`. The sensor's identify state mirrors the
  most recently received value, subject to the 60-second watchdog.

---

## 2. Wire protocol

### 2.1 Event hash

```
EVT_DASH_IDENTIFY_SET = fnv1a_32(b"r2.dash.identify_set")
```

Direction: dashboard → sensor, on the sensor's streaming socket
(same channel as `r2.dash.sync_pulse`, `r2.dash.ack`, etc.).

### 2.2 Payload

CBOR map with one key:

| Key | CBOR type | Meaning |
|---|---|---|
| `0` | unsigned integer | `0` = off, any non-zero value = on |

Encoded form for `on=true`:
```
A1            ; map(1)
   00         ; unsigned(0)
   01         ; unsigned(1)
```

Encoded form for `on=false`:
```
A1            ; map(1)
   00         ; unsigned(0)
   00         ; unsigned(0)
```

A malformed payload (wrong CBOR type, missing key, extra keys) MUST
be treated as `off` by the sensor — a corrupt command MUST NOT
accidentally light the LED.

### 2.3 Acknowledgement

None. The sensor's response is visible — the LED changes — and the
dashboard cannot meaningfully retry a missed identify without
disturbing the rest of the operator's mental model. A dropped frame
means the operator presses the button again.

---

## 3. Sensor behaviour

### 3.1 LED priority

Identify is the **highest-priority** LED overlay. The render order
(from bottom to top) MUST be:

1. Capture state (Idle / Calibrating / Recording — slow heartbeat,
   pulse, tick respectively).
2. Sim-fallback indicator (per `SPEC-R2-ROCKER-SENSOR-HEALTH`).
3. Low battery rhythm (per `SPEC-R2-ROCKER-SENSOR`).
4. OTA strobing white.
5. **Identify** — solid white.

When Identify is on, the LED MUST be solid white regardless of
underlying state. Low-battery rhythm is suppressed (the operator is
already at the sensor — they don't need an extra cue).

### 3.2 Auto-revert watchdog

If the sensor receives `identify_set on` and does not receive
`identify_set off` within **60 seconds** ± 5 s, it MUST revert to
its prior LED state automatically.

The watchdog protects against the operator forgetting to switch
Identify off (or a dashboard restart between the on and the off).
Solid white is the most power-hungry LED state, and an overlooked
Identify on a sensor running on LiPo could drain the battery by
~5 % per hour.

A subsequent `identify_set on` MUST restart the watchdog from
zero.

### 3.3 Persistence

Identify state MUST NOT persist across sensor reboot. A power cycle
or watchdog reset clears Identify. The operator can re-trigger it
from the dashboard if needed.

---

## 4. Dashboard

### 4.1 Operator-plane event

`r2.dash.cmd.identify` (WIRE row 34) on `/r2`.

Payload (CBOR map):

```
{ 0: req_id (u32), 1: addr (text — `ip` or `ip:port`), 2: on (bool) }
```

`addr` MAY be `ip` or `ip:port`; the streaming-socket port is used
regardless. The controller finds the matching peer by IP and queues
a `r2.dash.identify_set` frame on the sensor's streaming TCP
channel (fire-and-forget).

Response: `r2.dash.cmd.response` correlated by `req_id` carrying
`status: "ok"` iff the streaming-socket queue accepted the frame;
`status: "err"` with a `message` (e.g. `"no such peer"`,
`"peer queue closed"`) otherwise.

### 4.2 Dashboard-side auto-revert

The dashboard (or the viewer sentant in the webapp) SHOULD set a
60-second timer when the operator clicks Identify on, and
automatically emit `r2.dash.cmd.identify` with `{2: false}` when
it elapses. This is belt-and-braces alongside the sensor-side
watchdog (§3.2): if the dashboard restarts mid-window the
sensor-side watchdog still fires; if the sensor drops the off
command the dashboard re-sends one when the operator clicks again.

A subsequent on-click MUST restart this timer.

### 4.3 UI

Each device card MUST show a third action button alongside Update
Firmware and Reset Sensor, labelled **💡 Identify**. It is a
toggle:

* Idle state: same calm slate outline as the Logs button.
* Active state: filled white background, dark text, soft glow —
  mirroring the LED state on the bench.

Click behaviour:

* Click while idle: button switches to active, emit
  `r2.dash.cmd.identify {2: true}` in the background, 60-second
  auto-off timer starts.
* Click while active: button switches to idle, emit
  `r2.dash.cmd.identify {2: false}`, auto-off timer cleared.

The button MUST be hidden on viewer-role builds (same gating as
Update Firmware and Reset Sensor — Identify changes physical
sensor state and is therefore controller-only).

### 4.4 Text vocabulary

Per `feedback_ui_no_protocol_jargon`:

| Allowed | Forbidden |
|---|---|
| "Identify" | "Locate" / "Beacon" / "Find" — operator-friendly word that maps to a single visible effect |
| "💡 Identifying…" (while on) | "LED ON" / "White" |
| Tooltip: "Light this sensor's LED solid white so you can find it in the rack" | Anything mentioning "identify_set" or "fnv1a" |

---

## 5. Implementation notes (non-normative)

* Firmware: new event hash constant + parser in `wire.rs`; new
  `LedState::Identify` variant + render arm in `led.rs`; new
  `set_identify(on)` method on `LedHandle` storing an
  `AtomicBool`; new dispatch arm in `sender.rs::dispatch_inbound`.
  Watchdog implemented by storing the `Instant` of the last
  `identify_set on` and clearing the AtomicBool when 60 s have
  elapsed.
* Dashboard: new event hash constant; `encode_identify_set(on)`
  helper; `identify_handler` async fn matching the
  `reset_push_handler` shape; new route registration.
* Webapp: new `.btn-identify` element on each `R2DeviceCard`;
  `toggleIdentify()` async method on the card class; per-sensor
  `identifyOn` boolean carried on the sensor object so a card
  re-render preserves the visual.

---

## 6. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-22 | 0.1 | Initial draft. r2.dash.identify_set command, LedState::Identify, 60s sensor + dashboard watchdog, toggle button on Devices card. |
