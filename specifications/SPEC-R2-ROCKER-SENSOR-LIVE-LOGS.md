# SPEC-R2-ROCKER-SENSOR-LIVE-LOGS: Live Log Tail over WiFi

**Version:** 0.1 Draft
**Date:** 2026-05-16
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-DASHBOARD

---

## 1. Introduction

During bring-up and field debugging, the sensor's serial console
(`info!`/`warn!`/`error!` lines from `EspLogger`) is the most direct
source of truth. The sensors are mounted on the rig in places where a
USB cable is awkward or impossible — by the time a sensor is on a
spinning carrier, serial is unreachable.

This specification defines a thin TCP fan-out of the sensor's log
stream over WiFi, plus a dashboard WebSocket proxy that lets each
device card in the WASM viewer open a live log panel for that sensor.
The on-device serial output is unchanged; the new path is purely
additive.

### 1.1 Scope

In scope:

* Firmware: a TCP listener on the sensor that broadcasts every
  `log::Record` to all connected clients, in addition to the normal
  `EspLogger` UART path.
* Dashboard: an HTTP endpoint `/ws/logs/{addr}` that upgrades to
  WebSocket and pipes the sensor's TCP log stream back to the browser.
* Webapp: a "↓ Logs" button on each device card that opens an
  inline panel rendering the live tail.

Out of scope:

