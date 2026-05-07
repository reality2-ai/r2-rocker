---
title: r2-rocker — Plan
status: Living document v0.2
date: 2026-05-06
---

# r2-rocker — Plan

Single source of truth for **current project state**. This file overwrites
itself as work progresses; chronological history lives in `conversation/`.

> **Reading note.** If you want to know *why* a decision was made, check
> the conversation archive entry referenced in the right-hand column. If
> you want to know *what's next*, read this file top to bottom.

## 1. Project goal

Wireless structural-health-monitoring sensor system for a half-tonne
actuator-driven rocker rig. The rig tests tyre rubber on an asphalt bed,
but the *failure mode we instrument for* is shear at the actuator joints,
suspected of being driven by an unintended lateral component of the
rocker's motion. Sensors detect that lateral motion as a diagnostic.

The diagnostic of interest is **differential lateral motion across
joints**, computed dashboard-side from per-sensor body-frame samples
rotated to a common rig frame via two-position calibration.

## 2. Architecture (one-page)

```
sensor node (×N)                        controlling device (laptop or Pi)
─────────────────                       ─────────────────────────────────
ESP32-S3-DevKitC-1                      r2-rocker dashboard
+ ADXL355-PMDZ        (SPI2)              ├ creates WiFi hotspot
+ microSD breakout    (shared SPI2)       ├ BLE-bootstraps sensors
+ LiPo cell + charger                     ├ TCP receiver :21042
+ battery sense       (ADC1_CH3)          └ HTTP webapp :8080
+ on-board RGB LED    (status)

   BLE beacon (R2-BEACON)        →     dashboard scan
   L2CAP listen                  ←     L2CAP connect, #wifi_offer (signed by TG)
   WiFi join → TCP :21042        →     SENSOR_ANNOUNCE
   sample → SD ring (durable)
        └── network task ────────►     ┌─ multi-peer HashMap
   battery event every 30s ─────►      ├─ per-device cal matrix
                                  ←──  ├─ ACK every 200 ms / 100 samples
                                  ←──  └─ cal/stream/sync commands
                                       └→ browser at :8080
                                          (grid · canvas · joint groups
                                           · pairwise differential
                                           · stress indicator · trend)
```

## 3. Phasing

Status: ✅ done · 🔄 in progress · ⏳ pending · ⏸ blocked

