---
title: r2-rocker — conformance audit against r2-specifications
date: 2026-05-07
auditor: Claude Opus 4.7 (1M context), commissioned by roy.c.davies@ieee.org
session: design session 02
status: closed; recommendations folded into PLAN.md
scope: |
  Cross-validate r2-rocker's specs and implementation against the
  canonical specifications at /mnt/data/Development/R2/r2-specifications.
  Two audit passes: (1) wire-level protocols (R2-FNV / R2-CBOR / R2-WIRE
  / R2-TRUST signature canonicalisation / vendored-crate integrity),
  (2) architectural-layer conformance (R2-SENTANT, R2-PLUGIN,
  R2-ENSEMBLE, R2-DEF, R2-ENGINE, R2-AUTH, R2-CAP, R2-BLE, R2-BLESCHED,
  R2-BEACON, R2-BOOTSTRAP, R2-WIFI, R2-BUILD, R2-DEPLOY, R2-APIARY,
  R2-COMPILE, R2-CONTEXT, R2-CONSOLE).
---

# r2-rocker conformance audit — 2026-05-07

## Summary

| Layer | Verdict |
|---|---|
| **Wire-level** (FNV / CBOR / R2-WIRE compact frame / TCP framing / Ed25519 sig canonicalisation) | ✅ **Pass** — full byte-level conformance with one acknowledged future-phase gap |
| **Architectural** (sentant / plugin / ensemble model) | ⚠️ **Gap** — firmware and dashboard are monolithic Rust processes; the R2-native model expects sentants composing an ensemble. r2-engine is vendored but only used by r2-wasm. Path forward identified; not blocking Phase 5a. |

Phase 5a (firmware sign + dashboard verify) is **green to ship**. The
architectural refactor is the right Phase 5d work item, with the
canonical pattern coming from r2-notekeeper.

---

## Pass 1 — Wire-level conformance

### 1.1 R2-FNV (event-name hashing)

**Verdict:** ✅ pass.

Firmware's inline `fnv1a_32` in `firmware/esp32-s3/src/wire.rs` uses the
canonical FNV-1a-32 algorithm: offset basis `0x811C_9DC5`, prime
`0x0100_0193`, byte-wise XOR-then-multiply. The unit test
`fnv_known()` in `wire.rs:190` anchors against the known-good vector
`fnv1a_32(b"#ping") == 0x7CB36B0A` (per R2-FNV §3.2).

### 1.2 R2-CBOR (deterministic encoding)

**Verdict:** ✅ pass.

Both encoders (firmware inline, dashboard via vendored `r2-cbor`)
implement the smallest-form rule for integer/length headers (≤23
inline; ≤255 → 0x18; ≤65535 → 0x19; etc.) per R2-CBOR §3.1.
`CborWriter::head` in `firmware/esp32-s3/src/wire.rs:133-148`
implements the dispatch correctly. Maps are emitted in ascending
integer-key order in both encoders.

### 1.3 R2-WIRE (compact frame layout)

**Verdict:** ✅ pass.

Frame layout matches R2-WIRE §4.2.2 exactly:

| Bytes | Field |
|---|---|
| 0 | `(version<<6) \| (msg_type<<3) \| flags` |
| 1 | `(ttl<<4) \| k` |
| 2-3 | `msg_id` (BE u16) |
| 4-7 | `event_hash` (BE u32) |
| 8-11 | `target` (BE u32, broadcast=0 for r2-rocker) |
| 12+ | payload |

Verified against `firmware/esp32-s3/src/wire.rs:encode_event_compact`
and `dashboard/src/main.rs` parser. Flag bits: bit 0 = mcu_origin = 1,
bits 1+2 = 0 (no HMAC, no route) — correct for sensor→dash MCU
events.

### 1.4 TCP transport framing

**Verdict:** ✅ pass.

u16 BE length prefix on TCP per R2-WIRE §1.1.1 + SPEC-R2-ROCKER-WIRE
§1.4. Firmware's `frame_for_tcp` writes the prefix; dashboard's
listener reads it. Bytes 0-1 of each TCP message are the length BE.

### 1.5 Ed25519 signature canonicalisation

**Verdict:** ✅ pass — verified end-to-end on hardware (MAC
`1c:db:d4:41:28:3c`).

