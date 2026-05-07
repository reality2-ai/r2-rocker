---
title: r2-rocker design session 02 — code-driving specs through end-to-end demo
date: 2026-05-07
participants:
  - user: roy.c.davies@ieee.org
  - ai: Claude (Anthropic), Opus 4.7 (1M context)
session: 02
status: archived
purpose: |
  Primary research record. Session 02 took r2-rocker from "scaffolding +
  hardware spec done" (end of session 01) to "end-to-end demo running
  on real ESP32-S3 hardware with TG-signed announce." Material for the
  paper / report.
---

# Design session 02 — 2026-05-07

> **Reading note.** User messages are verbatim where short. AI
> responses are rendered with full technical content (decisions,
> tables, code snippets, file:line refs intact) but prose density is
> often reduced. A `[summary]` marker indicates a passage condensed
> for length.

## Context at start of session

* Session 01 closed end of 2026-05-06 with:
  * `HARDWARE-WIRING.md` v0.1 — Phase 1/2/3 wiring, RGB LED state map.
  * `SECRETS-POLICY.md` — TG private key never in repo.
  * `PROCESS.md`, `README.md`, `.gitignore`, `AI-CONTEXT.md`.
  * Hardware not yet soldered; user planning to.
* Open: write 4 code-driving specs, build the TG keygen tool, then
  start firmware.

The session ran from morning through evening (NZ time), spanning:
spec writing → tool building → firmware bring-up → dashboard adapt →
end-to-end demo → real Ed25519 announce signing.

---

## Block A — Code-driving specs (Phase 0e/0f/0g/0h)

**User:** "OK, I'm at work now…" (resumed session 01); "carry on"; later
"plan + first spec"; later "yes, keep going."

**AI delivered (in sequence):**

* `plan/PLAN.md` v0.2 — phasing table, 22 binding decisions
  carried over from session 01.
* `specifications/SPEC-R2-ROCKER-WIRE.md` v0.1 — 16 events:
  `r2.sensor.{announce, acceleration, acceleration.batch, battery,
  status, cal.sample.resp, sync_pong, event.log}` (sensor → dash) and
  `r2.dash.{ack, cal.sample.req, stream.start, stream.stop,
  sync_pulse, config.set, fw.update, reset}` (dash → sensor). CBOR
  schemas, frame envelope (R2-WIRE compact), sequencing & ACK
  semantics, calibration protocol, time-sync algorithm, versioning
  rules, conformance vectors.
* `specifications/SPEC-R2-ROCKER-SENSOR.md` v0.1 — boot/self-test, FSM
  (Idle → Advertising → BleListening → WifiConnecting → Streaming
  ↔ Catchup → Calibrating → LowBattery overlay → OTA → Error), sample
  acquisition, SD store-and-forward (segment files, ACK persistence,
  reconnect replay), battery monitoring (LiPo curve), calibration,
  time sync, NVS config, OTA, errors, conformance.
* `specifications/SPEC-R2-ROCKER-DASHBOARD.md` v0.1 — process model
  (single-process Tokio, peer/listener/WS/bootstrap/analytics tasks),
  state persistence, listeners, bootstrap engine, per-peer state
  machine, time sync, server-side calibration math, joint groups +
  differential analysis, stress indicator + trend, browser UI
  requirements, OTA push, scaling.
* `specifications/SPEC-R2-ROCKER-SYSTEM.md` v0.1 — top-level
  architecture, components, network topology, trust model, lifecycle
  (provisioning, cold start, calibration, steady state, OTA, key
  rotation, decommissioning), data flow, failure modes, deployment.

> **Total spec mass: ~3,100 lines of markdown across 4 files.**

A few sub-decisions emerged from drafting (acceleration on the wire is
raw LSB, not pre-scaled g; battery sampled at 30 s in `Streaming` /
5 min in `Idle`; optional `temp_c` from ADXL355's on-die sensor;
`factory: true` on reset clears NVS; `spec_ver` field reserved for
future major-version mode-switching).

---

## Block B — TG keygen tool (Phase 0i)

**User:** "OK rock on with the TG tool"

