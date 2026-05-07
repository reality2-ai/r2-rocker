# AI-CONTEXT.md — fresh-session entry point

If you are an AI assistant being asked to continue work on this project
with no prior conversation memory, **read this file first** (it should
take under 2 minutes), then the files it points at. Do not relitigate
binding decisions — if you think one needs reopening, raise it with the
user explicitly.

---

## Final / target architecture (canonical)

User-described 2026-05-07. The end-state shape — what we're
incrementally building toward:

```
                     ┌─────────────────────────────────┐
                     │  Own relay+archive server        │
                     │  (own VPS, eventually replaces   │
                     │   public r2-relay)               │
                     │  · forwards TG-encrypted frames  │
                     │  · TG-member archiver: stores    │
                     │    sensor data long-term         │
                     └────────▲───────────────▲─────────┘
                              │ WSS           │ WSS
                              │               │
   GitHub Pages               │               │
   (static WebApp bundle)     │               │
            │                 │               │
            │ HTTPS           │               │
            ▼                 │               │
   ┌──────────────────┐       │               │
   │ Browser hive     │       │  REMOTE       │
   │ (WASM stack +    ├───────┘               │
   │  JS UX, runs     │                       │
   │  ANYWHERE)       │  ONSITE (over hotspot)│
   │                  ├───────────────────────┤
   └─────▲────────────┘                       │
         │ download + enrolment-code          │
         │                                    │
   ┌─────┴──────────────────────┐             │
   │ Onsite host                │             │
   │  · sensor TCP listener     │◄────────────┘
   │  · bridges to relay        │ publishes encrypted events
   │  · LOCAL data archive      │
   │  · TG signing key + cert   │
   │    issuance (KeyHolder)    │
   └────────▲───────────────────┘
            │ WiFi (AP on onsite host)
            │
       ┌────┴────┐
       │ Sensors │  ESP32-S3 (×N), TG members,
       └─────────┘  signed announce + HMAC frames
```

Key distinctions:

* **One WebApp binary, two modes**: same WASM bundle from GitHub
  Pages, runs onsite (links to onsite host over hotspot) or remote
  (links via relay). Browser detects mode by which transport
  succeeds, or by an explicit operator-set flag.
* **Onsite mode is the privileged one**: the operator on the rig
  floor can Start/Mark/Stop a measurement session. Remote viewers
  are read-only by default — a remote viewer must not "upset a
  running experiment". This is enforceable via TG cert role
  (KeyHolder vs Member) plus a UI affordance (controls greyed out
  for remote).
* **Onsite host stores data locally** — independent of any cloud /
  relay. So an offline onsite session works without the internet,
  and survives even if the cloud server is unreachable.
* **Own relay + archive in one server** — eventually replaces the
  public `r2-relay` for this deployment. Same VPS forwards encrypted
  frames AND runs a TG-member archiver consumer that keeps the
  long-term store. (Public r2-relay is the bootstrap path; migrating
  to own server is a config swap, not an architecture change.)
* **TG membership: who joins how** —
  - **Sensors** (ESP32-S3): TG members **automatically** by their
    firmware build. The firmware bakes in `trust_keys/tg_pub.bin` via
    `include_bytes!` and generates its own per-device Ed25519 keypair
    in NVS on first boot (Phase 5a, done). No enrolment dance.
  - **Onsite controller** (the dashboard host): TG member
    **automatically** by config — it's the KeyHolder, holds
    `tg_priv.bin` off-tree at `~/.config/r2-rocker/tg_signer/`. It
    doesn't enrol; it issues enrolments.
  - **Browsers** (laptops, phones, tablets — anything that runs the
    WebApp): TG members **by the QR/link enrolment flow** below. This
    is the only path that uses one-time tokens.

