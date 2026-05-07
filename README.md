# r2-rocker

Wireless **structural-health-monitoring** sensor system for a half-tonne
actuator-driven rocker rig. The rig tests tyre rubber on an asphalt
bed, but the *failure mode we instrument for* is shear at the
actuator joints, suspected of being driven by an unintended lateral
component of the rocker's motion. Sensors detect that lateral motion
as a diagnostic.

The diagnostic of interest is **differential lateral motion across
joints**, computed dashboard-side from per-sensor body-frame samples
rotated to a common rig frame via two-position calibration. Single-
sensor lateral acceleration is too noisy on its own.

Built on the [Reality2][r2] protocol stack (BLE bootstrap → trust
group → WiFi → R2-WIRE) but **vendored self-contained** so the entire
project — protocol crates, dashboard, firmware, tools — can be handed
off to the university as a single repository.

## How it works (a non-traditional architecture)

If you're used to thinking *"the dashboard is a website running on a
server somewhere; the browser is just a viewer of that server's
data"*, r2-rocker is **not that**. The eventual shape (Phase 5d
onward) is closer to how an encrypted-messenger app works:

```
                        ┌──────────────────────────┐
                        │  Your browser            │
                        │   running the WebApp     │
                        │   IT IS the viewer       │
                        │   AND the protocol stack │
                        │   AND a trust-group      │
                        │   member, all in WASM    │
                        └──────────▲───────────────┘
                                   │ encrypted only
                                   │
                ┌──────────────────┴─────────────────┐
                │  Relay (or local hotspot link)     │
                │  Forwards sealed envelopes between │
                │  trust-group members. Never sees   │
                │  what's inside.                    │
                └──────────▲─────────────────────────┘
                           │
                ┌──────────┴───────────┐
                │  Onsite controller   │     The privileged authority
                │  (a Linux box on     │     for your trust group.
                │  the rig floor)      │     Talks to sensors, signs
                │  · TCP from sensors  │     things, archives data.
                │  · KeyHolder         │
                │  · Local archive     │
                └──────────▲───────────┘
                           │ WiFi (controller's hotspot)
                           │
                      ┌────┴────┐
                      │ Sensors │
                      └─────────┘
```

A few things this implies that may be unfamiliar:

- **There is no central web server with a database.** The "data" lives
  in: each sensor's SD card, the onsite controller's local archive,
  and (eventually) a small VPS that combines relay + long-term store
  — owned by you, not a third party. The relay forwards *sealed
  envelopes* and cannot read them.
- **The browser is a hive.** Same WebApp bundle (downloaded from
  GitHub Pages over the internet, or from the onsite controller over
  the local hotspot — byte-identical) runs in any browser. It loads
  the R2 protocol stack as WebAssembly, which decodes / verifies /
  signs frames itself. Plain JavaScript handles the UX (charts,
  layout, controls); WASM handles the cryptography.
- **Sensors and the controller are members of the trust group
  automatically.** They ship with the right keys baked into firmware
  / config. They don't enrol.
- **Every browser, however, must enrol** to become a trust-group
  member — there's no public log-in. The onsite dashboard's "Enrol
  device" button shows a QR code (or shareable link). You scan it
  with the device you want to add (laptop, tablet, phone). The
  WebApp opens, generates its own keypair, presents the one-time
  token, and the controller signs a device certificate that proves
  TG membership. The certificate stays in the device's IndexedDB —
  next time, the device is just a member.
- **Onsite and remote use the same app.** The onsite controller is
  the WebApp's host on the local hotspot; GitHub Pages is the
  WebApp's host on the public internet. Same app either way; the
  difference is which transport (local-hotspot or relay) the device
  uses to reach the trust group.
- **Closed-network deployments work without any internet.** Onsite
  controller, sensors, and a tablet on the controller's hotspot is
  the whole rig — no cloud, no third party, no GitHub.

If you want the full architectural details (deployment-host
breakdown, trust model, enrolment flow, phase plan), see
[`AI-CONTEXT.md`](AI-CONTEXT.md) and [`plan/PLAN.md`](plan/PLAN.md).

## Current status (2026-05-07)

