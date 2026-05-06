---
title: r2-rocker design session 01 — initial scoping & wiring
date: 2026-05-06
participants:
  - user: roy.c.davies@ieee.org
  - ai: Claude (Anthropic), Opus 4.7 (1M context)
session: 01
status: archived (verbatim where possible; AI responses condensed-but-faithful)
purpose: |
  Primary research record. This conversation drove the initial design of the
  r2-rocker structural-health-monitoring sensor system (ESP32-S3 + ADXL355 +
  Reality2 protocol) and the v0.1 wiring spec. Material for paper / report.
---

# Design session 01 — 2026-05-06

> **Reading note.** User messages are verbatim. AI responses are rendered
> with full technical content (tables, equations, decisions, citations
> intact) but prose density is reduced from the original where it doesn't
> change meaning. Future sessions append to this folder, one file per
> session. The convention is `YYYY-MM-DD-design-session-NN.md`.

## Context at start of session

* User has placed PDF datasheets and a link to ADXL355-PMDZ in
  `r2-rocker/docs/`.
* `r2-rocker/` is otherwise empty.
* Reality2 codebase exists at `r2-core/` with prior work on a related
  "rocker rig" demo using Unihiker M10s.
* User wants ESP32-S3-DevKitC-1 + ADXL355-PMDZ as the new sensor node.

---

## Exchange 1 — Opening request

**User:**

> In this folder are some files for a MCU and accelerometer sensor. I need
> to use the one with the other, and install the Reality2 software on this
> along with a controlling device to read all the data from the sensor(s)
> connected by wifi. The Reality2 core code can be found in ../r2-core

**AI (summary):** Surveyed the directory and `r2-core/`. Found existing
`platforms/esp32-s3/` firmware tree with WiFi, trust-group and R2-WIRE
plumbing — but every existing binary is tied to a specific board (DFR1195,
LilyGo T5-S3, WIO SX1262). No ADXL355 driver yet. `tools/r2-dashboard/`
already accepts UDP-presence + TCP R2-WIRE streams (the Unihiker demo
stack).

Recommended adding a new binary `platforms/esp32-s3/src/main_adxl355.rs`,
SPI to ADXL355, R2-WIRE frames over WiFi, reuse trust/bootstrap stack.

Asked five clarifying questions:
1. Board — ESP32-S3-DevKitC-1?
2. ADXL355 breakout — EVAL-ADXL355-PMDZ?
3. Controlling device — laptop, hotspot creator?
4. Scope of "install Reality2" — firmware only, or also Elixir engine?
5. One sensor or many?

---

## Exchange 2 — Hardware confirmed

**User:**

> 1. yes, that is the board, 2. I need assistance to connect the ADXL355 to
> the ESP32. 3. yes, something like r2-dashboard - at the very least
> creating a webapp that can be connected to. 4. rust code for the esp32,
> could be more for the controller (which will be on a raspberry pi or PC
> running linux). Should include OTA update capability. 5. Many

**AI (summary):** Delegated codebase exploration to a sub-agent to map ESP32-S3
firmware patterns + r2-dashboard wire protocol. Key findings:

* Simplest sensor template: `platforms/esp32-s3/src/main_wio.rs`. SPI bus
  init at lines 74–90.
* WiFi creds: compile-time via `build.rs` reading `wifi_config.toml`,
  fallback to NVS-stored credentials provisioned via BLE.
* r2-dashboard listens TCP on port 21042, browser webapp on :8080 (Axum +
  WebSockets). Multi-sensor via `peers: HashMap<SocketAddr, SensorPeer>`.
* OTA exists via `r2_esp::ota_tcp` listener + `tools/r2-build --push` —
  SHA-256 verified, **not yet signed**.
* Trust state: per-device Ed25519 generated at first boot via
  `esp_fill_random()`, persisted to NVS.

Proposed wiring (default ESP32-S3 SPI2 / FSPI pins):
* ADXL355 CS → GPIO10
* ADXL355 SCLK → GPIO12
* ADXL355 MOSI → GPIO11
* ADXL355 MISO → GPIO13
* ADXL355 DRDY → GPIO14

Proposed phasing:
1. Bring-up
2. WiFi-only sensor (hard-coded creds)
3. BLE bootstrap FSM
4. OTA
5. Multi-sensor

---

## Exchange 3 — Note the docs

**User:**

> do also note the pdf files and links in this folder

**AI:** WebFetched the EVAL-ADXL355-PMDZ link and read pages of the WROOM-1
datasheet. Confirmed:

