# SPEC-R2-ROCKER-DASHBOARD: Controlling-Device Dashboard Behaviour

**Version:** 0.1 Draft
**Date:** 2026-05-07
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-SENSOR, R2-BLE, R2-BOOTSTRAP, R2-WIFI, R2-TRUST

---

## 1. Introduction

This specification defines the behaviour of the **r2-rocker dashboard** —
the single controlling-device application that:

* Hosts a WiFi hotspot for sensors to join.
* Bootstraps sensors via BLE + L2CAP, signing `#wifi_offer` frames with
  the trust-group private key.
* Receives R2-WIRE TCP streams from sensors on port 21042.
* Decodes accelerometer + battery + status frames.
* Maintains per-device calibration matrices and joint-group definitions.
* Computes pairwise differential lateral motion across joints (the
  diagnostic per PLAN D-08).
* Serves a browser UI on port 8080.
* Manages OTA updates and reset commands.

There is **exactly one** dashboard per deployment. The dashboard is the
only entity holding the TG private key (see `SECRETS-POLICY.md`).

### 1.1 Scope

In scope:

* Process model, startup, and shutdown.
* On-disk state (peers, calibration, joints).
* Network listeners (TCP, HTTP, BLE).
* Bootstrap engine.
* Per-peer state machine on the dashboard side.
* Time-synchronisation algorithm.
* Server-side calibration math.
* Joint groups and differential analysis.
* Stress indicator and long-term trend.
* Browser UI requirements.
* OTA push and command dispatch.

Out of scope:

* Sensor firmware (`SPEC-R2-ROCKER-SENSOR`).
* Wire protocol (`SPEC-R2-ROCKER-WIRE`).
* Hotspot driver-level configuration (delegated to the host OS via
  NetworkManager / nmcli).

### 1.2 Terminology

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHOULD**,
**MAY** are interpreted per RFC 2119.

* **Dashboard** — the single application defined by this spec.
* **Host** — the Linux machine (laptop or Raspberry Pi) running the
  dashboard.
* **Peer** — a connected (or remembered) sensor, identified by its
  `device_pk`.
* **Joint** — a logical grouping of one or more peers that, together,
  diagnose a structural element of the rig.
* **TG** — trust group (see PLAN D-19).
* **Wall clock** — the host's monotonic-clock reading converted to
  milliseconds since dashboard start, used as the reference time for
  cross-sensor alignment.

### 1.3 Notation

UTC-style ISO 8601 timestamps in user-facing UI; integer milliseconds
internally. Path conventions assume Linux; Windows/macOS support is
out of scope for v0.1.

---

## 2. Process model

### 2.1 Single-process architecture

The dashboard is **one Rust process** with multiple Tokio tasks:

| Task | Purpose |
|---|---|
| `tcp_listener` | Accept inbound sensor connections on :21042 |
| `peer_handler` (per peer) | Decode R2-WIRE frames, dispatch to state |
| `http_server` | Serve the browser UI on :8080 |
| `websocket_handler` (per browser) | Push live updates |
| `bootstrap_engine` | BLE scan + L2CAP per sensor, on demand |
| `analytics` | Periodic stress indicator computation, trend update |
| `persistence_writer` | Rate-limited disk writes for state |

### 2.2 Startup sequence

1. Parse CLI args / config (§14).
2. Load TG private key from `tg_signer_path`
   (default `~/.config/r2-rocker/tg_signer/tg_priv.bin`); refuse to
   start if missing or unreadable.
3. Load TG public key from `trust_keys/tg_pub.bin`; verify it pairs
   with the private key.
4. Load on-disk state (§3) into memory: peers, calibration, joints,
   high_water.
5. Bind TCP listener on :21042 and HTTP listener on :8080. If either
   fails, exit with non-zero status — *do not* fall back to alternate
   ports.
6. Initialise the BLE adapter (do not yet scan).
7. Start the analytics task.
8. Log "ready" and serve.

### 2.3 Shutdown

On `SIGINT` / `SIGTERM` the dashboard shall:

