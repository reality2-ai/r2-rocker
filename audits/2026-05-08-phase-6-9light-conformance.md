---
title: r2-rocker — conformance audit (Phase 5L + 6 + 9-light)
date: 2026-05-08
auditor: Claude Opus 4.7 (1M context), commissioned by roy.c.davies@ieee.org
session: design session 03
status: open; recommendations carried forward to PLAN.md + R2-spec feedback
scope: |
  Cross-validate r2-rocker's protocol-touching work since the
  2026-05-07 audit. Specifically:
  - Phase 5L — `r2.sensor.status` event payload, FSM state encoding.
  - Phase 6 — BLE bootstrap: R2-BEACON advertisement, L2CAP CoC server,
    `#wifi_offer` event, UDP presence packet, persistent RBID.
  - Phase 9-light — TCP OTA push protocol (CMD_START + sha256 stream +
    response shape).
  - SPEC-R2-ROCKER-BRIDGE — first R2 entanglement implementation
    (specified, not yet implemented).
  Wire vectors generated in `testing/wire-vectors.json` (companion
  to this audit). The 2026-05-07 audit's wire-level pass
  (R2-FNV / R2-CBOR / R2-WIRE compact frame / TCP framing / Ed25519
  sig canonicalisation) is re-confirmed; nothing in the new work has
  altered those primitives.
---

# r2-rocker conformance audit — 2026-05-08

## Summary

| Layer | Verdict |
|---|---|
| **R2-FNV** (event-name hashing) | ✅ pass — known vectors match (`#wifi_offer = 0x01F77656`, `#ping = 0x7CB36B0A` etc.) |
| **R2-CBOR** (canonical encoding) | ✅ pass — smallest-form rule + ascending integer-key order across firmware / dashboard / WASM |
| **R2-WIRE compact frame** | ✅ pass — 12-byte header layout, encoder + decoder agreement re-confirmed |
| **R2-WIFI §3.4.2 `#wifi_offer`** | ✅ pass on the wire format; ⚠️ gap on signing (no Ed25519 sig in v0.1; spec requires it) |
| **R2-BEACON §7.3 Legacy 28-byte AD** | ✅ pass on class-hash derivation; 🔍 **field layout deserves a wire-vector cross-check** when r2-esp::beacon's encoder is exercised against an external scanner |
| **R2-BOOTSTRAP — L2CAP CoC framing** | ✅ pass — spec is clear at R2-BLE §5.2 + §6.4. The audit initially flagged ambiguity here; on re-read the spec text is unambiguous (Finding A walked back; the genuine improvement is library ergonomics — see Finding A revised below). |
| **TCP OTA push protocol** | ✅ pass — protocol matches `r2-core/platforms/linux/src/ota_tcp_push.rs` byte-for-byte (project-local, not in canonical r2-specifications) |
| **UDP presence packet** | 📋 **project-local, not in canonical r2-specifications** — recommend lifting into R2-BOOTSTRAP §X (or a new R2-PRESENCE) once the wire format settles |
| **R2-TRUST §7 entanglement** | ⏳ **not implemented yet** — `SPEC-R2-ROCKER-BRIDGE.md` is normative; conformance gate fires when the bridge crate lands (Phase 5d-bridge.4) |
| **Architectural** (sentant ensemble vs AOT) | ✅ unchanged from 2026-05-07 — AOT-compilation reconciliation still applies |

Net read: nothing on the wire is broken. **One finding worth feeding
back into r2-specifications** (the FrameHeader byte question), and a
small set of conformance-test work to do once the WASM viewer's
encoder paths are exercised.

---

## What's new since the 2026-05-07 audit

* **`r2.sensor.status` event** (`firmware/esp32-s3/src/sender.rs::send_status`).
  Payload `{0: state (uint), 1: ts_ms (uint)}`. Hash:
  `0x70BD64A5`. Used to drive the dashboard's virtual-LED state
  in lockstep with the physical RGB LED.
* **R2-BEACON advertisement** with class string
  `nz.ac.auckland.rocker.sensor` (FNV-1a-32 = `0x6A3B0860`).
  Implemented via vendored `r2-esp::beacon` (`crates/r2-esp/src/beacon.rs`),
  driven from `firmware/esp32-s3/src/main.rs`. Persistent RBID (Phase 6
  follow-up) means the dashboard's "wait-for-presence-by-RBID" step
  matches the post-reboot sensor.