* ADXL355-PMDZ is 12-pin Pmod, 3.3 V, <200 µA, SPI mode 0, max 10 MHz.
* Pmod pinout:

| Pin | Signal | Pin | Signal |
|---|---|---|---|
| 1 | CS    | 7  | INT1 |
| 2 | MOSI  | 8  | NC   |
| 3 | MISO  | 9  | INT2 |
| 4 | SCLK  | 10 | DRDY |
| 5 | DGND  | 11 | DGND |
| 6 | VDD   | 12 | VDD  |

---

## Exchange 4 — Prior rocker project surfaced

**User:**

> we had also create a demo 'rocker project' before as well, though with
> different hardware

**AI:** Located `r2-core/demos/rocker-rig/` README. The prior demo:

* Used Unihiker M10 (Linux + Python + GD32 co-processor) as sensor nodes.
* Streamed accelerometer x/y/z at 10 Hz over TCP port 21042 (R2-WIRE
  CompactHeader + CBOR `{0:x, 1:y, 2:z}`).
* Tuxedo laptop ran `r2-dashboard` (browser :8080) + `r2-bootstrap`
  (BLE+L2CAP+hotspot creation).
* FSM: Idle → Advertising → BleListening → WifiConnecting → Streaming →
  KeepAlive.
* Multi-sensor; parallel BLE bootstrap; KeepAlive every 30 s.

> **Implication.** The new project is a re-implementation of the same
> conceptual rig, replacing the Unihiker M10 nodes with ESP32-S3-DevKitC-1
> + ADXL355-PMDZ. Dashboard, bootstrap, R2-WIRE, trust stack stay.

---

## Exchange 5 — Datasheet confirmation

**User:** [Pasted PDF pages 9–13 of the WROOM-1 datasheet showing block
diagram, pin layout, pin definitions table, and boot configurations.]

**AI:** Confirmed:
* 41 GPIOs on WROOM-1.
* Strapping pins: GPIO0 (boot), GPIO3 (JTAG), GPIO45 (VDD_SPI), GPIO46
  (ROM print).
* Octal-PSRAM-reserved on R8/R16V variants: GPIO35, 36, 37.
* USB: GPIO19 (D−), GPIO20 (D+).
* UART0: GPIO43 (TX), GPIO44 (RX).

Then unified the design plan with citations, proposed project shape:
firmware `main_adxl355.rs` template with `adxl355.rs`, `sensor_fsm.rs`,
`r2-esp::wifi_prov`, `r2-esp::ota_tcp`. Posed phased plan.

---

## Exchange 6 — Add SD card and battery sense

**User:**

> Also to add - there will be a SD card hardware to add, and we want to be
> able to sense the level of the battery and send that via wifi too

**AI:** Updated wiring with SD on shared SPI2 bus + battery on ADC1
(must be ADC1, not ADC2 — ADC2 unusable while WiFi active).

| Function | GPIO | Notes |
|---|---|---|
| SD CS | GPIO9 | own CS, shared SPI2 |
| SD CD (optional) | GPIO15 | input w/ pull-up |
| Battery sense | GPIO4 | ADC1_CH3, via 100 k / 100 k divider |

Battery divider sized for ADC range 0–3.3 V vs cell 3.0–4.2 V → 0.5 ratio,
21 µA quiescent. Note: DevKitC-1 has no battery circuit; needs a separate
LiPo + charger module (TP4056 or PowerBoost).

Firmware additions:
* `sd_logger.rs` — write-behind ring of R2-WIRE frames; FATFS over SDSPI.
* `battery.rs` — ADC1 oneshot + calibration; emit `r2.sensor.battery` event
  at 30 s in Streaming, 5 min in Idle.

Battery event matches the dashboard's pre-known `BATTERY_STATUS` hash. No
dashboard changes needed.

Asked: SD card as offline buffer or always-log archive?

---

## Exchange 7 — Full R2-device system requirements

**User:**

> at the end, the sensor has to work as a proper R2 device. When the system
> is first started, each device is a beacon. The dashboard device finds
> them and connects. They are all part of the same (hardwired) trust group.
> once connected, the sensors are checked for battery levels etc and this
> can be displayed on the dashboard. There could be many sensors - we will
> start wih one, then 2, but later there could be dozens, so the dashboard
> needs to be able to accomodate that. Once all the sensors are confirmed
> ready, a calibration run is performed where the rocdker is static. Data
> is read once, the rocker is moved to its other position, data is read
> again, and that is used to establish the axes of movement. We are
> interested in the sideways movement of the rocker at 90 degrees to the
> main movement, but the sensors might not be aligned 100% to the main
> movement axis, therefore we need this calibration step.