1. Stop accepting new sensor connections.
2. Send a final `r2.dash.ack` to every connected sensor with the
   highest known `seq`.
3. Flush all pending state writes to disk.
4. Disconnect the WiFi hotspot iff the dashboard created it (§6.4).
5. Exit cleanly within 5 s; force-kill after 10 s.

---

## 3. State persistence

### 3.1 Layout

The dashboard maintains state in a host-local directory, default
`~/.config/r2-rocker/state/` (overridable via CLI; see §14):

```
state/
├─ peers.json            ← known sensors (device_pk → metadata)
├─ calibration.json      ← per-device R matrix
├─ joints.json           ← joint group definitions
├─ high_water.json       ← per-device last-received seq
└─ events.log            ← rolling event log (append-only, rotated daily)
```

These paths are **gitignored** (`SECRETS-POLICY.md`); they are local
runtime state.

### 3.2 `peers.json`

```json
{
  "<device_pk_hex>": {
    "device_pk": "<32-byte hex>",
    "hostname": "rocker-1",
    "first_seen": "2026-05-07T08:00:00Z",
    "last_seen": "2026-05-07T09:30:14Z",
    "mounting_role": "rocker",
    "joint": "front-left-actuator",
    "fw_ver": "0.1.0+abc1234",
    "label": "Front-left rocker pivot",
    "notes": "..."
  }
}
```

A device is added on first valid `r2.sensor.announce`. The dashboard
follows TOFU (trust-on-first-use) for v0.1: any peer whose announce
signature is well-formed (Ed25519 over the canonical CBOR per WIRE §3.1)
is admitted, with `device_pk` becoming the persistent identity.

A peer can be deleted from `peers.json` only via the UI, with confirm.

### 3.3 `calibration.json`

```json
{
  "<device_pk_hex>": {
    "g_a":   [gx, gy, gz],
    "g_b":   [gx, gy, gz],
    "R":     [[r11, r12, r13],
              [r21, r22, r23],
              [r31, r32, r33]],
    "main_axis":     [mx, my, mz],
    "vertical_axis": [vx, vy, vz],
    "sideways_axis": [sx, sy, sz],
    "calibrated_at": "2026-05-07T09:00:00Z",
    "ms": 1000,
    "n_samples": 99,
    "valid": true
  }
}
```

Computed from `g_a` and `g_b` per PLAN D-17. See §9 for math.

### 3.4 `joints.json`

```json
{
  "front-left-actuator": {
    "label": "Front left actuator joint",
    "members": ["<device_pk_hex_1>", "<device_pk_hex_2>"],
    "topology": "pair",
    "baseline_separation_mm": 350,
    "stress_threshold_g_rms": 0.25,
    "created_at": "2026-05-07T..."
  },
  "front-right-actuator": {
    "label": "Front right actuator joint",
    "members": ["<device_pk_hex_3>"],
    "topology": "single",
    "stress_threshold_g_rms": 0.25
  }
}
```

`topology` values: `"single"` (one sensor; absolute lateral motion), or
`"pair"` (two sensors; differential lateral motion). Future:
`"reference"` for bed-mounted reference channel.

### 3.5 `high_water.json`

```json
{
  "<device_pk_hex>": { "high_seq": 1234567, "as_of": "2026-05-07T..." }
}
```

Updated whenever the dashboard records a sample with `seq > high_seq`.
Used to resume after dashboard restart: on receipt of
`r2.sensor.announce {last_seq: N}`, the dashboard sends
`r2.dash.ack {through_seq: max(high_seq, N - 1)}` so the sensor knows
where to resume.

### 3.6 Persistence rate-limiting

State writes are **debounced** at 1 Hz per file. The
`persistence_writer` task collects dirty markers from other tasks and
flushes affected files at most once per second. On clean shutdown, all
dirty state is flushed unconditionally.

---

## 4. TCP listener (port 21042)

### 4.1 Accept loop

The `tcp_listener` task shall accept TCP connections on `0.0.0.0:21042`
and spawn a `peer_handler` task per connection. The connection is
unauthenticated until the announce signature verifies (§4.3).