* **Browser enrolment via QR / link** — **same model as r2-notekeeper**.
  Joining a WebApp requires either a link or a QR code (e.g. scan
  from a phone); there is no other way in. The onsite dashboard
  generates a one-time join token (single-use, ≤5 min expiry). The
  token is encoded into both:
  - a **QR code** displayed on the dashboard (operator points another
    device's camera at the screen), and
  - a **shareable link** of the form
    `https://reality2-roycdavies.github.io/r2-rocker/?join=<token>`
    (operator emails / messages it).

  Either path opens the WebApp **and** triggers enrolment in the same
  step — the WASM hive reads `?join=` from the URL on first load,
  generates its keypair (IndexedDB), submits its public key + the
  token to the onsite host's KeyHolder endpoint over the relay, gets
  back a TG-signed device cert, becomes a member. From then on the
  device just works (cert persists in IndexedDB).

  Fallback: a manual "paste a join link" field in the WebApp's
  not-enrolled landing page, in case QR-scan / link-click both fail.

This shape supersedes the earlier separate Phase 5d (relay) +
Phase 5e (cloud archive) — they're one component now.

## What this project is

Wireless **structural-health-monitoring** sensor system for a half-tonne
actuator-driven rocker rig that tests tyre rubber against an asphalt bed.
The user (roy.c.davies@ieee.org) is concerned that lateral motion is
stressing the rig's actuator joints toward shear failure. Sensors detect
that lateral motion as a diagnostic. The data also feeds a paper / report.

**Hardware**: ESP32-S3-DevKitC-1 + EVAL-ADXL355-PMDZ + microSD breakout +
LiPo cell. Multiple sensor nodes (start: 2; later: dozens).
**Controlling device**: laptop or Raspberry Pi running a browser
dashboard.
**Protocol stack**: Reality2 (BLE bootstrap → trust group → WiFi → R2-WIRE
TCP) — vendored self-contained for university handoff.

## Read these in this order

1. `README.md` — repo layout & quick start.
2. `PROCESS.md` — the five working rules (spec-first, etc.).
3. `plan/PLAN.md` *(if it exists yet)* — current phasing & status.
4. The latest file in `conversation/` — most recent design rationale.
5. `specifications/HARDWARE-WIRING.md` — physical sensor build.
6. `specifications/SECRETS-POLICY.md` — before touching any keys.
7. Any `specifications/SPEC-R2-ROCKER-*.md` files — code-driving specs.

## Working conventions (binding)

| # | Rule |
|---|---|
| 1 | **Spec before code.** Every firmware/dashboard change has a driving spec in `specifications/`. The spec wins disagreements unless the user re-opens. |
| 2 | **Conversation is research data.** Every session appends a new file `conversation/YYYY-MM-DD-<topic>-NN.md` — verbatim user, faithful AI, decisions table at the end. Never edit a closed session retroactively. |
| 3 | **Plan is consolidation.** `plan/PLAN.md` overwrites itself; conversation accumulates. |
| 4 | **Secrets stay out.** No private keys, no WiFi creds, no NVS dumps in the working tree. `.gitignore` blocks the patterns; *don't put them there in the first place* is the real rule. |
| 5 | **Cite sources.** Datasheet page, vendor URL, file:line for code refs — so the university can reconstruct reasoning without us. |

Project is **self-contained** — no path deps on `../r2-core`. R2 protocol
crates will be vendored into `crates/` when they're needed (Phase 4+
onwards). Don't add `path = "../../r2-core"` style references.

## Current state (as of end-of-session-02, 2026-05-07)

**End-to-end demo alive on real hardware.** Simulated-data ESP32-S3
sensor on a NetworkManager AP, signed announce + acceleration +
battery streaming via R2-WIRE TCP to a vendored r2-dashboard, browser
charts updating in Chart.js. Hardware: ESP32-S3-DevKitC-1-N8R8 MAC
`1c:db:d4:41:28:3c`. ADXL355 not yet soldered — sender uses a sine
+ cosine + 1 g synthetic generator until then.