Firmware (`firmware/esp32-s3/src/sender.rs:send_announce`) encodes the
canonical 6-key body once for signing, then re-encodes the same body
plus `key 6 = sig` for the on-wire payload. Dashboard
(`dashboard/src/main.rs:verify_announce_signature`) re-encodes the
6-key body in the same ascending integer-key order using the vendored
`r2-cbor` Encoder; verifies the Ed25519 signature against the
announced `device_pk`. Both encoders are RFC 8949 §4.2 deterministic,
so re-encoding produces byte-identical bytes. **`sig=Valid`** observed
in dashboard logs against the live firmware.

### 1.6 Vendored-crate integrity

**Verdict:** ✅ pass.

Spot-checked `crates/r2-fnv/src/lib.rs`, `crates/r2-wire/src/{types.rs,
compact.rs}`, `crates/r2-cbor/src/encode.rs` against r2-core
originals — byte-identical, no drift.

### 1.7 Event-name conventions

**Verdict:** ✅ pass.

Our `r2.sensor.*` / `r2.dash.*` namespace follows the reverse-DNS
class style cited in R2-WIRE §1.2 (e.g. `ai.reality2.device.screen`).
We do not use the `#`-prefix (per R2-FNV §2A that's reserved for
platform-defined events like `#ping`, `#wifi_req`, `#ota_query`) —
correct, since r2-rocker events are agent-defined.

### 1.8 Announce payload structure (the one wire-level gap)

**Verdict:** ⚠️ partial — known future-phase gap.

R2-TRUST §2 specifies that device membership in a TG should be
attested via a **device certificate** (147 bytes:
`version(1) | sig_algo(1) | device_pk(32) | tg_id(32) | role(1) |
issued_at(8) | expires_at(8) | signature(64)`). Our announce
(`SPEC-R2-ROCKER-WIRE.md` §3.1) currently carries `device_pk` + a
raw signature, **not the full cert structure**. This is a
deliberate Phase 5a→5b transition: TOFU now, full R2-TRUST cert
chain post-Phase 5d-enrol.

**Action:** captured in PLAN.md as part of Phase 5b/5d. Phase 5a
ships as-is.

---

## Pass 2 — Architectural-layer conformance

### 2.1 R2-SENTANT (what's a sentant)

R2-SENTANT §1–2 defines a sentant as a **self-contained autonomous
agent** with the IPUCO+D properties: Immutable definition, Persistent
runtime, Unique identity (UUID v4), Consistent visibility, Opaque
internals, Deterministic behaviour. Sentants communicate solely via
events, host one or more finite state machines, and may invoke
plugins.

**r2-rocker today**: no sentants. Firmware and dashboard are
monolithic. Not a wire-level issue (we *name* events correctly), but
the runtime composition layer is missing.

### 2.2 R2-PLUGIN (what's a plugin)

R2-PLUGIN §1 + §2 + §12: a plugin is anything that runs on a hive and
provides capabilities, communicating via the standard envelope
`{plugin, command, status, data/error}`. Use a plugin when a
capability is (a) potentially redeployable, (b) an isolable side
effect, or (c) cross-platform shareable.

**r2-rocker today**: features that *should* be plugins (SD storage,
battery monitor, WiFi connector, accelerometer driver, OTA fetcher)
are inline in the firmware. Acceptable as a Phase 5 prototype; gates
Phase 6+ portability.

### 2.3 R2-ENSEMBLE (composing features)

R2-ENSEMBLE §1–4: an ensemble is a **composite, distributed unit of
user-meaningful functionality** — a swarm of sentants + plugins +
optional UI, performed collectively by one or more hives. Ensembles
are *not installed on a device*; they're performed by the mesh.

**r2-notekeeper as reference** (the proven implementation): its
`ensemble/ensemble.yaml` (v0.5.0, 2026-04-14) declares a single Note
sentant + a Sync plugin + a Web-UI registration into the hive's
shared R2-WEB singleton. Hives consume the YAML and instantiate the
parts locally.

**r2-rocker today**: no `r2-rocker.ensemble.yaml` exists. The system
isn't packaged as an ensemble; firmware and dashboard are
hand-coded standalone processes.

### 2.4 R2-ENGINE (the runtime)

`crates/r2-engine` is vendored and a dependency of `crates/r2-wasm`.
Neither the firmware nor the dashboard use it. If/when we refactor
to sentants, both should run sentants on r2-engine.

### 2.5 R2-DEF (sentant/ensemble YAML schema)