| # | Phase | Status | Output |
|---|---|---|---|
| 0a | Scaffolding | ✅ | Folders, README, PROCESS, AI-CONTEXT, .gitignore |
| 0b | Wiring & secrets specs | ✅ | `HARDWARE-WIRING.md`, `SECRETS-POLICY.md` v0.1 |
| 0c | Conversation archive | ✅ | `conversation/2026-05-06-design-session-01.md` |
| 0d | Plan (this file) | ✅ | `plan/PLAN.md` v0.2 |
| 0e | Wire spec | ✅ | `specifications/SPEC-R2-ROCKER-WIRE.md` v0.1 |
| 0f | Sensor spec | ✅ | `specifications/SPEC-R2-ROCKER-SENSOR.md` v0.1 |
| 0g | Dashboard spec | ✅ | `specifications/SPEC-R2-ROCKER-DASHBOARD.md` v0.1 |
| 0h | System spec | ✅ | `specifications/SPEC-R2-ROCKER-SYSTEM.md` v0.1 |
| 0i | TG generation tool | ✅ | `tools/r2-rocker-tg/` v0.1 — keygen, verify, inspect; round-trip tested |
| 0j | Pre-soldering firmware | ✅ | `firmware/esp32-s3/` v0.1.0 — UART heartbeat over native USB-Serial-JTAG; flashed and running (MAC `1c:db:d4:41:28:3c`) |
| 0k | Two-slot OTA partition table | ✅ | `partitions.csv` baked in via build.rs OUT-dir copy trick (matching r2-core/platforms/esp32-s3); chip flashed with `ota_0` / `ota_1` / `otadata` / FAT storage layout. `tools/setup-firmware.sh` pre-stages on a fresh checkout. |
| 0L | Simulated-data sender | ✅ | Firmware connects to WiFi via `wifi_config.toml`, opens TCP to `gateway_ip:21042`, emits R2-WIRE frames (announce, acceleration @ 100 Hz, battery every 30 s) with synthetic ADXL355-shaped data. End-to-end verified on hardware. |
| 0m | Dashboard scaffold (port + adapt) | ✅ | Cargo workspace at root; vendored `r2-fnv`, `r2-cbor`, `r2-wire`, `r2-core`, `r2-bootstrap`, `r2-dashboard` from r2-core. Adapted CBOR decoder for our integer-keyed schemas. Server-side raw-LSB→g scaling. /api/version. **R2-WIRE compact frame parser fixed** (was off-by-one for the 7-byte M10 layout vs our 12-byte spec). 10 Hz live decimation per spec §5.2. |
| 0n | Versioning convention | ✅ | Both firmware and dashboard build.rs stamps `R2_GIT_SHA` (with `-dirty` suffix on uncommitted) + `R2_BUILD_TIMESTAMP`. `fw_ver` in announce frames is `<semver>+<sha>`; dashboard exposes same via /api/version. Drives OTA decision logic. |
| 0o | Setup helpers | ✅ | `tools/setup-hotspot.sh` brings up the NM AP profile on the USB WiFi adapter (default `wlx0c0e766e358c`), generating fresh creds with `--rotate` or reusing what's in `wifi_config.toml`. `tools/setup-firmware.sh` pre-stages `partitions.csv` for the chicken-and-egg fresh-clone case. |
| 1 | Hardware solder + smoke test | ⏳ | Phase-1 wires per `HARDWARE-WIRING.md` §2 — operator-driven; awaits user availability |
| 2 | ADXL355 SPI driver | ⏳ | Replaces `sim.rs`; `firmware/esp32-s3/src/adxl355.rs` + DRDY-driven 100 Hz sampler |
| 3 | SD ring + sequencing | ⏳ | `firmware/esp32-s3/src/sd_logger.rs`, fixed 20-byte records, NVS `last_acked_seq`, replay on reconnect |
| 4 | Live WiFi single-sensor + ACKs + time-sync | ✅ (effectively 0L) | Already delivered as part of 0L: WiFi, TCP, R2-WIRE compact frames, CBOR. Server-side ACKs (`r2.dash.ack`) + sync_pulse round-trip pending in firmware (currently sender drives the cadence solo). |
| 5a | Hardwired TG + signed announce | ✅ | TG keypair generated via `r2-rocker-tg`; `trust_keys/tg_pub.bin` + `tg_cert.bin` committed; firmware embeds TG pub via `include_bytes!`. **NVS-persistent Ed25519 device key** mints on first boot, reloads on subsequent boots. Real Ed25519 signature on the announce body, verified working end-to-end on MAC `1c:db:d4:41:28:3c`. |
| 5b | Dashboard verifies announce sig | ⏳ | Server-side Ed25519 verify against `device_pk` from the announce; reject if invalid. Trivially small once firmware-side is committed. |
| 5c | HMAC envelope per R2-WIRE frame | ⏳ | DEK + HK derivation from TG SK (HKDF-SHA256, `r2-trust` SPEC §3); HMAC tag on every frame; R2-WIRE has-hmac flag. Authenticates EVERY frame, not just announce. |
| 5d | WebApp = WASM hive (replaces current Axum-served dashboard) | ⏳ | **Replace, not augment.** Vendor `r2-wasm` + its transitive crates (`r2-trust`, `r2-route`, `r2-transport`, `r2-engine`) from r2-core into `crates/`. Set up `wasm-pack` build. Static bundle deployed to **GitHub Pages**. Onsite dashboard's Rust process is reduced to: (a) sensor TCP listener; (b) relay-compatible WSS forwarding raw R2-WIRE bytes to connected browser hives; (c) TG-signing authority (KeyHolder) that issues device certs to enrolling browsers; (d) local data archive (Phase 5f). It no longer decodes frames or serves HTML/JSON. The browser is the canonical viewer in both onsite and remote modes (browser detects which by which transport succeeds). Onsite mode = full operator control (Start/Mark/Stop); **remote mode = read-only by default — must not upset a running experiment** (enforced via TG cert role + UI). Each browser is its own enrolled TG member (keypair + cert in IndexedDB, per `r2-trust` §2). UX layer stays plain JS + Chart.js; WASM handles only protocol + crypto. Same deployment shape as r2-notekeeper / anthill. Initially uses public r2-relay; migrates to own relay+archive (5e) by config. |
| 5d-enrol | QR / link enrolment UX (browsers only) | ⏳ | **Sensors and the onsite controller are TG members by their build/config — they don't enrol.** This row covers browsers (laptops, phones, tablets). Onsite dashboard exposes an "Enrol device" button → generates a one-time join token (single-use, ≤5 min expiry, signed by KeyHolder) → renders a QR code on the dashboard screen AND a shareable link `https://<github-pages>/r2-rocker/?join=<token>`. The WebApp on first load reads `?join=` from the URL, generates its keypair in IndexedDB, sends `public_key + token` to the onsite KeyHolder via the relay, gets back a TG-signed device cert, persists, becomes a TG member. Same flow for any browser device. Fallback: manual paste of the join link on a "not enrolled" landing page. |
| 5e | Own combined relay + archive server | ⏳ | Self-hosted VPS that **both relays and stores data**. Replaces the public r2-relay for this deployment. Combines what was previously split (5d relay + 5e cloud archive) into one server: forwards TG-encrypted frames AND runs a TG-member archiver consumer that persists the long-term store. Frame plaintext only ever exists inside TG members; the relay sees only sealed envelopes; the archiver IS a TG member so it sees plaintext, but it's *our* server. |
| 5f | Onsite local data archive | ⏳ | The onsite host stores the data locally — independent of cloud / relay — so an offline session works without internet and survives the relay being unreachable. Filesystem-backed (e.g. SQLite or per-session newline-delimited CBOR) under `dashboard/.state/sessions/<session-id>/`. Eventually syncs to the own combined server (5e) when reachable. |
| 5L | RGB LED state machine (firmware) | ⏳ | WS2812 RMT driver + animator task. Per `HARDWARE-WIRING.md` §5 colour map. **OTA-active gets a distinct colour** (e.g. magenta cycle) to differentiate from boot's brief white flash. |
| 6 | BLE bootstrap FSM | ⏳ | Port `tools/r2-sensor`'s FSM to ESP32-S3 with NimBLE; sensor advertises R2-BEACON, receives signed `#wifi_offer`, persists in NVS. **Retires `wifi_config.toml`.** Calm-tech endpoint. |
| 7 | Per-sensor calibration | ⏳ | Two-position cal protocol + dashboard-side rotation matrix |
| 8a | Devices view (fleet status) | ⏳ | Alternative dashboard view (tab/toggle vs the live charts). One card per known peer showing: hostname, online indicator, battery (mV + percent + bar), `fw_ver`, last-seen timestamp, FSM state, **virtual LED** (small DOM element animated to mirror the physical RGB LED's colour + pattern via the colour map in `HARDWARE-WIRING.md` §5; depends on Phase 5L sending FSM state in `r2.sensor.status`). Each card has a **"Update Firmware" button** that triggers `r2.dash.fw.update` (stub until Phase 9). **Designed for 1 → 20+ sensors**: cards in a CSS-grid auto-fit layout, sortable / filterable header, peer count + status summary at top ("18 online · 2 stale · 1 needs update"), virtualised list past ~30 peers if browser jank shows. |
| 8b | Live charts grid + canvas | ⏳ | CSS-grid auto-fit per-peer cards; Canvas-based mini-charts (replace per-peer Plotly) — **must stay smooth at 20–30 sensors at 10 Hz**, the deployment scale we're targeting. Cards collapse to summary tiles past ~8 visible. Offscreen cards pause their chart loop. |
| 8c | Joint groups + pairwise differential + stress indicator + trend | ⏳ | The diagnostic view per `SPEC-R2-ROCKER-DASHBOARD` §10–§11. Joint-group editor, `Δa_lateral` plot per joint, sliding-window RMS, daily-summary trend chart. |
| 8d | Measurement sessions | ⏳ | A "Sessions" view (alongside Live / Devices / Joints) that lets the operator define and run named **measurement sessions** — e.g. "tyre-wear-load-test-2026-05-08-am". Session captures: start/stop timestamps, participating sensors, calibration snapshot at session-start, per-joint analytics traces, operator notes/metadata (weather, rig config, payload). Storage in browser IndexedDB (browser-hive owns the data); shared across TG-member browsers via the relay; archived long-term by the Phase 5e cloud consumer. **Session ID is a browser-/dashboard-side concept** — not on the wire — but the browser may tag stored frames with it, and replay/export by session. Enables the report-writing workflow ("session 47 — load test, mean stress 0.18 g RMS, Δrms vs baseline +12%"). |
| 9 | OTA + TG-signed images | ⏳ | `r2-build` push, signed binaries, SD-backed rollback |

Phases 0e–0h block all coding (PROCESS rule 1 — spec before code).
Phase 1 (soldering) can run in parallel with the spec writing.

## 4. Binding decisions (active)

Each row references the conversation exchange that produced it. To revisit
a decision, open a new session and explicitly mark the prior decision as
superseded.

| # | Decision | Source |
|---|---|---|
| D-01 | Hardware: ESP32-S3-DevKitC-1 + EVAL-ADXL355-PMDZ + microSD + LiPo | session 01, exchange 2 |
| D-02 | Wiring: SPI2 defaults (CS=10, MOSI=11, SCLK=12, MISO=13, DRDY=14) + SD CS=GPIO9 + battery=GPIO4 | session 01, exchange 5–6 |
| D-03 | Status indication: on-board RGB LED on GPIO38 (v1.1) / GPIO48 (v1.0) | session 01, exchange 14 |
| D-04 | Streaming default 100 Hz, NVS-tunable up to 4 kHz | session 01, exchange 8 |
| D-05 | g-range default ±2 g, NVS-tunable | session 01, exchange 8 |
| D-06 | LiPo single-cell, 100 k / 100 k divider, ~21 µA quiescent | session 01, exchange 6, 8 |
| D-07 | Problem domain is structural health monitoring (joint shear) | session 01, exchange 9 |
| D-08 | Diagnostic is differential lateral motion across joints | session 01, exchange 9 |
| D-09 | Initial topology B: 1 sensor per actuator joint (2 sensors total) | session 01, exchange 12 |
| D-10 | Mounting role (`rocker / bed / other`) is a dashboard-side schema | session 01, exchange 10 |
| D-11 | SD card is primary durable log; TCP is near-real-time tap | session 01, exchange 11 |
| D-12 | Sample record 20 bytes fixed: `(seq:u32, ts_ms:u32, x:i32, y:i32, z:i32)` | session 01, exchange 11 |
| D-13 | Two wire encodings: `r2.sensor.acceleration` (live) and `…batch` (~50 samples) | session 01, exchange 11 |
| D-14 | Cumulative ACK protocol: every 200 ms or 100 samples | session 01, exchange 11 |
| D-15 | SD ring overwrites oldest at full | session 01, exchange 11 |
| D-16 | Calibration matrix lives on dashboard, keyed by device public key | session 01, exchange 7 |
| D-17 | Two-position cal: g_A, g_B → main = norm(g_B−g_A), vertical = norm((g_A+g_B)/2), sideways = main × vertical | session 01, exchange 7 |
| D-18 | Cal threshold: refuse if `|g_B − g_A| < 0.3 g` | session 01, exchange 7 |
| D-19 | Trust group hardwired via compile-time `include_bytes!`; per-device Ed25519 in NVS | session 01, exchange 7 |
| D-20 | Time sync via `r2.dash.sync_pulse` (Cristian's algorithm), ~5 ms accuracy | session 01, exchange 9 |
| D-21 | Self-contained repo: vendor R2 crates into `crates/`, no path deps on `../r2-core` | session 01, exchange 12 |
| D-22 | Spec-first development; conversation archived per session; secrets out of repo | session 01, exchanges 13–15 |

## 5. Open questions / deferred decisions

| Q | Description | Defer until |
|---|---|---|
| Q-01 | Sample-rate ceiling — keep at 100 Hz or bump to 200/500 Hz | First real-rig data, after Phase 5 |
| Q-02 | Stress-indicator threshold per joint | First deployment, no historical baseline |
| Q-03 | OTA signing scheme (TG group key vs per-device key) | Phase 10 |
| Q-04 | When to add a second sensor per joint (move to topology A) | After topology B proves the concept |
| Q-05 | Bed-sensor deployment (mounting role = `bed`) | Future expansion; schema already supports it |
| Q-06 | License (paper / open source / private) | Before any public release |
| Q-07 | University handoff timing | TBD with stakeholder |

## 6. Files in repo right now

```
r2-rocker/
├─ AI-CONTEXT.md                 ✅ fresh-AI entry point
├─ README.md                     ✅
├─ PROCESS.md                    ✅
├─ .gitignore                    ✅
├─ docs/                         ✅ (vendor PDFs)
├─ specifications/
│   ├─ HARDWARE-WIRING.md        ✅ v0.1
│   └─ SECRETS-POLICY.md         ✅ v0.1
├─ plan/
│   └─ PLAN.md                   ✅ this file
├─ conversation/
│   └─ 2026-05-06-design-session-01.md  ✅
├─ trust_keys/                   (empty — public material only)
├─ firmware/                     (empty — Phase 1+)
├─ dashboard/                    (empty — Phase 4+)
├─ tools/                        (empty)
└─ crates/                       (not yet created — Phase 4+)
```

## 7. Immediately next

1. Finish Phase 0e: `SPEC-R2-ROCKER-WIRE.md` — event names, FNV hashes,
   CBOR payload schemas, frame envelope.
2. Phase 0f: `SPEC-R2-ROCKER-SENSOR.md`.
3. Phase 0g: `SPEC-R2-ROCKER-DASHBOARD.md`.
4. Phase 0h: `SPEC-R2-ROCKER-SYSTEM.md`.
5. User solders Phase 1 wiring (parallel track).
6. Phase 2: ADXL355 driver + UART-only firmware bring-up.

## 8. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-06 | 0.1 | Initial plan, mid-session 01. |
| 2026-05-06 | 0.2 | Session 01 closed; phase 0a–0c marked done; binding decisions stabilised. |
| 2026-05-07 | 0.3 | Session 02: phases 0d–0h complete (PLAN + four code-driving specs). All scaffolding & specification work for v0.1 done. Next phase: TG generation tool, then hardware solder, then firmware Phase 1. |
| 2026-05-07 | 0.4 | Session 02: phase 0i complete (`r2-rocker-tg` keygen utility built, tested, round-trip verified). All Phase-0 work done. Awaiting hardware solder before Phase 1. |
| 2026-05-07 | 0.5 | Session 02: phase 0j added — `firmware/esp32-s3/` skeleton with UART heartbeat firmware and OTA-ready two-slot partition table; ready to flash on the bare DevKitC-1 before soldering. |
| 2026-05-07 | 0.6 | Session 02: phase 0j complete — firmware flashed and running on MAC `1c:db:d4:41:28:3c`. End-to-end toolchain → build → flash → boot → UART proven. Custom partition table deferred to Phase 9 (esp-idf-sys metadata path); test board's reported MAC noted in PLAN. |
| 2026-05-07 | 0.7 | Session 02: phase 0k complete — custom two-OTA-slot partition table now baked into firmware and flashed on chip. Cribbed the OUT-dir-copy build.rs trick from r2-core/platforms/esp32-s3. `tools/setup-firmware.sh` added for fresh-clone bootstrap. OTA infrastructure ready ahead of Phase 9 implementation. |
| 2026-05-07 | 0.8 | Session 02: phase 0L complete — firmware streams synthetic data over WiFi/TCP per WIRE spec. Phase 0m in progress — dashboard vendored from r2-core into Cargo workspace at repo root, CBOR-payload remap adapts integer-keyed maps to friendly browser keys. Phase 0n complete — git-SHA + timestamp version stamping on both firmware and dashboard, /api/version endpoint exposed for OTA decision logic. |
| 2026-05-07 | 0.9 | Session 02: end-to-end demo verified on real hardware (MAC `1c:db:d4:41:28:3c`). Schema/parser fixes during integration: R2-WIRE compact frame is 12-byte header (not 7-byte M10 layout); `hostname` not `name` for the friendly label; raw-LSB→g scaling moved server-side. Live decimation to 10 Hz keeps the browser smooth. `tools/setup-hotspot.sh` automates the AP bring-up. **Phase 5a complete** — NVS-persistent Ed25519 device key, signed announce verified on the wire. Phase 5b–5e (dashboard verify, HMAC, relay, cloud) carried forward. Phase 4 effectively delivered as part of 0L (WiFi+TCP+frames). New tasks: #17 Trust Group + remote access (5b/c/d/e), #18 LED state machine. |
