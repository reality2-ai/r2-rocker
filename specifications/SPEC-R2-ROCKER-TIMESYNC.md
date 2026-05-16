# SPEC-R2-ROCKER-TIMESYNC: Cross-Sensor Time Synchronisation

**Version:** 0.1 Draft
**Date:** 2026-05-16
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-DASHBOARD
**Amends:** SPEC-R2-ROCKER-DASHBOARD §8, SPEC-R2-ROCKER-SENSOR §10

---

## 1. Introduction

Earlier drafts placed the responsibility for clock correction
**entirely** at the dashboard: the sensor emitted raw monotonic
uptime as `ts_ms`, and the dashboard maintained a per-sensor offset
in memory used to convert sensor timestamps to wall-clock at
analysis time. That works, but it creates a hidden join:

* SD-card records (per `SPEC-R2-ROCKER-SENSOR` §6.2) contain raw
  uptime values that are meaningless without the dashboard's offset
  table.
* Any tool reading an `.bin` file directly (operator, post-mortem,
  external research code) needs to cross-reference a second file
  to interpret timestamps. Errors silently creep in when the wrong
  offset is applied.
* The "black-box capture of failure events" goal (preserve a usable
  record on the SD card even if the controller is destroyed) is
  weakened.

This specification moves the **application** of the offset to the
sensor and keeps the **estimation** of the offset on the dashboard
— the hybrid pattern. After this spec lands, every `ts_ms` on the
wire AND on disk is already in synchronised milliseconds, on a
shared timeline across all sensors in the deployment.

### 1.1 Scope

In scope:

* The sensor-side `clock_offset_ms` state (NVS-persistent) and the
  exact rule applied to every emitted `ts_ms`.
* A new dashboard → sensor event `r2.dash.set_clock_offset` that
  pushes incremental clock corrections.
* The dashboard's correction policy: when to issue an initial
  baseline (at calibration), when to issue a delta (drift exceeds
  threshold), and how the existing `sync_pulse` / `sync_pong` pair
  drives both.
* The semantic change to `ts_ms` across the wire and on disk:
  "synchronised milliseconds since the deployment's shared
  reference" rather than "monotonic uptime".

Out of scope:

* Multi-controller deployments (a second dashboard joining mid-
  experiment) — not yet supported.
* GPS / external time source — none assumed.
* Sub-millisecond precision — the wire-level resolution stays at
  1 ms; deployment-internal correlation accuracy is limited by
  network RTT jitter (~ a few ms over WiFi).

### 1.2 Terminology

RFC 2119 terms apply. Additional terms:

* **Uptime** (`u_ms`) — the sensor's monotonic milliseconds since
  power-on, as read from `esp_timer_get_time() / 1000` (or
  equivalent). Starts at 0 every cold boot.
* **Clock offset** (`clock_offset_ms`) — a signed integer
  (`i64`) NVS-persisted per sensor. Added to `u_ms` to produce
  every emitted `ts_ms` value.
* **Wall clock** — the dashboard's reference time. Implementation-
  defined (system monotonic + sync-to-NTP if available, or pure
  monotonic). Treated as the deployment's authoritative timeline.

---

## 2. Sensor behaviour

### 2.1 State

Each sensor MUST maintain `clock_offset_ms: i64`, NVS-persisted
under the key `clock_offset` (namespace `r2_rocker`). Default value
on first boot: **0**.

### 2.2 Applied rule

Every `ts_ms` value the sensor emits or persists MUST be computed
as:

```
ts_ms = u_ms + clock_offset_ms
```

This includes (without limitation):

* `r2.sensor.announce` (WIRE §3.1) — key 4
* `r2.sensor.acceleration` and `.batch` (§3.2 / §3.3)
* `r2.sensor.status` (§3.5) — key 1 (uptime_ms semantically becomes
  synchronised ms; the field name is preserved for compatibility,
  but its meaning shifts per this spec — see §5)
* `r2.sensor.event.log` (§3.8) — key 0
* `r2.sensor.sync_pong` (§3.7) — key 1 (sensor_ts_ms; the
  Cristian's-algorithm math relies on this carrying the same
  rule as everything else, NOT raw uptime — see §3.2)
* **SD-card records** per `SPEC-R2-ROCKER-SENSOR` §6.2 — `ts_ms`
  at offset 4..7

