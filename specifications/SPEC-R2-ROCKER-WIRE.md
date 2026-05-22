# SPEC-R2-ROCKER-WIRE: Rocker-Rig Application Wire Events

**Version:** 0.1 Draft
**Date:** 2026-05-06
**Status:** Normative Draft
**Depends on:** R2-WIRE (compact frame), R2-FNV (event name hashing), R2-CBOR (payload encoding), R2-BOOTSTRAP (BLE/WiFi handshake), R2-TRUST (TG signatures)

---

## 1. Introduction

This specification defines the **application-layer wire events** used by
the r2-rocker structural-health-monitoring system. These events ride on
top of the R2-WIRE compact frame format (R2-WIRE §4.2) carried over TCP
between sensor nodes and the dashboard.

R2-WIRE itself is unchanged. This document is purely a **catalogue of
event names, FNV-1a hashes, CBOR payload schemas, and protocol semantics**
specific to the rocker-rig application.

### 1.1 Design goals

* Compact enough that a single accelerometer sample fits in a typical
  R2-WIRE compact frame (≤24 bytes payload preferred per R2-WIRE §1.1).
* Catch-up replay batches MUST fit in a single TCP-framed extended
  message (≤1500 bytes after CBOR + R2-WIRE header).
* Every event a sensor emits is **idempotent on (device_pk, seq)** — the
  dashboard MUST de-duplicate on the pair when replays overlap with live
  frames after a reconnection.
* All sensor → dashboard events that carry data MUST include a monotonic
  `seq` (per device) and `ts_ms` (sensor uptime in milliseconds) so the
  dashboard can reconstruct ordering and align across devices.
* Symmetrical: dashboard → sensor command events use the same R2-WIRE
  envelope; only the event-name namespace differs (`r2.dash.…`).

### 1.2 Terminology

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHOULD**,
**MAY** in this document are to be interpreted as in RFC 2119.

* **Sensor** — an ESP32-S3 + ADXL355 device running the r2-rocker
  firmware. Identified by its 32-byte Ed25519 public key (`device_pk`).
* **Dashboard** — the controlling-device application receiving sensor
  data. There is exactly **one** dashboard per deployment.
* **Trust group (TG)** — the shared cryptographic identity binding
  sensors and dashboard. Defined in R2-TRUST.
* **Sample** — a single triplet of accelerometer readings `(x, y, z)`
  read from the ADXL355 at one ODR tick.
* **Frame** — one R2-WIRE compact message carrying one event payload.
* **Live mode** — sensor is caught up: it emits one frame per sample
  using `r2.sensor.acceleration`.
* **Catch-up mode** — sensor backlog ≥ 200 samples: it emits batched
  frames using `r2.sensor.acceleration.batch`.

### 1.3 Notation

Multi-byte integers are big-endian (network byte order) at the framing
layer, in line with R2-WIRE §1.3. CBOR encoding follows RFC 8949,
deterministic encoding (RFC 8949 §4.2).

CBOR map keys are **integers** for compactness — never strings. Reserved
key ranges:

| Key range | Purpose |
|---|---|
| 0–9   | Required, schema-stable fields (the event's payload-of-record) |
| 10–19 | Optional, schema-stable fields (older keys remain when added) |
| 20–29 | Vendor-defined extensions (university lab may use these) |
| 30–63 | Reserved for future use; receivers MUST ignore unknown keys |

This convention preserves forwards compatibility (rule §8).

### 1.4 Frame envelope

Every event in this spec is carried in an R2-WIRE compact frame with:

| Field | Value |
|---|---|
| MsgType | `Event` (R2-WIRE §3.1) |
| event_hash | FNV-1a-32 of the event-name byte string (UTF-8, no terminator) |
| ttl | Default 8 (multi-hop irrelevant on direct TCP; keeps framing uniform) |
| payload | CBOR map per the event's schema below |

TCP transport binding (per R2-WIRE §1.1.1 and the existing dashboard at
`tools/r2-dashboard`): each frame is preceded by a **u16 big-endian
length prefix**, then the raw R2-WIRE bytes. This matches the existing
multi-sensor receive path on dashboard port 21042.

---

## 2. Event inventory

| # | Event name | Direction | Purpose |
|---|---|---|---|
| 1 | `r2.sensor.announce` | sensor → dash | Initial "hello", carries device public key + signed proof of TG membership |
| 2 | `r2.sensor.acceleration` | sensor → dash | One accelerometer sample (live mode) |
| 3 | `r2.sensor.acceleration.batch` | sensor → dash | N accelerometer samples (catch-up mode) |
| 4 | `r2.sensor.battery` | sensor → dash | Battery state |
| 5 | `r2.sensor.status` | sensor → dash | Health & runtime info on demand |
| 6 | `r2.sensor.cal.sample.resp` | sensor → dash | Calibration sample response |
| 7 | `r2.sensor.sync_pong` | sensor → dash | Time-sync echo |
| 8 | `r2.sensor.event.log` | sensor → dash | Notable on-device events (errors, mode transitions) |
| 9 | `r2.dash.ack` | dash → sensor | Cumulative ACK to free SD ring |
| 10 | `r2.dash.cal.sample.req` | dash → sensor | Take averaged calibration sample at position {A,B} |
| 11 | `r2.dash.stream.start` | dash → sensor | Begin/resume streaming at given rate + range |
| 12 | `r2.dash.stream.stop` | dash → sensor | Halt streaming (sensor still logs to SD) |
| 13 | `r2.dash.sync_pulse` | dash → sensor | Time-sync probe |
| 14 | `r2.dash.config.set` | dash → sensor | Update NVS-stored config (rate, range, hostname) |
| 15 | `r2.dash.fw.update` | dash → sensor | Trigger OTA fetch |
| 16 | `r2.dash.reset` | dash → sensor | Soft reset; optional `factory: true` clears NVS |
| 17 | `r2.dash.capture.start` | dash → sensor | Enter calibration window (SPEC-R2-ROCKER-CAPTURE §2) |
| 18 | `r2.dash.capture.mark`  | dash → sensor | `{0: i64 ts_ms, 1: str name}`; lock calibration offset and open `/sdcard/captures/<ts16>-<name>.csv` |
| 19 | `r2.dash.capture.stop`  | dash → sensor | Close the open capture file |
| 20 | `r2.sensor.capture.state` | sensor → dash | `{0: u8 state, 1: str? file}` — state ∈ {0=idle, 1=calibrating, 2=recording} |

Implementations MUST treat unknown event hashes as receivable but
non-actionable — log them and move on; never close the connection over
an unrecognised event.

---

## 3. Sensor → dashboard events

### 3.1 `r2.sensor.announce`

Sent **immediately after** TCP connect, before any other event. The
dashboard MUST verify `sig` against the embedded TG public key
(`tg_pub.bin`) before accepting the peer; rejection closes the TCP
session.

| CBOR key | Type | Description |
|---|---|---|
| 0 | bytes(32) | `device_pk` — Ed25519 public key |
| 1 | text | `hostname` — friendly device label, ≤ 32 ASCII bytes |
| 2 | text | `fw_ver` — semver + git short hash, e.g. `"0.2.1+a1b2c3d"` |
| 3 | uint32 | `last_seq` — last sample sequence number persisted in SD; 0 on factory-fresh device |
| 4 | uint32 | `boot_ts_ms` — sensor's uptime in ms at connect time |
| 5 | bytes(16) | `nonce` — random per connection (replay protection) |
| 6 | bytes(64) | `sig` — Ed25519 signature over the canonical CBOR encoding of keys 0..5 |
| 10 | uint8 | (optional) `mounting_role` — 0 = unset, 1 = rocker, 2 = bed, 3 = other |

Dashboard responds (after accepting) with `r2.dash.ack {through_seq:
last_seq}` confirming where to resume from, and zero or one
`r2.dash.config.set` to push any device-specific config.

### 3.2 `r2.sensor.acceleration` (live)

Emitted in **live mode** at the configured `rate_hz` (default 100 Hz).
One frame per sample.

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `seq` — monotonic per device, increments by 1 per sample, persists across reboot |
| 1 | uint32 | `ts_ms` — sensor uptime in milliseconds at the sample instant *(AMENDED BY [SPEC-R2-ROCKER-TIMESYNC](SPEC-R2-ROCKER-TIMESYNC.md) §2.2: carries synchronised deployment milliseconds, not monotonic uptime)* |
| 2 | int32 | `x` — 20-bit signed, sign-extended to i32, raw LSB (no scaling on the wire) |
| 3 | int32 | `y` |
| 4 | int32 | `z` |
| 10 | uint8 | (optional) `range` — 0 = ±2 g, 1 = ±4 g, 2 = ±8 g; absent ⇒ inherit last `stream.start.range` |

The dashboard scales raw LSB → g using the range constant on receipt
(±2 g: 256000 LSB/g; ±4 g: 128000 LSB/g; ±8 g: 64000 LSB/g — per
ADXL355 datasheet).

### 3.3 `r2.sensor.acceleration.batch` (catch-up)

Emitted in **catch-up mode** when the network task is ≥ 200 samples
behind the sample task. Replaces per-sample frames until live mode is
re-entered.

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `first_seq` — `seq` of the first sample in `samples` |
| 1 | uint32 | `first_ts_ms` — `ts_ms` of the first sample |
| 2 | uint16 | `dt_ms` — sample period (ms); samples are uniformly spaced |
| 3 | uint8 | `range` — as in §3.2 key 10, REQUIRED here |
| 4 | array of [int32, int32, int32] | `samples` — N triplets of (x, y, z), N ≤ 50 |

Implementations SHOULD pick N = 50 by default — yields ~620 byte CBOR
payload, well under TCP MSS. Batch frames also count toward ACK
freeing: the cumulative `seq` covered by a batch is `first_seq +
len(samples) − 1`.

### 3.4 `r2.sensor.battery`

Emitted every 30 s in `Streaming` state, every 5 min in `Idle`. Also
emitted on transition into `LowBattery` state (≤ 3.3 V).

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint16 | `voltage_mv` — battery cell voltage (e.g. 3700 = 3.700 V) |
| 1 | uint8 | `percent` — 0..100, computed via LiPo discharge curve |
| 2 | bool | `charging` — reserved; the v0.1 reference hardware has no on-board charger and the firmware shall always report `false`. Reserved so a future hardware revision with on-board charging can populate it without a wire-protocol break. |
| 3 | uint32 | `ts_ms` — sensor uptime when sampled |
| 10 | int8 | (optional) `temp_c` — board temp from ADXL355 internal sensor, if read |

### 3.5 `r2.sensor.status`

Sent in response to a `r2.dash.config.set` query, or unsolicited on
state transitions.

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint8 | `state` — see [SPEC-R2-ROCKER-SENSOR](SPEC-R2-ROCKER-SENSOR.md) §4.1.1 for the authoritative `state → u8` mapping (10-value enum; this row's earlier 7-value inline list was pre-Phase-5L). |
| 1 | uint32 | `uptime_ms` *(AMENDED BY [SPEC-R2-ROCKER-TIMESYNC](SPEC-R2-ROCKER-TIMESYNC.md) §2.2: carries synchronised deployment milliseconds, not monotonic uptime)* |
| 2 | uint32 | `samples_total` — total samples logged to SD (this run) |
| 3 | uint32 | `samples_acked` — cumulative `seq` ACKed by dashboard |
| 4 | uint8 | `sd_pct_used` — 0..100 |
| 5 | uint16 | `rate_hz_active` |
| 6 | uint8 | `range_active` — 0..2 |
| 10 | uint8 | (optional) `error_code` — 0 = none; non-zero codes per `SPEC-R2-ROCKER-SENSOR` §error-codes |

### 3.6 `r2.sensor.cal.sample.resp`

Response to a `r2.dash.cal.sample.req`. The sensor averages
`req.ms` worth of samples and replies once.

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `req_id` — echoes the request |
| 1 | uint8 | `position` — `0x41` = 'A', `0x42` = 'B' (echoed) |
| 2 | int32 | `gx` — averaged raw LSB |
| 3 | int32 | `gy` |
| 4 | int32 | `gz` |
| 5 | uint16 | `n_samples` — actual count averaged (may be fewer than requested if SD/network throttled the sampler) |
| 6 | uint8 | `range` — range during the averaging window |

### 3.7 `r2.sensor.sync_pong`

Response to `r2.dash.sync_pulse`, used for Cristian's-algorithm clock
offset estimation.

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `req_id` — echoed from `sync_pulse` |
| 1 | uint32 | `sensor_ts_ms` — sensor's monotonic time at frame-receive instant *(AMENDED BY [SPEC-R2-ROCKER-TIMESYNC](SPEC-R2-ROCKER-TIMESYNC.md) §2.2: carries synchronised deployment milliseconds, not monotonic uptime)* |

The dashboard computes `offset = dash_send_ts + (rtt / 2) − sensor_ts`
once per round and exponentially smooths. See `SPEC-R2-ROCKER-DASHBOARD`
§time-sync for the full algorithm.

### 3.8 `r2.sensor.event.log`

Notable on-device events (state transitions, errors, OTA status).

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `ts_ms` |
| 1 | uint8 | `level` — 0 = trace, 1 = debug, 2 = info, 3 = warn, 4 = error |
| 2 | uint8 | `code` — short code per `SPEC-R2-ROCKER-SENSOR` §events |
| 3 | text | (optional) `msg` — short human-readable string, ≤ 64 bytes |

---

## 4. Dashboard → sensor events

### 4.1 `r2.dash.ack`

Sent every 200 ms or every 100 received samples (whichever first), in
both live and catch-up modes.

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `through_seq` — sensor MAY free SD ring up to and including this `seq` |
| 1 | uint32 | `dash_ts_ms` — dashboard send time (advisory; for sync) |

### 4.2 `r2.dash.cal.sample.req`

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `req_id` |
| 1 | uint8 | `position` — `0x41` ('A') or `0x42` ('B') |
| 2 | uint16 | `ms` — averaging window in milliseconds (1000 default) |

The sensor MUST be in `Streaming` or `Idle` state to honour cal requests;
in any other state it replies with `r2.sensor.status` carrying an error
code instead.

### 4.3 `r2.dash.stream.start`

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint16 | `rate_hz` — 1..4000 (limited by ADXL355 ODR table) |
| 1 | uint8 | `range` — 0 = ±2 g, 1 = ±4 g, 2 = ±8 g |
| 10 | uint32 | (optional) `resume_from_seq` — override the implicit "resume from `last_acked + 1`"; MAY be lower for explicit replay |

If the sensor cannot satisfy the requested `rate_hz` (e.g. SD write
saturation), it MUST reply with `r2.sensor.status` containing the
actually-achieved `rate_hz_active`.

### 4.4 `r2.dash.stream.stop`

Empty payload (`{}`). The sensor MUST stop emitting acceleration frames
but MUST continue logging to SD; resume on the next `stream.start`.

### 4.5 `r2.dash.sync_pulse`

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint32 | `req_id` |
| 1 | uint64 | `dash_ts_ms` — dashboard wall-clock or monotonic; opaque to sensor |

Sent at 1 Hz during the first 30 s after a sensor connects, then every
30 s thereafter.

### 4.6 `r2.dash.config.set`

| CBOR key | Type | Description |
|---|---|---|
| 0 | uint16 | (optional) `default_rate_hz` |
| 1 | uint8 | (optional) `default_range` |
| 2 | text | (optional) `hostname` |
| 3 | uint8 | (optional) `mounting_role` — 1 = rocker, 2 = bed, 3 = other |

Any field present is persisted to NVS and takes effect immediately;
absent fields are unchanged.

### 4.7 `r2.dash.fw.update`

| CBOR key | Type | Description |
|---|---|---|
| 0 | text | `url` — TCP `host:port/path` of the firmware blob; ≤ 128 bytes |
| 1 | bytes(32) | `sha256` — expected hash of the binary |
| 2 | bytes(64) | (optional, REQUIRED in v1.0) `tg_sig` — TG signature over `(url || sha256)` |

The sensor MUST verify `sha256` after fetch and (when present) `tg_sig`
before swapping the OTA partition. On v0.x without `tg_sig`, sensors
SHOULD log a warning. See `SPEC-R2-ROCKER-SENSOR` §OTA.

### 4.8 `r2.dash.reset`

| CBOR key | Type | Description |
|---|---|---|
| 0 | bool | `factory` — if true, clears NVS (device key, last_acked_seq, calibration if any) before reboot |

A factory reset triggers re-pairing on next boot — the sensor will
generate a fresh `device_pk` and re-announce.

---

## 5. Sequencing & retention

### 5.1 Sequence numbers

`seq` is a per-device monotonic 32-bit counter:

* Starts at 0 on factory-fresh devices.
* Increments by exactly 1 per ADXL355 sample logged to SD.
* Persists across reboots: on boot, the sensor reads the SD log tail to
  find the highest `seq` written, sets the next one to that + 1.
* Wraps at 2³² ≈ 4.3 billion samples ≈ 1.4 years at 100 Hz. Wrap
  semantics: the sensor MUST emit `r2.sensor.event.log {code:
  SEQ_WRAP_IMMINENT}` 24 h before wrap; dashboard MUST treat
  `seq_new < seq_old` after a wrap-warning event as a wrap, not a bug.

### 5.2 ACK semantics

* Dashboard sends `r2.dash.ack {through_seq: N}` cumulatively.
* On receipt, sensor MAY free SD ring records with `seq ≤ N`.
* Sensor MUST persist `last_acked_seq` to NVS after every ACK (to a
  rate-limited write, e.g. once per second).
* On reconnect, sensor resumes from `last_acked_seq + 1`.

### 5.3 De-duplication

The dashboard MUST de-duplicate received samples on `(device_pk, seq)`.
Duplicates after a reconnect-replay are expected and harmless to
discard. A duplicate with **different** `(x, y, z)` for the same `(pk,
seq)` indicates corruption — log a warning, keep the first received.

---

## 6. Calibration protocol

The two-position calibration flow defined in `PLAN.md` D-17:

```
dashboard                         sensor
─────────                         ──────
[user clicks "Calibrate – A"]
r2.dash.cal.sample.req            ─→
{req_id:1, position:'A', ms:1000}
                                   averages 1000 ms of samples
                          ←─       r2.sensor.cal.sample.resp
                                   {req_id:1, position:'A', gx,gy,gz, n_samples}
[stores g_A[device_pk]]

[rocker is moved manually to position B]
[user clicks "Calibrate – B"]
r2.dash.cal.sample.req            ─→
{req_id:2, position:'B', ms:1000}
                          ←─       r2.sensor.cal.sample.resp

[computes R per PLAN.md D-17, persists]

[user clicks "Start streaming"]
r2.dash.stream.start              ─→
{rate_hz:100, range:0}
                          ←─       r2.sensor.acceleration … (live)
```

The dashboard MUST verify `|g_B − g_A| ≥ 0.3 g` (PLAN.md D-18); if
not, it MUST refuse to compute `R` and prompt the operator for a
larger calibration swing.

---

## 7. Time synchronisation

Per D-20: Cristian's-algorithm sync runs at:

* 1 Hz for the first 30 s after a sensor's TCP connect (rapid warm-up).
* 30 s thereafter (steady-state drift correction).

```
dashboard                         sensor
─────────                         ──────
T1 = wall_clock_ms()
r2.dash.sync_pulse                ─→
{req_id, dash_ts_ms: T1}
                                   T2 = sensor_ts_ms()
                          ←─       r2.sensor.sync_pong
                                   {req_id, sensor_ts_ms: T2}
T3 = wall_clock_ms()
rtt = T3 − T1
offset_estimate = T1 + (rtt/2) − T2
```

The dashboard maintains an exponentially smoothed offset per device:
`offset_smoothed = α · offset_estimate + (1 − α) · offset_smoothed`
with `α = 0.2`. To convert any `ts_ms` from a sensor frame to wall
clock: `wall = ts_ms + offset_smoothed[device_pk]`.

---

## 8. Versioning & forwards compatibility

### 8.1 Additive evolution

* New optional fields use **new CBOR map keys** (next free key in the
  appropriate range per §1.3).
* Deleted fields' keys are **never reused**.
* Receivers MUST silently ignore unknown keys.
* New event names (new FNV hashes) MAY appear; receivers MUST tolerate
  unknown event hashes per §2.

### 8.2 Breaking changes

A breaking change (semantic redefinition of an existing key, removal of
a required field, etc.) requires a **new event name** and new FNV hash
(typically by appending `.vN`, e.g. `r2.sensor.acceleration.v2`). The
old name remains valid for one migration cycle, both sides emit and
accept both.

### 8.3 Spec versioning

This document's frontmatter `Version` field uses **major.minor**
semver-ish:

* Minor bumps: additive changes only (new optional keys, new events).
* Major bumps: breaking changes (per §8.2) or substantial protocol
  reworks. Major bumps require a new `r2.sensor.announce` field
  (`spec_ver: text`) so peers can mode-switch.

`r2.sensor.announce` MAY include a `spec_ver` field at key 11 (added in
v0.2 of this spec, currently absent). When absent, peers assume v0.1.

---

## 9. Conformance test vectors

A conformance vector file `testing/wire-vectors.json` (TBD, generated
during firmware Phase 5) MUST contain at least one entry per event in
§2:

```json
[
  {
    "event": "r2.sensor.acceleration",
    "fnv": "<computed>",
    "payload": {"0": 42, "1": 12345, "2": -1024, "3": 512, "4": 0},
    "cbor_hex": "...",
    "wire_hex": "..."
  },
  ...
]
```

The vectors are generated by the sensor firmware's encoder unit tests
and consumed by the dashboard's decoder unit tests, ensuring both ends
agree byte-for-byte on the canonical encoding.

---

## 10. Security considerations

### 10.1 Authentication

All sensor → dashboard frames after `r2.sensor.announce` are implicitly
authenticated by virtue of the TCP session being TG-validated at
announce time. R2-WIRE does not yet add per-frame signatures; this is
acceptable here because:

* The TCP transport is over the dashboard's own hotspot — no external
  routing.
* TG membership is checked at session start; impersonation requires
  device-key compromise.

For future deployments where the network is not trusted (sensor on
external WiFi, etc.), per-frame HMACs SHOULD be added (see R2-WIRE §8).

### 10.2 Replay

The `nonce` in `r2.sensor.announce` (§3.1 key 5) prevents replay of an
old announce frame. Within a session, the monotonic `seq` and `ts_ms`
mean replayed acceleration frames are detectable as duplicates (§5.3).

### 10.3 Confidentiality

R2-ROCKER-WIRE frames are **not encrypted** in v0.1. The accelerometer
data is not commercially sensitive (mechanical motion of a test rig);
device public keys are public by design. If confidentiality becomes a
requirement, R2-WIRE §8 envelope-level HMAC + future encryption layer
applies uniformly.

---

## 11. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-06 | 0.1 | Initial draft. Event inventory, CBOR schemas, sequencing, calibration, time-sync. |
| 2026-05-07 | 0.1.1 | §3.4 clarified: `charging` field reserved but unused in v0.1 (no on-board charger); always emitted as `false`. |

## Appendix A — Event-name to FNV-1a-32 hashes

These are computed at firmware build time from the event-name UTF-8
bytes (no terminator). The table is informational; the source of truth
is the `r2_fnv::fnv1a_32` function.

| Event name (input bytes) | FNV-1a-32 (computed) |
|---|---|
| `r2.sensor.announce` | (TBD — verify on first build) |
| `r2.sensor.acceleration` | (TBD) |
| `r2.sensor.acceleration.batch` | (TBD) |
| `r2.sensor.battery` | (TBD) |
| `r2.sensor.status` | (TBD) |
| `r2.sensor.cal.sample.resp` | (TBD) |
| `r2.sensor.sync_pong` | (TBD) |
| `r2.sensor.event.log` | (TBD) |
| `r2.dash.ack` | (TBD) |
| `r2.dash.cal.sample.req` | (TBD) |
| `r2.dash.stream.start` | (TBD) |
| `r2.dash.stream.stop` | (TBD) |
| `r2.dash.sync_pulse` | (TBD) |
| `r2.dash.config.set` | (TBD) |
| `r2.dash.fw.update` | (TBD) |
| `r2.dash.reset` | (TBD) |

The dashboard and sensor MUST compute these from the **same** input
strings. Discrepancy is a correctness bug, not a wire-protocol issue.
The conformance test vector file (§9) pins each pair after the first
build.