**AI:** Drafted full lifecycle:

```
sensor                                 dashboard
──────                                 ─────────
boot
 ├ load TG cert (baked in firmware)
 ├ load device Ed25519 key (NVS)
 └ BLE advertise R2-BEACON
                                       [Discover button]
   ←─ #wifi_offer (signed by TG) ───
join hotspot → TCP gateway:21042
SENSOR_ANNOUNCE (device_pk, hostname,
  fw_ver, sig over TG) ─────────────►  verify sig vs TG cert
battery event every 30 s ───────────►  battery widget per sensor

                                       [Calibrate – position A]
   ←─ #cal.sample {ms: 1000} ───
1 s-averaged (ax,ay,az) ────────────►  store g_A[device_pk]
                                       [position B]
                                       [Start streaming]
   ←─ #stream.start {rate_hz: 100}
acceleration @ rate ────────────────►  rotate by R, display a_sideways
```

**Calibration math** (per sensor, in body frame):
* `g_A` = mean accelerometer at rest, position A.
* `g_B` = mean accelerometer at rest, position B.
* `main = normalize(g_B − g_A)` — rocker's principal-motion direction.
* `vertical = normalize((g_A + g_B) / 2)` — rest-gravity, "up".
* `sideways = main × vertical` — orthogonal third axis.
* Rotation matrix `R = [main; sideways; vertical]` (rows).
* Live samples become `(a_main, a_side, a_vert) = R · (ax, ay, az)`.

Edge case: if `|g_B − g_A|` is small (sensor barely moved between
calibration positions), `R` is ill-conditioned — dashboard refuses;
threshold 0.3 g.

**Recommendation:** calibration matrix lives on the dashboard, keyed by
device public key, persisted to `~/.config/r2-dashboard/calibration.json`.
Sensors stay stateless.

**Hardwired trust group** options:
* (A) Compile-time const arrays in firmware via `include_bytes!` from
  `trust_keys/`. Cleanest. Recommended.
* (B) NVS pre-flash via `r2-build provision` tool. More flexible.
* (C) Runtime BLE join (existing R2 default). Skipped per "hardwired".

**Multi-sensor scaling:** dashboard already supports multi-peer; the
browser UI was tuned for 2. For dozens: CSS grid auto-fit, Canvas charts
instead of Plotly per sensor, sensor labels & groups, health summary
header.

Open questions:
1. TG provisioning — fresh keypair or existing?
2. Calibration model — rest-position vs hand-held?
3. Streaming rate — default 100 Hz?
4. Battery chemistry — LiPo?

---

## Exchange 8 — Answers + rocker description

**User:**

> 1) new TG. 2) Yes, it has well defined rest positions. Imagine a half
> tonne metal 'thing' that is flat on one side and curved on the other.
> on the curved edge they put rubber like as found on tyres. The rocker
> sits on an asphalt bed, and has actuators that rock it back and forth in
> order to test then wear of the rubber on the asphalt. 3) agree 4) LiPo

**AI:** Noted hardware context — a half-tonne actuator-driven rocker on
asphalt. Implications:
* g-range default ±2 g may be tight; make it NVS-stored, configurable via
  `#stream.start` to ±4 g or ±8 g.
* ADXL355's 25 µg/√Hz noise floor is well-suited to small contact-patch
  AC signals.
* 100 Hz @ ±2 g gives plenty of headroom for <20 Hz contact-patch dynamics.

Proposed project layout under `r2-rocker/`:
```
r2-rocker/
├─ Cargo.toml          (workspace root)
├─ docs/
├─ trust_keys/
├─ firmware/esp32-s3/
├─ dashboard/
└─ tools/
```

Asked three setup questions:
1. Project layout — `firmware/esp32-s3/` or flatter?
2. r2-core path — `../r2-core` correct?
3. Generate TG now or defer?

---

## Exchange 9 — The actual problem

**User:**

> The problem they want to solve is that it seems to also have some lateral
> movement that is puttign stress on the joints causing them to shear -
> that is what we are trying to sense.

**AI:** Reframed the work as **structural health monitoring**, not
tyre-wear instrumentation. Lateral motion is the *diagnostic* for joint
shear, not the measurement objective.