| Folder | Status |
|---|---|
| `specifications/HARDWARE-WIRING.md` | ✅ v0.2 (battery removable; off-board charging) |
| `specifications/SECRETS-POLICY.md` | ✅ v0.1 |
| `specifications/SPEC-R2-ROCKER-{WIRE,SENSOR,DASHBOARD,SYSTEM}.md` | ✅ v0.1.x |
| `PROCESS.md`, `README.md`, `.gitignore` | ✅ |
| `plan/PLAN.md` | ✅ updated through Phase 5a |
| `conversation/2026-05-0[67]-design-session-0[12].md` | ✅ both closed |
| `tools/r2-rocker-tg/` | ✅ TG keygen/verify/inspect, round-trip tested |
| `tools/setup-{hotspot,firmware}.sh` | ✅ wifi_config + partition staging helpers |
| `crates/r2-{fnv,cbor,wire,core,bootstrap}/` | ✅ vendored from r2-core, path-fixed for our workspace |
| `dashboard/` | ✅ vendored r2-dashboard, schema-adapted for `r2.sensor.*`, server-side LSB→g, /api/version, 10 Hz decimation |
| `firmware/esp32-s3/` | ✅ WiFi+TCP+R2-WIRE+CBOR+sim+identity. NVS-persistent Ed25519, signed announce. Two-OTA-slot partitions. ~1 MB binary, 33% of slot. |
| `trust_keys/tg_pub.bin` + `tg_cert.bin` | ✅ generated, name "rocker-rig-uoa-2026", priv off-tree at `~/.config/r2-rocker/tg_signer/` |
| `firmware/esp32-s3/wifi_config.toml` | ✅ generated by setup-hotspot.sh (gitignored — temporary bridge until Phase 6 BLE bootstrap) |

## Architectural commitments worth knowing