The arithmetic is wrapping `u64` addition followed by truncation
to `u32` for the on-wire 32-bit fields. A negative
`clock_offset_ms` shifts `ts_ms` backwards (used when the sensor's
NVS-stored offset is from a prior session and wall-clock has since
"moved on" in a sense that the dashboard wants to correct).

### 2.3 Handler: `r2.dash.set_clock_offset`

Per the new wire event in §4. On receipt, the sensor SHALL:

1. Read `delta_ms` (signed i64) from the payload.
2. Update `clock_offset_ms += delta_ms` in RAM.
3. Schedule a rate-limited NVS persist (at most once per second,
   same wear-bound policy as `last_acked_seq` per
   `SPEC-R2-ROCKER-SENSOR` §6.4).
4. Acknowledge by emitting a fresh `r2.sensor.status` frame within
   the next 200 ms so the dashboard sees the updated `ts_ms` and
   can confirm the correction took effect.

The handler MUST be idempotent only in the trivial sense (a delta
of 0 is a no-op). Replaying the same non-zero delta SHALL apply
twice — protocol guarantees against replay are out of scope (this
is an unauthenticated channel pre-Phase-5c, same trust posture as
OTA and remote-reset).

### 2.4 Boot behaviour

On cold boot:

1. Load `clock_offset_ms` from NVS (default 0 if absent).
2. Begin emitting `ts_ms` per §2.2 from the first frame onwards.

The sensor MUST NOT zero its offset on boot. The NVS-persisted
value carries forward; the dashboard's first `sync_pulse` after
reconnect will refine it via §3.

### 2.5 Effect on SD records

After this spec, `SPEC-R2-ROCKER-SENSOR` §6.2's record-format
field `ts_ms` carries synchronised milliseconds, not raw uptime.
Operators reading a `.bin` file directly may interpret `ts_ms`
values across sensors on a shared axis with no further joins,
subject to the caveats in §5 (offline drift, pre-first-sync
period).

---

## 3. Dashboard behaviour

### 3.1 Estimation loop (existing, unchanged)

The dashboard runs the Cristian's-algorithm sync loop already
defined in `SPEC-R2-ROCKER-DASHBOARD` §8:

```
T1 = dash wall-clock at send (dash_ts_ms in sync_pulse)
T3 = dash wall-clock at recv
T2 = sensor_ts_ms in sync_pong
rtt = T3 − T1
offset_estimate = T1 + (rtt / 2) − T2
offset_smoothed = α · offset_estimate + (1 − α) · offset_smoothed
                  where α = 0.2
```

Cadence: 1 Hz for the first 30 s after a sensor connects, then
every 30 s thereafter. Same as before.

Crucially, after this spec, `T2 = sensor_ts_ms` already includes
the sensor's applied `clock_offset_ms`. So `offset_smoothed` is
the **residual correction** needed on top of what the sensor is
already applying — not the absolute offset.

### 3.2 Correction policy (new)

The dashboard MUST push a `r2.dash.set_clock_offset` frame to a
given sensor when **any** of the following hold:

| Trigger | Action |
|---|---|
| **Initial calibration** — first sync after a sensor's TCP connect, once `offset_smoothed` has stabilised (≥ 5 sync rounds, std-dev of last 3 estimates < 5 ms) | Push the full smoothed offset (`delta_ms = round(offset_smoothed)`). The sensor's resulting offset becomes the baseline for this session. |
| **Drift threshold** — `|offset_smoothed|` ≥ 10 ms after a steady-state sync round | Push the smoothed offset as a delta. |
| **Manual recalibration** — operator clicks "Re-sync" on the device card (UI affordance, future) | Force a full re-sync sequence as if a fresh connect. |

After pushing a delta, the dashboard MUST reset its local
`offset_smoothed` to 0. Subsequent `sync_pong` replies will rebuild
the smoothed estimate from the now-corrected baseline.

If the sensor's `r2.sensor.status` acknowledgement (per §2.3 step 4)
is not received within 1 s of pushing a delta, the dashboard MAY
log a warning and retry **once**. Repeated failure SHOULD be
surfaced on the device card (operator-visible "time-sync error"
indicator — UI surfacing TBD).

### 3.3 Storage

The dashboard MUST log every `set_clock_offset` push (timestamp,
device_pk, delta_ms) so that post-experiment analysis can
reconstruct the offset history if needed (e.g. cross-sample
correlation across boundaries where a correction was applied).
Persistence format: append-only line in
`<data_root>/<experiment_id>/timesync.log`, JSON-per-line.

