# SPEC-R2-ROCKER-SENTANTS: Sensor-firmware sentant + plugin catalog

**Version:** 0.1 Draft
**Date:** 2026-05-18
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-TIMESYNC, SPEC-R2-ROCKER-SENSOR-HEALTH, SPEC-R2-ROCKER-SENSOR-REMOTE-RESET, SPEC-R2-ROCKER-SENSOR-LIVE-LOGS, canonical R2-HIVE / R2-SENTANT / R2-CAP

---

## 1. Introduction

r2-rocker's sensor firmware today is a monolithic Rust binary
(`firmware/esp32-s3/<carrier>/src/main.rs`) composed by hand from a
handful of modules (`adxl355`, `sender`, `ring`, `sd`, `led`,
`clock`, etc.). This spec re-frames that monolith as an **R2 hive**:
a fixed ensemble of **sentants** (event-driven logic) and
**plugins** (hardware-side capability shims), so the firmware
matches the same architectural vocabulary as the dashboard and
webapp hives — and so individual building blocks can be reused or
swapped (e.g. ADXL355 → BNO055 → strain gauge) without restructuring
the rest of the firmware.

The catalog below is **what the firmware currently is**, named.
v0.2 will introduce a minimal `Sentant` trait + an ensemble
composer; the per-module code itself stays largely as-is.

### 1.1 Scope

In scope:

* The fixed sentant ensemble that ships in every r2-rocker sensor
  firmware build, with the events each sentant produces and
  consumes.
* The plugin set each sentant depends on (hardware shims +
  cross-cutting platform services).
* The minimal `Sentant` trait — surface only, not the full R2-HIVE
  runtime.

Out of scope:

* Dynamic sentant loading. The ensemble is fixed at build time,
  AOT-compiled into the firmware image. Operators reconfigure by
  re-flashing.
* Multi-hive cohabitation on one MCU. Each sensor board hosts
  exactly one hive (`rocker-<mac>`).