**End-to-end demo is alive.** A simulated-data ESP32-S3 sensor (the
ADXL355 is not yet soldered) connects to a NetworkManager-managed AP
on the laptop's USB WiFi adapter, joins, and streams R2-WIRE frames
to the dashboard. The dashboard decodes, decimates to 10 Hz for the
browser, and plots live x/y/z + battery in Chart.js.

| Done | What |
|---|---|
| ✅ | All v0.1 specs (`specifications/SPEC-R2-ROCKER-{WIRE,SENSOR,DASHBOARD,SYSTEM}.md`) |
| ✅ | Reference hardware wiring (`specifications/HARDWARE-WIRING.md`) — Phase 1/2/3, RGB-LED state map |
| ✅ | Secrets policy — TG private key off-tree; .gitignore + audit |
| ✅ | TG keygen / verify / inspect tool (`tools/r2-rocker-tg`) |
| ✅ | ESP32-S3 firmware skeleton — boot, WiFi, TCP, R2-WIRE compact frames, CBOR encoder |
| ✅ | Synthetic accelerometer + battery simulator (replaces the not-yet-soldered ADXL355) |
| ✅ | Two-OTA-slot partition table (3 MB ota_0 + 3 MB ota_1 + FAT storage) baked into firmware |
| ✅ | Per-device Ed25519 identity, NVS-persisted, signed announce |
| ✅ | Dashboard scaffold — vendored from `r2-core/tools/r2-dashboard`, schema-adapted, server-side LSB→g scaling, /api/version |
| ✅ | Versioning — git SHA + build timestamp stamped into both firmware fw_ver and dashboard /api/version |
| ✅ | Hotspot setup script (`tools/setup-hotspot.sh`) for the temporary `wifi_config.toml` bridge |
| ✅ | End-to-end verified on real hardware (MAC `1c:db:d4:41:28:3c`) — announce + acceleration + battery flowing |

| Pending | What |
|---|---|
| ⏳ | Dashboard verifies announce signature against announced device_pk |
| ⏳ | RGB LED state machine (WS2812 RMT driver + animator) per `HARDWARE-WIRING.md` §5 |
| ⏳ | HMAC envelope on every R2-WIRE frame (Phase 5c) |
| ⏳ | r2-relay forwarding for remote dashboards (Phase 5d) |
| ⏳ | Cloud archive consumer (Phase 5e) |
| ⏳ | BLE bootstrap FSM in firmware — retires `wifi_config.toml` (Phase 6) |
| ⏳ | SD-ring store-and-forward (Phase 3) — needs ADXL355 + microSD soldered |
| ⏳ | ADXL355 SPI driver (Phase 2) — needs hardware soldered |

Full phasing in `plan/PLAN.md`.

## Repo layout

```
r2-rocker/
├─ Cargo.toml          ← workspace root (excludes firmware/esp32-s3 — different toolchain)
├─ crates/
│   ├─ r2-fnv/         ┐
│   ├─ r2-cbor/        ├ Reality2 protocol crates, vendored self-contained
│   ├─ r2-wire/        │  (no path deps to outside the repo)
│   ├─ r2-core/        │
│   └─ r2-bootstrap/   ┘
├─ dashboard/          ← Axum + WS + Chart.js webapp on :8080, R2-WIRE TCP on :21042
├─ firmware/
│   └─ esp32-s3/       ← Sensor firmware. ESP-IDF + esp-idf-svc + Rust on xtensa.
├─ tools/
│   ├─ r2-rocker-tg/        ← TG keygen/verify/inspect CLI
│   ├─ setup-hotspot.sh     ← Bring up the NM AP profile on the USB WiFi adapter
│   └─ setup-firmware.sh    ← Pre-stage partitions.csv after a fresh clone
├─ trust_keys/         ← TG public key + cert (private key NEVER in repo)
├─ specifications/     ← Spec-first source of truth
│   ├─ HARDWARE-WIRING.md
│   ├─ SECRETS-POLICY.md
│   └─ SPEC-R2-ROCKER-{WIRE,SENSOR,DASHBOARD,SYSTEM}.md
├─ plan/PLAN.md        ← Living document — current state, decisions, open questions
├─ conversation/       ← Per-session verbatim design records (paper material)
├─ docs/               ← Vendor PDFs (ESP32-S3 datasheet, dev-kits, ADXL355) + ADICUP360 reference
├─ AI-CONTEXT.md       ← Fresh-AI-session entry point
├─ PROCESS.md          ← Workflow discipline (5 rules)
└─ README.md           ← (this file)
```

