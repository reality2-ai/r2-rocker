# SPEC-R2-ROCKER-SENSOR-HEALTH: Sensor-Health Indicators

**Version:** 0.1 Draft
**Date:** 2026-05-14
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-DASHBOARD

---

## 1. Introduction

A sensor that streams synthetic data (because its ADXL355 failed to
initialise) is **operationally indistinguishable** from a healthy one
at the network layer: it sends `r2.sensor.acceleration` frames at the
configured rate, the dashboard charts them, and the operator has no
way to know the samples are not measurements of the real world.

This specification adds a first-class **health signal** so that:

* The sensor emits a structured `data_source` field on every
  `r2.sensor.status` frame, distinguishing real ADXL355 readings from
  simulator output.
* The physical RGB LED has a dedicated indication for the degraded
  state.
* The dashboard surfaces the degraded state with a coloured dot and an
  explanatory text label per the `feedback_a11y_indicators` rule
  (pattern + colour + text — never colour alone).

The intent is **operator-visible**: a sensor housed in a sealed
enclosure that has fallen back to simulator on boot must be
distinguishable from a healthy one by anyone glancing at the
dashboard, without needing to read the version string or dig into
serial logs.

### 1.1 Scope

In scope:

* The wire-protocol extension for `r2.sensor.status` (a new CBOR key).
* The firmware health-state machine and its physical-LED indication.
* The dashboard's rendering of the degraded state on the device card
  (WASM viewer).
* Behaviour on boot, on recovery (init success after a retry), and on
  loss (init fail after previously being real).

Out of scope:

* The root-cause of ADXL355 init failure (timing race, miswiring,
  power) — investigated separately.
* Remote reset / power-cycle of the sensor — see
  `SPEC-R2-ROCKER-SENSOR-REMOTE-RESET.md` (TBD).
* Per-axis health (per-axis miscompare, saturation) — future work.

### 1.2 Terminology

RFC 2119 terms apply. Additional terms:

* **Real source** — samples produced by reading the ADXL355 over SPI;
  `Adxl355::new()` returned `Ok` and `read_xyz_lsb()` succeeds
  per-sample.
* **Sim source** — samples produced by `crate::sim::AccelSim` because
  either (a) `Adxl355::new()` returned `Err` at sender start, or (b) a
  per-sample `read_xyz_lsb()` errored and the sender substituted a
  simulated value for that sample (the per-sample path is logged but
  does not change `data_source`; see §3.2).
* **Degraded** — the sensor is producing frames at the configured
  rate but the source of those frames is the simulator. The
  network-layer behaviour is unchanged.

---

## 2. Health states

Two health states, complementing the existing FSM states in
`SPEC-R2-ROCKER-SENSOR` §4.1:

| Health | Description |
|---|---|
| `HEALTHY` | Streaming live, real ADXL355 source |
| `DEGRADED_SIM` | Streaming live, simulator source (ADXL355 init failed at sender start) |

`HEALTH` is **orthogonal** to the FSM state — a sensor can be
`STREAMING_LIVE` with `HEALTHY` source, or `STREAMING_LIVE` with
`DEGRADED_SIM` source. It is **not** a separate FSM state; the FSM
remains the existing 10-value enum defined in
`SPEC-R2-ROCKER-SENSOR` §4.1.1.

`DEGRADED_SIM` is **latched at sender start**. Once the sender has
fallen back to the simulator, it does not attempt to re-initialise the
ADXL355 during the same boot; recovery requires a reset. Section 5
covers recovery.

---

## 3. Wire protocol

### 3.1 Extension to `r2.sensor.status`

Add CBOR key `7` (`data_source`) to the `r2.sensor.status` event
defined in `SPEC-R2-ROCKER-WIRE` §3.5. The full updated key table:

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint8 | `state` — FSM state per `SPEC-R2-ROCKER-SENSOR` §4.1.1 |
| 1 | uint32 | `uptime_ms` |
| 2 | uint32 | `samples_total` |
| 3 | uint32 | `samples_acked` |
| 4 | uint8 | `sd_pct_used` |
| 5 | uint16 | `rate_hz_active` |
| 6 | uint8 | `range_active` |
| **7** | **uint8** | **`data_source` — 0 = real, 1 = sim. New in this spec.** |
| 10 | uint8 | (optional) `error_code` |