### 4.2 Frame decoding

Each TCP byte stream is parsed as a sequence of length-prefixed
R2-WIRE frames (per WIRE §1.4). The dashboard shall:

* Read a u16 big-endian length.
* Read `length` bytes as one R2-WIRE compact frame.
* Decode the event hash, dispatch by name.

Malformed length prefixes or oversize frames (> 4 KiB) shall close the
connection.

### 4.3 Announce verification

The first frame received MUST be `r2.sensor.announce`. The dashboard
shall:

1. Decode the CBOR payload.
2. Reconstruct the canonical CBOR over keys 0..5 (WIRE §3.1).
3. Verify `sig` (key 6) as Ed25519 over the canonical bytes, using
   `device_pk` (key 0) as the verifying key.
4. (TOFU policy) Accept any well-formed signature in v0.1; record
   `device_pk` to `peers.json` if new.

Future versions SHALL replace TOFU with TG-cert-chain verification per
R2-TRUST: the device presents a cert chain rooted at `TG_PUB_KEY`, and
the announce signature is over a fresh nonce signed by the cert's
device key.

### 4.4 Per-peer dispatch

After a successful announce:

* The dashboard sends `r2.dash.ack {through_seq: high_water[device_pk]
  or last_seq}` to confirm resume point.
* The dashboard MAY immediately send a `r2.dash.config.set` if the peer
  has been renamed/reroled in the UI since last contact.
* Subsequent frames are dispatched by event hash:

| Event hash | Handler |
|---|---|
| `r2.sensor.acceleration` | sample buffer + WS push (§5.2) |
| `r2.sensor.acceleration.batch` | sample buffer + WS push, ack-eligible |
| `r2.sensor.battery` | battery widget update |
| `r2.sensor.status` | peer status display |
| `r2.sensor.cal.sample.resp` | calibration handler (§9) |
| `r2.sensor.sync_pong` | time-sync handler (§8) |
| `r2.sensor.event.log` | event log panel |
| (unknown) | log + ignore (per WIRE §2) |

### 4.5 ACK cadence

The peer handler shall transmit `r2.dash.ack {through_seq: N}` where
`N` is the highest received `seq`, at the rate of:

* every **200 ms**, OR
* every **100 received samples**,

whichever occurs first (per WIRE §4.1).

---

## 5. HTTP + WebSocket (port 8080)

### 5.1 HTTP routes

| Route | Method | Purpose |
|---|---|---|
| `/` | GET | WASM viewer bundle — `webapp/` static tree (HTML + JS + `pkg/` WASM); the canonical browser frontend (the pre-Phase-5d server-decoded `/` HTML was removed once `/v/` reached parity and got promoted to root) |
| `/api/peers` | GET | Current peers (snapshot) |
| `/api/peers/:pk` | PATCH | Update label/role/joint assignment |
| `/api/joints` | GET, POST, PATCH, DELETE | Manage joint definitions |
| `/api/calibration/:pk` | GET, DELETE | Inspect / clear calibration |
| `/api/calibration/:pk/sample` | POST `{position: "A"\|"B", ms: 1000}` | Trigger cal sample request |
| `/api/calibration/:pk/compute` | POST | Compute R from stored g_A/g_B |
| `/api/stream/:pk/start` | POST `{rate_hz, range}` | Forward to sensor |
| `/api/stream/:pk/stop` | POST | Forward to sensor |
| `/api/bootstrap` | POST | Trigger BLE discovery + bootstrap loop (§6); re-press cancels and restarts |
| `/api/bootstrap/status` | GET | `{running: bool, log: [...]}` snapshot |
| `/api/ota` | POST `{pk, url, sha256}` | Trigger OTA (§13) |
| `/api/reset/:pk` | POST `{factory: bool}` | Forward to sensor |
| `/ws/raw` | WebSocket | **Binary** R2-WIRE frames forwarded verbatim from sensors (canonical Phase-5d transport) |
| `/ws/status` | WebSocket | **Text JSON** status events: bootstrap progress, hotspot up/down, server-side errors |