> **The signal is differential, not absolute.** Joint shear shows up as a
> *difference* in lateral motion between two sensors mounted on opposite
> sides of the joint. If both lateral-accelerate together, joint is fine
> (global rig motion). If they diverge, joint is shearing.

Therefore sensors get deployed in **pairs (or groups) across each joint of
interest**. With dozens of sensors → handful of joints with 2–4 sensors
each.

Firmware: nothing changes. Dashboard analytics layer:
1. Sensor-group / joint configuration (persistent, declares which
   device-pks pair across which joint).
2. Pairwise differential view: `Δa_lateral = R_a · sample_a − R_b · sample_b`.
3. Stress indicator per joint: sliding-window RMS or peak-magnitude.
4. Long-term trend per joint.
5. Inter-sensor time alignment via `r2.dash.sync_pulse` (Cristian's
   algorithm; ~5 ms accuracy after a few rounds).

---

## Exchange 10 — Bed sensors later, two on rocker first

**User:**

> Latger, we might also want to sense other vibrations with sensors placed
> elsewhere on the bed and rocker. Initially, the two sensors will be
> placed on the rocker near the actuator joints

**AI:** Schema generalisation: each sensor needs a configurable mounting
role (`rocker | bed | other`) on the dashboard, alongside calibration
matrix and group. Bed sensors get single-position calibration (gravity
reference only); they're a reference channel for environmental subtraction.

Asked clarifying topology question:
* (A) Pair across single joint — both sensors on one joint, opposite sides
  → directly measures shear.
* (B) One sensor per joint at different ends → compares symmetric signals.

---

## Exchange 11 — SD as primary buffer

**User:**

> yes, the sdcard is the buffer. given there could be many sensors, they
> might not be able to send data fully realtime, so it will be stored in
> the SDcard, and relayed when possible in near-real-time to the dashboard
> device

**AI:** Elevated SD to **primary durable log, with TCP as a near-real-time
tap**. Architecture:

```
sample task  ── ADXL355 ──► SD ring
                              │
network task ── reads ring ─► TCP gateway:21042
                              ▲
ack task     ◄── ACK {through_seq: N} ──┘
                update head pointer; free SD ring up to N
```

**Sample record (SD + wire), 20 bytes fixed:**
```
seq    u32   monotonic, persists across reboot via NVS
ts_ms  u32   ms since boot
x      i32   raw ADXL355 20-bit signed, sign-extended
y      i32
z      i32
```

Two wire encodings:
* `r2.sensor.acceleration` — live, per-sample, low-latency.
* `r2.sensor.acceleration.batch` — catch-up replay, ~50 samples per frame.

Sensor switches to batched form when ring backlog exceeds threshold.

Ack protocol: `r2.dash.ack {through_seq: N}` every 200 ms or 100 samples.
Sensor advances ring head; next reconnect resumes from
`last_acked_seq + 1`. Time sync: `ts_ms` monotonic; per-device boot offset
captured at announce.

SD ring policy on full: overwrite-oldest (recommended, default) at ring
size 100 MB ≈ 19 h at 100 Hz.

Phasing revised — SD moves to Phase 3 (structural, not nice-to-have).

---

## Exchange 12 — Final answers, scratch start

**User:**

> 1. we will be developing for both the esp32 and the dashboard, so need a
> structure to store both. 2. yes, though check. Best to copy over the
> necessary files so this folder is complete and not dependent on a
> relationship with another folder. Later I have to hand this off to the
> university, and it needs to be complete in itself. 3. might as well
> create the TG now. 4. initially 1 sensor per joint, but we will probably
> add 1 more each. Subsequent sensors have not yet been decided - they
> want to see this working first. 5. we are starting from scratch

**AI:** Acknowledged. Switching to execution mode: self-contained repo
(no path deps on r2-core), fresh TG, two sensors (one per joint, with
expansion to pairs later). Began loading task tools.

---

## Exchange 13 — Process discipline added

**User:**

> as part of this process, I want to store this conversation - we are
> pioneering not just the code but also the process. Create a folder for
> that. Also, create a folder to store the plan. We MUST create a
> specifications file first (cf ,,.r2-specifications), then the code from
> that.

**AI:** Updated approach: specs-first development, conversation archived
for posterity, plan tracked separately. Began inspecting `r2-specifications`
conventions.

---

## Exchange 14 — Wiring first, paper coming, RGB LED

**User (multiple system-reminded messages, consolidated):**