* **L2CAP CoC server** on PSM `0x00D2` (R2-BOOTSTRAP convention; matches
  `r2-bootstrap::R2_PSM`). Receives the controller's `#wifi_offer`,
  decoded via `r2-esp::wifi_prov::decode_wifi_offer` (renamed during
  vendoring from upstream's `wifi_config`).
* **UDP presence broadcast** to `255.255.255.255:21044` after WiFi-up,
  carrying `{rbid (8B), ip (text), class_hash (u32), port (u16)}` as
  bare canonical CBOR — no R2-WIRE wrapper. 5-packet burst at 1 s
  intervals.
* **TCP OTA push** on port `21043`. CMD_START preamble: `[cmd(1) +
  size(4 LE) + sha256(32)] + firmware_bytes + write-half-close +
  response: [status(1) + len(2 LE) + msg]`. Project-local protocol
  vendored from `r2-core/platforms/linux/src/ota_tcp_push.rs` /
  `r2-esp/src/ota_tcp.rs`.
* **`SPEC-R2-ROCKER-BRIDGE.md`** — normative draft for the production↔
  viewing TG bridge. First R2 deployment of R2-TRUST §7 entanglement.
  Not yet implemented; spec-only conformance.

---

## Pass 1 — Wire-level conformance (re-confirmed)

The 2026-05-07 audit's pass on R2-FNV / R2-CBOR / R2-WIRE compact
frame / TCP framing / Ed25519 sig canonicalisation stands. No code
in the new work has altered the primitive layers.

Concrete cross-checks now in `testing/wire-vectors.json`:

| Vector | Event | FNV-1a-32 | Notes |
|---|---|---|---|
| 1 | `r2.sensor.acceleration` | `0x94FEF38F` | 5-key map, mixed uint + neg-int |
| 2 | `r2.sensor.battery` | `0xA2751318` | 4-key map, includes `bool` |
| 3 | `r2.sensor.status` | `0x70BD64A5` | 2-key map (Phase 5L addition) |
| 4 | `#wifi_offer` | `0x01F77656` | matches R2-WIFI §3.4.2 known value |
| 5 | UDP presence | (n/a) | bare CBOR, no event-name |
| 6 | R2-BEACON legacy AD | (n/a) | binary; field-layout cross-check pending |

All vectors are byte-for-byte deterministic. Firmware / dashboard /
WASM encoder unit tests can consume them as ground truth.

---

## Pass 2 — Bootstrap-layer conformance

### 2.1 R2-BEACON §7 — Legacy 28-byte AD

**Verdict:** ✅ pass on class-hash derivation; 🔍 wire-vector cross-check
recommended for byte-layout.

`r2-esp::beacon::start` produces an advert containing the class-hash
of `nz.ac.auckland.rocker.sensor` = `0x6A3B0860` per R2-FNV. Verified
end-to-end on bench: dashboard's `bluer`-based scanner sees the
advert and matches the hash, triggers L2CAP connect.

**Cross-check pending**: the exact byte assignment in the 28-byte AD
(R2-BEACON §7.3 Table) hasn't been wire-vector-validated against an
independent decoder. Vector 6 in `wire-vectors.json` proposes a
canonical layout against which `r2-esp::beacon`'s encoder can be
unit-tested once the test harness lands.

### 2.2 R2-WIFI §3.4.2 — `#wifi_offer` event

**Verdict:** ✅ on the wire format; ⚠️ on signing.

* Event hash `0x01F77656` matches the spec's known constant
  (R2-WIFI §3.4.2). Verified via `python3 -c
  'fnv1a_32("#wifi_offer")'` and bench `[PROV] #wifi_offer received via
  BLE L2CAP` log.
* Payload field layout matches R2-WIFI §3.4.2 example: `{0: ssid,
  1: psk, 2: gateway_ip, 3: port, 4: ttl_secs}`.
* **Gap**: R2-WIFI §3.5 + R2-TRUST require the offer to be Ed25519-
  signed by the producing TG's KeyHolder; both `r2-bootstrap::build_wifi_offer`
  AND `r2-esp::wifi_prov::decode_wifi_offer` skip signing/verifying
  in v0.1. This is a pre-existing gap (vendored from r2-core in the
  same un-signed state); tracked as TASK #24 / Phase 5c + 9-secure.
  Not blocking the green path; **MUST be closed before university
  handoff**.

### 2.3 R2-BOOTSTRAP — L2CAP CoC framing

**Verdict:** ✅ pass on the spec; library-ergonomics improvement
identified — see revised Finding A below.

`r2-bootstrap` (controller side) wraps each `#wifi_offer` frame as:

```
[u16 LE length][u8 R2-WIRE FrameHeader byte][R2-WIRE compact frame ...]
```

`r2-esp::l2cap` (sensor side) strips the length prefix but pushes the
*remaining bytes* (FrameHeader byte + frame) up to the application —
which then has to peel off byte 0 before calling `decode_compact`.
We discovered this empirically when our firmware's main loop initially
fed the whole buffer to `decode_compact` and got `event_hash=0x0d01f776`
instead of `0x01F77656` — exactly one byte's misalignment.

The fix in our firmware (`r2_wire::FrameHeader::decode(data[0])` then
`decode_compact(&data[1..])`) is correct *and matches what the spec
already mandates at R2-BLE §6.4*. The audit initially read this as
spec ambiguity — that was a misread. R2-BLE §6.4 explicitly defines
the L2CAP CoC SDU as `Byte 0 = Fragment flag + sequence; Byte 1+ =
R2-WIRE compact event payload`, and §5.2 nails down the byte's bit
layout end-to-end. The spec is unambiguous.

What *did* trip us up was specification topology — R2-BOOTSTRAP says
"use L2CAP CoC for the offer" but doesn't repeat the framing layer's
byte 0 (rightly — that's R2-BLE's domain), so a reader who jumps
straight to the BLE-specific protocol spec without first reading the
lower transport spec misses the framing. That's an audit-process
note, not a spec defect.

### 2.4 UDP presence packet

**Verdict:** 📋 project-local convention, not in canonical r2-specifications.

The post-WiFi-up broadcast that closes the dashboard's bootstrap loop
is defined by `r2-bootstrap::parse_presence_packet`:

```
CBOR map { 0: rbid (bytes 8), 1: ip (text), 2: class_hash (u32), 3: port (u16) }
```

Sent to `255.255.255.255:21044` as bare canonical CBOR (no R2-WIRE
wrapper, no signature, no encryption). v0.1 is fine on a private
hotspot; on a hostile network the unsigned ip-claim is a trivial
spoof.

**Recommendation**: lift this into R2-BOOTSTRAP (or a new R2-PRESENCE
section) once the wire format is stable, OR document it in
`SPEC-R2-ROCKER-WIRE.md` as a project-local extension. Either way,
the format is currently captured *only* in r2-bootstrap source, not in
spec docs.

### 2.5 TCP OTA push protocol

**Verdict:** ✅ pass (as a project-local protocol).

Implementation (`r2-esp::ota_tcp` + `dashboard/src/main.rs::push_firmware`)
matches `r2-core/platforms/linux/src/ota_tcp_push.rs` byte-for-byte:

```
client → server: cmd(1=0x01) + size(4 LE) + sha256(32) + firmware_bytes + write_shutdown
server → client: status(1) + len(2 LE) + utf8_message
```

Bootloader rollback (`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`) +
firmware-side `mark_app_valid` on first frame round-trip catches a
broken image: the new firmware never marks itself valid, the next
reset rolls back. Tested wirelessly end-to-end this session.

**Gap**: image is unsigned in Phase 9-light. TG-signed images (TASK
#24 / Phase 9-secure) is the matching half of Phase 5c.

### 2.6 R2-TRUST §7 entanglement (specification only)

**Verdict:** ⏳ specification authored; implementation pending.

`SPEC-R2-ROCKER-BRIDGE.md` (v0.2, this session) defines:

* Single bilateral entanglement between production TG (sensors +
  controller) and viewing TG (operator devices).
* Outbound + inbound scope tables, role-based admission control, per-
  viewer subscription state.
* Three-layer model: TG-level entanglement (R2-TRUST §7) / sentant-
  level policy (this spec) / mesh-level delivery (R2-ROUTE).
* Five-step incremental rollout (`5d-bridge.1–.5`) so the bridge can
  land over the existing `/ws/status` text-JSON path before the full
  encrypted entanglement is wired (depends on r2-core's
  R2-TRUST §7 implementation maturity).

This is the **first R2 deployment of entanglement**. Conformance vectors
(R2-TRUST §7.5 key derivation, §7.5 envelope ciphertext, §7.6
negotiation) are owed when the bridge crate lands. Three of the §9
audit vectors will likely be lifted into `r2-specifications/testing/`.

---

## Findings

### Finding A (revised) — Library ergonomics: every L2CAP caller hand-rolls the FrameHeader prepend

**Severity:** library-side improvement; **not** a spec gap.

**Earlier framing of this finding was wrong.** R2-BLE §5.2 + §6.4
already define the L2CAP CoC SDU layout unambiguously:
`Byte 0 = FrameHeader (Fragment flag + sequence); Byte 1+ = compact
event payload`, with §5.2 specifying the bit layout. r2-bootstrap's
prepend pattern is exactly what the spec mandates. A clean-room
implementer who reads R2-BLE first will get this right; an implementer
who jumps from R2-BOOTSTRAP straight to writing transport code (which
is what we did during Phase 6) can miss the cross-reference and
end up with a one-byte misalignment in their decoder. That misread
is on the reader, not the spec.

**The genuine improvement** is on the library side. Three call sites in
r2-core hand-roll the same prepend pattern:

```
r2-core/tools/r2-bootstrap/src/lib.rs:558
    let mut l2cap_payload = vec![r2_wire::FrameHeader::Complete.encode()];
    l2cap_payload.extend_from_slice(&wifi_offer_frame);

r2-core/tools/ota-server/backend/src/ble.rs:230, 362
    stream.write_all(&[0x00]).await?;            // FrameHeader::Complete
    // (with a comment: "r2-demo expects [header_byte, wire_data...]")

r2-core/platforms/linux/src/main.rs:562
    let mut frame = vec![wire::FrameHeader::Complete.encode()];
    frame.extend_from_slice(&payload);
```

Each call site re-derives the same wrapper independently, with comments
saying "the receiver expects this." There's no shared helper inside
`r2-wire` itself.

**Recommendation**: add a small pair of helpers to `r2_wire::compact`
(or a new `r2_wire::framing`) that encode/decode the L2CAP-CoC SDU
form:

```rust
pub fn encode_compact_for_l2cap(msg: &CompactMessage<'_>, buf: &mut [u8]) -> Result<usize, WireError> {
    if buf.is_empty() { return Err(WireError::BufferTooSmall); }
    buf[0] = FrameHeader::Complete.encode();
    let n = encode_compact(msg, &mut buf[1..])?;
    Ok(1 + n)
}

pub fn decode_compact_from_l2cap_sdu(sdu: &[u8]) -> Result<(FrameHeader, CompactMessage<'_>), WireError> {
    if sdu.is_empty() { return Err(WireError::EmptySdu); }
    let header = FrameHeader::decode(sdu[0]);
    let msg = decode_compact(&sdu[1..])?;
    Ok((header, msg))
}
```

The four current call sites + r2-rocker's firmware then funnel through
one library entry point. New implementations of L2CAP CoC transport
(future browser hives, MicroPython sensors, whatever) reach for the
helper instead of re-deriving R2-BLE §6.4. **No spec change needed**;
the spec already says what the bytes are. The improvement is keeping
the implementations from diverging when the spec gets re-edited later.

**Tracked separately as code-side r2-wire work**, not a r2-rocker
deliverable.

### Finding B — UDP presence is project-local

**Severity:** documentation gap; not a bug.

The UDP presence packet that closes the bootstrap loop is currently
defined only in r2-bootstrap source. A future R2 deployment that wants
to use the same bootstrap loop will need to either re-vendor
r2-bootstrap or re-derive the format. **Recommendation**: document
in `SPEC-R2-ROCKER-WIRE.md` §X (project-local extension) AND propose
upstream as `R2-PRESENCE` or an addition to R2-BOOTSTRAP.

### Finding C — Unsigned offer / unsigned OTA image

**Severity:** known gap; tracked.

R2-WIFI §3.5 + the calm-tech-security memory both require Ed25519
signing on the offer; R2-TRUST + future R2-OTA spec require signing
the OTA image header. Neither is in v0.1. Pre-existing condition
from before this session; tracked as TASK #24 (Phase 5c + 9-secure).
**Hard requirement before university handoff.**

### Finding D — `r2.sensor.status` schema is project-local

**Severity:** minor documentation gap.

The status event payload schema (`{0: state, 1: ts_ms}` with state
∈ {0..9} mapping to LedState enum values) is implicit in
`firmware/esp32-s3/src/sender.rs::send_status` and the WASM viewer's
`SCHEMA[r2.sensor.status]` map, but isn't in any spec doc.
**Recommendation**: add a short table to `SPEC-R2-ROCKER-SENSOR.md`
§4 listing the state→u8 encoding so the wire schema is operator-
auditable from spec alone.

### Finding E — Bridge spec is normative-only until Phase 5d-bridge.4

**Severity:** expected; tracked.

`SPEC-R2-ROCKER-BRIDGE.md` is authoritative on what the bridge will
do, but the bridge crate doesn't exist yet. Until then, the project
has a legitimate spec/implementation gap on the entanglement layer.
The §10.2 incremental-rollout plan is the agreed path. **Phase Z
re-fires** when 5d-bridge.4 lands — wire vectors against R2-TRUST
§7.5/§7.6 are owed at that point.

---

## Recommendations

1. **Add `r2_wire::compact::encode_compact_for_l2cap` /
   `decode_compact_from_l2cap_sdu` helpers** (see revised Finding A).
   Library-side change in r2-wire, not a spec change. Centralises
   the four current hand-rolled call sites + future ones.
2. **Document the UDP presence format** locally
   (`SPEC-R2-ROCKER-WIRE.md`) AND propose upstream.
3. **Document the `r2.sensor.status` state→u8 mapping** in
   `SPEC-R2-ROCKER-SENSOR.md` §4.
4. **Land Phase 5c + 9-secure** before university handoff. Both
   pieces (sign the offer, sign the OTA image) reuse the same
   Ed25519 + canonical CBOR primitive.
5. **Wire-vector unit tests in three places**: firmware encoder,
   dashboard encoder, WASM encoder. Consume `testing/wire-vectors.json`
   as ground truth; CI fails on mismatch. Currently the vectors exist
   on disk but no test harness consumes them — that's the next
   conformance work.
6. **Phase Z re-fires** when 5d-bridge.4 lands. Add R2-TRUST §7.5 +
   §7.6 conformance vectors at that point; consider lifting them into
   `r2-specifications/testing/`.

---

## Test vectors

`testing/wire-vectors.json` — generated 2026-05-08, six vectors
covering acceleration / battery / status / `#wifi_offer` / UDP
presence / R2-BEACON legacy AD, plus the canonical FNV-1a-32 hashes
used across the project.

The vectors are deterministic (no random nonces / timestamps in the
"input" side) so they can be regenerated and diffed for CI.

---

## What this audit doesn't cover

* **R2-TRUST §7.5 envelope** (XChaCha20-Poly1305 + HMAC + peering
  keys). Specified in `SPEC-R2-ROCKER-BRIDGE.md` §3.5; will be
  conformance-tested when 5d-bridge.4 implements it.
* **HMAC envelope per R2-WIRE frame** (Phase 5c). Pre-existing gap.
* **R2-PROVISION § enrolment-cert format**. Phase 5d step 5 owes a
  spec; the bridge spec depends on it.
* **R2-ROUTE observed-path routing** under partition. Not exercised
  by the rocker's onsite-only deployment yet; relevant for Stage 2
  / Phase 5e remote-relay.

---

## Verdict for ship-readiness

The bench-validated wireless OTA path + the Phase 6 BLE bootstrap +
the Phase 5L LED feedback are all conformant on the wire. **Nothing
in this audit blocks continuing to forward chunks**. The two
conformance gates that DO block university handoff —

* Phase 5c + 9-secure (Finding C)
* Bridge implementation conformance (Finding E)

— are tracked and not regressing.

— end audit —
