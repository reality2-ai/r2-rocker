# SPEC-R2-ROCKER-BRIDGE: Production ↔ Viewing Trust-Group Bridge

**Version:** 0.2 Draft
**Date:** 2026-05-08
**Status:** Normative Draft (FIRST R2 ENTANGLEMENT IMPLEMENTATION)
**Depends on:** R2-TRUST §7 (Cross-Trust-Group Peering / Entanglement),
SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-DASHBOARD, SPEC-R2-ROCKER-SENSOR

---

## 1. Introduction

The r2-rocker rig uses two trust groups (per
`memory/project_two_tgs_entangled.md`):

* **Production TG** — sensors + the active controller. Holds the data
  plane.
* **Viewing TG** — operator phones, tablets, laptops, observers,
  alert recipients.

The two TGs are linked by a single **bilateral entanglement**
(R2-TRUST §7.5–§7.6). Per the spec, entanglement is a mutual agreement
**between two trust groups** — not between individual sentants. The
peering keys derive from both TGs' `TG_PK`/`TG_SK` (R2-TRUST §7.5
"Key Exchange and Derivation") and, once established, are TG-level
material available to all members of either TG.

This specification defines the **bridge**: the sentant on the
controller that is the production-side **implementation** of that
entanglement's outbound + inbound policy. The bridge:

* subscribes to a curated subset of production events and emits them
  through the entanglement (§3),
* receives inbound events from the entanglement, validates them
  against viewer cert role, and re-emits them into production (§4),
* maintains per-viewer subscription state, quotas, and emits status /
  error events back to viewers (§5, §8).

The bridge is a member of the **production TG only**, with its own
device cert signed by the production KeyHolder. It is the chokepoint
where rocker-specific policy lives, but it is not the entanglement
itself — that exists at the TG level. Other production sentants
*could* in principle handle entangled events; in v0.1 they don't, and
the bridge is the only sentant subscribed to the relevant event names.

### 1.1 Status — first entanglement implementation in R2

R2-TRUST §7 defines entanglement; this is the first deployment that
implements it. The conformance gate (Phase Z, PLAN row Z) cross-
validates wire vectors against R2-TRUST §7.5 / §7.6 / §7.5.1 / §7.5.2,
not just rocker-specific behaviour. Errors found here may indicate
spec ambiguities to feed back into r2-specifications. Until at least
one independent R2 hive interoperates with the rocker bridge, the spec
should be treated as candidate-final.

### 1.2 Three layers, separately conformant

This deployment exercises three R2 layers simultaneously, each of which
this spec depends on but does not redefine:

| Layer | Concern | Who implements |
|---|---|---|
| **TG-level entanglement** (R2-TRUST §7) | Bilateral agreement between two TGs. Peering keys derived from `TG_PK`/`TG_SK`. Scope, direction, permission bits. | Both KeyHolders cooperate at the §2.3 handshake; once derived, peering keys are TG-level material. |
| **Sentant-level policy** (this spec) | What events flow each way, role-based admission control, per-viewer subscription state, quotas. | The bridge sentant on the production side; designated viewer-side hive(s) on the viewing side. |
| **Mesh-level delivery** (R2-ROUTE / R2-WIRE) | Routing entangled events across intermittent connectivity — sensors that drop, controllers that swap, viewers that join from elsewhere on the internet. | The R2 mesh as deployed (rocker hotspot + relay + Stage 2 project relay). |

Practically, while entanglement is TG-to-TG, the on-the-wire packets
travel between specific hives: on the production side, the bridge is
the practical sender/receiver; on the viewing side, the receiving hive
is whichever viewer-TG sentant is currently subscribed to a given
event class. The mesh layer routes packets to TG-addressed destinations
and tolerates network transience — viewers come and go, the relay may
be unreachable for stretches, the controller may swap mid-session.
This deployment is therefore also a real-world test of R2's
**Transient Networking** properties, not just entanglement.

### 1.3 Dependency: R2 mesh maturity

The bridge can only be as robust as the mesh underneath it. Gaps
discovered while implementing the bridge — observed-path routing
behaviour under partition, keepalive jitter, archive-replay across
relay restart, etc. — may surface as work needed in `r2-core` /
r2-specifications. Such gaps are tracked separately (not in this
spec) but the rocker work plan must accommodate the possibility.

### 1.4 v0.1 scope

