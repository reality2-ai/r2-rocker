# r2-dashboard

Live sensor dashboard for R2 networks.

Receives R2-WIRE event frames from sensor nodes over TCP and serves a live browser UI showing accelerometer traces, device names, run state, and session controls.

## Usage

```bash
# Build
cargo build --release -p r2-dashboard

# Run (listens on TCP:21042 for sensors, HTTP:8080 for browser)
./target/release/r2-dashboard
```

Open **http://localhost:8080** in a browser.

## Architecture

```
Sensor (r2-sensor)        Dashboard (r2-dashboard)        Browser
──────────────────        ────────────────────────        ───────
TCP:21042 ──────────────► decode R2-WIRE frames
                          broadcast via WebSocket ────────► live charts
                          sensor.connected replay ─────────► on page load

Browser ──── WebSocket ──► receive DashboardCommand
                          forward as R2-WIRE ──────────────► sensor
```

### State replay

When a browser connects via WebSocket, the dashboard immediately replays `sensor.connected` events for all currently-connected sensors. This means opening the browser after sensors are already streaming will show live data instantly.

### Device naming

Sensors send an `r2.sensor.announce` frame on TCP connect containing their hostname. The dashboard attaches this name to all subsequent events and shows it in the panel header.

## Events

| Event | Source | Description |
|-------|--------|-------------|
| `acceleration` | sensor | x/y/z accelerometer data (g) |
| `gyroscope` | sensor | x/y/z gyroscope data |
| `run_state` | sensor | idle / running / calibrating / stopped |
| `r2.sensor.announce` | sensor | device name (hostname) |
| `sensor.connected` | dashboard | new sensor connected (browser notification) |

## Commands (browser → sensor)

| Command | Description |
|---------|-------------|
| `start` | Begin recording session |
| `stop` | End recording session |
| `mark` | Mark a lap/event |
| `calibrate` | Set calibration offset |

## Field-proven

Tested with:
- **Sensor**: Unihiker M10 (aarch64, RTL8723DS) running r2-sensor + m10_sensor.py
- **Gateway**: Tuxedo laptop (x86_64) running r2-bootstrap hotspot
- **Data rate**: 10Hz accelerometer, <1ms dashboard processing latency
- **Multi-sensor**: up to N sensors simultaneously (no cap)
