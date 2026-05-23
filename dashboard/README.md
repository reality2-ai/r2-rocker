# r2-dashboard

Live sensor dashboard for R2 networks.

Receives R2-WIRE event frames from sensor nodes and serves a live browser UI showing accelerometer traces, device names, run state, capture controls, and operator-plane actions (bootstrap, OTA, access management).

## Usage

```bash
# Build
cargo build --release -p r2-dashboard

# Run (single unified R2 port — sensors + browser + raw R2-WIRE)
./target/release/r2-dashboard
```

Open **http://localhost:21042** in a browser.

The dashboard listens on a single TCP port (`:21042` by default; override with `--port`). Per WIRE §13.5, every accepted connection is peeked at first byte:

* `0x00…` → raw R2-WIRE frame from a sensor or relay peer; dispatched to the sensor pipeline.
* `[A-Z]…` → HTTP `GET …` / `POST …`; handed to axum.
* WebSocket upgrades on `/r2` are negotiated by axum and then carry binary R2-WIRE frames (operator-plane cmd events + status broadcasts) in both directions.

## Architecture

```
Sensor (firmware)          Dashboard (r2-dashboard)             Browser (webapp = R2 hive)
─────────────────          ────────────────────────             ─────────────────────────
TCP :21042 ─ raw R2 ────►  peek-detect → R2 pipeline
                           verify announce (TG cert)
                           drive capture / SD ack /
                           ota / reset / identify

                           ─── /r2 WS (binary) ─────────────►   DashboardViewerSentant
                           status: r2.dash.* events             (inside R2RockerHive)
                                                                renders UI from sentant
                           ◄─── /r2 WS (binary) ────────────    state
                           cmd:  r2.dash.cmd.* events           operator clicks emit cmd
                                                                events via sendCmd()
                           OTA push: POST /api/ota/{addr}       (multi-MB binary upload;
                                                                the one operator-plane
                                                                HTTP route that survived
                                                                v0.2)
```

### State replay

When a browser opens the `/r2` WebSocket, the dashboard immediately replays an `r2.sensor.connected` event for every currently-connected sensor plus a snapshot of capture / run state. Opening the browser after sensors are already streaming shows live data instantly without polling.

### Device naming

Sensors send an `r2.sensor.announce` frame (TG-cert-signed) on TCP connect carrying their `device_pk`. The dashboard verifies under the embedded TG public key, attaches the operator-assigned alias (set via `r2.dash.cmd.device.alias.set`), and uses it in panel headers, CSV column titles, and capture filenames.

## Operator-plane events (`/r2`, binary R2-WIRE)

See `specifications/SPEC-R2-ROCKER-WIRE.md` rows 22–43 for the canonical table. Highlights:

| Family | Event | Direction |
|---|---|---|
| capture | `r2.dash.cmd.capture.{start,mark,stop}` | viewer → controller |
| sensor  | `r2.dash.cmd.{reset,identify}` | viewer → controller |
| bootstrap | `r2.dash.cmd.bootstrap` | viewer → controller |
| device  | `r2.dash.cmd.device.alias.set` | viewer → controller |
| access  | `r2.dash.cmd.access.{request,check,pending,approve,deny,revoke,members}` | viewer → controller |
| status  | `r2.dash.{bootstrap,ota,capture,reset}.progress`, `r2.dash.access.event`, `r2.peer.disconnected`, `r2.dash.device.alias.changed` | controller → viewers |

Every cmd carries a `req_id` in CBOR key 0 and is correlated by a `r2.dash.cmd.response`.

## Surviving HTTP routes

The v0.2 cleanup deleted ~17 legacy operator-plane HTTP handlers in favour of the cmd events above. Only a small set of operator-helper routes remain on the HTTP side of `:21042`:

| Route | Purpose |
|---|---|
| `GET /` + static assets | webapp host |
| `POST /api/ota/{addr}` | multi-MB firmware push (wrong shape for per-frame WS) |
| `GET /api/version` + `GET /api/firmware/*` | "needs update" version check + GH-release proxy |
| `GET /api/devices/aliases` | bulk alias map on webapp boot |
| `GET /api/access/onboard` + `GET /api/access/whoami/{pk}` | Link-tab UI helpers |
| `GET /api/data/{addr}/...` + `DELETE` + `GET /api/data/merged` | per-sensor + merged capture export |
| `GET /ws/logs/{addr}` | per-sensor live log tail (dashboard internal) |

## Field-proven

* **Sensors**: ESP32-S3 (DevKitC + XIAO carriers) with ADXL355 + microSD ring + LiPo.
* **Gateway**: any Linux host with WiFi (development on Tuxedo laptop x86_64; field use on Pi 5).
* **Data rate**: 1 kHz acceleration per sensor, decimated 100× before `/r2` broadcast (Pi5 deployment).
* **Multi-sensor**: 4 sensors validated end-to-end; no architectural cap.
