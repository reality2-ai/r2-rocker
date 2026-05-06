# r2-rocker

Wireless structural-health-monitoring sensor system for a half-tonne
actuator-driven rocker rig. Detects lateral-shear at actuator joints by
streaming differential accelerometer data from many ESP32-S3 + ADXL355
sensor nodes to a Reality2 dashboard over WiFi, with on-device SD-card
buffering for store-and-forward when the network can't keep up.

Built on the [Reality2][r2] protocol stack (BLE bootstrap → trust group
→ WiFi → R2-WIRE) but vendored self-contained for university handoff.

## Status

**Phase 0 — scaffolding & specs.** No code yet. Hardware wiring frozen
at v0.1. See `plan/PLAN.md` for current phasing and `specifications/` for
the wiring spec; firmware specs to follow before any code lands.

## Repo layout

```
r2-rocker/
├─ specifications/   ← source-of-truth specs (drive code, not generated from it)
│   ├─ HARDWARE-WIRING.md      ← soldering-ready wiring (Phase 1/2/3)
│   ├─ SECRETS-POLICY.md       ← what's secret, where it lives, why
│   └─ SPEC-R2-ROCKER-*.md     ← system, sensor, dashboard, wire (TBD)
├─ plan/             ← consolidated phasing & decisions (PLAN.md)
├─ conversation/     ← verbatim design sessions (research data; paper material)
├─ docs/             ← vendor datasheets, external links
├─ trust_keys/       ← TG public material only (never private keys)
├─ firmware/         ← ESP32-S3 sensor firmware (Phase 1+)
├─ dashboard/        ← controlling-device dashboard (Phase 4+)
├─ tools/            ← provisioning, OTA, build helpers
├─ PROCESS.md        ← workflow discipline
└─ README.md         ← (this file)
```

## Hardware (Phase 1)

* **Sensor node**: ESP32-S3-DevKitC-1 + EVAL-ADXL355-PMDZ + microSD breakout
  + LiPo cell + charger module.
* **Controlling device**: Linux laptop or Raspberry Pi running
  `dashboard/` (creates WiFi hotspot, BLE-bootstraps sensors, displays
  data in browser at `http://localhost:8080`).

See `specifications/HARDWARE-WIRING.md` for the soldering plan.

## Quick start (once Phase 1 is implemented)

```bash
# Sensor (ESP32-S3) — flash firmware:
cd firmware/esp32-s3
cargo run --release

# Controlling device — run dashboard:
cd dashboard
cargo run --release
# open http://localhost:8080
```

Click "Discover" — sensors are auto-paired via BLE, joined to the laptop's
hotspot, and start streaming. Calibrate via the in-browser flow.

## License

TBD before public/university release.

## Reading order for a new contributor

1. `README.md` (you are here)
2. `PROCESS.md` — how we work
3. `plan/PLAN.md` — what we're building, what's done, what's next
4. `specifications/HARDWARE-WIRING.md` — physical sensor build
5. `specifications/SECRETS-POLICY.md` — before touching any keys
6. Latest entry in `conversation/` — design rationale & current thinking

[r2]: https://github.com/reality2-ai
