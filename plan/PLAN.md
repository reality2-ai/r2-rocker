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
+ battery sense       (ADC1_CH3)          └ Unified R2 port :21042
+ on-board RGB LED    (status)              (HTTP + WS /r2 + raw R2-WIRE
                                             multiplexed via peek-detect,
                                             WIRE §13.5)

   BLE beacon (R2-BEACON)        →     dashboard scan
   L2CAP listen                  ←     L2CAP connect, #wifi_offer (signed by TG)
   WiFi join → TCP :21042        →     SENSOR_ANNOUNCE (TG-cert-signed)
   sample → SD ring (durable)
        └── network task ────────►     ┌─ multi-peer HashMap
   battery event every 30s ─────►      ├─ per-device cal matrix
                                  ←──  ├─ ACK every 200 ms / 100 samples
                                  ←──  └─ cmd events (r2.dash.cmd.*)
                                       └→ browser at :21042
                                          (R2RockerHive + ViewerSentant
                                          (grid · canvas · joint groups
                                           · pairwise differential
                                           · stress indicator · trend)
```

## 3. Phasing

> **v0.2.0 status note (2026-05-24):** The architectural-gaps roadmap
> (`audits/2026-05-23-architectural-gaps.md`, Tracks A/B/C/D) has
> landed in v0.2.0. Sensors are now formal TG members with KeyHolder-
> signed `DeviceCertificate`s; the webapp runs an `R2RockerHive`
> with a `DashboardViewerSentant`; the operator plane that used to
> ride as ~17 `POST /api/*` routes + a `/ws/status` JSON channel
> now rides as `r2.dash.cmd.*` and `r2.dash.*.progress` R2-WIRE
> events on the unified `/r2` WebSocket; the dashboard listener
> unified on TCP `:21042` with peek-based protocol detect (WIRE
> §13.5). The row-level statuses below predate that arc — many ⏳
> items are now ✅ but await a deliberate plan refresh.

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
| 5b | Dashboard verifies announce sig | ✅ | Server-side Ed25519 verify against `device_pk` from the announce; Track A extended this with full KeyHolder-cert-chain verification — every announce on `/r2` now shows `sig=ValidWithCert`. Legacy TOFU branch removed. |
| 5c-cert | Cert-chain envelope on announce | ✅ | KeyHolder-signed `DeviceCertificate` issued at BLE bootstrap, persisted in NVS, carried on every announce; dashboard verifies cert under embedded TG pk and announce under cert's `device_public_key`. Track A landed in v0.2.0. |
| 5c-hmac | HMAC envelope per R2-WIRE frame | ⏳ | DEK + HK derivation from TG SK (HKDF-SHA256, `r2-trust` SPEC §3); HMAC tag on every frame; R2-WIRE has-hmac flag. Authenticates EVERY frame, not just announce. Cert is on the channel; the per-frame tag is still pending. |
| 5d | WebApp = WASM hive (replaces current Axum-served dashboard) | ✅ (revised) | **R2RockerHive + DashboardViewerSentant** landed in v0.2.0. The webapp owns peer state, capture state, alias map, LED phases, access flow inside the sentant; the page renders from sentant state. Operator actions emit `r2.dash.cmd.*` on `/r2`; status broadcasts arrive on the same socket. Original row scoped a full dashboard *replacement* (controller process reduced to listener + relay + bundle host); the actual shape landed *additively* — the dashboard process keeps its sensor pipeline + a small set of HTTP helper routes (`/api/ota/{addr}`, `/api/data/*`, `/api/firmware/*`, `/api/access/{onboard,whoami}`, `/api/devices/aliases` GET, `/api/keyholder/tg-pub`, `/api/version`) and the operator plane moved to events. The "dashboard is a transport plugin to the hive" framing (D-24 below) covers the gap.<br><br>**Original scope retained:** ✅ bundle served from BOTH GitHub Pages + onsite controller (byte-identical); ✅ relay-compatible WSS forwarding; ✅ TG-signing KeyHolder authority. **Original scope deferred:** ⏳ remote mode = read-only by default (today's enforcement is operator-discretion, not cert-role-gated).<br><br>~~**Replace, not augment.**~~ Vendor `r2-wasm` + its transitive crates (`r2-trust`, `r2-route`, `r2-transport`, `r2-engine`) from r2-core into `crates/`. Set up `wasm-pack` build. Static bundle is **served from BOTH GitHub Pages (for remote viewers, requires internet) AND the onsite controller's HTTP server (for onsite viewers, no internet needed)** — byte-identical bundle. Operator-discretion which one the QR/link points at. Onsite controller's Rust process is reduced to: (a) sensor TCP listener; (b) relay-compatible WSS forwarding raw R2-WIRE bytes to connected browser hives; (c) HTTP serving of the WebApp bundle; (d) TG-signing authority (KeyHolder) that issues device certs to enrolling browsers; (e) local data archive (Phase 5f). It no longer decodes frames or serves a JSON-pushing dashboard. The browser is the canonical viewer in both onsite and remote modes. Onsite mode = full operator control; **remote mode = read-only by default** (enforced via TG cert role + UI). Each browser is its own enrolled TG member (keypair + cert in IndexedDB, per `r2-trust` §2). UX layer stays plain JS + Chart.js; WASM handles only protocol + crypto. Same deployment shape as r2-notekeeper / anthill. Initially uses public r2-relay for remote; migrates to own relay+archive (5e) by config. |
| 5d-enrol | Pairing flow for browsers | ✅ (different shape) | **Pivoted from QR/token to request/approve.** ACCESS v0.3 (specified at SPEC-R2-ROCKER-ACCESS) ships the working flow: visitor opens the webapp on the LAN, types a name, clicks "Ask to pair", which emits `r2.dash.cmd.access.request`. The KeyHolder sees the request on their Link tab and approves; webapp emits `r2.dash.cmd.access.check` every ~2s, picks up the approved bundle (cert + key + DEK + HK + relay URL, JSON-stringified in response key 5), persists in IndexedDB, becomes a TG member. The original QR/token design was retired (D-30) — having to mint short-lived tokens added complexity that the request/approve flow doesn't need; the operator's approve click *is* the authorisation. Onboard QR helper still exists in the Link tab but only renders the dashboard URL (no token), as a hand-friendly URL for visitors scanning with their phones. |
| 5e | Own combined relay + archive server | ⏳ | Self-hosted VPS that **both relays and stores data**. Replaces the public r2-relay for this deployment. Combines what was previously split (5d relay + 5e cloud archive) into one server: forwards TG-encrypted frames AND runs a TG-member archiver consumer that persists the long-term store. Frame plaintext only ever exists inside TG members; the relay sees only sealed envelopes; the archiver IS a TG member so it sees plaintext, but it's *our* server. |
| 5f | Onsite local data archive | ⏳ (partial) | Capture files exist per-sensor (`/api/data/{addr}/list|file|all` + the `data_tcp` protocol on each sensor's SD ring); the dashboard merged-export endpoint composes them on demand. **What's missing:** a sessions/<id>/ tree on the dashboard that captures named long-form sessions per the original scope. Today's capture files are the de-facto archive but they're per-sensor and per-capture, not per-session. |
| 5L | RGB LED state machine (firmware) | ✅ | WS2812 RMT driver + animator task running on both DevKitC and XIAO carriers. Per `HARDWARE-WIRING.md` §5 colour map. **OTA-active uses a distinct magenta cycle.** XIAO single-colour LED carries the same patterns minus the RGB channel (`xiao_led_single_color` memory). |
| 6 | BLE bootstrap FSM | ✅ | Ported to ESP32-S3 with NimBLE; sensor advertises R2-BEACON with class string `nz.ac.auckland.rocker.sensor` (FNV-1a-32 hash `0x6A3B0860`; locked at SPEC-R2-ROCKER-DASHBOARD §6.3), receives signed `#wifi_offer`, persists in NVS. Retired `wifi_config.toml`. Bench-validated end-to-end including the settle-after-cycle delay (`lwip_bind_pre_init_hangs` memory + `get_active_sensor_ips` loopback filter). |
| 7 | Per-sensor calibration | ⏳ | Two-position cal protocol + dashboard-side rotation matrix |
| 8a | Devices view (fleet status) | ✅ | Per-peer cards shipped in the v0.2.0 webapp: hostname/alias, online indicator, battery (mV + percent + bar), `fw_ver`, last-seen timestamp, FSM state, virtual LED mirroring the physical pattern. Per-card "Update Firmware" button drives the v0.2 OTA push (Phase 9-light); per-card Reset + Identify emit `r2.dash.cmd.reset` / `r2.dash.cmd.identify` on `/r2`. Originally scoped 20+ sensors; bench-validated to 4 — no architectural cap. |
| 8b | Live charts grid + canvas | ⏳ | CSS-grid auto-fit per-peer cards; Canvas-based mini-charts (replace per-peer Plotly) — **must stay smooth at 20–30 sensors at 10 Hz**, the deployment scale we're targeting. Cards collapse to summary tiles past ~8 visible. Offscreen cards pause their chart loop. |
| 8c | Joint groups + pairwise differential + stress indicator + trend | ⏳ | The diagnostic view per `SPEC-R2-ROCKER-DASHBOARD` §10–§11. Joint-group editor, `Δa_lateral` plot per joint, sliding-window RMS, daily-summary trend chart. |
| 8d | Measurement sessions | ⏳ | A "Sessions" view (alongside Live / Devices / Joints) that lets the operator define and run named **measurement sessions** — e.g. "tyre-wear-load-test-2026-05-08-am". Session captures: start/stop timestamps, participating sensors, calibration snapshot at session-start, per-joint analytics traces, operator notes/metadata (weather, rig config, payload). Storage in browser IndexedDB (browser-hive owns the data); shared across TG-member browsers via the relay; archived long-term by the Phase 5e cloud consumer. **Session ID is a browser-/dashboard-side concept** — not on the wire — but the browser may tag stored frames with it, and replay/export by session. Enables the report-writing workflow ("session 47 — load test, mean stress 0.18 g RMS, Δrms vs baseline +12%"). |
| 9-light | OTA over WiFi (no signing) | ✅ | Operator submits a `.bin` via `POST /api/ota/{addr}`; firmware streams it from the embedded ESP-IDF OTA partition pair; `r2.dash.ota.progress` events fan out on `/r2`. Sensor reboots and reconnects on success. Multi-MB binary push is the one operator-plane HTTP route that survived the v0.2 event migration (`/api/ota/{addr}`). |
| 9-secure | TG-signed OTA images + SD-backed rollback | ⏳ | The "signed binaries" half of the original Phase 9. `r2-build` produces TG-signed `.bin`s; sensor verifies under the embedded TG pub before committing the OTA slot; rollback path uses the SD ring's boot-recovery hook. Phase 9-light is the operational stand-in until this lands. |
| 9-fwreg | Current-firmware register + out-of-date badging | ✅ (partial) | "Out-of-date" badge and one-button "Update Firmware" on Devices cards ✅ — driven by the GitHub-Releases query (`/api/version` + `/api/firmware/*`) compared against each peer's announced `fw_ver`. **What's missing:** a controller-owned target-register independent of GH (so an offline operator can pin a specific reference build). Today's flow is "latest GH release = the target"; the originally-scoped "promote a reference device" workflow isn't built. |
| Z (gating, recurring) | R2-specifications conformance check | 🔄 | Cross-validate every protocol-bearing component against the canonical specs in `../r2-specifications/specs/r2-core/`: test vectors for R2-FNV, R2-WIRE compact + extended, R2-CBOR deterministic encoding, R2-TRUST cert format + HKDF, R2-BEACON, R2-BOOTSTRAP. Outcome: a `testing/wire-vectors.json` (per `SPEC-R2-ROCKER-WIRE.md` §9) generated by firmware encoder + dashboard encoder + WASM encoder, all three byte-identical for the same inputs; same for HMAC tags and Ed25519 signatures over canonical CBOR. Runs **after each protocol-touching phase** (5c HMAC, 5d WASM port, 6 BLE bootstrap) AND **before any university / public release**. Not a one-shot. **First audit committed at `audits/2026-05-07-conformance-audit.md` (2026-05-07): wire-level ✅; architectural-layer ⚠️ — firmware + dashboard are monolithic Rust processes, should be sentants composing an ensemble per R2-SENTANT/R2-PLUGIN/R2-ENSEMBLE/R2-DEF. r2-engine is vendored but only used by r2-wasm. r2-notekeeper is the working reference. Recommendation rolled into Phase 5d below.** |
| 5d-ensemble | Author `r2-rocker.ensemble.yaml` (compile-time composition) | ⏳ | Define the canonical ensemble YAML per R2-DEF §7 — declares the sensor sentant + bridge/calibration/archive sentants on the dashboard hive + plugins (SD storage, battery, WiFi, accelerometer, OTA, calibration). Same shape as `r2-notekeeper/ensemble/ensemble.yaml`. **NOT a runtime rewrite** — per the 2026-05-07 audit's "AOT compilation as conformance" reconciliation, the firmware/dashboard binaries are manually-compiled forms of this ensemble; the YAML documents that composition. Runtime sentant interpretation via `r2-engine` is required only for the browser WASM hive (Phase 5d step 4); the firmware stays AOT-compiled until/unless multi-hive sentant migration is needed. |
| 10 | Heterogeneous-fleet support (mixed sensors / carriers / variants) | ⏳ | Today's "Pull → N sensor(s)" assumes one carrier + one sensor type. Mixed deployments (e.g. a DevKitC + ADXL355 alongside an XIAO + IMU + a future LoRa-bridged sensor) need a spec slice first: announce-time variant tag, dashboard fleet-routing by variant, GH-release `.bin` naming convention. Spec is the first deliverable; implementation follows once the rocker rig itself benefits (`project_heterogeneous_fleet_open_question`). |
| 11a | Sensor Idle mode (cheap-win low power) | ⏳ | Mid-power state between "streaming at 100 Hz" and "deep sleep" — sensor cuts the streaming task on operator command, parks at 1 Hz heartbeat, keeps WiFi up. Operator wakes with a cmd event. Big battery win without the NimBLE-migration scope of phase 11b. |
| 11b | Deep low-power: NimBLE migration + light-sleep between adverts | ⏳ | Replace the current bluedroid stack with NimBLE; add light-sleep windows between R2-BEACON advertise intervals; SD-flushes batched to ≥30s. Targets a multi-day battery profile rather than the current overnight-with-30%-left figure (`project_battery_test_2026_05_18_result`). |
| 12 | Viewer-side relay leg (github.io transport) | 🔄 | Webapp hosted on `reality2-ai.github.io/r2-rocker/` opens a WSS to the operator's configured r2-relay using the relay URL that landed in IndexedDB at approve time; relay forwards encrypted blobs to the controller's relay-side session. Operator-plane events ride the same path. Currently in flight (task #62) — Anywhere pairing reaches the relay but not the Link tab as of 2026-05-18; diagnostic logs in place (`project_phase5_relay_pairing_blocker_2026_05_18`). |
| 13 | LoRa gateway architecture (R2-LORA / R2-WIRE-over-LoRa) | ⏳ | Pi 5 + RAK2287 single-channel concentrator OR Arduino Uno-Q + SX1302-USB. Carries R2-WIRE frames over LoRa with LoRaWAN coexistence for legacy sensors. Specced for the broader R2 Transient Networking arc, not the immediate rocker deployment. Architecture pinned to a future ADR. |

Phases 0e–0h block all coding (PROCESS rule 1 — spec before code).
Phase 1 (soldering) can run in parallel with the spec writing.

### 3.1 Architectural tracks (v0.2.0 → onwards)

Where the row-numbered phases above capture sequential build steps,
the **tracks** capture cross-cutting architectural arcs that landed
(or are in flight) on top of those phases. The canonical record of
the architectural-gaps work that produced Tracks A/B/C/D is in
[`audits/2026-05-23-architectural-gaps.md`](../audits/2026-05-23-architectural-gaps.md).

| Track | Scope | Status | Notes |
|---|---|---|---|
| A | Sensors as formal TG members (KeyHolder-signed `DeviceCertificate`s) | ✅ v0.2.0 | Closes Gap D. Cert minted at bootstrap, persisted in NVS, carried on every announce; dashboard verifies cert chain under TG pk + announce under cert's `device_public_key`. Task #65. |
| B | Status broadcasts as `r2.dash.*.progress` events on `/r2` | ✅ v0.2.0 | Closes the "status JSON channel" half of Gap B. The `/ws/status` text channel and ~9 ad-hoc JSON shapes were retired. Task #67. |
| C | Operator actions as `r2.dash.cmd.*` events on `/r2` | ✅ v0.2.0 | Closes the "operator HTTP API" half of Gap B. ~17 `POST /api/*` handlers retired; surviving HTTP routes are operator-helpers (OTA push, data export, version check, alias bulk-fetch, ACCESS UI helpers). Task #67. |
| D | Webapp runs `R2Hive` event loop | ✅ v0.2.0 | Closes the conceptual half of Gap C. `R2RockerHive` + `DashboardViewerSentant` own state; UI renders from sentant state. Task #66. |
| E | Two-TG split + bridge sentant (production / viewing) | ⏳ deferred | Reserved for the post-rocker R2 work — implementation deferred. Event-name space in Tracks B+C was picked to avoid forcing a wire break later. See `SPEC-R2-ROCKER-BRIDGE.md` and task #52. |
| F | Unified R2 port (`:21042`) with peek-based protocol detect | ✅ v0.2.0 | Per WIRE §13.5. Single TCP listener; first byte routes to raw R2-WIRE pipeline (`0x00…`) or HTTP/WS upgrade (`[A-Z]…`). `socket2::SockRef` keepalive pattern after a `stream.peek()` corrupted the connection in early builds (`feedback_keepalive_via_sockref`). |
| G | Controller process becomes an `r2-engine` hive | ⏳ deferred | The dashboard's *internal* dispatch staying bus-driven (rather than just its wire shape) is a separate refactor. Not required for Tracks A/B/C/D to be "closed". |

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
| D-23 | Unified R2 port `:21042` with peek-based protocol detect (WIRE §13.5) — single TCP listener, first byte routes raw R2-WIRE vs HTTP/WS upgrade | Track F (v0.2.0) |
| D-24 | Dashboard process is a transport plugin to the controller hive; new logic defaults to sentants, not routes | `project_dashboard_is_a_plugin` |
| D-25 | Every cross-hive byte is either an R2 event on R2-WIRE or a named plugin protocol owned by a sentant | `project_sentants_as_specs` |
| D-26 | Sentants describable as FSMs in spec form (`state × event → state' × {emitted_events, plugin_actions}`); code form optional | `project_sentants_as_specs` |
| D-27 | Two-TG entangled topology (production + viewing) per R2-TRUST §7.5; KeyHolder is a separable role from controller; multi-KeyHolder + save/restore operator-managed | `project_two_tgs_entangled` |
| D-28 | Controller is fixed per experiment (chosen at setup); if it dies the experiment restarts — design for simple, not seamless failover | `project_controller_is_fixed_per_experiment` |
| D-29 | Rig is operator-supervised, not unattended; durability burden lighter than spec implies; weight "operator intervenes" over "system recovers autonomously" | `project_operational_supervised` |
| D-30 | ACCESS v0.3: request/approve is the sole pairing flow; invite/claim removed (`POST /api/access/invite|claim` deleted; the approve click *is* the authorisation) | SPEC-R2-ROCKER-ACCESS v0.3, task #58 |
| D-31 | Calm-tech security UX: TG/cert/OTA/enrolment machinery invisible by default; one-button-equals-one-thing; status via ambient signals; expert mode opt-in | `feedback_calm_tech_security` |
| D-32 | Accessible visual indicators: pattern carries info alongside colour; dashboard always shows text next to any coloured indicator | `feedback_a11y_indicators` |
| D-33 | Sensors are formal TG members carrying KeyHolder-signed `DeviceCertificate`s on every announce; dashboard verifies cert chain (no TOFU) | Track A (v0.2.0), task #65 |
| D-34 | Webapp = R2 hive (each browser instance runs `R2RockerHive` + `DashboardViewerSentant`); role-from-cert chooses which sentants instantiate | Track D (v0.2.0), task #66, `project_webapp_is_a_hive` |
| D-35 | Acceleration broadcasts to `/r2` are decimated 100× server-side (1 kHz capture → 10 Hz wire) for Pi5-class deployments | `project_pi5_acceleration_rate_limit`, task #68 |
| D-36 | Release build order: tag the tree first, then build with `R2_RELEASE=1`; otherwise `fw_ver` bakes `-dirty` and the "needs update" check sticks | `feedback_release_build_order` |
| D-37 | Heterogeneous-fleet support is v0.2+ — current "Pull → N sensor(s)" assumes one carrier + one sensor type; mixed deployments need a spec slice first | `project_heterogeneous_fleet_open_question`, task #57 |
| D-38 | No R2 protocol jargon in user-visible UI ("Connect Sensors", "Link", "Connection Log"); code + specs keep canonical terms (TG, Bootstrap, Enrol) | `feedback_ui_no_protocol_jargon` |

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

Post-v0.2.0. In rough priority order:

1. Phase 12 — finish the **viewer-side relay leg** so the github.io-hosted webapp can reach the controller off-network through the relay (task #62, currently blocked on the 2026-05-18 Anywhere-pairing bug not surfacing in the Link tab).
2. Phase 7 — **per-sensor calibration** protocol + dashboard-side rotation matrix. The classifier arc (`project_catastrophic_joint_failures`) needs clean training data; uncalibrated samples are training noise.
3. Phase 8d — **measurement sessions** view + the §5f local archive sketch that goes with it. Today's capture files are the de-facto archive but per-sensor and per-capture; a session record is what the eventual paper / report writes from.
4. Phase 8c — **joint groups + pairwise differential + stress indicator + trend**. The diagnostic view that the rig actually exists to support.
5. Phase 5c-hmac — **per-frame HMAC envelope**. Cert-on-channel is in place; per-frame tag closes the integrity story.
6. Phase 10 — **heterogeneous-fleet** spec slice, before the LoRa-bridged sensor shows up uninvited.
7. Phase 11a — **sensor Idle mode** as a cheap battery-life win before the NimBLE-migration scope of Phase 11b.

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
| 2026-05-24 | 1.0 | Deliberate refresh against the v0.2.0 ship state. **§3 status pass:** flipped 5b, 5d, 5L, 6, 8a, 9-light, 9-fwreg (partial) to ✅; split 5c → 5c-cert (✅) + 5c-hmac (⏳); pivoted 5d-enrol from QR/token to ACCESS v0.3 request/approve (D-30); marked 5f as partial; added phases 10 (heterogeneous fleet), 11a (Idle mode), 11b (deep low-power / NimBLE), 12 (viewer relay leg), 13 (LoRa gateway). **New §3.1 tracks subsection** (A–G) capturing the cross-cutting architectural arcs from `audits/2026-05-23-architectural-gaps.md` — Tracks A/B/C/D/F all closed in v0.2.0; E + G deferred. **§4 decisions:** appended D-23 … D-38 covering unified port, dashboard-as-plugin, sentants-as-specs/FSMs, two-TG entanglement, fixed-controller-per-experiment, operator-supervised framing, ACCESS v0.3 pivot, calm-tech UX, a11y indicators, sensors-as-TG-members, webapp-as-hive, Pi5 decimation, release build order, heterogeneous-fleet caveat, no-jargon UI. **§7 immediately-next** rewritten to the post-v0.2.0 priority list (was still listing Phase 0e specs). |