* Authentication / TG-signed log subscriptions — the listener is bound
  to the experiment's WiFi only; access is implicit in being on the
  network. Aligned with the existing unauthenticated OTA / reset paths
  (deferred to Phase 5c, #24).
* Historical log persistence on the dashboard — the panel is a live
  tail with a bounded in-browser buffer. Operators who want a durable
  record copy lines out manually.
* `log!()` filtering or level controls per-client — every connected
  client sees the same stream at the global `log::set_max_level`.

### 1.2 Terminology

RFC 2119 terms apply.

* **Log listener** — the on-device TCP server on port 21046 that
  fans out `log::Record`s to subscribed sockets.
* **Log proxy** — the dashboard HTTP→WS endpoint that bridges a
  browser to a sensor's log listener.
* **Subscriber queue** — the bounded per-client channel between the
  capturing logger and the TCP writer. Overflow drops lines silently;
  it MUST NOT block the logger.

---

## 2. Architecture

```
log!() ─► CapturingLogger ─┬─► EspLogger ─► UART / USB-Serial-JTAG
                           │
                           └─► per-client mpsc ─► TCP 21046
                                                  ▲
                                                  │
   browser ◄── WS /ws/logs/{addr} ── dashboard ───┘
```

The capturing logger wraps `esp_idf_svc::log::EspLogger`. Every
`Log::log` call:

1. Delegates to the inner `EspLogger` (UART output preserved verbatim).
2. Formats the record as `"{level:>5} {target} {args}\n"`.
3. Iterates over registered subscriber senders and `try_send`s the
   line to each, retaining senders whose receiver is still alive.

Subscriber senders use `sync_channel(SUBSCRIBER_QUEUE)`. Full queues
drop lines (the line is discarded, the subscriber is kept); disconnected
queues are pruned.

The TCP listener accepts on port 21046 and spawns a small writer thread
per client. Each writer registers a `SyncSender<String>` with the
logger and writes received lines to its socket until the socket fails
or the writer thread exits.

---

## 3. Firmware: `r2-esp::log_tcp`

### 3.1 Public API

```rust
pub fn start_listener();
```

Idempotent on the listener side. Replaces a call to
`EspLogger::initialize_default()`; firmware MUST call exactly one of
the two during boot.

`start_listener()`:

1. Calls `log::set_logger(&CAPTURING_LOGGER)`. If another logger has
   already been installed, the call is ignored (no panic).
2. Sets `log::set_max_level(LevelFilter::Info)`.
3. Spawns the `log-tcp` listener thread on port 21046.

### 3.2 Wire Format

The TCP stream is plain UTF-8 text. Each `log::Record` becomes one line
terminated with `\n`:

```
 INFO some::module the message text
```

The first six bytes are the right-padded level (`"TRACE"`, `"DEBUG"`,
` "INFO"`, ` "WARN"`, `"ERROR"`) followed by a space, then the
`record.target()`, a space, and `record.args()`. No timestamp is
included — the browser stamps arrival time if it needs one.

On connect the firmware MAY send a one-line banner
(`"-- r2-rocker log stream --\n"`) before live data.

### 3.3 Backpressure

The capturing logger MUST NOT block on slow clients. The bounded queue
(`SUBSCRIBER_QUEUE = 128` lines) absorbs short bursts; on overflow,
incoming lines are dropped silently for that subscriber only. The
firmware-side socket writer is allowed to block on the kernel-side
socket buffer, but its task is per-client and isolated from the log
hot path.

### 3.4 Resource limits

Listener thread stack: 4 KiB.
Per-client writer thread stack: 4 KiB.
Per-client queue: 128 lines.

There is no enforced cap on the number of simultaneous clients —
operationally we expect ≤2 (one operator + one developer). If the
sensor runs out of memory or task slots the listener accept call will
fail; the existing client(s) are unaffected.

---

## 4. Dashboard: `/ws/logs/{addr}` proxy

### 4.1 HTTP route

```
GET /ws/logs/{addr}
```

`addr` is a sensor IP (or `ip:port`; the port suffix is stripped before
use). On upgrade, the dashboard opens a TCP connection to
`{ip}:21046` and pipes each newline-terminated line received from the
sensor to the WebSocket as a text frame.

### 4.2 Lifecycle

* The dashboard MUST apply a 3 s connect timeout. On timeout or refused
  connection it MUST send one diagnostic text frame
  (`"[ws/logs] connect to <target> failed: <reason>\n"`) and close.
* Either side closing the connection MUST cause the other side to be
  closed cleanly. There is no reconnection logic on the dashboard;
  the browser re-opens the panel if the operator wants to reconnect.

### 4.3 No interpretation

The proxy is byte-transparent: it does not parse, filter, or
re-format. Backpressure between the sensor TCP socket and the WebSocket
relies on tokio's natural read/write coupling.

---

## 5. Webapp: per-card log panel

### 5.1 UI

Each device card in the Devices view has a tertiary "↓ Logs" button
(slate outline) next to the existing "⬆ Update Firmware" and "↻
Reset Sensor" buttons. Clicking it:

1. Opens a `WebSocket` to `/ws/logs/{key}` (where `key` is the card's
   IP).
2. Expands an inline `.device-log-panel` below the action row.
3. Changes the button label to "↑ Logs" and adds an `.open` class for
   highlight.

Clicking again closes the WS and collapses the panel.

### 5.2 Buffer behaviour

The panel maintains a rolling 500-line buffer per sensor (in-memory
on the `sensors` map, not the DOM). On render the panel is auto-
scrolled to the bottom. Lines beyond 500 are discarded oldest-first.

The buffer is not preserved across page reload; that is acceptable
for a debugging affordance.

### 5.3 No protocol jargon

The button reads "Logs", the panel header is not protocol-named. Per
project convention, user-facing strings do not include the words
"TCP", "WebSocket", "log_tcp", etc.

---

## 6. Security model

The log stream contains diagnostic text only; no key material is ever
fed through `log!()`. Subscribers on the LAN can read all logs from
any sensor that is online. This matches the threat model already
accepted for the unauthenticated OTA and reset listeners (single
trust group, operator-supervised network). Hardening to TG-signed
subscriptions is folded into the same Phase 5c migration (#24).

---

## 7. Versioning

| Date       | Ver | Change                                                 |
|------------|-----|--------------------------------------------------------|
| 2026-05-16 | 0.1 | Initial draft. TCP listener on port 21045, /ws/logs proxy, per-card panel in webapp. |
| 2026-05-18 | 0.2 | Listener port moved to **21046** — 21045 collides with canonical R2 Console / GraphQL (R2-TRANSPORT §5, R2-CONSOLE §3.2). See audits/2026-05-18-post-v0.1.0-conformance.md Finding F. |
