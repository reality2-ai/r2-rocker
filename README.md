# r2-rocker

A wireless sensor system for monitoring a tyre-testing rig.

## What this does, in one paragraph

There's a half-tonne machine in a lab that studies how tyre rubber
wears against road surfaces. The asphalt sits as a flat bed at the
bottom; a slab of rubber sample is mounted on the *rocker* — the
moving upper part — and is driven back and forth across the asphalt
by hydraulic actuators to simulate a tyre travelling over a road.
The actuators bolt to the rocker through metal joints, and these
joints are showing stress in a way that suggests the rocker is
moving *sideways* a tiny bit when it's only meant to move *along
the direction of travel*. r2-rocker is the instrument that watches
for that sideways motion. Small battery-powered sensors clip onto
each joint, send their accelerometer readings live to a laptop on
the lab bench, and the laptop computes the difference in sideways
motion between joints — which is what we believe correlates with
the joint-shear failure mode.

The hardware is open. The protocol stack is open. The whole thing is
designed to be handed off to a university group, who can run, modify,
or extend it without depending on any third-party service.

## What you'll find in the box

This project has three kinds of device:

| | What it is | What it does |
|---|---|---|
| **Sensors** | Small ESP32-based boards (microcontroller + accelerometer + battery + SD card) | Sit on the rig at each joint. Sample motion ~100 times a second. Send the readings to the controller over WiFi. |
| **A controller** | Any Linux machine — your laptop, or a Raspberry Pi | Hosts a private WiFi hotspot the sensors join. Receives sensor data over that WiFi. Serves a web app you use to monitor and control the rig. Stores data locally. |
| **Viewers** | Any device with a web browser — laptop, tablet, phone | Show live data. From a few metres away (over the controller's hotspot) or from anywhere on the internet (over a relay). The controller chooses what each viewer is allowed to see and do. |

You always need at least one sensor and one controller. Viewers are
optional — the controller's own browser counts as a viewer.

## How it fits together

```
                ┌──────────────────────────┐
                │  Operator's browser      │   The web app: see live
                │  (laptop / phone /       │   data, manage devices,
                │   tablet)                │   push firmware updates.
                └──────────▲───────────────┘
                           │ encrypted
                           │
                ┌──────────┴───────────┐
                │  Controller          │   A Linux laptop or Pi on
                │  (on the rig floor)  │   the lab bench.
                │  · Hosts WiFi        │
                │  · Holds the keys    │
                │  · Stores the data   │
                └──────────▲───────────┘
                           │ WiFi (controller's hotspot)
                           │
                      ┌────┴────┐
                      │ Sensors │   Small boards on the rig joints.
                      └─────────┘
```

Things this implies that may surprise you if you've used a "normal"
cloud-app:

- **There's no central web server in the cloud holding your data.**
  Data lives on each sensor's SD card, plus the controller's local
  archive. If you want long-term offsite storage you can set it up,
  but it's optional and you own it. Nothing leaves the lab unless you
  decide it does.
- **Security is part of the protocol, not bolted on.** Sensors and
  the controller hold cryptographic keys; everything they send is
  signed and encrypted. A device that doesn't hold the right keys
  can't decode anything passing through, even if it's plugged into
  the same network.
- **The web app doesn't live on a server.** It loads as a small
  bundle of files into your browser, and once loaded it talks
  directly to the controller (or, for remote viewers, through a
  relay that just forwards sealed envelopes — it can't read them).
  This means the same app works onsite (no internet needed) and
  remotely (over the internet) without any change.
- **Browsers join temporarily.** When you want to view data on a new
  device, the operator presses a button on the dashboard which makes
  a QR code (or shareable link). Scanning it pairs that browser. No
  accounts, no passwords. The pairing can be revoked any time.
- **Closed-network deployments work without any internet.** A
  controller laptop, two sensors, and a tablet on the controller's
  hotspot are a complete instrument. No cloud, no GitHub, no
  third-party service.

## Reference hardware

You need:

- **One ESP32-S3-DevKitC-1-N8R8** development board per sensor.
  (Available from most electronics distributors. ~NZD 50.)
- **One ADXL355-PMDZ** accelerometer module per sensor. (Analog
  Devices' evaluation board for the ADXL355 chip. ~NZD 100.)
- **One microSD card breakout + microSD card** per sensor (any
  capacity ≥ 4 GB). For local data buffering.
- **One single-cell LiPo battery** per sensor (3.7 V, 1–2 Ah, JST-PH
  connector). Removable for off-rig charging.
- **One Linux laptop or Raspberry Pi** as the controller. Two WiFi
  adapters recommended — one to keep your usual internet, one
  dedicated to the rig.
- **Female-to-female DuPont jumper wires** (about 6 per sensor, for
  the Pmod-to-DevKitC connection).

The full wiring is in [`specifications/HARDWARE-WIRING.md`](specifications/HARDWARE-WIRING.md).

## Setting it up the first time

You only do this once per fresh checkout.

```bash
# 1. Get the source.
git clone https://github.com/reality2-ai/r2-rocker
cd r2-rocker

# 2. Install the Rust embedded toolchain (one-time, ~5 minutes).
#    Espressif's installer for the Xtensa toolchain the firmware needs.
cargo install espup
espup install
# Source the toolchain into your shell. Add to your ~/.bashrc /
# ~/.zshrc to do this automatically on future shells:
source ~/export-esp.sh

# 3. One-time firmware build setup.
./tools/setup-firmware.sh

# 4. Generate the cryptographic keys for your deployment. Run ONCE
#    per rig. The private key MUST be stored outside the repo
#    (e.g. an encrypted USB key); only the public key + cert get
#    committed. See specifications/SECRETS-POLICY.md.
cargo run -p r2-rocker-tg -- keygen \
    --priv  ~/secure/tg_priv.bin \
    --pub   trust_keys/tg_pub.bin \
    --cert  trust_keys/tg_cert.bin \
    --name  "my-rocker-rig"
```

## Day-to-day operation

After the first-time setup, normal use is:

```bash
# 1. Bring up the lab WiFi hotspot the sensors will join.
#    --rotate generates fresh credentials; without it, the previous
#    credentials are reused.
./tools/setup-hotspot.sh

# 2. Build the firmware and package it ready to flash AND ready to
#    push wirelessly. Produces TWO files: one for cabled flashing,
#    one archived under firmware/esp32-s3/releases/ for posterity.
./tools/build-firmware.sh

# 3. Flash a fresh sensor over USB. The DevKitC's USB-OTG port shows
#    up as /dev/ttyACM0 on Linux. Only needed once per chip — after
#    that, updates push wirelessly.
cd firmware/esp32-s3 && source ~/export-esp.sh
cargo espflash flash --release --port /dev/ttyACM0
cd ../..

# 4. Start the dashboard. Prints a banner with version + ports.
cargo run --release -p r2-dashboard

# 5. Open http://localhost:8080/v/ in your browser.
# 6. Click "Connect Sensors" and watch the LEDs.
```

The sensor's small RGB LED tells you what it's doing at a glance:

| LED | Meaning |
|---|---|
| Quick white flash | Just powered on. |
| Pulsing blue | Looking for a controller (no WiFi credentials yet). |
| Pulsing cyan | Joining the WiFi network. |
| Steady-heartbeat green | Connected, streaming data. |
| Strobing white | Receiving a firmware update. |
| Pulsing orange | Battery low. |
| Pulsing red | Something went wrong; reset and try again. |

The dashboard's web app shows a virtual copy of each sensor's LED
next to the device's name, so you can see the same status from across
the room.

## Updating firmware wirelessly

Once a sensor has been flashed once over USB, you don't need the
cable again:

```bash
./tools/build-firmware.sh        # produces a new .bin
```

In the dashboard, switch to the **Devices** tab, click *Update
Firmware* on the sensor's card, and pick the new `.bin` file
(`firmware/esp32-s3/target/xtensa-esp32s3-espidf/release/r2-rocker-firmware.bin`).
The sensor receives the image, checks its integrity, writes the
inactive partition, reboots into the new firmware, and rejoins.
Takes about 15 seconds.

If the new firmware is broken — can't join WiFi, or can't reach the
dashboard — the bootloader notices on the next boot and rolls back
to the previous version automatically. So you can't accidentally
brick a sensor over the air.

Every wireless-update build is also archived under
`firmware/esp32-s3/releases/` with the version string in the
filename, so you can always find the exact bytes a given sensor is
running.

## Where to look when something doesn't work

| Symptom | First place to look |
|---|---|
| LED stays dark | Battery dead or USB-OTG cable not seated. |
| LED pulses red | Hardware fault. Look at the serial console — `cat /dev/ttyACM0` (after `stty -F /dev/ttyACM0 115200 raw`). |
| Sensor never connects | Check the hotspot is up (`./tools/setup-hotspot.sh status`). Check the WiFi credentials match. Try clicking *Connect Sensors* on the dashboard to push fresh credentials over Bluetooth. |
| Dashboard says "no peers" | The TCP listener is on port 21042. Check no firewall is blocking it. |
| OTA update fails | Bootloader will roll back to the previous firmware on the next sensor reboot — usually within 30 seconds. Then you can try the update again. |
| Live data stops mid-session | The sensor probably disconnected from WiFi. Its LED tells you what state it's in. The dashboard's "last seen" age shows how long it's been silent. |

For deeper diagnosis, the dashboard prints all events and errors to
its terminal. The Connection Log panel in the web app shows the same
information.

## Repo layout

```
r2-rocker/
├─ Cargo.toml                ← workspace root (the dashboard, tools, and protocol crates)
├─ crates/                   ← protocol building blocks (compact frames, CBOR, crypto)
├─ dashboard/                ← the controller's web server (Rust)
├─ firmware/esp32-s3/        ← sensor firmware (Rust on Xtensa)
├─ webapp/              ← the web app (HTML + JS + WASM bundle)
├─ tools/                    ← scripts and CLIs (build, flash, key generation, setup)
├─ trust_keys/               ← public keys + cert (PRIVATE KEY NEVER LIVES HERE)
├─ specifications/           ← spec-first source of truth for what the system does
├─ plan/PLAN.md              ← living roadmap: what's done, what's next, why
├─ conversation/             ← per-session design records (raw material for the paper)
├─ docs/                     ← vendor PDFs (datasheets) and reference materials
├─ AI-CONTEXT.md             ← entry-point doc for AI assistants helping with the project
├─ PROCESS.md                ← five workflow rules we follow
└─ README.md                 ← this file
```

## Project status

End-to-end works wirelessly today: real ESP32-S3 hardware, the
dashboard's bootstrap loop discovers it over Bluetooth, pushes
WiFi credentials, the sensor reboots into WiFi, streams simulated
accelerometer data (real ADXL355 chip will be soldered in shortly),
and accepts firmware updates over the air. LED state, battery
state, and on-screen indicators are all in lockstep.

What's left before the rig is "production-ready":

- Sign + verify firmware updates and WiFi-credential offers (the
  cryptography primitives are in place; the integration is the next
  piece of work).
- Onsite long-term data archive.
- Remote-viewing rollout — the spec is written, implementation is
  staged across several incremental milestones.
- Real ADXL355 chip soldered in (replacing the simulator).
- Per-sensor calibration.

`plan/PLAN.md` has the full roadmap with current status against each
milestone.

## Glossary

A few terms used elsewhere in the docs that don't appear in this
README:

- **Trust group / TG** — the set of devices (sensors, controller,
  viewers) that share cryptographic keys and trust each other. There
  are two trust groups in this project: one for sensors+controller
  ("production"), one for viewers ("viewing"). They talk to each
  other through a controlled bridge on the controller.
- **R2 / Reality2** — the underlying messaging protocol stack. It
  defines how devices identify themselves, encrypt traffic, route
  messages across intermittent networks, and bootstrap new members.
- **OTA** — over-the-air firmware update. The "wireless update"
  feature.
- **BLE** — Bluetooth Low Energy. Used briefly during sensor setup
  to deliver WiFi credentials before a sensor knows how to join the
  network.
- **R2-WIRE** — the binary message format sensors use to send
  events. Compact (12-byte header + payload), so a battery-powered
  sensor can stream it cheaply.
- **Sentant** — a small piece of code inside a device that handles
  one kind of event. The dashboard, the firmware, and the web app
  are each made of several sentants composed into an "ensemble".
- **Hive** — a single device running a set of sentants. Each sensor
  is a hive; the controller is a hive; each browser viewer is a
  hive.

## Reading order

If you're new and want to understand the whole thing:

1. This README.
2. [`PROCESS.md`](PROCESS.md) — five rules for how we work on this project.
3. [`plan/PLAN.md`](plan/PLAN.md) — what we're building, in what order, and why.
4. [`specifications/HARDWARE-WIRING.md`](specifications/HARDWARE-WIRING.md) — physical sensor build (pinouts, photos).
5. [`specifications/SECRETS-POLICY.md`](specifications/SECRETS-POLICY.md) — before you touch any keys.
6. The latest entry in [`conversation/`](conversation/) — the most recent thinking, in raw form.

For AI assistants helping with the project: read
[`AI-CONTEXT.md`](AI-CONTEXT.md) first; it's a curated entry point.

The full normative specs are under [`specifications/`](specifications/):

- `SPEC-R2-ROCKER-SYSTEM.md` — the system as a whole.
- `SPEC-R2-ROCKER-WIRE.md` — the message format on the wire.
- `SPEC-R2-ROCKER-SENSOR.md` — what the sensor firmware does.
- `SPEC-R2-ROCKER-DASHBOARD.md` — what the controller does.
- `SPEC-R2-ROCKER-BRIDGE.md` — how the production and viewing trust
  groups talk to each other.
- `HARDWARE-WIRING.md`, `SECRETS-POLICY.md` — operational specs.

## License

To be decided before public / university release.

[r2]: https://github.com/reality2-ai