---

## 4. Wire protocol (new event)

### 4.1 `r2.dash.set_clock_offset`

Add a new event in `SPEC-R2-ROCKER-WIRE` §4. Suggested FNV slot
TBD by the WIRE catalogue update.

| CBOR key | Type | Description |
|---|---|---|
| 0 | int64 | `delta_ms` — signed milliseconds to **add** to the sensor's current `clock_offset_ms`. |

Direction: dashboard → sensor. Carried over the streaming TCP
socket (port 21042), same as `r2.dash.ack` and `r2.dash.sync_pulse`.

Response: an updated `r2.sensor.status` frame (no dedicated reply
event — keeps the catalogue small; status is already sent
spontaneously at 2 s cadence per `SPEC-R2-ROCKER-WIRE` §3.5).

### 4.2 No change to `sync_pulse` / `sync_pong`

The existing pair (WIRE §3.7 / §4.5) is unchanged. What changes
is the **meaning** of `sensor_ts_ms` (key 1 in `sync_pong`): it
now carries `u_ms + clock_offset_ms`, not raw uptime. This is
correct for Cristian's-algorithm purposes — the algorithm
computes the residual between dashboard wall-clock and the
sensor's currently-emitted timeline, which is exactly what we
want.

---

## 5. Caveats and edge cases

### 5.1 First boot / pre-first-sync

A fresh sensor with `clock_offset_ms = 0` and no prior sync will
emit `ts_ms = u_ms` (raw uptime, small numbers starting from 0).
SD records written before the first `set_clock_offset` round will
have these uptime-shaped values. After the dashboard pushes the
baseline correction, all subsequent records (and the dashboard's
view of the prior frames it already received) will be on the
synchronised timeline.

Tools reading the SD card SHOULD treat ts_ms values numerically
below ~`60_000` (one minute of uptime) as **potentially**
pre-sync; the dashboard's `timesync.log` (per §3.3) is
authoritative for distinguishing pre/post-sync regions.

### 5.2 Offline drift between sessions

A sensor that operates offline (no WiFi / no dashboard) for an
extended period continues to apply its last-known
`clock_offset_ms`, accumulating drift relative to wall-clock at
the rate of its local oscillator (typically ±50 ppm, ≈
180 ms/hour). On reconnect, the dashboard's first `sync_pulse`
round will measure and correct the accumulated drift.

For operator-supervised experiments (per the project's design
posture), the operator is expected to confirm dashboard
connectivity before starting a run.

### 5.3 Sensor reboot mid-experiment

A reboot resets `u_ms` to 0 but preserves `clock_offset_ms` from
NVS. So `ts_ms = 0 + clock_offset_ms = clock_offset_ms` —
post-reboot records continue on roughly the same timeline as
pre-reboot, modulo the few-seconds boot delay (during which no
records are written). The dashboard re-syncs immediately on TCP
reconnect.

### 5.4 ts_ms u32 overflow

`u_ms + clock_offset_ms` is computed in wider arithmetic and
truncated to `u32` for the existing wire fields. The u32 range
covers ~49.7 days; any analysis tool that may operate over a
longer continuous timeline MUST handle u32 wraparound. For
v0.1, this is acceptable — the longest planned run is a few
hours.

---

## 6. Implementation notes (non-normative)

* Firmware: add `r2-core/src/clock.rs` (or
  `firmware/.../sensor/src/clock.rs`) exposing a single
  `ts_now() -> u32` helper that all callers route through.
  Backing state: `AtomicI64` for `clock_offset_ms` (RAM)
  plus an NVS persister thread (debounced 1 Hz writes, same
  pattern as `last_acked_seq` persistence in §6.4).
* Wire-handler: extend the existing inbound-frame dispatcher
  (currently empty for dashboard → sensor commands except
  reset/OTA) to route the new event. Reuse the OTA listener's
  dispatch shape.
* Dashboard: extend the existing `sync_pulse` scheduler in
  `dashboard/src/main.rs` with the per-sensor state machine
  described in §3.2 (smoothed offset, stability counter,
  outstanding-correction tracking).

---

## 7. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-16 | 0.1 | Initial draft. Hybrid time-sync model: sensor applies `clock_offset_ms` (NVS-persistent), dashboard refines via existing sync_pulse/sync_pong and pushes corrections via the new `r2.dash.set_clock_offset` event. |
