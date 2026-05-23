# SPEC-R2-ROCKER-VIEWER-SENTANT: Browser viewer sentant

**Version:** 0.1 Draft
**Date:** 2026-05-23
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-CAPTURE, SPEC-R2-ROCKER-ACCESS, SPEC-R2-ROCKER-SENTANTS (sibling firmware-side spec), canonical R2-HIVE / R2-SENTANT.

---

## 1. Introduction

The rocker webapp (`webapp/index.html`) loaded in a paired
browser is, per AI-CONTEXT.md §"Webapp tab = R2 hive", an R2 hive
running on the browser substrate. The 2026-05-23 architectural
audit (`audits/2026-05-23-architectural-gaps.md` Finding C)
recorded that this hive existed as a name in the AI-CONTEXT but
not yet as code — the webapp used `r2-wasm` primitives (frame
decode, FNV, cert ops) but never instantiated the `R2Hive` event
loop.

This spec is the normative shape of the rocker-side hive's
viewer sentant — what events it subscribes to, what state it
maintains, what events it (will eventually) emit. The
implementation lives in
`crates/r2-wasm/src/rocker_viewer.rs` (sentant) +
`crates/r2-wasm/src/rocker_hive.rs` (wasm-bindgen wrapper).

This is the **browser counterpart** to SPEC-R2-ROCKER-SENTANTS
(the firmware-side hive). Both specs describe an ensemble of
sentants composed onto an `r2-engine::EventBus`; the difference
is the hardware substrate and which events each side originates.

### 1.1 Scope

In scope:

* The `DashboardViewerSentant` — class hash, subscriptions, per-
  sensor state shape, snapshot JSON schema.
* How the hive is constructed in the browser, what JS forwards
  into it, and what JS reads back.
* The migration path: v0.1 is observation-only (JS still owns UI
  rendering); v0.2 promotes the sentant's state to the primary
  source-of-truth for the UI; later (Tracks B+C) the operator
  plane migrates to R2 events end-to-end.

Out of scope:

* The wasm-bindgen / wasm-pack build pipeline. Build-system
  conventions live in `webapp/README.md`.
* Specific UI render logic — the spec only constrains the data
  shape the sentant produces, not how the operator's tabs render
  it.
* Cross-tab session management. Each browser tab is its own
  hive instance with its own bus.

### 1.2 Terminology

RFC 2119 terms apply.

* **Hive** — a process or tab running an `r2-engine::EventBus`.
  Per AI-CONTEXT §"Webapp tab = R2 hive" a browser tab is one
  hive.
* **Sentant** — an event-driven module registered on the bus,
  implementing `r2_engine::Sentant`.
* **Snapshot** — the read-only view of the sentant's internal
  state exposed via `R2RockerHive::peek_state()`.

---

## 2. Class identity

| Field | Value |
|---|---|
| Class identifier | `nz.ac.auckland.rocker.viewer` |
| Class hash (FNV-1a) | `fnv1a_32("nz.ac.auckland.rocker.viewer")` |
| Friendly name | `rocker-viewer` |
| Single-instance per hive | yes — one viewer sentant per browser tab |

---

## 3. Subscriptions

The viewer sentant subscribes to the sensor-event family the
dashboard publishes on `/r2`. Event hashes are FNV-1a-32 of
the dotted names per SPEC-R2-ROCKER-WIRE §2.

| Event | Direction (to hive) | Purpose for the sentant |
|---|---|---|
| `r2.sensor.announce` | sensor → controller → viewer | Initialise per-sensor record: hostname, fw_ver, has_cert, last_seq. |
| `r2.sensor.acceleration` | sensor → controller → viewer | Per-sample telemetry. Sentant maintains sample_count + last_ts_ms; chart data continues to be JS-driven in v0.1. |
| `r2.sensor.battery` | sensor → controller → viewer | Update battery_pct. |
| `r2.sensor.status` | sensor → controller → viewer | Update fsm_state. |
| `r2.sensor.capture.state` | sensor → controller → viewer | Update capture_state (0=idle, 1=calibrating, 2=recording) + capture_file. |
| `r2.peer.disconnected` | controller → viewer (controller-synthesised) | Drop the sensor from the snapshot. Lookup is by `device_pk_hex` (key 3); when absent the event is informational only. First migrated status notification under Tracks B+C — legacy `/ws/status type=peer_disconnected` JSON stays alive for one release for backward compat. |
| `r2.dash.ota.progress` | controller → viewer | OTA push lifecycle (uploading / applied / rejected / error). v0.1 of this sentant subscribes for `event_count` bookkeeping only; UI continues to render from the parallel `/ws/status type=ota` JSON message in this release. Per SPEC-R2-ROCKER-WIRE row 23. |
| `r2.dash.reset.progress` | controller → viewer | Sensor-reset lifecycle (requested / applied / error). v0.1 bookkeeping-only; UI rendering follows legacy JSON for one release. Per SPEC-R2-ROCKER-WIRE row 24. |
| `r2.dash.capture.progress` | controller → viewer | Capture lifecycle (start / mark / stop) — distinct from the per-sensor `r2.sensor.capture.state`; this event carries the controller's aggregate count of acknowledging peers. v0.1 bookkeeping-only. Per SPEC-R2-ROCKER-WIRE row 25. |
| `r2.dash.access.event` | controller → viewer | Access-flow notifications (request_pending / request_approved / request_denied / revoked). v0.1 bookkeeping-only. Per SPEC-R2-ROCKER-WIRE row 26. |
| `r2.dash.bootstrap.progress` | controller → viewer | BLE bootstrap progress events (Reset / Log / SensorFound / SensorConnected / Done / Error — flattened from the structured BootstrapEvent). v0.1 bookkeeping-only. Per SPEC-R2-ROCKER-WIRE row 27. |
| `r2.dash.device.alias.changed` | controller → viewer | Operator-set sensor alias updated. v0.1 bookkeeping-only; alias is also carried in the next `r2.sensor.announce` cycle so per-sensor state stays consistent. Per SPEC-R2-ROCKER-WIRE row 28. |