**AI delivered** `tools/r2-rocker-tg/` — three subcommands (keygen,
verify, inspect). Generates raw 32-byte Ed25519 priv + pub + a CBOR
self-signed cert (`{0:name, 1:created_at, 2:pub_key, 3:sig}` with sig
over canonical CBOR of keys 0..2). Writes priv with mode 0600, warns
if path is inside the repo working tree, refuses to clobber existing
files unless `--force`. Round-trip tested. Smoke test passed:

```
ssid: r2-rocker-e0a21d
password: aKHdSRyHZzE3Tc5IpVeC
gateway_ip: 10.42.0.1
```

(That was the test cert keypair generated for setup-hotspot.sh; the
real `rocker-rig-uoa-2026` TG was generated later in Block H.)

---

## Block C — User adds new context mid-stream

Several insertions throughout the session:

* "do also note the pdf files and links in this folder" — esp32-s3
  datasheets & ADXL355 wiki integrated into the wiring doc with
  page citations.
* "we had also create a demo 'rocker project' before as well, though
  with different hardware" — found the **rocker-rig** demo at
  `r2-core/demos/rocker-rig/` (Unihiker M10 nodes, Tuxedo dashboard,
  the original calm-tech reference).
* "Also to add - there will be a SD card hardware to add, and we
  want to be able to sense the level of the battery and send that via
  wifi too" — extended the wiring spec.
* "at the end, the sensor has to work as a proper R2 device. When the
  system is first started, each device is a beacon. The dashboard
  device finds them and connects." — this fixed the protocol north
  star: BLE beacon → bootstrap → wifi_offer → TCP stream.
* "yes, the sdcard is the buffer" — elevated SD from "offline backup"
  to "primary durable log; TCP is near-real-time tap."
* "looking at the hardware-wiring - one thing that is not correct -
  we are not charging the battery from the board, we are removing the
  battery and charging in a charging unit" — `HARDWARE-WIRING.md`
  bumped to v0.2; off-board charging via JST-PH disconnect; buck-boost
  regulator added to BoM.

Each of these inserts cascaded into the relevant specs (with version
bumps + changelog entries).

---

## Block D — ADICUP360 vendor

**User:** "re the repo inside the docs folder - perhaps we can strip
out the .git stuff and keep just the current files, but could be useful
to have for reference with a note to say where it comes from"

**AI:** Stripped `docs/EVAL-ADICUP360/.git`, added `PROVENANCE.md` with
the upstream URL (`github.com/analogdevicesinc/EVAL-ADICUP360`) and a
note that the vendored snapshot exists for reference only — register-
level ADXL355 driver examples informing our own Rust driver. 12 MB,
1084 files (down from 31 MB with git history).

---

## Block E — Pre-soldering firmware (Phase 0j)

**User:** "perhaps we could load to the board to test, even without
the soldering?"

**AI:** Wrote `firmware/esp32-s3/` — minimal ESP-IDF + esp-idf-svc
firmware that boots, prints MAC + FSM-state heartbeat over UART. Build
config:

* `Cargo.toml` — esp-idf-svc 0.51, embuild 0.32, anyhow + log.
* `build.rs` — `embuild::espidf::sysenv::output()`.
* `.cargo/config.toml` — `target = xtensa-esp32s3-espidf`,
  `runner = espflash flash --monitor`, `linker = ldproxy`.
* `rust-toolchain.toml` — `channel = "esp"`.
* `sdkconfig.defaults` — N8R8 (8 MB flash + 8 MB octal PSRAM),
  `CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y` so console output goes to
  the native USB-OTG port.