> step 1, though is a wiring diagram so I can solder up the boards...
>
> just by the by, I've just connected the ESP32
>
> Out of this, we will write a paper and a report, so all interactions must
> be kept for posperity.
>
> One thing I note is that it has a multicoloured LED on it. We can use
> that to indicate status.

**AI:** Read DevKitC-1 J1/J3 header pin tables from
`docs/esp-dev-kits-en-master-esp32s3.pdf` p.7. Confirmed lucky pin layout:
GPIO9-14 are six adjacent pins (J1 16-20 + J1 15), making the SPI cluster
a clean ribbon. GPIO4 at J1 pin 4 for battery sense.

Created project folder structure:
```
r2-rocker/
├─ docs/                (existing)
├─ specifications/      (specs-first source of truth)
├─ plan/                (consolidated design)
├─ conversation/        (this folder — research record)
├─ trust_keys/          (TG)
├─ firmware/            (Phase 1+ firmware)
├─ dashboard/           (Phase 4+ dashboard)
└─ tools/               (build, bootstrap)
```

Wrote `specifications/HARDWARE-WIRING.md` v0.1 — soldering-ready, three
phases, full pin table, layout ASCII, pre-power checklist, RGB LED status
indicator mapping.

---

## Exchange 15 — GitHub secret-scanning concern

**User:**

> This will ultimately be stored in github, so we will need to be careful
> of the encryption keys so that github doesn't flag them. it will be a
> private repo though.

**AI:** Treated as if the repo could be public tomorrow (private-repo
secret-scanning still active under some org policies; clones / mirrors
widen the leak surface). Wrote two files:

* `.gitignore` at repo root — patterns blocking `*_priv*`, `*.priv`,
  `*.pem`, `wifi_config.toml`, `*.nvs`, `dashboard/.state/` etc. Rule:
  *no private key material in the working tree, ever*.
* `specifications/SECRETS-POLICY.md` — what's a secret, where each lives,
  TG provisioning flow (one-time, off-tree on the signing host, only
  `tg_pub.bin` + `tg_cert.bin` ever enter the repo), key-rotation
  procedure, esp-idf NVS encryption for per-device keys.

---

## Exchange 16 — Pin numbering clarification

**User:**

> The numbering of the pins on the board is not sequential. Do I use the
> numbers written on the board or the sequentual numbers from, say, top
> left?

**AI:** Trust the silkscreen GPIO numbers. The "J1 pin N" position in the
datasheet is a counting aid only; what's printed on the board is the GPIO
number, and that's what your firmware sets. The numbering looks
non-sequential because Espressif placed pins by PCB-layout convenience,
not GPIO order — that's normal.

Added a §2.0 "How to read the connection tables" preamble to
`HARDWARE-WIRING.md` making this explicit, with a side-by-side comparison
of the two identifier schemes.

Also wrote `PROCESS.md` (workflow discipline — five rules: spec-first,
conversation-as-research-data, plan-as-consolidation, secrets-out, cite
sources) and `README.md` (orientation for new readers — repo layout,
status, reading order).

---

## Exchange 17 — Stop

**User:**

> stop for now - I have to get ready and go to work. Save this script so
> far and create a memory file in case we have to start from a new AI
> session.

**AI:** Updated this conversation archive to current state. Wrote
`AI-CONTEXT.md` at the repo root — a fresh-session entry point capturing
state, conventions, and binding decisions in compact form so a new AI
(no prior memory) can pick up cleanly. Saved equivalent structured
entries to the auto-memory system as a backup for same-machine sessions.

End of session 01.

---

## Session 01 — Final state

**What's in the repo:**

```
r2-rocker/
├─ README.md
├─ PROCESS.md
├─ AI-CONTEXT.md
├─ .gitignore
├─ specifications/
│   ├─ HARDWARE-WIRING.md   (v0.1)
│   └─ SECRETS-POLICY.md    (v0.1)
├─ conversation/
│   └─ 2026-05-06-design-session-01.md   (this file)
└─ {plan, trust_keys, firmware, dashboard, tools, docs}/
```

**Where the user is:** Has the ESP32-S3-DevKitC-1 connected via USB.
Phase 1 wiring (7 wires for ADXL355) ready to solder against
`HARDWARE-WIRING.md`.

**Carried forward to session 02:**

* Write `plan/PLAN.md` — consolidated phasing & decisions.
* Write `specifications/SPEC-R2-ROCKER-WIRE.md` — event hashes + CBOR
  schemas (the wire contract — first proper code-driving spec).