## Reference hardware

* **Sensor node** (×N): ESP32-S3-DevKitC-1-N8R8 + EVAL-ADXL355-PMDZ +
  microSD SPI breakout + single-cell LiPo (3.7 V, 1–2 Ah). Battery is
  removable via JST-PH; charging happens off-board in a dock.
* **Dashboard host**: Linux laptop or Raspberry Pi 4. Two WiFi adapters
  recommended — one to stay on your normal network, one dedicated to the
  r2-rocker AP (so the operator keeps internet access during deployment).

See `specifications/HARDWARE-WIRING.md` for connections.

## Quick start (current Phase 0.5+ demo path)

Phase 0.5+ uses a temporary `wifi_config.toml` bridge while the firmware
doesn't yet do BLE bootstrap. Once Phase 6 lands, the dashboard
auto-creates the AP and pushes signed wifi_offers via BLE — operator's
only action becomes "start the dashboard."

One-time setup after `git clone`:

```bash
# Pre-stage the partition table for the firmware build
./tools/setup-firmware.sh
```

Bring up the hotspot + flash + run:

```bash
# 1. Bring up the AP on the USB WiFi adapter (override iface with --iface=).
#    Generates fresh credentials with --rotate, otherwise reuses existing.
./tools/setup-hotspot.sh

# 2. Build + flash the firmware (one-time `espup install` for the Xtensa toolchain).
cd firmware/esp32-s3 && source ~/export-esp.sh
cargo build --release
espflash flash --port /dev/ttyUSB0 \
    --partition-table partitions.csv \
    target/xtensa-esp32s3-espidf/release/r2-rocker-firmware

# 3. Run the dashboard (foreground, prints a banner with version + ports).
cd ../.. && cargo run --release -p r2-dashboard

# 4. Open http://localhost:8080 in a browser.
```

Expected: a sensor card appears within ~30 s of the firmware booting,
showing live x/y/z (sine wave + lateral cosine + 1 g vertical), battery
voltage, and a fw_ver including the git short SHA.

## Tooling

| Command | What |
|---|---|
| `./tools/setup-hotspot.sh [--rotate]` | NetworkManager AP on the USB WiFi adapter. `--rotate` regenerates SSID/PSK in `wifi_config.toml`. |
| `./tools/setup-firmware.sh` | Pre-stage `partitions.csv` so the first firmware build picks up the custom OTA layout. |
| `cargo run -p r2-rocker-tg -- keygen --priv <off-tree> --pub trust_keys/tg_pub.bin --cert trust_keys/tg_cert.bin --name "..."` | Generate a fresh trust group keypair. Run **once** per deployment, off-tree. |
| `cargo run -p r2-rocker-tg -- verify trust_keys/tg_cert.bin` | Validate a TG cert's signature. |
| `cargo build --release -p r2-dashboard` | Build the dashboard. Then run from `target/release/r2-dashboard`. |
| `curl http://localhost:8080/api/version` | Dashboard version (semver + git SHA + build timestamp). |

## Versioning

Both firmware and dashboard stamp `<semver>+<git-short-sha>` (with
`-dirty` if uncommitted) at build time. The firmware reports its
version via `r2.sensor.announce`'s `fw_ver` field; the dashboard
exposes its version via `/api/version`. Together, these let the OTA
decision logic compare "what's running" vs "what's available" without
guesswork.

## License

TBD before public / university release.

## Reading order for a new contributor

1. `README.md` (you are here)
2. `PROCESS.md` — how we work (the 5 rules)
3. `AI-CONTEXT.md` — if you're an AI assistant, read this first instead
4. `plan/PLAN.md` — what we're building, what's done, what's next
5. `specifications/HARDWARE-WIRING.md` — physical sensor build
6. `specifications/SECRETS-POLICY.md` — before touching any keys
7. Latest entry in `conversation/` — design rationale & current thinking

[r2]: https://github.com/reality2-ai