Backwards compatibility: receivers that do not understand key 7 MUST
silently ignore it (already required by the CBOR-deterministic
encoding rule in `SPEC-R2-ROCKER-WIRE` §1.3). Senders that emit key 7
MUST emit it on every `r2.sensor.status` frame (i.e. it is not
optional once supported), so the dashboard never has to fall back to
"unknown" once a sensor is known to support it.

A sensor implementing this spec MUST emit `data_source` on every
`r2.sensor.status` frame.

### 3.2 Per-sample sim substitution

If the sender is in real source (key `7` = 0) but a single
`read_xyz_lsb()` call errors, the firmware MAY substitute a simulator
sample for that single tick (logged as `[ADXL355] read failed`) and
continue in real source. This per-sample fallback does **not** change
`data_source`. If per-sample errors persist (an implementation-defined
threshold; suggested: ≥ 5 consecutive errors over ≥ 50 ms), the sender
SHOULD escalate to `data_source = sim` and remain there until reset.

This rule keeps `data_source` a stable, latched signal — operators
should see it transition rarely, not flicker.

### 3.3 fw_ver

The firmware build identifier MUST continue to embed the `-sim`
segment in `fw_ver` (per `firmware/esp32-s3/<carrier>/src/sender.rs`
`build_fw_ver()`) whenever `data_source = sim`. Two signals (one in
the announce, one in status) is redundant by design: the announce
fires once at connect, status fires every 2 s; either is sufficient
to drive the indicator.

---

## 4. Physical LED indication

### 4.1 New state

Add to the LED state enum (`firmware/esp32-s3/<carrier>/src/led.rs`,
matched in `SPEC-R2-ROCKER-SENSOR` §4.1):

| Wire value | State | LED indication |
|---|---|---|
| **10** | `STREAMING_DEGRADED_SIM` | **Purple, slow pulse (0.5 Hz)** — symmetric to `STREAMING_LIVE`'s heartbeat but distinguishable by colour and rhythm |