First-build gotcha: `embuild` needs `features = ["espidf"]` enabled
(default features don't include it). After fixing, the first build
took ~15 minutes (full ESP-IDF C SDK compile from source); subsequent
builds were ~30 s.

Flash dance: the chip reported "Error while connecting to device" via
the native USB-OTG port (`/dev/ttyACM0`); resolved by holding BOOT +
pressing RESET to enter download mode. After the initial flash, plugging
in the **other** USB port (the USB-UART CP2102 bridge at
`/dev/ttyUSB0`) gave us auto-reset via DTR/RTS — no more BOOT/RESET
button dance. UART output then visible via `cat /dev/ttyACM0` (the
console route is USB-Serial-JTAG; the UART port is for flash).

End-to-end Phase 0j proven: chip MAC `1c:db:d4:41:28:3c` printing FSM
state names at 1 Hz over the native USB CDC.

**User observation: "which is why we need OTA udpates...** " (in
response to the BOOT/RESET requirement). Reinforced the calm-tech
trajectory.

---

## Block F — Two-OTA-slot partition table (Phase 0k)

**User:** "is the OTA plugin included? ie, we should be using the
two-memory slot process"

**AI initial attempt:** added `[package.metadata.esp-idf-sys]
esp_idf_sdkconfig_defaults = ["sdkconfig.defaults", "sdkconfig.partitions.generated"]`
+ build.rs to write the generated file with absolute path. Partition
table on chip stayed default.

Diagnosis dive: the metadata key was correct, but
`list_specific_sdkconfigs` in esp-idf-sys silently drops files that
don't exist when the build runs — and esp-idf-sys's build.rs runs
*before* our crate's build.rs (regular cargo dep ordering). So the
generated file isn't there when esp-idf-sys looks for it.

**User:** "if you check the specs and logs, you will find that is a
problem solved before."

**AI looked at `r2-core/platforms/esp32-s3/build.rs:1-95`** and found
the canonical workaround: ESP-IDF's CMake resolves
`CONFIG_PARTITION_TABLE_CUSTOM_FILENAME` relative to esp-idf-sys's
auto-generated build dir, so `partitions.csv` is physically copied
there each build cycle. r2-core also has a build script
(`build-and-flash-dfr1195-peripheral.sh`) that pre-stages the file
because the FIRST build still misses it.

**Adopted that pattern.** Wrote `tools/setup-firmware.sh` which copies
`partitions.csv` into `target/.../esp-idf-sys-*/out/partitions.csv`
for the fresh-clone case. Removed the metadata-based hack. Verified
on chip:

```
nvs,data,nvs,0x9000,24K
otadata,data,ota,0xf000,8K
phy_init,data,phy,0x11000,4K
ota_0,app,ota_0,0x20000,3M
ota_1,app,ota_1,0x320000,3M
storage,data,fat,0x620000,1920K
```

`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` in sdkconfig.

(Later in the session, this bit us again: a rebuild after schema
changes regenerated the esp-idf-sys OUT dir, which doesn't have
partitions.csv until our build.rs runs — meaning the rebuilt firmware
went back to the default 1-app layout. Re-running setup-firmware.sh
+ rebuilding fixed it. Documented in AI-CONTEXT.md "lessons learned"
so it doesn't bite a third time.)

---

## Block G — Simulated-data sender (Phase 0L)

**User:** "kick off the build in the background" / "go ahead and
flash" / "Use the USB connected wifi for the hotspot"

**AI** wrote four new firmware modules:

* `src/wire.rs` — inline FNV-1a (`fnv1a_32`), R2-WIRE compact frame
  encoder (`encode_event_compact`, `frame_for_tcp` with u16-BE length
  prefix per the dashboard's TCP framing), minimal CBOR encoder
  (`CborWriter::new/map/key/u/i/bool/bytes/text/as_bytes`). Inline
  rather than via vendored r2-fnv/r2-wire/r2-cbor — minimal
  dependency set on the firmware side; refactor to vendored crates
  later if useful.
* `src/sim.rs` — synthetic accelerometer (sine on x at 0.5 Hz, lateral
  cosine on y, 1 g on z) and synthetic LiPo battery (linear discharge
  curve). Numbers shaped like real ADXL355 raw LSB (256 000 LSB/g at
  ±2 g) so dashboard's scaling matches the eventual real sensor.
* `src/wifi.rs` — `BlockingWifi::wrap` + `connect()` with creds from
  build-time env vars (`R2_WIFI_SSID`, `R2_WIFI_PASS`, `R2_GATEWAY_IP`).
  Adapted from `r2-core/crates/r2-esp/src/wifi_sta.rs`.
* `src/sender.rs` — TCP client + per-session loop: announce →
  acceleration @ 100 Hz → battery every 30 s. Exponential backoff
  reconnect (1, 2, 4, 8, 16, 30 s).
* `build.rs` extended with `load_wifi_config()` reading
  `wifi_config.toml`, plus version stamping (Block I).
* `wifi_config.toml.example` committed; `wifi_config.toml` gitignored.

Two compile bugs found via background build:

1. `embuild = "0.32"` had no `espidf` feature — fixed `features = ["espidf"]`.
2. `CborWriter` had two methods named `bytes()` (one returned the
   buffer, one wrote a CBOR bytes value). Renamed the buffer-view to
   `as_bytes()`.

After fixes, firmware built clean. Flashed. ESP32 connects to the
hotspot, sends frames at 10.42.0.103.

---

## Block H — Vendoring r2-core into the workspace (Phase 0m)

**User:** "skip straight to dashboard" / "look at what was done before"
/ "but also look at what was done before in r2-notekeeper, and previous
work for the rocker project. Even Anthill."

**AI delegated a survey to the Explore agent** — found
`r2-core/tools/r2-dashboard/src/{main.rs, dashboard.html}` is the
canonical reference (Axum + WebSocket + Chart.js, ~700 lines Rust +
HTML). Verdict: **port wholesale, vendor r2-bootstrap as-is, write
the persistence layer fresh.**

Vendored:

* `crates/r2-fnv/`, `crates/r2-cbor/`, `crates/r2-wire/`,
  `crates/r2-core/` — protocol stack.
* `crates/r2-bootstrap/` (formerly `tools/r2-bootstrap/` in r2-core).
* `dashboard/` (formerly `tools/r2-dashboard/`).

Created Cargo workspace at repo root with `firmware/esp32-s3` excluded
(different toolchain). Edition / license set in `[workspace.package]`.
Path deps inside crates rewritten (`../../r2-core` → `../r2-core`,
etc.). After path-fix, `cargo check --workspace` clean.

**Schema adaptation in `dashboard/src/main.rs`:**

* Event hash constants renamed: `acceleration` →
  `r2.sensor.acceleration`, `battery_status` → `r2.sensor.battery`,
  etc., per `SPEC-R2-ROCKER-WIRE.md`. Legacy M10 names kept as
  fallback for mixed deployments.
* `remap_payload(event_hash, raw)` translates integer-keyed CBOR maps
  ({0:seq, 1:ts_ms, …}) into named-key JSON ({seq, ts_ms, …}) so the
  browser side stays clean of integer keys.
* For acceleration specifically, raw LSB values are scaled to g
  server-side (256 000 LSB/g at ±2 g) so the existing browser chart
  code doesn't need to know about ADXL355 ranges.

UI HTML: switched the `case 'acceleration'` matcher to also accept
`'r2.sensor.acceleration'` (legacy M10 name retained for backward
compat).

---

## Block I — Versioning convention (Phase 0n)

**User:** "we should be setting up the firmware and dashboard with
version numbers too - so they can be checked and used for deciding on
updates."

**AI:** Both firmware/build.rs and dashboard/build.rs now stamp
`R2_GIT_SHA` (with `-dirty` suffix when working tree has uncommitted
changes) + `R2_BUILD_TIMESTAMP` as compile-time env vars. Firmware's
announce reports `fw_ver = "<semver>+<sha>[-dirty] sim"`. Dashboard
prints version banner on startup and exposes `/api/version`:

```json
{
  "version": "0.1.0",
  "git_sha": "31b94395-dirty",
  "built_at": "2026-05-07T00:33:06Z",
  "component": "r2-rocker-dashboard"
}
```

This drives the OTA decision logic later: dashboard compares running
fw_ver against the latest known image to decide whether to push.

---

## Block J — Setup helpers (Phase 0o) + first end-to-end test

**User:** "OK. Set up some some semi-random creds for now to do 1 that
we can drop in to test." / later "you can run them..." / "the laptop
has two wifi devcies"

**AI:** Wrote `tools/setup-hotspot.sh` — uses NetworkManager `nmcli`
to bring up an AP profile (`r2-rocker-ap`) on the configured interface
(default `wlx0c0e766e358c`, the USB WiFi adapter — leaves the internal
PCIe radio free for the operator's normal connectivity). Generates
fresh SSID/PSK with `--rotate` or reuses existing in `wifi_config.toml`.

Successful test: hotspot up at 10.42.0.1, ESP32 connects, dashboard
listens on :21042, frames flow.

---

## Block K — Schema integration debugging

The dashboard initially logged every received frame as
`unknown from 10.42.0.103:57764: None` despite the firmware sending
proper `r2.sensor.acceleration` frames. Three bugs in the vendored
M10 dashboard's frame parser:

1. **Off-by-one frame parser.** The M10 demo had used a custom 7-byte
   header (`msg_type | msg_id | event_hash | payload`); our firmware
   uses the canonical R2-WIRE compact 12-byte header
   (`byte0 | byte1 | msg_id | event_hash | target | payload`). The
   parser was reading event_hash from bytes 3-6 (M10 layout) instead
   of bytes 4-7 (R2-WIRE compact). Payload was at byte 7 instead of
   byte 12. Fixed both parser sites in `dashboard/src/main.rs`.
2. **`name` vs `hostname`.** The M10 schema used `name` for the
   friendly device label; our spec uses `hostname` (WIRE §3.1 key 1).
   Updated the announce-handling path to try both for forward compat.
3. **Announce payload dropped before reaching browser.** The
   `DashboardEvent` for `sensor.connected` had `payload: None` —
   meaning fw_ver, device_pk, boot_ts_ms didn't propagate. Updated to
   pass the remapped announce payload through.
4. **Bonus**: `remap_payload` was leaking the original integer keys
   alongside the renamed ones (preserve-unmapped-for-forward-compat
   loop didn't track which keys it had consumed). Fixed with a
   `consumed: HashSet<&str>`.

After these fixes, full pipeline alive end-to-end:

```
sensor.announce  name="rocker-1cdbd441283c"
                 fw_ver="0.1.0+ee7b48e5 sim"
                 device_pk=4ec7e808…947c (random per-boot pseudo-pk)
                 sig=64 zero bytes (Phase 5 placeholder)

acceleration     seq=621, ts_ms=376145, x=0.177 g, y=0.045 g, z=1.000 g

battery          voltage_mv=4129, percent=92%, charging=false
```

---

## Block L — Live decimation (Phase 0m completion)

**User:** "I do notice some glitches in the sine wave - presumably
because we are not using a buffer perhaps?"

**AI:** Per `SPEC-R2-ROCKER-DASHBOARD` §5.2 the dashboard should
decimate live acceleration to ≤ 10 Hz on the wire to the browser; the
full 100 Hz lands in the SD ring (when Phase 3 implements it). The
vendored M10 dashboard didn't decimate — pushed every sample. At 100
Hz the broadcast channel filled and Chart.js dropped frames.

Added per-peer counter in the per-connection task; emit 1 in
`ACCEL_DECIMATION = 10` for live; gated the console log under the
same check. Verified: `seq` jumps by exactly 10 in adjacent log
lines (3061, 3071, 3081, …); browser charts smooth.

---

## Block M — Multi-dashboard / relay architectural direction

**User:** "Let's get Trust Groups setup next. One of the things we
want to be able to do is not only connect with the onsite main
computer, but also via a remote laptop, say, via the relay. We might
even set up a data storage in the cloud later."

**AI surveyed `r2-relay` README + `r2-trust` SPEC.** Found the relay's
architecture matches exactly what the user described:

> "The relay never sees your data. It forwards encrypted messages
> between devices that belong to the same trust group. Think of it as
> a postal service that carries sealed envelopes."

`r2-trust` provides the cryptographic primitives:

* Ed25519 device certificates: `version | sig_algo | device_pk | tg_id |
  role | issued_at | expires_at | signature` (147 bytes).
* HKDF-derived group keys: DEK (data encryption) + HK (HMAC) from TG_SK
  via HKDF-SHA256.
* X25519 peering for cross-TG.

**Architecture sketch** (laid out as a future-vision diagram):

```
                      ┌─ Cloud archive (TG member, eventual)
                      │
            ┌─────────┴──────────┐
            │      r2-relay      │   "postal service of sealed envelopes"
            └──▲─────────────▲───┘
               │             │
       ┌───────┴──┐    ┌─────┴─────────┐
       │  Onsite  │    │   Remote      │
       │ dashboard│    │  dashboard    │
       └─────▲────┘    └───────────────┘
             │
        ┌────┼────┐
        │ Sensors │  on local hotspot
        └─────────┘
```

Phase split:

* **5a** Local TG signing — generate TG, embed pub key, real Ed25519
  announce sig, NVS-persistent device key, dashboard verify. *(This
  session)*
* **5b** Dashboard verifies announce sig — pending (next session).
* **5c** HMAC envelope on every R2-WIRE frame (per WIRE §10.1).
* **5d** Relay forwarding for remote dashboards.
* **5e** Cloud archive consumer.

---

## Block N — Phase 5a: NVS-persistent identity + signed announce

**User:** "carry on" → AI executed Phase 5a:

* Generated TG via `r2-rocker-tg keygen --name "rocker-rig-uoa-2026"`.
  TG pub key `7feb688e61e09174ed47249bcfec0af6059577930f689ac96da35fb44bd2869f`.
  Priv off-tree at `~/.config/r2-rocker/tg_signer/tg_priv.bin`,
  pub + cert committed to `trust_keys/`.
* New module `firmware/esp32-s3/src/identity.rs` — `Identity::load_or_generate(nvs)`
  reads device priv from NVS or mints a fresh keypair via
  `esp_fill_random` and persists. `pub const TG_PUB_KEY: [u8; 32] =
  *include_bytes!("../../../trust_keys/tg_pub.bin")`.
* `sender.rs` reworked: build canonical CBOR body (keys 0..5),
  Ed25519-sign it, then build full payload with sig at key 6. The
  body bytes used for signing are byte-identical to the prefix of the
  on-wire payload (deterministic CBOR), so the dashboard can verify
  by re-encoding the same fields.
* `Cargo.toml`: `ed25519-dalek = "=2.1.1"` (pinned to r2-core's pin
  for cross-component compat).

**Hardware integration speed bumps:**

* The hotspot dropped during the rebuild — NetworkManager auto-
  reconnected the USB adapter back to SDLmetaverse. Ran
  `nmcli connection up r2-rocker-ap` to reactivate.
* On-chip partition table reverted to default (single 8 MB factory
  app) because the rebuild's esp-idf-sys regenerated its OUT dir
  without our `partitions.csv` staged. Re-ran
  `tools/setup-firmware.sh` + rebuilt.
* Ran `espflash flash --partition-table partitions.csv …` to flash
  both app + table.

**End-to-end verified:**

```
boot_ts_ms:  18                                    ← fresh boot
device_pk:   ac9fc00566b66ccd…d7a043c              ← real Ed25519 pub key (NVS-persistent)
fw_ver:      0.1.0+fa4bc681-dirty sim
sig:         9d056be69ad51678…2c511f5564c7d02      ← real Ed25519 signature, 64 bytes, non-zero
```

This is the **calm-tech foundation**: even though Phase 5b (dashboard
verify) is still pending, the firmware is now producing
cryptographically-attestable announces.

---

## Decisions captured this session

| # | Decision | Source block |
|---|---|---|
| D-23 | All v0.1 specs adopt the integer-keyed CBOR convention (keys 0..9 schema-stable, 10..19 optional, 20..29 vendor) | A |
| D-24 | R2-WIRE compact 12-byte header (per spec) is canonical for r2-rocker — not the M10 demo's 7-byte custom header | K |
| D-25 | Acceleration on the wire is raw ADXL355 LSB; dashboard scales to g (server-side) using the announced range | A, K |
| D-26 | Live acceleration to browser decimated to 10 Hz; full 100 Hz lands in SD ring (Phase 3) | L |
| D-27 | Two-OTA-slot partition table baked in from Phase 0.5 onward (3 MB ota_0 + 3 MB ota_1 + 1.875 MB FAT storage) | F |
| D-28 | `tools/setup-firmware.sh` pre-stages `partitions.csv` into the esp-idf-sys OUT dir on fresh checkouts (chicken-and-egg fix) | F |
| D-29 | `tools/setup-hotspot.sh` brings up the NM AP profile on the USB WiFi adapter (`wlx0c0e766e358c`) leaving the internal radio free for the operator | J |
| D-30 | Both firmware and dashboard stamp `R2_GIT_SHA` (with `-dirty` suffix) + `R2_BUILD_TIMESTAMP` at build time; firmware's announce carries `fw_ver = "<semver>+<sha>"`; dashboard exposes `/api/version` | I |
| D-31 | NVS namespace for r2-rocker is `"r2-rocker"`; `device_priv` blob persists the 32-byte Ed25519 seed | N |
| D-32 | TG generated and named `"rocker-rig-uoa-2026"`; pub `7feb688e…2869f` committed to `trust_keys/`; priv kept at `~/.config/r2-rocker/tg_signer/tg_priv.bin` | N |
| D-33 | Cargo workspace at repo root excludes `firmware/esp32-s3` (different toolchain) | H |
| D-34 | `crates/r2-{fnv,cbor,wire,core,bootstrap}/` and `dashboard/` are vendored from r2-core; no path deps to outside the repo (university handoff requirement) | H |
| D-35 | Phase 5 expanded into 5a (announce sign) + 5b (dashboard verify) + 5c (HMAC envelope) + 5d (relay) + 5e (cloud archive) | M |
| D-36 | Battery is removable (JST-PH); charging is off-board in a separate dock; HARDWARE-WIRING.md bumped to v0.2 | C |
| D-37 | RGB LED state machine deserves a distinct OTA-active colour (different from boot's brief white flash) | (interleaved with N) |

## Hard-won technical lessons

* **esp-idf-sys + custom partitions** — chicken-and-egg of build script
  ordering means `setup-firmware.sh` is required for fresh checkouts.
  After any clean rebuild that regenerates the esp-idf-sys OUT dir,
  re-run setup-firmware.sh + rebuild. Documented in AI-CONTEXT.md.
* **NetworkManager AP autoreconnect** — bringing up `r2-rocker-ap`
  works, but if NM later auto-reconnects the same interface to a
  saved network, the AP profile silently drops. Re-run
  `setup-hotspot.sh` to recover.
* **R2-WIRE compact frame is 12-byte header** — not the 7-byte M10
  layout; the dashboard parser had to be fixed at two sites.
* **Friendly device label is `hostname` not `name`** — the
  vendored M10 dashboard tries both for compat.
* **100 Hz to browser is too fast** — broadcast channel fills,
  Chart.js drops frames. Spec-mandated 10 Hz decimation fixed it.
* **NVS encryption deferred** — `CONFIG_NVS_ENCRYPTION` requires eFuse
  setup; v0.1 leaves NVS unencrypted with a TODO. Threat model
  acknowledges this in `SECRETS-POLICY.md`.

## Open questions / pending after this session

* **Q-08** Phase 5b dashboard verification of announce — small chunk,
  next session. Re-encode body bytes from received fields, verify
  Ed25519 sig against announced device_pk.
* **Q-09** RGB LED OTA-active colour choice — current spec says white-
  fast-strobe but that conflicts with boot's brief white flash. User
  requests a distinct colour; suggested rainbow-cycle or magenta-strobe.
* **Q-10** Phase 5c HMAC envelope — every frame, not just announce.
  Requires DEK + HK derivation from TG_SK; only the dashboard host
  has TG_SK, so HK derivation happens there (and on each sensor that
  needs to derive — but sensors don't have TG_SK; they receive HK
  somehow). This subprotocol needs design before implementation.
  Likely: HK delivered to sensors during BLE bootstrap, encrypted
  with the device's announce key. Defer until Phase 6 or later.
* **Q-11** Relay design specifics — does the onsite dashboard
  forward to the relay, or do sensors publish to the relay directly?
  Onsite-dashboard-as-bridge is the simpler model (sensors stay on
  the local hotspot with no internet).
* **Q-12** Cloud archive design — database tech (Postgres? S3?
  TimescaleDB?) and schema. Defer.

## Carried forward to session 03

1. **Phase 5b** — dashboard verifies announce signature.
2. **Task #18** — RGB LED state machine (with distinct OTA colour).
3. **Phase 5c** — HMAC envelope per frame.
4. **Phase 5d** — relay forwarding for remote dashboards.
5. **Phase 5e** — cloud archive consumer.
6. **Phase 6** — firmware BLE bootstrap (the calm-tech endpoint that
   retires `wifi_config.toml`).
7. **Phase 1** — hardware soldering (operator-driven; can happen in
   parallel with software phases since simulated data covers the
   protocol layer).

## Where the user left off

Hardware streaming on the hotspot, dashboard running at
`http://localhost:8080`. End-of-session repo update + push to GitHub
in progress.