* Exactly **two TGs**: rocker production + rocker viewing.
* Cross-organisation entanglement (e.g. with a university's existing
  TG, or a customer's TG) is **out of scope**. The architecture
  permits it, but no interop testing or naming negotiation is
  designed in v0.1.
* Both TGs' KeyHolders may be colocated on the same physical device
  in v0.1 (separate key files). Future versions may separate them.

### 1.5 Notation

* MUST / SHOULD / MAY per RFC 2119.
* Event hashes are `r2_fnv::r2_hash(event_name)` per R2-FNV.
* All CBOR is canonical per R2-CBOR.
* `TG_PK` and `TG_SK` refer to a trust group's public/private keypair
  per R2-TRUST §2.2.

---

## 2. Architecture

### 2.1 Position

The bridge runs **inside the controller process** as a sentant. It is
a member of the **production TG**, with its own Ed25519 device cert
signed by the production KeyHolder (R2-TRUST §4). The bridge is
distinct from the controller's existing dashboard/listener sentants:
they share a process but have separate identities, so the entanglement
binds to the bridge cert specifically and can later move to a
different host.

### 2.2 The single bilateral entanglement

At controller startup, the bridge establishes **one** bilateral
entanglement (R2-TRUST §7.5) with the viewing TG, identified by
`viewing_tg_pk` (the viewing TG's `TG_PK`). The entanglement carries:

* **Key material** — peering HMAC and encryption keys derived per
  R2-TRUST §7.5 from `PS = X25519(production_TG_SK, viewing_TG_PK)`,
  HKDF-SHA256 with the canonical salt + info strings.
* **Scope** — *class-based*, advertised as `nz.ac.auckland.rocker.*`.
  Concretely the bridge subscribes (outbound) and accepts (inbound)
  the events listed in §3 and §4 of this document. Per-viewer and
  per-role filtering is a *sentant-layer concern above the
  entanglement* — see §5.
* **Direction** — `both` (R2-TRUST §7.5.1): outbound for telemetry +
  alerts, inbound for viewer subscribe + operator commands.
* **Permission bits** (R2-TRUST §7.5.2): `EVENT_ROUTE` (bit 0) +
  `DISCOVERY` (bit 1). `MANAGEMENT` (bit 2) is **NOT** granted —
  viewers cannot perform production-TG management operations.
* **Keepalive**: 60 s idle interval, 5 missed → stale, per R2-TRUST
  §7.3 defaults.
* **Crypto level**: matches both TGs' `min_crypto_level`. v0.1 uses
  classical (`sig_algo=0x01`, `kem_algo=0x01` = X25519). Future PQ-
  hybrid (`0x02`) is opt-in per R2-TRUST §7.6 step 2a; both TGs are
  configured identically for v0.1.

There is exactly one such entanglement. Adding more (e.g. a second
viewing TG for a different operator team) is a future extension out of
v0.1 scope.

**Member lifecycle in each TG.** Viewer enrolment into the
**viewing** TG is specified in `SPEC-R2-ROCKER-ACCESS.md` §3-§4
(the QR / link / 3-word-code flow the operator drives from the
Access tab). Sensor enrolment into the **production** TG happens
during BLE bootstrap per `SPEC-R2-ROCKER-SENSOR.md` §4. The
controller process holds the **KeyHolder** role in both TGs (see
ACCESS §2.4) and is the sole authority that may invite a new
member into either; invitations are explicit, KeyHolder-initiated,
and time-limited per ACCESS §3.0. Revocation in either TG is
KeyHolder-only, propagates per ACCESS §7, and works regardless of
whether the revoked device is currently online (ACCESS §7.6) —
this is the routine operator path for retiring a sensor or
removing a viewer that has left the project.

### 2.3 Negotiation handshake

The bridge initiates the entanglement at startup per R2-TRUST §7.6:

1. Bridge sends `r2.entangle.offer` to `viewing_tg_pk` with the scope
   described in §2.2.
2. Viewing TG's entanglement-manager sentant (running on its
   KeyHolder, or a delegated member) receives the offer, verifies the
   `min_crypto_level` requirements, and responds with
   `r2.entangle.accept`.
3. Both sides derive peering keys per R2-TRUST §7.5.
4. Bridge announces ready by emitting `r2.bridge.summary` on the
   newly-established entanglement (§5.3).

Until the accept arrives, the bridge accepts no inbound events from
viewing-TG members and does not forward outbound events. The legacy
`/ws/status` text channel (Phase 5d-bridge.1 — see §10.2) bridges this
gap during incremental rollout.

### 2.4 Failure modes

| Failure | Bridge behaviour |
|---|---|
| Viewing-TG unreachable / stale entanglement | Per R2-TRUST §7.3, route is marked stale at 5 missed keepalives but kept (revivable). Outbound events are dropped (viewers will resync from archive on revival — Phase 5f). Production keeps running. |
| Production TG silent (no sensors) | Bridge has nothing to forward; viewers see "no peers". |
| Controller (and bridge) restart | Entanglement stale on the viewing side until re-negotiation. Bridge re-initiates §2.3 handshake on startup. Per-viewer subscription state is **NOT persisted** in v0.1 — viewers MUST resubscribe. |
| Negotiation rejected (`r2.entangle.reject`) | Bridge logs and retries with backoff (60 s, 120 s, …, capped at 1 h). The legacy `/ws/status` path remains available during incremental rollout. |

---

## 3. Outbound Policy (production → viewing)

The bridge subscribes to the following production events and forwards
them, transformed as noted, into the entanglement. A viewer does not
receive an event unless (a) it is in this table AND (b) the viewer's
active sentant-layer subscription filter (§5) includes it.

### 3.1 Always-forwarded (default subscription)

| Production event | Forwarded as | Transformation | Notes |
|---|---|---|---|
| `r2.sensor.acceleration` (100 Hz) | `r2.sensor.acceleration.live` (10 Hz) | Server-side decimation: every 10th frame per peer | WIRE §4.1. Catch-up arrives only on archive query. |
| `r2.sensor.battery` | (verbatim name + payload) | — | |
| `r2.sensor.status` | (verbatim) | — | Drives virtual LEDs. |
| `r2.sensor.event.log` | (verbatim) | — | |
| `r2.peer.connected` | (verbatim) | `{addr, hostname, fw_ver, ts_ms}` | Synthesised by controller's TCP-accept; not sensor-emitted. |
| `r2.peer.disconnected` | (verbatim) | `{addr, ts_ms, reason}` | Synthesised on TCP close / 5 s read timeout. |
| `r2.alert.*` | (verbatim subtype) | — | Alert events emitted by sentants when conditions trip. |

### 3.2 On-request only

| Production event | Forwarded as | Triggered by |
|---|---|---|
| `r2.sensor.acceleration.batch` (raw 100 Hz, paginated) | `r2.archive.replay.chunk` | Inbound `r2.archive.query` (§4) |
| `r2.sensor.cal.sample.resp` | (verbatim) | Inbound `r2.dash.cmd.calibrate` (§4) |

### 3.3 Never forwarded

| Production event | Why |
|---|---|
| `r2.sensor.announce` | Production-internal trust handshake. Surface the *result* via `r2.peer.connected`. |
| `r2.sensor.sync_pong` | Time-sync internal. |
| `r2.dash.cmd.*` (production-internal commands) | Production-internal command bus. |
| Production-TG management events (member add/remove, KeyHolder transfer, group key rotation, OTA-image signing internals) | Trust boundary. |
| `r2.entangle.*` events on the entanglement itself | Protocol-internal. |

### 3.4 Decimation invariant

`r2.sensor.acceleration.live` MUST be exactly 10 Hz per peer
(±1 frame/sec slack for jitter). The bridge counts source frames per
peer and emits every 10th. If the source rate changes (operator
commands a higher rate), the decimation ratio changes such that
outbound stays ≤10 Hz. This invariant protects viewer browsers'
render budget.

### 3.5 Wire framing

Each outbound event is delivered per R2-TRUST §7.5 "Authenticated
Event Delivery":

* CBOR-encode the payload.
* Encrypt with the peering encryption key (XChaCha20-Poly1305, fresh
  random nonce).
* HMAC over (R2-WIRE frame header || nonce || ciphertext) with the
  peering HMAC key.
* Emit as a normal R2-WIRE event addressed to `viewing_tg_pk`.

---

## 4. Inbound Policy (viewing → production)

The bridge accepts the following events from viewing-TG members,
validates the viewer cert and role, and re-emits the underlying request
into production.

Per R2-TRUST §7.5 "Authenticated Event Delivery", the entanglement
layer first verifies the HMAC and decrypts the envelope. Only after
that succeeds does the bridge perform the §4.x role / payload checks
defined here. The R2-TRUST envelope check is a *transport guarantee*;
the §4 role check is *application authorisation*.

v0.1 defines two viewer roles, encoded in the viewer's enrolment cert
(see forthcoming `SPEC-R2-ROCKER-ENROL.md`): **observer** and
**operator**.

### 4.1 Open to any viewer (observer or operator)

| Viewer event | Production effect | Validation |
|---|---|---|
| `r2.viewer.subscribe` | Set this viewer's outbound filter (§5). | Filter values must be event hashes from §3.1/§3.2. Unknown hashes → ignored, not error. |
| `r2.viewer.viewport_hint` | Bridge prioritises forwarding for the named peers. | `{visible_peers: [str]}`. Advisory; bridge MAY ignore under load. |
| `r2.archive.query` | Trigger archive-replay (Phase 5f). | `{session_id?, sensor?, since_ms, until_ms, limit}`. Bridge enforces a max chunk count per query. |
| `r2.alert.ack` | Mark an alert acknowledged in the alert log. Other viewers see the ack as a fresh `r2.alert.acked` outbound event. | `{alert_id}`. |

### 4.2 Operator-role only

| Viewer event | Production effect | Validation |
|---|---|---|
| `r2.dash.cmd.stream.start` | Re-emit as production `r2.dash.cmd.stream.start`. | `{pk, rate_hz?, range?}`. |
| `r2.dash.cmd.stream.stop` | Re-emit. | `{pk}`. |
| `r2.dash.cmd.calibrate` | Re-emit. | `{pk, position}` per SENSOR §6. |
| `r2.dash.cmd.reset` | Re-emit, but ONLY with `factory: false`. | `factory: true` is REFUSED (out-of-band only). |
| `r2.dash.cmd.bootstrap` | Trigger BLE bootstrap loop. | (no payload) |
| `r2.dash.fw.update` | Trigger OTA: bridge accepts the firmware payload OR a URL the controller fetches; controller's KeyHolder signs the image header (Phase 9-secure / TASK #24); production OTA handler verifies sig and writes partition. | `{pk, image_url? \| image_b64?, sha256, sig?}`. |

### 4.3 Refused by policy

The bridge MUST refuse and emit `r2.bridge.error` (§5.3) for:

| Viewer event | Reason |
|---|---|
| `r2.dash.cmd.reset {factory: true}` | Factory reset is out-of-band only — physical button hold. |
| Any production-TG management event | Trust boundary. |
| Any event whose hash is not in §4.1 or §4.2 | Unknown / unsupported. |
| `r2.dash.cmd.*` from observer-role cert | Insufficient role. |
| Inbound from a cert not signed by viewing-TG KeyHolder | Untrusted cert. |
| Inbound from a cert in the cached revocation list | Revoked cert. |
| Event whose payload fails schema validation | Malformed payload. |

### 4.4 Role enforcement is at the bridge

Other production sentants do NOT need to re-validate the viewer's role.
The bridge has already authorised. This is the single chokepoint
principle — re-checks elsewhere risk drift between the bridge's policy
and downstream sentants' assumptions.

### 4.5 R2-TRUST permission bits vs sentant-layer roles

Both layers exist:

* **R2-TRUST §7.5.2 permission bits** (`EVENT_ROUTE`, `DISCOVERY`,
  `MANAGEMENT`) apply to the entire entanglement and are negotiated
  bilaterally at §2.3 handshake. v0.1 grants `EVENT_ROUTE` +
  `DISCOVERY`; never `MANAGEMENT`. This bounds what *any* viewer can
  cause regardless of their cert role.
* **§4 sentant-layer roles** (observer / operator) further constrain
  *which* events within the granted scope each *individual viewer*
  can cause.

The R2-TRUST layer is the floor; the sentant layer is the policy.

---

## 5. Per-Viewer State (Sentant Layer)

The bridge holds a `Subscriber` record per viewing-TG cert that has
sent at least one event through the entanglement. Records are
in-memory only in v0.1.

### 5.1 Subscriber fields

```
Subscriber {
    cert_pk:        [u8; 32]    // viewer's Ed25519 device public key (cert.device_pk)
    role:           Role        // Observer | Operator (cert.role)
    filter:         BTreeSet<u32>   // FNV-32 of event names this viewer wants (default: §3.1 set)
    viewport_hint:  Vec<String>     // peer addrs this viewer prioritises
    last_seen_ms:   u64
    quota:          QuotaState  // see §5.4
}
```

The viewer's cert is resolved against the viewing-TG cert cache
(populated from R2-TRUST §3.2 member-recognition + §4 device certs).
Cache is refreshed on entanglement re-negotiation and on operator-
triggered "refresh certs" (rare).

### 5.2 Default filter

A viewer with no `r2.viewer.subscribe` issued yet receives the §3.1
"always-forwarded" set with no per-event filter. Viewers wanting less
explicitly subscribe to a narrower set.

### 5.3 Bridge-emitted events

| Event | When | Payload |
|---|---|---|
| `r2.bridge.subscribe.ack` | After accepting a `r2.viewer.subscribe` | `{accepted: [event_hashes], ignored: [event_hashes]}` |
| `r2.bridge.error` | On refused inbound (§4.3) | See §8 |
| `r2.bridge.summary` | Every 30 s while ≥1 subscriber active, plus once on entanglement-ready | `{n_subscribers, n_peers, outbound_rate_eps, entanglement_state: "fresh" \| "stale"}` |

These are bridge-originated events and travel through the entanglement
the same as forwarded ones (encrypted + HMAC'd).

### 5.4 Quotas

To prevent a single viewer from saturating the bridge:

* Each viewer has a token-bucket inbound rate limit: 10 events/s
  burst, 1 event/s sustained. Excess is dropped silently.
* `r2.archive.query`: 1 query / 10 s per viewer, max 1000 frames per
  query.
* Quotas are advisory in v0.1; enforcement counters are exposed via
  `r2.bridge.summary`.

---

## 6. Authentication Layers (Recap)

In order of evaluation on each inbound event:

1. **R2-WIRE frame parse** (transport).
2. **R2-TRUST §7.5 envelope check** — HMAC verification + decryption
   with peering keys. Failure → silently drop (likely replay or
   wrong group); not even logged at info level.
3. **Cert resolution** — viewer cert pk in payload looked up against
   viewing-TG cert cache. Failure → `r2.bridge.error` reason "unknown
   cert".
4. **Cert signature check** — cert chain to viewing-TG KeyHolder.
   Failure → `r2.bridge.error` reason "untrusted cert".
5. **Cert validity** — `expires_ms > now`, not in revocation list.
   Failure → reason "expired" or "revoked".
6. **Sentant-layer policy** — §4 table check against cert role.
   Failure → reason "insufficient role" or "refused by policy".
7. **Schema validation** — payload structure per the §4 row.
   Failure → reason "malformed payload".

Each successful inbound flows through to production (§4 column 2).

---

## 7. Pause / Power

* Zero subscribers AND no archive queries in flight → bridge stops
  decrypting non-essential outbound events. Still listens for
  `r2.peer.*` so the count is accurate when a subscriber returns.
* Outbound rate is intrinsically capped by §3.4 (10 Hz × n_sensors per
  viewer × n_viewers).
* The bridge runs in the controller's tokio runtime; CPU footprint is
  proportional to active subscribers, not sensor data rate.

---

## 8. Error Reporting

`r2.bridge.error` payload (CBOR map):

```
{
  0: event_hash:   u32   // the offending hash; 0 if not extractable
  1: reason:       text  // operator-readable reason
  2: ts_ms:        u64
  3: cert_pk_id?:  u32   // FNV-32 of the offending viewer's cert pk (debug only)
}
```

Emitted as a unicast event to the offending viewer (not broadcast).
The viewer's UI MUST surface it (per
`feedback_calm_tech_security` and `feedback_a11y_indicators`: text +
colour + icon, not silent fail).

---

## 9. Conformance Vectors

The Phase Z conformance audit (PLAN row Z) SHALL include:

### 9.1 R2-TRUST §7 conformance (foundational)

1. **§7.5 key derivation** — given two known TG_SK / TG_PK pairs and
   the canonical salt + info strings, both sides MUST derive the same
   peering HMAC key and peering encryption key (test vector).
2. **§7.5 envelope** — encrypting a known plaintext under known peering
   keys with a known nonce MUST produce the published ciphertext + HMAC.
3. **§7.6 negotiation** — `r2.entangle.offer` + `r2.entangle.accept`
   round-trip with `min_crypto_level` mismatch MUST be rejected with
   `crypto_level_insufficient`.

### 9.2 Bridge policy conformance (rocker-specific)

4. **Outbound decimation** — synthetic 100 Hz acceleration → exactly
   N=10 frames in any 1.05 s window per peer.
5. **Inbound role enforcement** — observer-role cert sending
   `r2.dash.cmd.stream.start` MUST be refused with `r2.bridge.error`
   reason "insufficient role".
6. **Factory reset refused** — `r2.dash.cmd.reset {factory: true}`
   MUST be refused with reason "factory reset is out-of-band".
7. **Subscribe filter ack** — `r2.viewer.subscribe` with mixed known
   + unknown event hashes MUST produce `r2.bridge.subscribe.ack` with
   both `accepted` and `ignored` lists.
8. **Revoked cert** — cert in the revocation list MUST have all inbound
   events refused with reason "cert revoked".
9. **Trust boundary** — inbound event whose hash matches a production-
   TG management op MUST be refused even from operator-role.

These vectors live in `testing/wire-vectors-bridge.json` once the
bridge is implemented. Vectors 9.1.1–9.1.3 may be lifted into a
shared `r2-specifications/testing/` location since they validate the
foundational R2-TRUST §7 spec, not just rocker.

---

## 10. Implementation Notes

### 10.1 v0.1 simplifications

* Both TG KeyHolders colocated on the same physical device, separate
  key files (`trust_keys/production_priv.bin`, `trust_keys/viewing_priv.bin`).
* Subscriptions are in-memory; viewers re-subscribe on reconnect.
* Archive replay (§3.2) is stubbed — `r2.archive.query` returns
  `r2.bridge.error` reason "archive not available" until Phase 5f.
* OTA (`r2.dash.fw.update`) is unsigned in Phase 9-light; signing
  fold-in is Phase 9-secure / TASK #24.
* Cert revocation list is a local file refreshed on entanglement
  re-negotiate.

### 10.2 Incremental rollout

The bridge lands in steps that always leave the rocker working:

| Step | What | Transport |
|---|---|---|
| 5d-bridge.1 | Crate scaffold; port existing `/ws/status` semantics into the bridge's outbound (§3.1 default set). | Legacy `/ws/status` text JSON. |
| 5d-bridge.2 | Viewer-side subscribe + viewport_hint + alert.ack. Bridge-emitted ack/error events. | Still `/ws/status`. |
| 5d-bridge.3 | Operator-role gating on §4.2 inbound. Cert validation against viewing-TG KeyHolder. | Still `/ws/status` BUT now requires viewer cert. |
| 5d-bridge.4 | Real bilateral entanglement (R2-TRUST §7.6) handshake; peering keys; encrypted envelopes. **Replaces /ws/status as canonical transport.** | Entangled R2-WIRE. |
| 5d-bridge.5 | Archive query + chunk reply (depends on Phase 5f). | Entangled. |

Step 5d-bridge.1 is what we can ship next without blocking on cert
infrastructure.

### 10.3 File layout

```
crates/r2-bridge/             # new workspace member
  Cargo.toml
  src/
    lib.rs                    # public Bridge struct + spawn entry point
    handshake.rs              # §2.3 entanglement negotiation (R2-TRUST §7.6)
    envelope.rs               # §3.5 + §6 R2-TRUST §7.5 encrypt/HMAC
    outbound.rs               # §3 policy table + decimation
    inbound.rs                # §4 policy table + role enforcement
    state.rs                  # §5 Subscriber record + quotas
    errors.rs                 # §8 error envelope
dashboard/src/main.rs         # spawns the Bridge alongside existing
                              # tcp listener + http server
```

### 10.4 Hooks the existing dashboard already provides

* `event_tx: broadcast::Sender<DashboardEvent>` — production events
  the bridge subscribes to (acceleration, battery, status, log).
* `peers: RwLock<HashMap<SocketAddr, SensorPeer>>` — peer table; the
  TCP accept/disconnect paths already fan out `r2.peer.*` events.
* `ws_broadcast_tx: broadcast::Sender<String>` — the legacy
  `/ws/status` JSON path; serves as the bridge's transport in
  Phases 5d-bridge.1–.3 before the encrypted entanglement is live.

---

## Revision History

| Date | Version | Change |
|---|---|---|
| 2026-05-08 | 0.1 | First draft, post architecture lock-in (two-TG topology + bilateral entanglement). |
| 2026-05-08 | 0.2 | Reworked to align with R2-TRUST §7 carefully. The bridge is the production-side endpoint of *one* bilateral entanglement (with the viewing TG), not "with each viewer". R2-TRUST §7.5 envelope (X25519 + HKDF + XChaCha20-Poly1305 + HMAC) is the transport; per-viewer filtering is a sentant-layer concern above it. R2-TRUST §7.5.2 permission bits (EVENT_ROUTE + DISCOVERY, never MANAGEMENT) negotiated at handshake. Conformance vectors split into R2-TRUST §7 foundational + bridge-specific. v0.1 scope limited to two TGs (production + viewing), no cross-org. Flagged as first R2 entanglement implementation. |