* **The remote browser IS a hive — not a thin viewer of a remote
  server.** This is the architectural model `r2-notekeeper` and
  `anthill` already use: there is no remote Rust web server serving
  "data" to the browser. The full R2 stack runs *inside the browser*
  via WASM (`r2-core/crates/r2-wasm/` exposes FNV, CBOR, R2-WIRE,
  R2-TRUST, R2-ROUTE, R2-TRANSPORT, R2-ENGINE through `wasm-bindgen`),
  and the browser is itself a TG member: holds its own keypair +
  TG-signed cert, decrypts + verifies frames, talks peer-to-peer
  with other TG members through the relay.

  **Phase 5d is REPLACE, not AUGMENT** — the existing Axum-served
  HTML+JS dashboard goes away. The Rust process stays only for: (a)
  sensor TCP listener, (b) relay-compatible WSS forwarding raw R2-WIRE
  bytes to connected browser hives, (c) TG KeyHolder cert issuance,
  (d) local data archive (Phase 5f). It no longer decodes frames or
  serves HTML/JSON. The browser is the canonical viewer in both
  onsite and remote modes — same WebApp binary either way.

  **Why do this now rather than retrofit later** (user, 2026-05-07):
  the longer the current Rust-server-decoded model is in use, the
  more accumulated state it picks up — calibration storage, joint
  groups, session history, per-peer metadata — and the harder it
  gets to migrate to the WASM-hive model without breaking saved data
  or rewriting half the code twice. Migrating before that drift
  accumulates is the cheaper path. Phase 5b's Rust-side announce
  verification is acknowledged as transitional — the same logic
  re-emerges in WASM during 5d, just compiled differently.

  Hosts at deployment of Phase 5d:

  | Host | Role | Why this host |
  |---|---|---|
  | GitHub Pages (or any static CDN) | Serves the `wasm-viewer/` bundle (HTML + JS + .wasm). Updated via `git push`. | Static hosting for the **remote** path. Public, cacheable, no execution, no plaintext, no secrets. |
  | Onsite controller | Hosts its own copy of the same `wasm-viewer/` bundle on its HTTP server (e.g. `http://10.42.0.1:8080/`). Plus: sensor TCP listener + relay-compatible WSS forwarder + TG KeyHolder cert issuance + local data archive (Phase 5f). | Lets onsite browsers get the WebApp **without internet** — open a tablet on the hotspot, browse to the controller's IP, scan the QR from the same dashboard's "Enrol device" UI, join TG. Closed-network deployments work end-to-end. |
  | r2-relay (e.g. $5 VPS) | Forwards TG-encrypted frames between members over WSS. Sees no plaintext. | Public-internet rendezvous so remote browsers can reach the controller across NAT. Skipped when everything's onsite on the hotspot. |

  Both bundles are byte-identical (same `cargo build --target wasm32`
  output) — operator-discretion which host the QR/link points at:
  GitHub URL for remote viewers, controller-local URL for onsite
  viewers. Same WebApp either way.

  The remote browser loads the static page from GitHub, opens a WSS
  to the relay using its enrolled TG cert, and is then an active
  member of the rocker-rig TG. Updates to the viewer = `git push`;
  updates to the protocol stack = `r2-wasm` rebuild + push.

  **Layering inside the browser** (same split as the current local
  dashboard's server/JS split, just relocated):

  | Layer | Purpose |
  |---|---|
  | WASM | Protocol + crypto: frame decode/encode, HMAC verify, TG key derivation (HKDF), cert validation, Ed25519 sig checks, R2-WIRE state, per-event dispatch. |
  | Plain JavaScript | UX: DOM, Chart.js, layout, event handlers, calibration wizard, joint-group editor, the Devices view, the LED animations. |

  **Same deployment shape as `r2-notekeeper`.** When implementing
  Phase 5d, study notekeeper's enrolment flow + per-device cert
  management; we inherit the proven UX rather than designing fresh.

* **Each remote browser is its own enrolled TG member** — not a copy
  of the dashboard's keys. Per `r2-trust` SPEC §2, each browser
  generates its own Ed25519 keypair on first run (persisted in
  IndexedDB), gets a TG-signed device certificate via a one-time
  enrolment flow (operator presents a join code on the onsite
  dashboard's UI; the browser submits it back along with its public
  key; the dashboard's TG KeyHolder issues a cert binding device_pk
  to the TG with role + expiry). Browser then presents that cert
  when subscribing to the relay. Cert revocation lives with the TG
  KeyHolder (onsite). This means: stolen laptop → revoke its cert,
  no need to re-key the whole TG. Same trust semantics as the
  sensors (each sensor has its own keypair + TG cert), just on a
  different platform.

## Dashboard scaling target (binding)

**1 sensor today, 20+ sensors at full deployment.** Every Phase 8
deliverable shall stay smooth + readable across that range:

* CSS grid auto-fit (cards reflow from 1 column → 6+ columns).
* Cards collapse to summary tiles when count > ~8 visible.
* Canvas-based mini-charts (Plotly per-peer ≠ scalable).
* Offscreen cards pause their chart loops.
* Health-summary header shows aggregate state at a glance.
* Sorting + filtering (by joint, by status, by needs-update).
* Virtualised list past ~30 peers if browser jank shows.

Confirmed by user 2026-05-07: "in time there could be 20 or so, so
the dashboard has to expand automatically." Per
`SPEC-R2-ROCKER-DASHBOARD` §12.1.

## Dashboard UX direction (Phase 8 captured intent)

The dashboard grows three views (operator picks via a tab/toggle):

| View | Purpose | Phase |
|---|---|---|
| **Live charts** | Real-time x/y/z per sensor — what we have today | (delivered in 0L) |
| **Devices** | Fleet-status overview: per-sensor online state, battery, fw_ver, last-seen, FSM state, **virtual LED** mirroring the hardware RGB LED (colour + animation per `HARDWARE-WIRING.md` §5), and an **"Update Firmware"** button per card (stub → real OTA in Phase 9) | 8a |
| **Joints** | Diagnostic view: pairwise differential lateral motion, stress indicator per joint, long-term trend chart | 8c |
| **Sessions** | Named measurement sessions: define / start / stop, capture participating sensors + calibration snapshot + per-joint traces + operator notes; replay + export. Storage in browser IndexedDB (browser-hive owns it); shared across TG-member browsers via relay; archived long-term by cloud consumer (5e). | 8d |

The virtual-LED is a one-line addition once Phase 5L is in: firmware
already tracks FSM state internally for the physical LED; just include
the state in `r2.sensor.status`. The browser's CSS animations
(solid / pulse / heartbeat / strobe) match the physical LED 1:1.

## Lessons learned the hard way (carry these forward)

* **esp-idf-sys + custom partition table**: ESP-IDF resolves
  `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME` relative to esp-idf-sys's
  auto-generated build directory, not the crate root. Solution:
  `firmware/esp32-s3/build.rs` walks up to find the `esp-idf-sys-*/out/`
  directory and copies `partitions.csv` there. First-build chicken-
  and-egg solved by `tools/setup-firmware.sh`. Rebuild a SECOND time
  if a clean rebuild produces the default 1-app layout.
* **NetworkManager auto-reverts AP profile**: bringing up
  `r2-rocker-ap` on the USB adapter works, but if NM later auto-
  reconnects the same interface to a regular network (e.g. on a
  reboot or NM restart), the AP profile drops silently. Re-run
  `tools/setup-hotspot.sh` to bring it back up.
* **R2-WIRE compact frame layout** is 12-byte header (per spec), not
  the 7-byte custom header the M10 demo dashboard had hardcoded. The
  dashboard parser was fixed in this session — check both
  decoder sites if porting any further r2-core dashboard code.
* **Schema rename**: M10's `acceleration` → ours `r2.sensor.acceleration`,
  `battery_status` → `r2.sensor.battery`. Server-side `remap_payload`
  in `dashboard/src/main.rs` translates integer-keyed CBOR to friendly
  named keys.
* **Dashboard expects `name` for hostname**; our spec uses `hostname`.
  The dashboard tries both for compat.
* **Decimation matters**: 100 Hz to the browser is too fast — Chart.js
  drops frames + the broadcast channel fills. Per
  `SPEC-R2-ROCKER-DASHBOARD` §5.2, we decimate to 10 Hz on the wire to
  the browser; full rate goes to the SD ring (when Phase 3 lands).
* **Two repo paths**: `/mnt/data/Development/R2/r2-rocker` and
  `/home/roycdavies/Development/R2/r2-rocker` resolve to the same
  inode (one is a symlink). Don't be confused by the dual paths.

## Binding architecture decisions

Don't relitigate these without explicit user re-opening:

1. **Hardware**: ESP32-S3-DevKitC-1 + EVAL-ADXL355-PMDZ + microSD + LiPo.
2. **Wiring**: SPI2/FSPI defaults (CS=GPIO10, MOSI=11, SCLK=12, MISO=13,
   DRDY=14) + SD CS=GPIO9 + battery sense=GPIO4 (ADC1_CH3) via 100k/100k
   divider. RGB LED on GPIO38 (v1.1) as status indicator.
3. **Sample rate**: default 100 Hz; NVS-tunable up to 4 kHz.
4. **g-range**: default ±2 g; NVS-tunable.
5. **SD card is primary durable log** — TCP is a near-real-time tap, not
   the source of truth. Producer / consumer / ack tasks. Ring overwrite-
   oldest at full.
6. **Sample record** (SD + wire): 20 bytes fixed: `(seq:u32, ts_ms:u32,
   x:i32, y:i32, z:i32)`.
7. **Two wire encodings**: `r2.sensor.acceleration` (live, per-sample)
   and `r2.sensor.acceleration.batch` (catch-up, ~50 samples per frame).
8. **Cumulative ACK**: dashboard sends `r2.dash.ack {through_seq:N}` every
   200 ms or 100 samples; sensor advances ring head on receipt.
9. **Calibration**: per-sensor two-position rest method
   (g_A, g_B → main = norm(g_B−g_A), vertical = norm((g_A+g_B)/2),
   sideways = main × vertical, R = [main; sideways; vertical]).
   **Calibration matrix lives on dashboard**, keyed by device public key,
   persisted to `dashboard/calibration.json`.
10. **Trust group**: hardwired via compile-time `include_bytes!` from
    `trust_keys/tg_pub.bin`. Per-device Ed25519 in NVS, generated first boot.
11. **Diagnostic model**: structural health monitoring via **differential
    lateral motion between sensors across joints**. Initial deployment is
    **topology B**: 1 sensor per actuator joint (2 sensors total). Future:
    pairs across each joint, plus bed sensors as reference channel for
    environmental subtraction.
12. **Sensor mounting role** is dashboard-side schema (`rocker / bed /
    other`) — affects calibration treatment and analytics, not firmware.
13. **OTA**: TCP-push with SHA-256 verify; signing with TG key is a
    follow-up. SD card holds last-known-good firmware backup.
14. **Time sync**: monotonic per-device `ts_ms` + `r2.dash.sync_pulse`
    (Cristian's algorithm) for cross-sensor alignment, ~5 ms accuracy.

## Phasing

| # | Phase | Output |
|---|---|---|
| 0 | Scaffolding & specs | Wiring + secrets + process docs (this session) |
| 1 | ADXL355 SPI bring-up | Sample readout to UART, no networking |
| 2 | Battery readout | ADC1 + divider, log to UART |
| 3 | SD ring + sequencing | Durable log, fixed-size records, replay-on-reconnect |
| 4 | WiFi single-sensor + ACKs + time-sync | Dashboard sees one sensor's accel + battery |
| 5 | Hardwired TG + signed `SENSOR_ANNOUNCE` | TG-authenticated peers |
| 6 | BLE bootstrap FSM | Sensor FSM ports the M10 design |
| 7 | Per-sensor calibration | Two-position cal + rotation matrix on dashboard |
| 8 | Multi-sensor dashboard UI | Grid + canvas + joint groups + pairwise differential + stress indicator + trend |
| 9 | OTA + TG-signed images | Production-grade update path |

## When the user says…

* **"Add this to the wiring"** → edit `specifications/HARDWARE-WIRING.md`,
  bump version, add change-log entry.
* **"Let's start coding"** → check spec exists for the part being coded;
  if not, write the spec first (PROCESS.md rule 1).
* **"Generate the trust group"** → off-tree, on the signing host; only
  copy `tg_pub.bin` + `tg_cert.bin` into `trust_keys/`. See
  `SECRETS-POLICY.md`.
* **"Add a feature for X"** → ask whether it should land in firmware or
  dashboard; default dashboard for analytics, firmware only for things
  that must run on-device.
* **"Save the conversation"** → append a new file in `conversation/` with
  today's date and a `-NN.md` suffix.

## What NOT to do

* Don't add `path = "../r2-core"` deps. The repo is self-contained.
* Don't write the TG private key into `trust_keys/`. Public material only.
* Don't commit `wifi_config.toml`. Only `.example` is committed.
* Don't relitigate the 14 binding decisions above without explicit
  user re-opening.
* Don't summarise what you just did at the end of every response — the
  user reads the diff. Tight end-of-turn summaries (one or two sentences)
  per the system prompt.

## References embedded in this project

* `docs/esp-dev-kits-en-master-esp32s3.pdf` — DevKitC-1 pin tables (p.7)
  and labelled photo (p.8).
* `docs/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf` — pin definitions
  (p.10–11), boot configurations (p.13).
* `docs/ADXL355.md` — link to EVAL-ADXL355-PMDZ wiki.
* Reality2 protocol stack patterns: see `r2-core/` outside this repo (for
  reference only — vendor selectively when needed).

---

*Last touched 2026-05-07, end of session 02.*