Implementations **SHOULD** ignore unknown event hashes silently
(per the bus's default dispatch). Adding new sensor events later
SHALL be additive.

Note on the v0.1 "bookkeeping-only" status of the six
`r2.dash.*` events: subscribing now ensures the sentant's
`event_count` is the canonical counter and prevents a future
slice (UI rendering migration) from needing to revisit the
subscription list. The actual UI continues to render from the
legacy `/ws/status` JSON channel until the webapp is migrated to
read state from `peek_state()` (Track D follow-up, separate
commit).

---

## 4. Per-sensor state

Keyed by `device_pk_hex` (64 ASCII characters, lowercase). Fields:

| Field | Type | Source event | Notes |
|---|---|---|---|
| `device_pk_hex` | str(64) | announce key 0 | Stable per device across reboots (NVS-persisted on the sensor side per SPEC-R2-ROCKER-SENSOR §3.1). |
| `hostname` | opt str | announce key 1 | Operator-visible label. |
| `fw_ver` | opt str | announce key 2 | semver + git short. |
| `has_cert` | bool | announce key 8 presence | True iff the announce carried a 147-byte DeviceCertificate (post-Track-A, see SPEC-R2-ROCKER-SENSOR §3.5). |
| `last_seq` | u64 | acceleration key 0 | Most-recent sample sequence. |
| `last_ts_ms` | u64 | acceleration key 1, status key 1 | Synchronised deployment milliseconds per TIMESYNC §2.2. |
| `battery_pct` | opt u8 (0..=100) | battery key 2 | |
| `fsm_state` | opt u8 (0..=9) | status key 0 | Per SPEC-R2-ROCKER-SENSOR §4.1.1. |
| `capture_state` | opt u8 (0..=2) | capture.state key 0 | 0=idle, 1=calibrating, 2=recording. |
| `capture_file` | opt str | capture.state key 1 | File name when recording. |
| `sample_count` | u64 | derived | Count of acceleration events seen since hive boot. Roll-over OK (`wrapping_add`). |

v0.1 sensors that don't carry their `device_pk` on each
non-announce event (acceleration / status / battery / capture.state
do not include key 0 in the firmware today) are scoped to the
**most-recently-announced** sensor — the sentant's internal
`last_pk` cursor. A future spec slice could thread `device_pk`
through every event for unambiguous routing; v0.1 trades that
precision for compatibility.

---

## 5. Snapshot JSON

`R2RockerHive::peek_state()` returns a JSON string with shape:

```json
{
  "event_count": <u64>,
  "sensors": [
    {
      "device_pk": "<64 hex>",
      "hostname": "<str>?",
      "fw_ver": "<str>?",
      "has_cert": <bool>,
      "last_seq": <u64>,
      "last_ts_ms": <u64>,
      "battery_pct": <u8>?,
      "fsm_state": <u8>?,
      "capture_state": <u8>?,
      "capture_file": "<str>?",
      "sample_count": <u64>
    },
    ...
  ]
}
```

Optional fields (`?` above) are omitted entirely when absent.

`event_count` increases monotonically (wrapping at u64) — a
diagnostic counter the operator can watch from devtools to
confirm the hive is consuming events.

`sensors[]` is in sorted-by-key (lex-ordered by `device_pk_hex`)
order; the JS-side renderer SHOULD NOT depend on any specific
order beyond stability across consecutive `peek_state()` calls.

---

## 6. Outbound events (future)

v0.1 of the viewer sentant emits no events — it's purely
observational. The follow-up slices and Tracks B+C will introduce
outbound events for operator actions (capture/start, identify,
device aliases, etc.) so the webapp's HTTP-and-JSON operator
plane migrates to R2-WIRE. When that lands this spec gains a §7.

The event-name design for the outbound side **MUST** align with
`SPEC-R2-ROCKER-BRIDGE.md` §3.1's outbound table so a future
two-TG split (Track E in
`/home/roycdavies/.claude/plans/sleepy-snuggling-tome.md`) does
not force a wire break — see that plan's "critical event-name
design call" note.

---

## 7. Conformance

A rocker webapp build conforms when:

1. `bootstrapHive()` constructs an `R2RockerHive` after WASM init.
2. Every R2-WIRE event arriving via `/r2` (and, for off-network
   loads, via the relay leg) is forwarded into the hive via
   `send_event(event_hash, payload)`.
3. The sentant's class hash matches §2; its subscriptions list
   matches §3.
4. `peek_state()` returns a JSON string conforming to §5 (fields
   present iff §4 says they are).
5. A hive panic / borrow-conflict during `send_event` does NOT
   take down the surrounding JS dispatch — the rocker webapp is
   defensive against the hive layer (try/catch around the call).
6. The hive instance is stashed on `window.rockerHive` so the
   operator can inspect via devtools without a separate exposure
   path. (This is a v0.1-only convenience; v0.2 may drop the
   global.)

---

## 8. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-23 | 0.1 | Initial draft. Subscriptions, per-sensor state, snapshot schema. Lands alongside `crates/r2-wasm/src/rocker_viewer.rs` and `rocker_hive.rs`. |