Auth: v0.1 has no auth — the dashboard is bound to localhost-or-LAN
on a private hotspot. Future versions SHOULD add a session cookie or
token if exposed beyond the lab network.

### 5.2 `/ws/raw` — binary frame transport

The canonical Phase-5d browser data path. Each WS message is a single
binary envelope wrapping one verbatim R2-WIRE frame from one sensor:

```
[u16 BE  src_addr_len ][src_addr  utf-8       ]
[u32 BE  ts_ms_low32  ][u16 BE  frame_len    ][frame  raw bytes]
```

* `src_addr` is the sensor's TCP socket address (`ip:port`) as observed
  by the dashboard — used by the browser to key per-peer state.
* `ts_ms_low32` is the dashboard wall-clock time of receipt, low 32 bits
  of `unix_epoch_ms`. The full upper bits are reconstructed
  client-side from local clock context.
* `frame` is the unmodified R2-WIRE compact frame as defined in
  `SPEC-R2-ROCKER-WIRE` §3 — header + signed CBOR payload.

Frames are forwarded **without decode, decimation, or filtering** at the
server. The browser's vendored `r2-wasm` performs decode, signature
verification, and rate decimation per `SPEC-R2-ROCKER-WIRE` §4.1.

Browser → server: messages on `/ws/raw` are dropped silently in v0.1
(out-of-band control will move to `/ws/status` or a dedicated channel).

### 5.3 `/ws/status` — text JSON status channel

Status events that are **not** R2-WIRE frames — primarily the bootstrap
loop's progress, plus server-side conditions the operator should see.
Each WS message is a UTF-8 JSON object of the form `{type: "...", ...}`:

| Type | Payload | Description |
|---|---|---|
| `bootstrap` | `{event: <BootstrapEvent>}` | One step in the discovery loop; payload mirrors `r2_bootstrap::BootstrapEvent` serialised as `{kind, data}` (Log / SensorFound / SensorConnected / Done / Error), plus the synthetic `{Reset: null}` shape emitted by the dashboard on operator-triggered restart. See §6.5. |
| `hotspot` | `{state: "up" \| "down" \| "creating", ssid?: str}` | NetworkManager hotspot lifecycle |
| `server_error` | `{level: "warn"\|"error", message: str}` | Operator-visible server condition |

Browser → server messages on `/ws/status` are reserved for future
use (subscribe / viewport_hint per §5.4); v0.1 ignores them.

### 5.4 Browser → server messages

The browser does not currently send messages on `/ws/raw` or `/ws/status`.
Future versions will use `/ws/status` for:

| Type | Payload | Description |
|---|---|---|
| `subscribe` | `{peers: [...] \| "all"}` | Filter pushes |
| `viewport_hint` | `{visible_peers: [...]}` | Server prioritises these |

### 5.5 Removed legacy endpoints

The pre-Phase-5d dashboard exposed two transitional channels — `GET /`
(server-decoded SPA bundle, embedded HTML) and `GET /ws` (bidirectional
JSON WebSocket where the server decoded R2-WIRE frames). Both were
removed once the WASM viewer reached parity. New consumers MUST use
`/ws/raw` + `/ws/status` (defined above) and connect to the WASM viewer
served at `/`.

---

## 6. Bootstrap engine

### 6.1 Trigger

Bootstrap runs on demand: the user clicks "Connect Sensors" in the UI,
hitting `POST /api/bootstrap`. The dashboard shall NOT auto-discover on
start; discovery is operator-controlled. Re-pressing the button while a
bootstrap loop is active SHALL cancel the in-flight task and start a
fresh one (a `Reset` `bootstrap` event is emitted on `/ws/status` to
signal this to viewers).

### 6.2 WiFi hotspot

The dashboard SHALL ensure a WiFi hotspot is active before starting
discovery:

1. Check NetworkManager for a hotspot connection on the configured
   adapter (default `wlan0` on Pi, configurable per host).