The choice of purple is deliberate: it is distinct from green (live)
and yellow (catch-up) and avoids re-using red (which already means
"fatal — manual reset required") or orange (which means "low
battery"). The slow pulse distinguishes it rhythmically from the
heartbeat patterns of the healthy streaming states, per the
`feedback_a11y_indicators` "pattern carries info alongside colour"
rule.

### 4.2 Overlay precedence

The existing overlay rules in `SPEC-R2-ROCKER-SENSOR` §4.1 are
preserved. Updated precedence ordering (highest wins):

1. `LOW_BATTERY` (orange slow pulse) — overrides everything
2. `ERROR` (red fast pulse)
3. `OTA` (white fast strobe)
4. `STREAMING_DEGRADED_SIM` (purple slow pulse) — **new**
5. `CALIBRATING` (purple solid)
6. `STREAMING_CATCHUP` (yellow heartbeat)
7. `STREAMING_LIVE` (green heartbeat)
8. The remaining transient FSM states (advertising, connecting, etc.)

`STREAMING_DEGRADED_SIM` and `CALIBRATING` share purple but differ in
rhythm: degraded-sim pulses slowly (0.5 Hz, evenly), calibrating
holds solid. A calibration sequence that overlaps a degraded source
shows solid purple until calibration completes, then returns to slow-
pulse purple.

### 4.3 Wire encoding

The wire-encoded state value `10` (`STREAMING_DEGRADED_SIM`) is
emitted on `r2.sensor.status` key `0` (the FSM state field) whenever
the sender is in degraded mode AND no higher-precedence overlay is
active. Key `7` (`data_source`) carries the source independently of
the state value; the dashboard renders the two together (see §5).

The dashboard MUST tolerate the two signals disagreeing transiently
during a state change. If they disagree for ≥ 1 status period (2 s),
the dashboard SHOULD prefer key `7` for the data-source indicator and
flag the disagreement in the device-card text.

---

## 5. Recovery

`DEGRADED_SIM` is latched until reset. A sensor that recovers an
ADXL355 (e.g. operator opens housing, fixes a loose wire) MUST be
reset (button or remote, per `SPEC-R2-ROCKER-SENSOR-REMOTE-RESET.md`
when implemented) to re-attempt `Adxl355::new()`.

Rationale: an automatic retry path would require periodically
disturbing the SPI bus during streaming (the ADXL355 driver holds an
exclusive SPI device handle), and the cost of a missed retry window
is low (operator reset is cheap once the indicator is visible).

A future revision MAY add a periodic-retry option behind a config
flag if field experience shows it is needed.

---

## 6. Dashboard rendering

### 6.1 Device card

Each device card MUST show **two indicators side by side**:

| Indicator | Purpose | Source |
|---|---|---|
| Primary dot | FSM state (`r2.sensor.status` key 0) | Existing — colour per §4 mapping |
| Health dot  | Data-source health (`r2.sensor.status` key 7) | **New** — green = real, purple slow pulse = sim |

The health dot MUST always be visible (whether the source is real or
sim) and MUST be accompanied by an unambiguous text label:

* Real: small green dot + `"Real ADXL355"`.
* Sim: purple slow-pulse dot + `"Sim fallback — sensor not reading"`.

Always-on rendering (rather than warning-only) gives the operator a
positive reassurance signal that the dashboard *is* receiving health
data, distinguishes "healthy sensor" from "no recent status frame",
and trains the operator's eye to the row where the warning will
appear if it does.

If only the announce is available (no status frame received yet for
this device since dashboard start), the dashboard MAY infer the
health from the `-sim` segment in `fw_ver` and show the same
indicators. Once the first status frame arrives, key `7` is
authoritative.

### 6.2 Text vocabulary

Per `feedback_ui_no_protocol_jargon`, the user-visible strings MUST
NOT use R2-protocol jargon. Allowed terms:

| Allowed | Forbidden |
|---|---|
| "Real ADXL355" | "real data_source = 0" |
| "Sim fallback — sensor not reading" | "DEGRADED_SIM state" |
| "Sensor not reading" | "ADXL355 init failed" |

The wire-protocol identifiers (`r2.sensor.status`, `data_source`)
remain in code and specs; only what the operator sees in the UI is
constrained.

### 6.3 WASM viewer parity

The WASM viewer (`webapp/index.html`) MUST mirror the legacy
dashboard's behaviour. Both renderers consume the same WebSocket
stream and the same `r2.sensor.status` events; the rendering of the
two indicators MUST be visually consistent across the two views.

---

## 7. Implementation notes (non-normative)

* Firmware: extend `EVT_SENSOR_STATUS` payload encoding in
  `firmware/esp32-s3/<carrier>/src/wire.rs` and the encoder in
  `sender.rs`. Add `LedState::StreamingDegradedSim` (wire value 10) to
  `led.rs`. Plumb `data_source` from `Sender::adxl.is_some()` (the
  same signal that already drives `build_fw_ver`).
* Dashboard: extend the device-card renderer in `webapp/index.html`
  (the canonical browser frontend) — update `ledClassFor()` switch
  and add a new CSS class for the purple slow-pulse health dot.
* Both carrier firmware trees (`devkitc/` and `xiao/`) implement
  this; the spec is carrier-agnostic.
* `r2-wire` / `r2-core` crate changes: none required — the additional
  CBOR key is opaque at the wire-framing layer.

---

## 8. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-14 | 0.1 | Initial draft. New `data_source` key on `r2.sensor.status`; new `STREAMING_DEGRADED_SIM` LED state (purple slow pulse); dashboard two-dot rendering. |