* The dashboard- and webapp-side hives. Those are R2-HIVE-conformant
  in their own right and are not catalogued here.

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**,
**SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**,
and **OPTIONAL** in this document are to be interpreted as
described in [RFC 2119](https://www.rfc-editor.org/info/rfc2119),
when they appear in capitals.

### 1.2 Terminology

* **Sentant** — a self-contained piece of event-driven logic. Has
  a name, declares the event hashes it consumes and produces, and
  exposes a small lifecycle: `boot`, `tick` (called from the main
  loop), `on_event`. Sentants do not talk to hardware directly —
  they hold references to plugins.
* **Plugin** — a capability shim that abstracts a hardware
  peripheral or platform service. Has no event surface of its own;
  exposes typed methods to sentants. Examples: `adxl355`, `nvs`,
  `led`, `wifi_sta`. (R2-CAP §3 terminology — closely related to
  a Capability, but rendered in the firmware as a Rust struct
  rather than an opaque token.)
* **Ensemble** — the ordered list of sentants + plugins composed by
  `main()`. The ensemble for r2-rocker is fixed and defined in §3.
* **Event hash** — FNV-1a-32 over the lowercase event name (per
  R2-WIRE / R2-FNV). Sentants subscribe by hash, not by string.

---

## 2. `Sentant` trait (minimal surface)

Every sentant in a conforming firmware **SHALL** implement the
following surface. The runtime **MAY** be implemented as whatever
`main()` builds — this spec does not mandate a separate scheduler
crate.

```rust
pub trait Sentant {
    /// Canonical name, e.g. `"r2.sensor.accelerometer"`. MUST be
    /// stable across builds. Used as the log target and as the
    /// future R2-HIVE introspection key.
    fn name(&self) -> &'static str;

    /// Event hashes this sentant wants to receive. Hashes MUST
    /// be FNV-1a-32 of the lowercase event name per R2-WIRE /
    /// R2-FNV, computed at compile time.
    fn subscribed_events(&self) -> &'static [u32] { &[] }

    /// One-shot boot. The runtime SHALL call this exactly once
    /// after every plugin in the ensemble has been constructed
    /// and SHALL deliver no `on_event` callbacks before `boot`
    /// has returned.
    fn boot(&mut self, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }

    /// Cooperative tick. The runtime SHALL call this at least as
    /// frequently as the sentant's declared cadence (see §3.2);
    /// implementations SHOULD return promptly. A sentant that
    /// needs its own thread (e.g. a long-running TCP listener)
    /// MAY spawn one in `boot` and leave `tick` as a no-op.
    fn tick(&mut self, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }

    /// Inbound event. The runtime SHALL deliver only events whose
    /// hash is in `subscribed_events()`. A sentant MUST NOT
    /// assume any ordering across event types.
    fn on_event(&mut self, _ev: &Event, _ctx: &mut HiveCtx) -> Result<()> { Ok(()) }
}
```

`HiveCtx` **SHALL** expose references to every plugin in §3.1 and a
small event bus the runtime feeds back into `on_event`. A sentant
publishes by calling `ctx.emit(hash, payload)`; the runtime
**SHALL** then deliver the event to other subscribed sentants
in-process and **MAY** additionally forward it onto the wire via
the `uplink` sentant (see §3.2). Sentants **MUST NOT** access
ESP-IDF peripherals directly; they **MUST** go through a plugin.

---

## 3. The r2-rocker sensor ensemble

### 3.1 Plugins (hardware + platform shims)

| Plugin | Owns / wraps | Used by |
|---|---|---|
| `nvs` | ESP-IDF NVS partition. Reads/writes WiFi creds, RBID, clock offset, last-acked seq. | `clock`, `identity`, `wifi-prov`, `recorder` |
| `led` | WS2812 / RGB driver + state machine (`LedState`). | `health`, `uplink`, `wifi-sta`, `beacon`, `ota` |
| `adxl355` | ADXL355 over SPI2, shared bus. | `accelerometer` |
| `sd-card` | Mounted FATFS on `/sdcard`. | `recorder` |
| `battery-adc` | Single-channel ADC + divider for the LiPo cell. | `battery` |
| `wifi-sta` | esp-idf-svc WiFi station + reconnect machinery. | `uplink`, `clock`, all listeners |
| `ble-beacon` | R2-BEACON legacy 28-byte AD advertiser. | `beacon` (sentant of the same name) |
| `ble-l2cap` | L2CAP CoC server on PSM 0x00D2. | `bootstrap` |
| `ota-tcp` | TCP listener on port 21043; receives a firmware image, stages to the inactive OTA partition, restarts. | `ota` |
| `reset-tcp` | TCP listener on port 21044; accepts a single `CMD_RESET` byte. | `reset` |
| `log-tcp` | TCP fan-out on port 21046 of the wrapping logger's records (SPEC-R2-ROCKER-SENSOR-LIVE-LOGS). | every sentant, transparently via `log::info!` etc. |
| `data-tcp` | TCP listener on port 21047; LIST / GET / DEL / DEL_ALL over the captures sub-directory (SPEC-R2-ROCKER-CAPTURE §6). | external — dashboard's `/api/data/...` handlers. |
| `clock` | Monotonic + offset clock. Reads/writes the NVS-persisted `clock_offset_ms`. | `accelerometer`, `uplink`, `recorder`, `health`, `sync` |

### 3.2 Sentants (event-driven logic)

Hashes shown in hex are FNV-1a-32 over the lowercase event name
per R2-WIRE / R2-FNV.

| Sentant | Subscribes to | Emits | Role |
|---|---|---|---|
| `r2.sensor.identity` | (none) | (none — populated into `HiveCtx` at boot) | One-shot. Loads device keypair from NVS (creates one if absent), loads the persistent RBID, exposes both via the context for other sentants. Mirrors R2-HIVE §4 device-identity contract. |
| `r2.sensor.wifi-prov` | (none) | (none — drives the `wifi-sta` plugin) | One-shot at boot. Reads WiFi credentials from NVS / `wifi_config.toml` / env per SPEC-R2-ROCKER-SENSOR §2.1.1, and tells `wifi-sta` to associate. On association failure flips `led` to `Advertising` (blue) and yields to `bootstrap`. |
| `r2.sensor.bootstrap` | (none — listens on the `ble-l2cap` plugin) | (none) | Owns the `#wifi_offer` listener over BLE L2CAP CoC. On a valid signed offer, writes credentials to NVS via the `nvs` plugin and triggers `esp_restart()`. Per R2-BOOTSTRAP §4 + SPEC-R2-ROCKER-SENSOR §2.2. |
| `r2.sensor.beacon` | (none) | (none) | Drives the `ble-beacon` plugin with the rocker class hash + RBID + provisioning flag from `identity`. Always running once `identity` has booted. |
| `r2.sensor.accelerometer` | (none) | `r2.sensor.acceleration` 0x94fef38f at 100 Hz | Reads x/y/z via `adxl355` plugin, stamps with `clock.ts_ms_i64()`, emits onto the bus. Falls back to a built-in simulator if the IC fails to enumerate (per SPEC-R2-ROCKER-SENSOR-HEALTH). |
| `r2.sensor.battery` | (none) | `r2.sensor.battery` 0xa2751318 every 30 s | Polls `battery-adc`, emits voltage / percent / charging flag. |
| `r2.sensor.status` | (none) | `r2.sensor.status` 0x70bd64a5 every 2 s | Emits FSM state + `data_source` + `seq` watermark + uptime. Drives the dashboard's virtual LED. |
| `r2.sensor.sync` | `r2.dash.sync_pulse` 0x80a7… `r2.dash.set_clock_offset` 0xae40… | `r2.sensor.sync_pong` 0xccae4ebb | Implements SPEC-R2-ROCKER-TIMESYNC §2 (Cristian's algorithm). Applies `set_clock_offset` deltas to the `clock` plugin and persists via `nvs`. |
| `r2.sensor.recorder` | `r2.sensor.acceleration`, `r2.dash.ack` 0xab… | (none) | Writes every acceleration record to the SD ring (CSV per SPEC-R2-ROCKER-SENSOR §6.2 v0.2) with periodic fsync; frees segments whose `last_seq ≤ through_seq` on each ack. |
| `r2.sensor.uplink` | every event the dashboard cares about | (none — TCP egress) | Single TCP session to the gateway (port 21042). Sends the announce frame on connect, then forwards subscribed events as R2-WIRE compact frames. On session error, reconnects with exponential backoff; flips `led` between `WifiConnecting` and `StreamingLive`/`StreamingDegradedSim` per session state. |
| `r2.sensor.ota` | (driven by the `ota-tcp` plugin) | (none) | TCP listener that accepts firmware via SPEC-R2-ROCKER-SENSOR §12. Verifies SHA-256, swaps OTA partitions, reboots. Calls `esp_ota_mark_app_valid_cancel_rollback()` after the first frame round-trips via `uplink`. |
| `r2.sensor.reset` | (driven by the `reset-tcp` plugin) | (none) | TCP listener implementing SPEC-R2-ROCKER-SENSOR-REMOTE-RESET. Calls `esp_restart()`. |
| `r2.sensor.health` | `r2.sensor.acceleration` | (none) | Watches for a stuck data source (SPEC-R2-ROCKER-SENSOR-HEALTH §6) and surfaces `data_source = sim` on the next `r2.sensor.status` emission. |
| `r2.sensor.capture` | `r2.sensor.acceleration`, `r2.dash.capture.start`, `r2.dash.capture.mark`, `r2.dash.capture.stop` | `r2.sensor.capture.state` | Owns `CaptureMgr`. Implements the Idle / Calibrating / Recording state machine per SPEC-R2-ROCKER-CAPTURE §2. Writes calibrated CSV rows to `/sdcard/captures/<ts16>-<name>.csv` via the `sd-card` plugin while in Recording. Emits a state event on every transition. |
| `r2.sensor.presence` | (none) | UDP burst | One-shot at boot: 5× UDP packets to `255.255.255.255:21044` carrying the persistent RBID + own IP. Drives the dashboard's RBID-based bootstrap reconciliation. |

### 3.3 Required boot order

The firmware **SHALL** boot the ensemble in the following order.
Steps marked OPTIONAL are conditional on the firmware build.

1. `identity` (**REQUIRED**) — populates the device keypair + RBID
   into the context.
2. Plugins (**REQUIRED**) — `nvs`, `led`, `adxl355`, `sd-card`,
   `battery-adc`, `clock`, `wifi-sta`, `ble-beacon`, `ble-l2cap`,
   `ota-tcp`, `reset-tcp`, `log-tcp` constructed and registered on
   `HiveCtx`. A plugin's construction failure **MUST NOT** be
   fatal if the plugin's spec allows graceful degradation
   (e.g. `sd-card.try_mount` returning `None` per
   SPEC-R2-ROCKER-SENSOR §6).
3. `wifi-prov` (**REQUIRED**) — either `wifi-sta` succeeds or
   control yields to `bootstrap` per
   SPEC-R2-ROCKER-SENSOR §2.1.1.
4. `beacon` (**REQUIRED**) — starts unconditionally once
   `identity` is populated.
5. `presence` (**REQUIRED**) — one UDP burst once `wifi-sta` is
   associated.
6. `clock` (**REQUIRED**) — offset loaded from NVS.
7. All remaining sentants in §3.2 (**REQUIRED**) — `boot()`-ed in
   any order; the runtime **MUST NOT** deliver events between
   them until every `boot()` returns.
8. Main loop (**REQUIRED**) — the runtime **SHALL** call each
   sentant's `tick()` at least as frequently as its declared
   cadence in §3.2.

### 3.4 Implementation note (non-normative)

For v0.2, sentants **SHOULD** be realised as Rust structs in
`firmware/esp32-s3/<carrier>/src/sentants/*.rs` and plugins as
Rust structs in `firmware/esp32-s3/<carrier>/src/plugins/*.rs` (or
the shared `crates/r2-esp/`). The ensemble composer **SHOULD**
live in `main()`. There is no dynamic registry — adding a sentant
is a source-tree edit.

A future v0.3 **MAY** move to a build-time descriptor (YAML / TOML
listing sentants and plugins) compiled to the same Rust ensemble.
That is consistent with the "devise the sentants and plugins, then
compile them to working code" workflow noted in the README.

---

## 4. Conformance

A sensor build **conforms** to this spec when ALL of the following
hold:

1. Every sentant listed in §3.2 **MUST** be present in the
   firmware image.
2. Each sentant's emitted-event hashes **MUST** equal the FNV-1a-32
   of the lowercase event name listed in §3.2.
3. Every plugin in §3.1 **MUST** be reachable from sentants via
   the `HiveCtx` (or equivalent ownership pattern). No sentant
   **SHALL** access an ESP-IDF peripheral directly.
4. The boot order in §3.3 **MUST** be respected.
5. A sentant or plugin **SHOULD** be portable to a non-rocker
   ESP-IDF project by porting the file plus its declared plugin
   dependencies — i.e. no rocker-specific globals.

---

## 5. Versioning

| Date       | Ver | Change                                                 |
|------------|-----|--------------------------------------------------------|
| 2026-05-18 | 0.1 | Initial draft — catalog of the existing firmware modules, framed as sentants + plugins. No code change yet. |