2. If active, reuse it.
3. If not, create one via `nmcli connection add type wifi mode ap`
   with SSID `r2-rocker-<6 hex>` and a random WPA2-PSK regenerated
   only if no prior `r2-rocker` profile exists.
4. Wait for the hotspot to be ready (carrier up, IP assigned).

The hotspot's gateway IP (typically 10.42.0.1 with NetworkManager) is
the value embedded in `#wifi_offer.gateway_ip`.

### 6.3 BLE scan

The dashboard shall scan for BLE advertisements matching R2-BEACON
(per R2-BEACON spec). The canonical r2-rocker sensor class string is:

```
nz.ac.auckland.rocker.sensor   →   FNV-1a-32 hash 0x6A3B0860
```

(Reverse-DNS per R2-BEACON §4 recommendation; institutional + project
scope so a stray r2-notekeeper or generic R2 device on the air doesn't
get pulled into the rig's bootstrap loop.) For each unique RBID found,
spawn a per-sensor bootstrap task.

Scan duration: 10 s; if zero matches, the loop sleeps 20 s and retries.

### 6.4 Per-sensor bootstrap

Per the R2-BOOTSTRAP flow (with the deviation that the dashboard
initiates per the M10 demo):

1. Connect L2CAP CoC (PSM 0x00D2) — retry up to 5× with 3 s delay.
2. Compose `#wifi_offer {ssid, psk, gateway_ip}`.
3. **Sign** the offer with the TG private key (Ed25519 over canonical
   CBOR per R2-WIFI).
4. Send the signed offer over L2CAP.
5. Disconnect L2CAP.
6. Wait for the sensor to appear at `gateway_ip:21042` (TCP connect
   inbound), with a 60 s presence timeout.
7. On TCP connect, the announce verification (§4.3) closes the loop;
   the bootstrap task reports success to the UI.

Per-sensor tasks run in parallel — sensors are not queued (per the M10
demo's lesson; each sensor filters by its own RBID).

### 6.5 Bootstrap log

Every bootstrap step emits a `bootstrap` event on `/ws/status` (per §5.3)
so the operator can watch progress. The event's inner `event` field
serialises `r2_bootstrap::BootstrapEvent` as `{kind: <variant>, data: <payload>}`:

| `kind` | `data` | When |
|---|---|---|
| `Log` | `"<free-text status line>"` | Generic progress (BLE adapter init, retry counters, hotspot up, etc.) |
| `SensorFound` | `{addr: str, name: str}` | BLE beacon matched; about to connect L2CAP |
| `SensorConnected` | `{addr: str, name: str, ip: str}` | Sensor passed TCP ping/pong validation |
| `Done` | `{count: int}` | Scan window closed; loop continues with next retry interval |
| `Error` | `"<message>"` | Recoverable per-sensor failure |

Additionally the dashboard emits a synthetic `{Reset: null}` event (note:
not in the `{kind, data}` shape) when the operator re-presses *Connect
Sensors*; viewers SHOULD treat this as a signal to clear their bootstrap
log panel before further events arrive.

---

## 7. Per-peer state machine (server view)

| State | Description |
|---|---|
| `KNOWN_OFFLINE` | In `peers.json`, no current TCP session |
| `CONNECTING` | TCP up, awaiting announce |
| `ONLINE_LIVE` | Live frames arriving, latency healthy |
| `ONLINE_CATCHUP` | Receiving batched frames |
| `STALE` | No frame for > 10 s but TCP still open |
| `OFFLINE` | TCP closed; reverts to `KNOWN_OFFLINE` after 30 s grace |

Transitions are observable to the UI via `peer_state` WS events.

---

## 8. Time synchronisation

Per WIRE §7. The dashboard maintains a per-peer offset:

* On TCP connect, send `r2.dash.sync_pulse {req_id, dash_ts_ms}` at
  1 Hz for the first 30 s.
* Thereafter, every 30 s.
* On each `r2.sensor.sync_pong` reply:
  ```
  T1 = dash_ts_ms_at_send
  T3 = dash_ts_ms_at_recv
  T2 = sensor_ts_ms_in_pong
  rtt = T3 − T1
  offset_estimate = T1 + (rtt / 2) − T2
  offset_smoothed = α · offset_estimate + (1 − α) · offset_smoothed
                    where α = 0.2
  ```

To convert a sensor `ts_ms` to dashboard wall clock:

```
ts_local = ts_ms + offset_smoothed[device_pk]
```

The dashboard SHALL store the smoothed offset alongside the peer
metadata (in-memory; recomputed on each session — not persisted).

---

## 9. Server-side calibration

### 9.1 Sample collection

On `POST /api/calibration/:pk/sample {position, ms}`, the dashboard
shall:

1. Send `r2.dash.cal.sample.req {req_id, position, ms}` to the sensor.
2. Await `r2.sensor.cal.sample.resp {req_id, position, gx, gy, gz,
   n_samples}` (10 s timeout).
3. Store `(gx, gy, gz)` in the calibration entry's `g_a` or `g_b`
   field per `position`.
4. Return success to the UI.

### 9.2 Computation

On `POST /api/calibration/:pk/compute` (or automatically once both
positions exist):

```rust
let g_a = vec3(g_a_x, g_a_y, g_a_z);
let g_b = vec3(g_b_x, g_b_y, g_b_z);

let delta = g_b - g_a;
let separation_g = delta.norm() / LSB_PER_G[range];
if separation_g < 0.30 {
    return Err("Calibration swing too small (need ≥ 0.3 g separation)");
}

let main     = delta.normalize();
let vertical = ((g_a + g_b) * 0.5).normalize();
let sideways = main.cross(&vertical).normalize();

// R rotates body-frame samples to (main, sideways, vertical) frame:
let r = Mat3::from_rows(&main, &sideways, &vertical);

calibration.r            = r;
calibration.main_axis    = main;
calibration.sideways_axis = sideways;
calibration.vertical_axis = vertical;
calibration.valid        = true;
```

Where `LSB_PER_G` is `[256000, 128000, 64000]` indexed by range.

The dashboard SHALL refuse calibration when `|g_b − g_a| < 0.30 g`
(PLAN D-18) and surface the error in the UI.

### 9.3 Application

For every received `r2.sensor.acceleration{x,y,z}` from a calibrated
peer, the dashboard computes:

```
(a_main, a_sideways, a_vertical) = R · (x, y, z)  / LSB_PER_G[range]
```

These rotated values are emitted in the WS `acceleration` push (§5.2).
For un-calibrated peers, only `(x_g, y_g, z_g)` raw axes are emitted;
the UI shows a "Calibrate" badge.

---

## 10. Joint groups & differential analysis

### 10.1 Joint definitions

Joints are defined via the UI (`POST /api/joints`); each joint has a
list of member device-pks and a topology (§3.4). For v0.1, the
default joint configuration is created automatically on first run with
each rocker-mounted peer assigned to its own single-sensor joint;
operators promote pairs as additional sensors join.

### 10.2 Per-joint metric (single-sensor)

For a joint with `topology = "single"` containing peer `P`:

```
metric(t) = a_sideways[P, t]
```

Reported as instantaneous magnitude on the WS stream.

### 10.3 Per-joint metric (pair)

For a joint with `topology = "pair"` containing peers `A` and `B`:

```
Δa(t) = a_sideways[A, t_aligned] − a_sideways[B, t_aligned]
```

`t_aligned` requires interpolating both peers' samples to a common
time grid using each peer's smoothed offset (§8). The dashboard
maintains a per-peer ring buffer of the last 10 s of rotated samples
and resamples to a 1 ms grid for differencing.

### 10.4 Reference channel (informative, future)

For joints with a member of `mounting_role = "bed"` declared as a
reference, the dashboard MAY subtract:

```
Δjoint_clean(t) = Δa(t) − a_sideways[bed_ref, t_aligned]
```

This isolates joint-local motion from environmental vibration. Not
required in v0.1.

---

## 11. Stress indicator & trend

### 11.1 Sliding-window RMS

The analytics task shall, at 1 Hz per joint, compute:

```
rms(t) = sqrt( mean( Δa(t − W..t)² ) )      where W = 10 s
peak(t) = max( |Δa(t − W..t)| )
```

Emitted as a `joint_metric` WS event each second.

### 11.2 Long-term trend

The analytics task shall persist daily summaries to disk under
`state/trends/<joint_id>/<YYYY-MM-DD>.json`:

```json
{
  "joint_id": "front-left-actuator",
  "date": "2026-05-07",
  "samples_n": 86400,
  "rms_p50": 0.04, "rms_p95": 0.08, "rms_max": 0.18,
  "peak_p50": 0.10, "peak_p95": 0.22, "peak_max": 0.45,
  "first_ts": "...", "last_ts": "..."
}
```

The trend chart in the UI plots `rms_p50` and `rms_max` against date
for each joint.

### 11.3 Threshold alerting

When `rms(t) > stress_threshold_g_rms` (joint config) for a continuous
2 s window, the dashboard shall:

1. Emit a `joint_alert` WS event.
2. Append to `events.log`.
3. Surface a banner in the UI until acknowledged.

Thresholds are configured per joint via the UI; default 0.25 g RMS,
adjustable empirically once baseline data is collected.

---

## 12. Browser UI requirements

### 12.1 Layout

The SPA shall use a **CSS grid auto-fit** container with `min-width:
320px` per card so the layout reflows from 1 column (mobile) up to
~6 columns at 1920 px width.

A persistent header at the top shows:

* Total peers (online / known).
* Total joints with current stress: green / yellow / red counts.
* Connection-state widget for the dashboard itself.
* "Discover" button (BLE bootstrap trigger).

### 12.2 Cards

Three card types arranged in the grid:

1. **Peer card** — one per online sensor:
   * Hostname / label / mounting role.
   * Live state (LIVE / CATCHUP / STALE / etc.).
   * Battery widget (voltage + percent + charging icon).
   * Mini-chart (canvas, 200 × 60 px) showing `a_sideways` for the
     last 30 s (or `a_z` if uncalibrated).
   * Status line: `seq`, `rate_hz`, `sd_pct_used`.
   * Actions: Calibrate / Stream Start / Stream Stop / OTA / Reset
     (collapsed into a menu).

2. **Joint card** — one per defined joint:
   * Joint label.
   * Member peer hostnames.
   * Stress gauge: current RMS + threshold, colour-coded.
   * Trend sparkline (7 days).
   * Actions: edit threshold, edit members.

3. **Bootstrap log card** — appears during discovery:
   * Per-sensor stage progress.
   * Auto-dismisses 30 s after discovery completes.

### 12.3 Charts

For sensor count > 8, the per-peer mini-charts shall render via
**HTML5 Canvas** (not SVG / Plotly) to avoid jank. Update rate ≤
30 Hz; coalesce multiple samples per frame. Larger detail charts
(opened by clicking a peer) MAY use richer libraries.

### 12.4 Calibration flow

A modal wizard with three steps:

1. "Place rig at position A. Hold steady. Click Capture."
2. "Move rig to position B. Hold steady. Click Capture."
3. "Calibration computed. Δ = X.XX g across (mx, my, mz). Save?"

If `|g_b − g_a| < 0.3 g`, the wizard refuses to advance past step 2
with a clear error.

### 12.5 Accessibility

The UI shall be usable with keyboard navigation, screen-reader labels
on all interactive elements, and prefers-reduced-motion respected for
chart animations. Colour-only signalling is forbidden — every red /
yellow / green state has a textual or iconographic counterpart.

---

## 13. OTA push

### 13.1 Trigger

Operator submits `POST /api/ota {pk, url, sha256}`. The dashboard
shall:

1. Verify the host serving `url` is reachable (HEAD request, 5 s
   timeout).
2. Compute and verify SHA-256 of the binary by streaming a HEAD-then-GET
   if possible, or trust-but-warn.
3. (v1.0) Sign the `(url || sha256)` tuple with the TG private key.
4. Send `r2.dash.fw.update {url, sha256, tg_sig}` to the peer.
5. Monitor `r2.sensor.event.log` for `OTA_*` codes; surface to UI.
6. After the sensor reboots and reconnects, log success.

### 13.2 Bulk OTA

Bulk OTA across multiple peers shall be sequential by default to avoid
saturating the hotspot. A future "rolling parallel" mode MAY be added.

---

## 14. Configuration

### 14.1 CLI / config file

The dashboard reads config in this precedence (highest first):

1. CLI flags (e.g. `--state-dir`, `--http-port`, `--tcp-port`, `--tg-priv`).
2. Environment variables (`R2_ROCKER_STATE_DIR`, etc.).
3. `~/.config/r2-rocker/dashboard.toml`.
4. Built-in defaults.

### 14.2 Required keys

| Key | Default | Description |
|---|---|---|
| `state_dir` | `~/.config/r2-rocker/state/` | Where peers/calibration/joints live |
| `tg_priv_path` | `~/.config/r2-rocker/tg_signer/tg_priv.bin` | TG signing key (off-tree) |
| `tg_pub_path` | `<repo>/trust_keys/tg_pub.bin` | TG verifier key (in-tree) |
| `http_port` | 8080 | UI |
| `tcp_port` | 21042 | Sensor inbound |
| `wifi_iface` | `wlan0` (linux) | Hotspot adapter |
| `hotspot_ssid_prefix` | `r2-rocker-` | Generated SSID = prefix + 6 hex |

The TG private key path must be **outside** the repo working tree
(see SECRETS-POLICY).

---

## 15. Conformance

A dashboard build conforms to this specification when the following
acceptance tests pass against the reference firmware (or a simulator
implementing `SPEC-R2-ROCKER-WIRE`):

### 15.1 Listener acceptance

1. TCP :21042 accepts a connection, decodes a valid announce, admits
   the peer to `peers.json`, replies with an ACK within 100 ms.
2. Malformed length prefix closes the connection; the dashboard does
   not crash.
3. Concurrent connections from 30 simulated peers complete announce
   within 2 s of each peer's connect.

### 15.2 UI acceptance

1. Browser at `http://localhost:8080/` loads the SPA in < 1 s on a
   modern laptop.
2. Per-peer cards update at ≥ 10 Hz; with 30 simulated peers
   transmitting at 100 Hz, the browser's main thread frame rate stays
   ≥ 30 fps.
3. Calibration wizard refuses `|g_b − g_a| < 0.3 g`.
4. Stream start / stop / OTA / reset commands round-trip to a
   connected peer and reflect back as state changes within 2 s.

### 15.3 Bootstrap acceptance

1. With a fresh sensor advertising R2-BEACON, the "Discover" flow
   completes (BLE → L2CAP → wifi_offer → WiFi join → TCP announce)
   within 60 s.
2. The signed `#wifi_offer` verifies on the sensor (announce arrives).
3. Aborting discovery (UI button) cleanly tears down in-flight L2CAP
   sessions and leaves the WiFi hotspot intact.

### 15.4 Analytics acceptance

1. With a single calibrated peer, the dashboard's `a_sideways` matches
   a hand-computed value (via captured raw samples and `R`) to within
   1% over a 60 s window.
2. With a paired joint, a synthetic correlated motion injected into
   both peers produces near-zero `Δa`; a synthetic differential
   produces the expected magnitude.
3. Threshold alerting fires within 2.5 s of a sustained over-threshold
   condition; clears within 5 s of the condition ending.

### 15.5 Persistence acceptance

1. Dashboard restart preserves peers, calibration, joints, and
   high_water; on reconnect, sensors resume from `last_acked` without
   data loss.
2. Killing the dashboard during write does not corrupt JSON files
   (atomic replace via tempfile + rename).

---

## 16. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-07 | 0.1 | Initial draft. Process model, listeners, bootstrap, calibration, joints, analytics, UI, OTA, conformance. |