* Write `specifications/SPEC-R2-ROCKER-SENSOR.md` — sensor FSM, calibration
  request/response, OTA, store-and-forward semantics.
* Write `specifications/SPEC-R2-ROCKER-DASHBOARD.md` — multi-sensor UI,
  joint groups, pairwise differential analysis, time-sync pulse.
* Write `specifications/SPEC-R2-ROCKER-SYSTEM.md` — top-level system
  architecture tying the above together.
* Then begin firmware Phase 1 implementation.

---

## Decisions captured this session

| # | Decision | Source exchange |
|---|---|---|
| 1 | ESP32-S3-DevKitC-1 + EVAL-ADXL355-PMDZ as sensor node hardware | 1, 2 |
| 2 | Reuse the prior rocker-rig conceptual stack (R2 BLE bootstrap → WiFi → TCP R2-WIRE → browser dashboard) | 4 |
| 3 | SPI2/FSPI default pins: CS=10, MOSI=11, SCLK=12, MISO=13, DRDY=14 | 5 |
| 4 | SD card on shared SPI2 bus, CS=GPIO9 | 6 |
| 5 | Battery sense on GPIO4 (ADC1_CH3) via 100 k / 100 k divider | 6 |
| 6 | Hardwired trust group via compile-time `include_bytes!` from `trust_keys/` | 7 |
| 7 | Calibration matrix lives on dashboard, keyed by device public key | 7 |
| 8 | Calibration math: `main = norm(g_B−g_A)`, `vertical = norm((g_A+g_B)/2)`, `sideways = main × vertical` | 7 |
| 9 | Streaming rate default 100 Hz, configurable via `#stream.start` | 8 |
| 10 | g-range default ±2 g, NVS-tunable | 8 |
| 11 | LiPo single-cell, 100 k / 100 k divider, ~21 µA quiescent | 8 |
| 12 | Problem domain is **structural health monitoring** — sensing joint shear via differential lateral motion between paired sensors | 9 |
| 13 | Sensor mounting role (`rocker / bed / other`) is a dashboard-side schema field | 10 |
| 14 | SD card is **primary durable log**; TCP is near-real-time tap | 11 |
| 15 | Sample record: 20 bytes fixed `(seq, ts_ms, x, y, z)`; live + batch wire encodings | 11 |
| 16 | Cumulative ACK protocol `r2.dash.ack {through_seq:N}` every 200 ms or 100 samples | 11 |
| 17 | SD ring overwrite-oldest at full | 11 |
| 18 | Self-contained repo (no path deps on r2-core; vendor what's needed) | 12 |
| 19 | Topology B initial: 1 sensor per joint, 2 joints; expandable to pairs later | 12 |
| 20 | Specs-first: spec file before code | 13 |
| 21 | Conversation archived as primary research record (paper-grade) | 14 |
| 22 | On-board RGB LED used as status indicator (GPIO38 v1.1, GPIO48 v1.0) | 14 |

## Open questions still pending after this session

* **Topology A vs B** — exchange 10 asked whether the two initial sensors
  span one joint (A) or sit at separate joints (B); exchange 12 answered B
  ("initially 1 sensor per joint"). Confirmed.
* **University handoff timing** — when does the repo need to be
  publishable / handed off? Affects how much polish we apply per phase.
* **Sample rate ceiling** — exchange 8 fixed default at 100 Hz; if early
  data shows transient slip events with content above 50 Hz, may bump to
  200 Hz. Decision deferred to first real-rig test data.
* **Baseline thresholds for stress indicator** — no historical data;
  thresholds will be established empirically on first deployment. No
  decision needed pre-deployment.

## Next session entry points

* Write `plan/PLAN.md` consolidating all decisions with phasing.
* Write `PROCESS.md` documenting the spec-first / conversation-archived
  workflow as durable convention.
* Begin specifications. Likely first specs:
  * `SPEC-R2-ROCKER-SYSTEM.md` (top-level system architecture).
  * `SPEC-R2-ROCKER-SENSOR.md` (per-sensor firmware behaviour, FSM,
    events, calibration request/response).
  * `SPEC-R2-ROCKER-DASHBOARD.md` (multi-sensor UI, joint groups,
    differential analysis, time-sync pulse).
  * `SPEC-R2-ROCKER-WIRE.md` (event hashes + CBOR payload schemas — the
    wire contract between sensor and dashboard).
* User to begin soldering Phase 1 wiring per `specifications/HARDWARE-WIRING.md`.