R2-DEF §7 defines the schema used by ensemble YAML files (notekeeper
follows it). r2-rocker would author its `ensemble.yaml` against R2-DEF.

### 2.6 R2-CAP (event capability advertising)

✅ — R2-CAP advertises which events a hive handles via bloom filter +
class hash. Since our event names are FNV-hashed correctly (Pass 1),
once we move to a sentant model the engine will advertise per R2-CAP
automatically. No gap; just contingent on the sentant refactor.

### 2.7 R2-BLE / R2-BEACON / R2-BOOTSTRAP

Phase 6 territory (firmware-side BLE bootstrap retires
`wifi_config.toml`). The dashboard uses the vendored `r2-bootstrap`
library which encapsulates these specs; once the firmware sentant
runs the matching bootstrap state machine, conformance closes.
**Not a Phase 5a gap.**

### 2.8 R2-WIFI

Spec §1.1 prefers UDP for fire-and-forget events; permits TCP for
reliable bulk delivery (OTA). r2-rocker uses TCP for the entire
sensor→dashboard event stream. Not a violation — a deliberate
trade-off given small sensor counts on a private hotspot. If we
migrate to TG-encrypted UDP later, the wire encoding doesn't change.

### 2.9 R2-BUILD / R2-COMPILE / R2-DEPLOY

R2-BUILD specifies the firmware compilation toolchain (`r2-forge`),
including AOT compilation of sentant YAML to native code. r2-rocker
firmware is hand-coded Rust without going through `r2-forge`. This
is acceptable for a v0.5 prototype. R2-DEPLOY (OTA) is captured as
Phase 9 in PLAN.md; nothing to verify until then.

### 2.10 R2-AUTH / R2-CAP / R2-APIARY / R2-CONTEXT / R2-CONSOLE

* **R2-AUTH** (continuous identity confidence, Bayesian decay) — N/A
  for Phase 5a; relevant if Phase 6 anomaly detection is added.
* **R2-CAP** — see 2.6.
* **R2-APIARY** (multi-hive administrative organisation) — future
  multi-site work only.
* **R2-CONTEXT** (vocabulary for context-aware apps) — design tool,
  not a conformance requirement.
* **R2-CONSOLE** (administrative TUI) — N/A; r2-rocker dashboard is
  application-specific.

---

## Recommendations

### High value (architectural)

1. **Author `r2-rocker.ensemble.yaml`** during Phase 5d. Cribbed from
   `r2-notekeeper/ensemble/ensemble.yaml` per R2-DEF §7. Declares
   sensor sentant(s) + dashboard sentants + plugins (SD-storage,
   battery, WiFi, OTA, calibration). This is the canonical
   description that makes r2-rocker an R2 application rather than a
   bespoke system.

2. **Refactor firmware + dashboard to run on `r2-engine`** during
   Phase 5d. Move the sample loop into a sensor sentant; move the
   dashboard's TCP relay + KeyHolder + archive into dashboard
   sentants. Plugins for things that may move (storage backends,
   sensor drivers, WiFi).

3. **Formalise bootstrap + OTA as plugins** (Phase 5b/5d). They
   currently live in `r2-bootstrap` (a vendored library) and
   `r2-build` (not yet used). Promoting them to plugin form makes
   them reusable on future R2 rigs without re-implementation.

### Lower value (clarification only)

4. Cross-reference Pass 1 finding 1.8 (announce uses raw sig, not
   R2-TRUST device cert) into `SPEC-R2-ROCKER-SENSOR.md` §3 so the
   Phase 5a → 5d transition for cert handling is explicit.

5. Add a Phase 6 task to write a beacon-format conformance test
   against R2-BEACON once the firmware emits beacons.

### Things NOT to change

* **Wire-level encoders are correct.** Don't refactor them away just
  because the architectural layer needs work.
* **TCP-vs-UDP choice for sensor stream.** Keep TCP — deliberate
  trade-off, not a violation.
* **`r2-forge` toolchain integration**. Defer; hand-coded Rust is
  fine for a v0.5 prototype with one device class.

---

## Bottom line

* **Phase 5a ships now** — wire-level conformance is solid.
* **Phase 5d gets the architectural refactor** — ensemble.yaml,
  sentants on `r2-engine`, plugins for the side-effects.
* **r2-notekeeper is the working reference** for both the ensemble
  shape and the WASM-hive enrolment flow.
