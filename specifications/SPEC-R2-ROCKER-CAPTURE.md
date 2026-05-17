# SPEC-R2-ROCKER-CAPTURE: Named experimental captures

**Version:** 0.1 Draft
**Date:** 2026-05-18
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-SENSOR (§6 SD ring), SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-TIMESYNC, SPEC-R2-ROCKER-SENSOR-HEALTH

---

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**,
**SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**,
and **OPTIONAL** in this document are to be interpreted as
described in [RFC 2119](https://www.rfc-editor.org/info/rfc2119),
when they appear in capitals.

---

## 1. Introduction

The rolling SD ring (`/sdcard/logNNNN.csv` per
SPEC-R2-ROCKER-SENSOR §6) is a continuous backstop for the live
stream. It is **not** the right format for deliberate experimental
runs — there is no operator-given name, no per-run calibration,
no notion of "this run starts here, ends there".

This specification defines the **capture** workflow: discrete,
named, calibration-zeroed CSV files created on every sensor's SD
card in lockstep with a controller-driven Start → Mark → Stop
sequence. Captures live alongside the rolling ring, not in place
of it.

### 1.1 Scope

In scope:

* The three-state capture lifecycle (Idle → Calibrating →
  Recording → Idle).
* Four R2-WIRE events that drive it.
* On-disk layout under `/sdcard/captures/`.
* Calibration semantics (fixed-window baseline, additive
  per-axis offset applied to the row values).
* Sensor-side TCP listener `data_tcp` on port 21047 that
  enumerates, downloads, and deletes capture files for the
  dashboard.

Out of scope:

* Calibration that compensates for orientation or temperature.
  v0.1 captures a static per-axis additive offset and stops
  there; finer-grained calibration is a future extension.
* Crash-safety guarantees beyond fsync-on-Stop. A loss of power
  mid-Recording **MAY** leave a partially-written file; that
  partial file is still valid CSV up to its last fsync.
* Re-arming the calibration offset across boots. Each capture
  session re-calibrates from scratch — a sensor reboot between
  Mark and Stop **MUST** result in the file being closed by the
  next dashboard Stop or by file-list cleanup.

### 1.2 Terminology

* **Capture session** — one Start → Mark → Stop triple.
* **Calibration window** — the `CAL_WINDOW_MS` milliseconds
  immediately after Start during which the sensor accumulates a
  baseline mean per axis.
* **Capture offset** — the locked-in mean computed at Mark,
  applied as `output_axis = raw_axis - offset_axis` to every row
  written during Recording.
* **Capture file** — a CSV at
  `/sdcard/captures/<ts>-<name>.csv` written exclusively during
  Recording.
* **Run name** — the operator-supplied label, sent by the
  controller in `r2.dash.capture.mark`.

---

## 2. State machine

```
                       Start            Mark
              Idle ─────────► Calibrating ─────► Recording
                ▲                  │                │
                │                  │ Start (re-arm) │ Stop
                │                  ▼                │
                └───────── Stop ◄──────────────────┘
```

Transitions:

| From | Event | To | Action |
|---|---|---|---|
| `Idle` | `r2.dash.capture.start` | `Calibrating` | Reset the cal accumulator. Stamp `cal_start_ms = clock.ts_ms_i64()`. LED `Calibrating` (purple). |
| `Calibrating` | every sample during the window | `Calibrating` | Add the raw sample to the running sum. |
| `Calibrating` | `cal_start_ms + CAL_WINDOW_MS` elapsed (a sample arrives after that) | `Calibrating` (locked) | Mean of the accumulated samples becomes the candidate offset. Further samples are dropped from the accumulator. |
| `Calibrating` | `r2.dash.capture.mark` | `Recording` | Lock the candidate offset as `capture_offset`. Open `/sdcard/captures/<ts>-<name>.csv` for write (filename built from the payload's `ts_ms` + `name`). Begin writing **calibrated** rows. LED returns to `StreamingLive` / `StreamingDegradedSim`. |
| `Recording` | `r2.dash.capture.stop` | `Idle` | `sync_all()` the file, close it, drop the `capture_offset`. |
| `Recording` | `r2.dash.capture.start` | `Calibrating` | Equivalent to Stop then Start in one event. File is closed via fsync; the new cal window begins. |
| any | `r2.dash.capture.stop` while `Idle` | `Idle` | No-op. **MUST** acknowledge silently. |

Behaviour while `Calibrating` but before `CAL_WINDOW_MS` has
elapsed and a `r2.dash.capture.mark` arrives early: the firmware
**MUST** clamp the accumulated mean to the samples received so
far and proceed to `Recording`. Operators **SHOULD NOT** Mark
inside the window — the controller's UI **SHOULD** disable the
Mark button until the window has elapsed.

`CAL_WINDOW_MS` **SHALL** default to **2000 ms**. Carriers
**MAY** override via an NVS key (a future spec extension).

---

## 3. Wire events

All four events ride the existing R2-WIRE compact frame on the
streaming TCP session (port 21042). CBOR payloads use the
integer-key + smallest-encoding convention from R2-WIRE / R2-CBOR.

| Event name | Hash (FNV-1a-32) | Direction | Payload |
|---|---|---|---|
| `r2.dash.capture.start` | computed at compile time | dash → sensor | `{}` (empty CBOR map) |
| `r2.dash.capture.mark`  | computed at compile time | dash → sensor | `{0: i64 ts_ms, 1: str name}` |
| `r2.dash.capture.stop`  | computed at compile time | dash → sensor | `{}` |
| `r2.sensor.capture.state` | computed at compile time | sensor → dash | `{0: u8 state, 1: str file_opt}` where `state ∈ {0=idle, 1=calibrating, 2=recording}` and `file` is the open filename when `state=2`, omitted otherwise |

Sensors **MUST** emit `r2.sensor.capture.state` on every state
transition. The controller uses these to update the webapp.

The `name` field on `r2.dash.capture.mark`:

* **SHALL** be UTF-8.
* **SHALL** be no longer than 32 bytes.
* **SHALL** match `[A-Za-z0-9_-]+`. Any character outside that
  charset **MUST** cause the sensor to refuse the Mark, remain in
  `Calibrating`, and emit a `r2.sensor.event.log` with code
  `CAPTURE_BAD_NAME`.

The `ts_ms` field is supplied by the dashboard so every sensor in
the fleet builds the **same** filename. Sensors **MUST NOT**
substitute their local clock at file-open time.

---

## 4. Filesystem layout

Capture files live under a sub-directory of the SD mount root:

```
/sdcard/
├─ log0001.csv               ← rolling-ring segment, untouched
├─ log0002.csv
├─ …
└─ captures/
   ├─ 0001779000000000-run-01-asphaltA.csv
   ├─ 0001779000003000-run-02-asphaltA.csv
   └─ …
```

Filename convention: `<ts16>-<name>.csv` where `<ts16>` is the
dashboard-supplied `ts_ms` rendered as a **16-digit zero-padded
decimal** (i64 range fits in 19 digits but realistic epoch-millis
values fit in 13; 16 chosen for forward-compatibility and stable
lexical sort). `<name>` is the validated run name.

This filename is **longer than 8.3** and therefore requires FATFS
Long-Filename support to be enabled in the firmware build. ESP-IDF
disables LFN by default. Conforming sensor builds **SHALL** set:

```
CONFIG_FATFS_LFN_HEAP=y
CONFIG_FATFS_MAX_LFN=255
```

(or `CONFIG_FATFS_LFN_STACK=y`) in `sdkconfig.defaults`. Without
this, every `File::create` for a capture filename fails with
`EINVAL`/`ENOENT` and the capture state machine can never leave
`Calibrating`.

Sensors **MUST** create the `captures/` sub-directory if absent
(via `fs::create_dir_all`). If `create_dir_all` fails (e.g. due
to the ESP-IDF FATFS quirk noted in
SPEC-R2-ROCKER-SENSOR §6.1), the sensor **MAY** fall back to
placing capture files at the SD root with a `cap-` prefix
(`cap-<ts16>-<name>.csv`). The `data_tcp` LIST command **SHALL**
return either layout transparently.

Row format: **identical** to the rolling ring (62-byte
fixed-width CSV per SPEC-R2-ROCKER-SENSOR §6.2 v0.2) **except**
the x, y, z columns carry calibrated values:

```
output_x = raw_x − capture_offset.x
output_y = raw_y − capture_offset.y
output_z = raw_z − capture_offset.z
```

The `seq` and `ts_ms` columns are unchanged.

---

## 5. Calibration semantics

The capture offset is a static per-axis additive value computed
once per session:

```
offset.x = mean(raw_x_i)  for samples i in the cal window
offset.y = mean(raw_y_i)
offset.z = mean(raw_z_i)
```

The mean is integer division over signed `i32` accumulators
(saturating add). Sample sources during calibration are the same
as during normal sampling — real ADXL355 or sim per
SPEC-R2-ROCKER-SENSOR-HEALTH; sim-fallback samples **MAY** be
included in the calibration mean (operators wanting a clean
baseline should ensure no sim-fallback before Mark).

`CAL_WINDOW_MS = 2000` at 100 Hz yields ≈ 200 samples per axis,
which is sufficient to drive the per-axis standard error below
1 LSB at ±2 g for a stationary mount.

The rolling ring **MUST** continue writing **raw** (uncalibrated)
samples regardless of capture state. The durable backstop never
depends on a per-session calibration value.

---

## 6. `data_tcp` listener (port 21047)

A dedicated TCP listener on the sensor enumerates, fetches, and
deletes capture files for the dashboard. Mirrors the ergonomics
of `ota_tcp` (port 21043) and `reset_tcp` (port 21044).

### 6.1 Framing

Plain binary framing — no CBOR — chosen for `xxd`/`nc`-readable
wire vectors and a tight implementation on a small heap. Every
command begins with a single-byte opcode; bodies use big-endian
length-prefixed strings and big-endian integers.

```
client → sensor : [opcode u8][body…]
sensor → client : [status u8][body…]
```

Status bytes:
* `0x00 OK`
* `0x01 ERROR` — body is `[u16 BE msg_len][msg utf-8]`
* `0x02 BUSY` — capture is `Recording`; the requested file is the
  one currently open. Body is `[u16 BE msg_len][msg utf-8]`.
  Client **SHOULD** retry after a Stop.

### 6.2 Opcodes

| Opcode | Name | Request body | Response on OK |
|---|---|---|---|
| `0x01` | `LIST` | (none) | `[u32 BE count]` then `count` × `[u16 BE name_len][name utf-8][u64 BE size][i64 BE mtime_ms]` |
| `0x02` | `GET`  | `[u16 BE name_len][name utf-8]` | `[u64 BE size][size bytes raw file content]` |
| `0x03` | `DEL`  | `[u16 BE name_len][name utf-8]` | (empty) |
| `0x04` | `DEL_ALL` | (none) | `[u32 BE deleted_count]` |

The sensor **SHALL** refuse `GET` and `DEL` on the
currently-recording file with `BUSY`. `DEL_ALL` **SHALL** skip
the currently-recording file and report the surviving count
correctly.

The sensor **SHALL** reject any `name` that doesn't match the
basename charset `[A-Za-z0-9_.-]{1,64}` — guards against path
traversal. The webapp never composes a name itself; it passes
back the basenames it received from a prior `LIST`.

### 6.3 Resource budget

* Listener thread stack: 8 KiB.
* Per-client name buffer: 64 B; per-client streaming buffer: 4 KiB.
* The listener **MUST** accept exactly one client at a time;
  further `accept()`s wait. This keeps the sensor's SD bandwidth
  exclusive to one consumer.

### 6.4 Capture-state sharing

The capture sentant and the `data_tcp` listener run in different
threads. The capture sentant **MUST** publish the
currently-recording filename (or `None`) into a shared handle
that the listener reads on every `GET` / `DEL` / `DEL_ALL`. The
reference implementation uses
`Arc<Mutex<Option<String>>>`; see
`r2_esp::data_tcp::CurrentRecording`.

### 6.4 Port choice

21047 is the first port above the rocker block (21042..21046).
Canonical R2 has not claimed it. See
`audits/2026-05-18-post-v0.1.0-conformance.md` Finding F for the
prior precedent that motivated avoiding the canonical 21042..21045.

---

## 7. Dashboard responsibilities

### 7.1 Forced sync_pulse on Start

Before sending `r2.dash.capture.start` to the fleet, the
dashboard **SHALL** issue one immediate `r2.dash.sync_pulse`
round to every connected peer. The smoothed clock-offset deltas
from the subsequent `r2.sensor.sync_pong` responses flow back to
the sensors via `r2.dash.set_clock_offset` through the existing
Cristian's-algorithm path (per SPEC-R2-ROCKER-TIMESYNC §2.3) so
the `ts_ms` values appearing in all sensors' subsequent capture
files share a freshly-tightened baseline.

The dashboard **SHALL** issue `r2.dash.capture.start`
immediately after kicking the sync round — it does **NOT** await
the pongs. Each pong, when it arrives, refines the offset
asynchronously and applies to subsequent samples; the period
between `start` and `mark` (≥ `CAL_WINDOW_MS` ≈ 2000 ms) is
more than enough for the refinement to land in practice.

### 7.2 Filename consistency

The dashboard **SHALL** generate the `ts_ms` value once on
`Start` (not on `Mark`) and pass the same value to every sensor
when sending `r2.dash.capture.mark`. This guarantees the same
filename across the fleet for one capture session.

### 7.3 HTTP routes

Per SPEC-R2-ROCKER-DASHBOARD §5.1 (this spec adds):

| Route | Method | Body | Purpose |
|---|---|---|---|
| `/api/capture/start` | POST | `{}` | Fan out sync_pulse + `r2.dash.capture.start` |
| `/api/capture/mark`  | POST | `{name: str}` | Fan out `r2.dash.capture.mark` with the dashboard's chosen `ts_ms` |
| `/api/capture/stop`  | POST | `{}` | Fan out `r2.dash.capture.stop` |
| `/api/data/{addr}/list` | GET | — | `data_tcp` `LIST` to one sensor; returns the JSON-mapped CBOR response. |
| `/api/data/{addr}/file/{name}` | GET | — | `data_tcp` `GET`; streams the raw bytes back. |
| `/api/data/{addr}/file/{name}` | DELETE | — | `data_tcp` `DEL`. |
| `/api/data/{addr}/all` | DELETE | — | `data_tcp` `DEL_ALL`. |
| `/api/data/merged` | GET `?file=<basename>` | — | Heap-merges the named capture file from every connected sensor into a single long-format CSV (`ts_ms, sensor, x, y, z` ascending by `ts_ms`). |

The per-sensor zip route mooted in earlier drafts is deferred —
operators wanting all files from one sensor can iterate `LIST`
then `GET name` per file. The webapp's "Download merged" button
passes the most-recent `<ts16>-<name>.csv` from the current
session as `?file=`.

---

## 8. Conformance

A firmware build conforms to this spec when ALL of the following
hold:

1. The CaptureMgr (or equivalent) **MUST** implement the three
   states + transitions in §2.
2. The four wire events in §3 **MUST** be present with the
   payload shapes shown.
3. Capture files **MUST** be written to `/sdcard/captures/` (or
   the fallback `/sdcard/cap-*` per §4).
4. The `seq` and `ts_ms` columns in capture rows **MUST** match
   the rolling ring; x, y, z **MUST** be `raw − offset`.
5. The `data_tcp` listener on port 21047 **MUST** implement
   `LIST`, `GET`, `DEL`, `DEL_ALL` per §6.
6. Names violating the `[A-Za-z0-9_-]{1,32}` charset **MUST**
   cause the Mark to be refused per §3.

A dashboard build conforms when:

1. `Start` triggers the sync_pulse round per §7.1.
2. The same `ts_ms` is sent to every sensor on `Mark` per §7.2.
3. All HTTP routes in §7.3 are present and proxy to the
   sensor's `data_tcp` listener as specified.

A webapp build conforms when:

1. The Data tab disables the Mark button while any peer reports
   `capture_state = 1 (calibrating)` and that peer's
   `cal_start_ms + CAL_WINDOW_MS` has not yet elapsed.
2. Per-card "delete" + fleet-wide "delete all" actions fan out
   via `Promise.allSettled` over the per-sensor DELETE routes.

---

## 9. Versioning

| Date       | Ver | Change                                                     |
|------------|-----|------------------------------------------------------------|
| 2026-05-18 | 0.1 | Initial draft.                                             |
