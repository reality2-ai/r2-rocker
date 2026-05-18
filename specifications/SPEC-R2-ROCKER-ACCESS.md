# SPEC-R2-ROCKER-ACCESS: Device-access lifecycle (enrolment, certs, revocation)

**Version:** 0.1 Draft
**Date:** 2026-05-18
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-SYSTEM (§3 trust), SPEC-R2-ROCKER-DASHBOARD (§5 HTTP), SPEC-R2-ROCKER-BRIDGE (§2 two-TG topology), SPEC-R2-ROCKER-WIRE, R2-TRUST (canonical, vendored under `crates/r2-trust/`), R2-TRANSPORT (relay, when needed for §5.2)

---

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**,
**SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**,
and **OPTIONAL** in this document are to be interpreted as
described in [RFC 2119](https://www.rfc-editor.org/info/rfc2119),
when they appear in capitals.

---

## 1. Introduction

The r2-rocker dashboard exposes live sensor data, fleet status,
named captures, and OTA control. Some of these surfaces are
read-only ("what's happening on the rig right now?") and some
mutate state ("update the firmware on every sensor"). Different
people in the lab — and around the world — have legitimate need
for different subsets. **Access** is the operator-facing model
that decides who sees what and who can change what.

The R2 substrate (`crates/r2-trust/`) already provides the
cryptographic primitives — Ed25519 device keys, signed device
certificates, key-rotation envelopes, a G-Set revocation CRDT. The
existing dashboard and firmware already issue device keys (sensor
identity is generated on first boot and persisted in NVS per
SPEC-R2-ROCKER-SENSOR §3.1). What's missing in v0.1 is the
operator-facing **glue**: a way for the operator to invite a new
viewer browser, hand it a single-use token, watch it become a
member, and revoke it later if the laptop walks out of the lab.

This spec defines that glue.

### 1.1 Scope

In scope:

* Roles, lifecycle, and identity of each kind of device that
  joins the Trust Group(s) (§2).
* The one-time **enrolment token** that bridges the gap between
  "the operator wants to invite a viewer" and "the viewer has a
  signed cert" — token format, transport, expiry (§3).
* HTTP routes on the controller that issue and consume tokens,
  list members, and revoke them (§4).
* The two **post-enrolment connection paths** a viewer may take:
  same-WiFi (direct) and off-network (R2 relay). Wire-protocol
  detail of the relay itself is cross-referenced, not duplicated
  (§5).
* **Persistence** of viewer identity in the browser (IndexedDB),
  and what survives refresh / tab close / device-loss (§6).
* **Revocation propagation** — once the operator revokes a viewer,
  what guarantees the rest of the system makes about tearing down
  the offending session (§7).
* The operator-visible **"Access" tab** UX, normatively: which
  elements MUST appear, in what order, with which labels. The
  exact markup is implementation-defined (§8).
* The v0.1 invariant **exactly one KeyHolder per system**, with
  hooks for the future multi-KeyHolder + save/restore work (§9).
* Conformance criteria for dashboard, webapp, and firmware (§11).

Out of scope:

* The R2 relay's own wire protocol — see R2-TRANSPORT and
  `crates/r2-transport/`. This spec only describes what r2-rocker
  asks of a relay, not the relay itself.
* OTA-image signing. Phase 9-secure (open task #24) covers that;
  §10 here records the cross-reference so the two specs stay in
  step on the KeyHolder's private-key responsibilities.
* The cryptographic primitives. R2-TRUST is the canonical source
  for `DeviceCertificate`, `TrustGroup::create`, `process_join_request`,
  the revocation G-Set CRDT, etc. This spec **uses** them; it does
  not redefine them.

### 1.2 Terminology

| Term | Meaning |
|---|---|
| **KeyHolder** | The R2-TRUST role with custody of the TG's signing key. In r2-rocker v0.1 the controller process is the KeyHolder by configuration (it reads `tg_priv.bin` from outside the working tree per `SECRETS-POLICY.md`). |
| **Member-Sensor** | A sensor firmware instance that has been issued a `DeviceCertificate` by the KeyHolder. v0.1 sensors are enrolled during BLE bootstrap (SPEC-R2-ROCKER-SENSOR §4 — already operational). |
| **Member-Viewer** | A browser instance that has been issued a `DeviceCertificate` by the KeyHolder via the QR/link flow defined in this spec. |
| **Token** | A 16-byte single-use secret + 8-hex-char TG hash that authorises exactly one `/api/access/claim`. See §3. |
| **Production TG** | The Trust Group sensors and the controller belong to. Carries telemetry. SPEC-R2-ROCKER-BRIDGE §2. |
| **Viewing TG** | The Trust Group viewers belong to. Receives a policy-filtered subset of production-TG traffic via the bridge. SPEC-R2-ROCKER-BRIDGE §2. The two TGs are bilaterally entangled at the KeyHolder layer. |
| **"this device"** | The browser instance currently rendering the dashboard, whatever its role. Used in the Access tab to mark the operator's own session as non-revocable (§8). |

### 1.3 Notation

CBOR maps follow the integer-key + smallest-encoding convention
from R2-WIRE / R2-CBOR. JSON shapes shown in this document are
the on-the-wire form of `/api/access/*` HTTP responses, not the
internal R2-TRUST serialisation (which is binary; see
`crates/r2-trust/src/persist.rs`).

---

## 2. Roles and identity flow

### 2.1 Three roles

r2-rocker recognises exactly three role values in v0.1, mapped to
`crates/r2-trust/src/lib.rs` `DeviceRole`:

| Role | R2-TRUST `DeviceRole` | Who | How they enrol |
|---|---|---|---|
| **KeyHolder** | `KeyHolder` | The controller process | Reads `~/.config/r2-rocker/tg_signer/tg_priv.bin` at startup, or whatever path the operator points it at via `--tg-priv`. Not an enrolment — direct possession of the private key. |
| **Member-Sensor** | `Member` | Each sensor firmware instance | BLE bootstrap (SPEC-R2-ROCKER-SENSOR §4): controller pushes a TG-signed `#wifi_offer` over L2CAP; sensor verifies the signature against `TG_PK` embedded at compile time; persists creds to NVS; reboots into WiFi. **Already operational** in v0.1. |
| **Member-Viewer** | `Member` | Each browser instance | QR / link enrolment per this spec. |

Future roles (sensors with elevated permissions, KeyHolder
delegates, multi-KeyHolder peers per §9) MAY extend this table.
v0.1 implementations **MUST** reject any role value not in the
table above.

### 2.2 Identity is per-device, not per-user

A laptop has one `device_pk`. The same laptop loaded twice (two
tabs) shares the cert via IndexedDB. A different laptop is a
different member, even when used by the same operator.

This matches `r2-notekeeper`'s model and the AI-CONTEXT note that
*"each browser instance is a TG-member hive"*: r2-rocker does not
have users in the application-account sense; it has **devices**
that hold cryptographic identity.

### 2.3 The KeyHolder is exactly one process in v0.1

Per `AI-CONTEXT.md` §"Authoritative ledger" and the calm-tech
"Controller fixed per experiment" rule: there is **one**
controller per experiment, and that controller is the **only**
KeyHolder. Implementations **MUST NOT** silently accept multiple
processes claiming the KeyHolder role for the same TG. Multiple
controllers on the same hotspot, both holding `tg_priv.bin`, is
operator misconfiguration and the dashboard SHOULD detect it
(see §9 future work — the v0.1 detection is best-effort).

### 2.4 Two TGs, one KeyHolder

Per SPEC-R2-ROCKER-BRIDGE §2, the system has two Trust Groups
(production + viewing) bilaterally entangled. The controller
process holds the KeyHolder role in **both**. Member-Sensors
belong to the production TG; Member-Viewers belong to the
viewing TG; the bridge governs traffic between them.

Enrolment of a Member-Viewer per this spec therefore yields a
member of the **viewing TG**. Enrolment of a Member-Sensor per
SPEC-R2-ROCKER-SENSOR §4 yields a member of the **production TG**.
The two paths never cross.

---

## 3. Enrolment token

### 3.0 Invitations are active and KeyHolder-only

A foundational R2-TRUST invariant, restated here for emphasis:

* **Inviting** a new device into a Trust Group is **always an
  active process initiated by a member device that holds the
  KeyHolder role.** A device cannot "ask to join"; it can only
  *redeem an invitation* that a KeyHolder has already issued.
  There is no anonymous self-enrolment, no public join page, no
  pre-shared community password. The token is the explicit
  output of an explicit KeyHolder action.
* **Every invitation is time-limited.** Expiry is server-side
  enforced (§3.3). After expiry, the token is unrecoverable
  even by the KeyHolder that issued it — re-inviting means
  issuing a fresh one.

These invariants frame the rest of this section: the token
format (§3.1), its three representations (§3.2), and the
dashboard's server-side record (§3.3) all serve the KeyHolder
explicitly choosing to admit one specific new device for a
bounded window.

### 3.1 Format

A token is a tuple:

```
  tg_hash : 8 hex chars     ← first 16 bits of SHA-256(TG_PK)
  entropy : 16 bytes        ← random, ed25519-pk-grade (csprng)
```

The TG hash binds the token to a specific Trust Group; the entropy
is the secret. A device claiming the token **MUST** prove
knowledge of both. The TG hash is short on purpose — it's
identifying, not authenticating; the secrecy lives in the
16-byte entropy.

Tokens are **single-use** and **expire 300 seconds (5 min)** after
issuance. The dashboard **MUST** enforce both invariants
server-side (§4.1). Browser-side countdowns are display only.

### 3.2 Three representations

The token is presented to the operator in three forms
simultaneously, all encoding the same `(tg_hash, entropy)` tuple:

* **QR code** — embeds an `r2:` URL of the form
  ```
  r2://join/<tg_hash>/<entropy_hex>?relay=<relay_url>
  ```
  where `relay_url` is the URL of the relay the viewer should use
  if it cannot reach the controller on the local network (see
  §3.4 for the relay default).
* **Plain shareable URL** — two variants generated together:
  * `url_local = http://<controller_lan_ip>:8080/?join=<tg_hash>.<entropy_hex>`
  * `url_relay = https://reality2-ai.github.io/r2-rocker/?join=<tg_hash>.<entropy_hex>&relay=<relay_url>`
  The viewer picks the one that matches its network situation.
  The static-host URL is the same pattern used by `r2-notekeeper`
  (AI-CONTEXT §"Browser enrolment via QR / link"). The webapp
  hosted at either URL **MUST** implement the same enrolment
  flow.
* **3-word code** (OPTIONAL display) — three space-separated
  BIP39 words deterministically derived from the entropy. Same
  wordlist as r2-notekeeper for ecosystem parity. The viewer
  webapp **MUST** accept a typed 3-word code as an alternative
  to the URL `?join=` parameter. The dashboard **SHOULD** hide
  this representation behind a "show 3-word code" toggle in the
  invite modal — useful only when QR + URL both fail (the
  operator and viewer are on the phone together rather than in
  the same room).

### 3.3 Server-side state

Between `/api/access/invite` (issue) and `/api/access/claim`
(consume), the dashboard **MUST** keep a per-token record:

| Field | Type | Purpose |
|---|---|---|
| `entropy` | 16 bytes | the secret being authorised |
| `issued_at` | i64 ms | issue timestamp (wall clock) |
| `expires_at` | i64 ms | `issued_at + 300_000` |
| `claimed_by` | optional `device_pk` (32 bytes) | set on first successful claim |
| `claimed_at` | optional i64 ms | set on first successful claim |
| `nonce` | u64 | monotonic per-process counter for log correlation |

The record is in-memory only in v0.1. It **MUST NOT** be
persisted to disk; a dashboard restart invalidates every
unclaimed token. Operator-level re-issue is trivial and the
shorter the persistence window, the smaller the replay surface.

Successfully-claimed tokens **MUST** be retained in the record
table for at least their original expiry window so that
re-claims with the same `device_pk` are idempotent (§4.2). After
that they MAY be garbage-collected.

### 3.4 Relay endpoint

The relay URL embedded in tokens is **operator-configurable** per
deployment. The dashboard reads it from `--relay-url` at startup
(default: empty — no relay path advertised, viewers must use the
same-WiFi URL). A future deployment SHOULD publish a canonical
relay URL for the university handoff; the spec deliberately does
not name one in v0.1.

When `--relay-url` is empty, `url_relay` and the QR's `?relay=`
fragment **MUST** be omitted. Implementations **MUST NOT** invent
a placeholder relay URL.

---

## 4. HTTP routes on the controller

The four routes below are added to `SPEC-R2-ROCKER-DASHBOARD §5.1`.
They formalise (and supersede) the stubs presently at
`dashboard/src/main.rs:642–645` (`/api/enrol-init`,
`/api/enrol-complete`).

| Route | Method | Body | Returns | Auth |
|---|---|---|---|---|
| `/api/access/invite` | POST | `{name?: str}` | `{token, qr_png_b64, url_local, url_relay?, words_3?, expires_at}` | KeyHolder-only |
| `/api/access/claim`  | POST | `{tg_hash: hex, entropy_hex: hex, device_pk: hex, device_name: str}` | `{cert: r2trust-CBOR, encrypted_creds: bytes_b64, tg_pk: hex, paired_at}` | none (the token IS the auth) |
| `/api/access/members` | GET | — | `{members: [{device_pk, name, role, paired_at, last_seen, revoked}]}` | KeyHolder-only |
| `/api/access/revoke/{device_pk}` | POST | `{}` | `{ok: bool, revoked_at}` | KeyHolder-only |

The legacy `/api/enrol-init` and `/api/enrol-complete` routes in
v0.1 **MAY** be deleted (no implementation depends on them) or
**MAY** be retained as 410-Gone aliases pointing at the new
routes for one release of forward-compat. The spec does not
prefer either.

The existing `/api/keyholder/tg-pub` (`dashboard/src/main.rs:2754-2783`)
remains as-is — it returns the TG public key for any caller to
verify cert chains.

### 4.1 `/api/access/invite`

The KeyHolder presses "Grant access to a new device" in the Access
tab; the webapp POSTs to this route. The dashboard:

1. Generates 16 bytes of CSPRNG entropy.
2. Computes `tg_hash = first 8 hex chars of SHA-256(TG_PK)`.
3. Inserts a record per §3.3 with `expires_at = now + 300_000`.
4. Builds `url_local` from the controller's primary IPv4 on the
   hotspot interface (the same address sensors connect to on
   port 21042).
5. If `--relay-url` is set, builds `url_relay` and the QR's
   `r2:` URL with `?relay=`.
6. Renders the QR PNG (base64-encoded, ≤ 4 KB for 16-byte +
   short URL payload).
7. Returns the JSON envelope. The token itself (raw `tg_hash` +
   `entropy_hex`) is **NOT** returned as a separate field —
   it's already embedded in the URLs and reachable via the QR.

Implementations **MUST** rate-limit `/api/access/invite` to at
most one issue per second per source IP to prevent token-flooding
DoS. Past the rate limit, the route returns 429.

### 4.2 `/api/access/claim`

The viewer's webapp POSTs to this route on first load with a
`?join=` URL parameter present. The dashboard:

1. Validates that `tg_hash` matches its own TG. If not, returns
   404.
2. Looks up the record by `entropy`. If not found, returns 404.
3. Checks `now < expires_at`. If expired, returns 410.
4. Checks `claimed_by`:
   * `None` → first claim. Set `claimed_by = device_pk`,
     `claimed_at = now`. Proceed.
   * `Some(other_pk) where other_pk != device_pk` → already
     consumed by a different device. Returns 409.
   * `Some(same_pk)` → idempotent re-claim. Proceed with the
     same response.
5. Validates `device_name` (1..=64 bytes, charset
   `[A-Za-z0-9 ._-]`, no leading/trailing whitespace). On
   failure, returns 400.
6. Invokes `crates/r2-trust/src/lifecycle.rs:257-313`
   `process_join_request` with the supplied `device_pk` to
   issue a `DeviceCertificate` and the encrypted `(DEK, HK)`
   credentials bundle.
7. Persists the new member in the KeyHolder's
   `TrustGroup::members` via `r2-trust/src/persist.rs`.
8. Broadcasts `r2.dash.access.member_added {device_pk, name,
   role: "viewer", paired_at}` on `/ws/status` so other connected
   members see the change without polling.
9. Returns the JSON envelope. The webapp persists the cert per
   §6.

The route is **public** — anyone who knows the token can claim
it. That's the design: the token IS the authentication. If the
token leaks, the leak window is 5 minutes maximum and the
operator can revoke immediately.

### 4.3 `/api/access/members`

KeyHolder-only. Returns the full member list, including
revoked members (with `revoked: true` and a `revoked_at`
timestamp). Webapp consumers MAY filter the revoked rows out
of the operator-facing list; the spec returns them so the
audit trail is reachable.

`last_seen` is the wall-clock timestamp of the most recent
authenticated frame from that member. For sensors this is the
last R2-WIRE frame on port 21042; for viewers it's the last
`/ws/raw` or `/ws/status` keep-alive. If a member has never
been seen since the dashboard process started, `last_seen` is
`null`.

### 4.4 `/api/access/revoke/{device_pk}`

KeyHolder-only. Adds `device_pk` to the revocation G-Set per
`crates/r2-trust/src/revocation.rs`, persists, broadcasts
`r2.dash.access.revoked {device_pk, revoked_at}` on `/ws/status`,
and tears down any open `/ws/raw` or `/ws/status` connection
from that `device_pk` immediately (§7).

The route **MUST** succeed regardless of whether the target
device is currently online — a KeyHolder can remove a device
from the Trust Group at any time, and offline targets learn of
their revocation by cert-check failure on next connect (§7.6).
"My laptop got left at the bar" is a routine operator scenario
and the spec accommodates it by treating revocation as a
state-change on the KeyHolder's authoritative ledger, not as a
synchronous handshake with the revoked party.

The KeyHolder **MUST NOT** revoke itself. Attempting to do so
returns 400.

---

## 5. Connection paths after enrolment

A viewer holds a `DeviceCertificate` after a successful claim.
It can reach the dashboard in two ways. The spec defines the
*behaviour at the path boundary*; the wire protocol of each leg
lives elsewhere.

### 5.1 Same-WiFi (direct)

The viewer loaded the webapp from `http://<controller_lan_ip>:8080/`,
so its WASM hive can open a WebSocket directly to the controller's
`/ws/raw`. In v0.1, per the operator-chosen **additive auth
model**, `/ws/raw` does not require a cert handshake — the
WebSocket opens anonymous and the viewer streams whatever the
controller sends. The viewer's cert is held client-side, ready
for the v1 cert-handshake variant.

Implementations of v0.1 conforming to this spec **MUST**:

* persist the cert per §6 even when the WS handshake itself
  doesn't use it (the cert is needed for the relay path AND for
  the v1 upgrade);
* respect a `r2.dash.access.revoked` event for their own
  `device_pk` arriving on `/ws/status` by closing the WS, wiping
  the IndexedDB cert, and rendering the "not-enrolled" landing
  page (§7).

The cert-handshake variant of `/ws/raw` is the v1 target and is
documented here as future work:

> v1: `/ws/raw` upgrade carries a `Sec-WebSocket-Protocol` value
> `r2-access-v1` and an opening client message of
> `{auth: {device_pk, sig_over_nonce}}`. The dashboard verifies
> the signature against the device's cert chain, accepts or
> rejects the WS, and tears down on revocation. This variant is
> implementable behind a single feature flag and ships in the
> Phase-5 implementation slice after the v0.1 slice is on the
> bench.

### 5.2 Off-network (relay)

The viewer loaded the webapp from the static host
(`https://reality2-ai.github.io/r2-rocker/`) and is not on the
controller's hotspot. Its WASM hive connects to the operator's
configured R2 relay (from `?relay=` or the persisted cert
bundle) and the dashboard's relay-side state forwards
encrypted blobs.

The wire protocol between viewer ↔ relay ↔ dashboard is
specified in **R2-TRANSPORT** (vendored under `crates/r2-transport/`).
This spec only constrains the bits r2-rocker contributes:

* The relay-side **dashboard** session is established by the
  controller process at startup if `--relay-url` is set. It
  authenticates to the relay using the KeyHolder's cert. The
  relay forwards inbound viewer envelopes to that session.
* The **viewer**'s relay session authenticates using its
  `DeviceCertificate`. The cert chain terminates at the
  KeyHolder cert that the relay already trusts via the
  controller's session, so the relay does not need to verify
  it independently — the controller does, on receipt.
* Inbound envelopes from the relay are dispatched on the
  controller side through the same pipeline as `/ws/raw`
  inbound — i.e. the bridge policy in SPEC-R2-ROCKER-BRIDGE
  §3-§4 governs which events flow which direction. The relay
  is just a long-haul transport; it is NOT an authorisation
  point.

If the operator's `--relay-url` configuration is empty, the
off-network path is not available in this deployment. Tokens
issued in such a deployment omit `url_relay`; the operator and
viewers are expected to be co-located on the hotspot.

---

## 6. Cert + key persistence (browser)

A successfully-enrolled viewer holds:

| Field | Bytes | Stored as |
|---|---|---|
| `device_sk` | 32 | raw |
| `device_pk` | 32 | derived from `device_sk` (cached) |
| `device_cert` | ~120 | r2-trust CBOR DeviceCertificate |
| `DEK` | 32 | raw |
| `HK` | 32 | raw |
| `tg_pk` | 32 | raw — used to verify peer certs |
| `relay_url` | str | optional |
| `device_name` | str | for display in the Access tab |

This is the 277-byte `R2Member` shape in `r2-trust/src/persist.rs:57-132`,
plus the relay URL and human-readable name.

Implementations **MUST** store this state in **IndexedDB** under
the database name `r2-rocker-access`, store `members`,
key `self`. The state **MUST** survive:

* Tab close + reopen.
* Browser restart.
* Mobile-device app-switch (where the OS doesn't clear site
  storage).

The state **MUST NOT** survive:

* "Clear site data" in the browser's developer tools.
* Cert revocation (§7) — receiving `r2.dash.access.revoked` for
  the local `device_pk` **MUST** delete the record.
* User-initiated "Leave" action — a UI affordance the
  implementation MAY provide for the viewer to deregister itself
  cleanly. On Leave, the viewer SHOULD POST to a future
  `/api/access/leave` route (not part of v0.1 scope; the operator
  performs the equivalent action via §4.4) and wipe IndexedDB
  locally regardless of route success.

IndexedDB is chosen over localStorage despite notekeeper's
localStorage precedent because the binary blobs are awkward to
base64-encode at every read, and a future schema bump (e.g.
multiple members per device for KeyHolder-delegate sessions) is
much cleaner against IndexedDB. The base64-in-localStorage path
is a documented v0.1 fallback ONLY for browsers without IndexedDB
support; IndexedDB has been universal for years and the fallback
will likely never fire.

---

## 7. Revocation propagation

### 7.1 Add to the revocation set

When the KeyHolder revokes a member via §4.4, the dashboard
adds `device_pk` to the revocation G-Set
(`r2-trust/src/revocation.rs`) and persists it.

### 7.2 Broadcast to currently-connected members

The dashboard broadcasts
`r2.dash.access.revoked {device_pk, revoked_at}` on `/ws/status`
to all currently-connected viewers. Every viewer **MUST** check
whether the announced `device_pk` matches its own.

### 7.3 Revoked-self behaviour

If a viewer is the revoked party, it **MUST**:

* Close any open `/ws/raw` and `/ws/status` connections.
* Delete the IndexedDB record per §6.
* Render the "not-enrolled" landing page (manual paste-a-link
  fallback, no resumed session).

### 7.4 Server-side teardown

The dashboard **MUST** close the per-peer TCP / WS connection
from the revoked `device_pk` immediately, regardless of whether
the viewer cooperated with §7.3. The voluntary client-side wipe
is a courtesy; the involuntary server-side teardown is the
guarantee.

### 7.5 Durability across restarts

The revocation set is persisted (via `r2-trust/src/persist.rs`)
so a dashboard restart preserves it. A revoked device that
reconnects after a controller process restart still fails the
cert check on the next handshake.

### 7.6 Offline revocation

A KeyHolder revoking an offline device is a routine, supported
operation — not an edge case. The dashboard's revocation flow
**MUST NOT** depend on the target being reachable.

For viewers that are offline at the time of revocation: on next
connect (whether same-WiFi or via relay), the dashboard **MUST**
check `device_pk` against the revocation set in the v1 cert-
handshake variant of `/ws/raw` and refuse the connection. In v0.1
where `/ws/raw` is anonymous, the dashboard MAY accept the WS
but **MUST NOT** route any inbound R2-WIRE frame to or from a
revoked `device_pk` — bridge policy per SPEC-R2-ROCKER-BRIDGE
§3-§4 still applies, and a revoked member fails that check
before any payload moves.

The revocation G-Set is durable across dashboard restarts (§7.5),
so a target that's offline for days or months still learns of its
revocation the next time it tries to connect, and the revocation
takes effect even if it tried to connect once during the offline
window (the dashboard's check is against the persisted set, not
just connections seen during the current process lifetime).

The sharp edge `r2-notekeeper` carries — that revocation is local
only — is **closed** by §7.2's `/ws/status` broadcast and the
dashboard-side connection teardown in §7.4. Implementations
**MUST NOT** rely on the revoked viewer voluntarily wiping its
state.

---

## 8. The "Link" tab (operator UX)

The webapp **MUST** present a top-level navigation tab whose
operator-visible label is literally **"Link"** (calm-tech: no R2
jargon — no "Trust Group", no "KeyHolder", no "Enrolment", per
`feedback_ui_no_protocol_jargon`). It sits alongside the existing
Live / Devices / Data tabs.

The internal name used in code, URL paths, this spec's title, and
the HTTP route table (`/api/access/*`) is still **"access"** —
the operator-facing rename is cosmetic only. Implementations
**MUST NOT** rename the URL routes or the spec module.

Below the tab heading, in order:

1. **A button labelled "Link a new device"**. Visible
   only when the local device is the KeyHolder (the controller's
   own browser). When clicked, the webapp POSTs to
   `/api/access/invite` and opens a modal containing:
   * The QR code, sized at ≥ 256 × 256 CSS pixels.
   * The `url_local` URL, in a copy-to-clipboard chip.
   * The `url_relay` URL, in a separate copy-to-clipboard chip,
     **iff** the response included one (per §3.4).
   * A 5-minute countdown timer, prominent.
   * A "Show 3-word code" toggle that reveals `words_3` on
     demand (§3.2).
   * A close button that dismisses the modal but does NOT
     cancel the token — the operator's intent might be "save
     for later". Tokens expire by §3.3, not by modal close.

2. **A list of currently-paired devices**, one card per device:
   * Device name (operator-chosen at claim time).
   * Role (Controller / Sensor / Viewer), as
     human-readable text — never "KeyHolder", never
     "Member".
   * Paired-at timestamp, rendered as a local-time string per
     `feedback_ui_no_protocol_jargon`.
   * Last-seen timestamp, with relative "5 minutes ago" form
     for recent activity and absolute timestamps for older.
   * A "Revoke" button. The current operator's own device card
     is rendered with `"this device"` instead of a revoke
     button — the KeyHolder cannot revoke itself (§4.4).

3. **No other elements in v0.1**. Specifically, the operator
   does NOT see TG signing key paths, cert byte-blobs, relay
   debug, R2-TRUST schema versions, or any other internal state.

Non-KeyHolder viewers visiting the Access tab see the device list
(read-only, no Revoke buttons, no "Grant access" button) and
their own card marked "this device". The list is the same shape
per §4.3, just rendered in read-only mode.

A viewer that has not yet enrolled (no cert in IndexedDB and no
`?join=` parameter) **MUST** render a single landing page in
place of the regular tabs:

> *"This device hasn't been paired yet. Open the link or scan
> the QR code your operator sent you, or paste the 3-word code
> below."*

with a paste field that accepts URLs and 3-word codes. On valid
paste, the webapp synthesises a `?join=` and runs §4.2.

---

## 9. Multi-KeyHolder (future)

v0.1 invariant: **exactly one KeyHolder process per Trust Group.**
A second process holding `tg_priv.bin` and claiming the KeyHolder
role is operator misconfiguration; the v0.1 dashboard SHOULD
detect this on R2 mesh peering (two `r2.keyholder.heartbeat`
events with different source `device_pk` per TG within the same
window) and log a `[access] WARNING: multiple KeyHolders observed
for TG xxx` line. Best-effort only — the spec does not require
the dashboard to refuse to start.

Future work for multi-KeyHolder + operator-managed save/restore
(`feedback_calm_tech_security`):

* Define the cert chain so a delegated KeyHolder's signing key
  is itself signed by the original — at most one hop in v0.2.
* Reconcile two `r2-trust` instances' revocation G-Sets on
  reconnect; the CRDT shape is already commutative so the merge
  is mechanical, but the operator-visible "which KeyHolder did
  this" trail is new.
* "Save the TG" export → encrypted file + paper backup of the
  3-word recovery code. "Restore the TG" import on a new
  controller process.

None of the above is required by this spec. The §2.1 role
table is **closed** for v0.1 and **open** for v0.2.

### 9.1 Future tab split — Link vs Admin

In v0.1 the operator-facing "Link" tab (§8) handles exactly one
flow: inviting a **viewer** browser. A second, conceptually
distinct flow — inviting a **standby / alternative controller**
machine — is queued for v0.2.

Standby-controller pairing is not a pure cryptographic act. The
receiving laptop must already have the native dashboard binary
built, a WiFi adapter capable of hosting a hotspot, and
operator-side access to install / configure the prerequisites.
A QR-or-link affordance only handles the **cert-delegation
half** of the handoff; the **binary + OS setup half** is
out-of-band and operator-managed. v0.1 deployments use file
transfer of `tg_priv.bin` for both halves; v0.2 separates them.

When the v0.2 flow lands, the two affordances **SHALL** live on
**separate tabs**:

* **Link** (existing, this spec §8) — for adding viewers. Routine,
  low-impact, frequently used.
* **Admin** (new) — for the rare, higher-impact operations:
  pairing a standby controller, transferring KeyHolder duty,
  rotating the TG signing key, post-incident audit. The Admin
  tab is visible only to the currently-active KeyHolder.

Implementations that ship before the v0.2 multi-KeyHolder work
**MUST NOT** preemptively render an empty Admin tab — it would
confuse the operator. The tab arrives with the first concrete
admin operation.

---

## 10. OTA-signing implications

Phase 9-secure (open task #24) signs OTA firmware images with the
KeyHolder private key. The Access spec is consistent with that:

* The KeyHolder is identified by §2.1 / §2.3 as the controller
  process. Phase 9-secure uses the same identity.
* The sensors' `TG_PK` embedded at compile time (per
  SPEC-R2-ROCKER-SENSOR §3.1) is the verifying key for both
  enrolment offers AND OTA images. The spec does not introduce a
  second key.
* OTA image signatures use the same `crates/r2-trust/src/cert.rs`
  Ed25519 primitive as DeviceCertificate signatures.

Implementations of Phase 9-secure **MUST** verify the OTA-image
signature against the sensor's compile-time `TG_PK` before
writing the inactive partition. This is unchanged by the Access
spec.

---

## 11. Conformance

### 11.1 Dashboard

A dashboard build conforms to this spec when:

1. The four `/api/access/*` routes (§4) are implemented with the
   payload shapes shown.
2. The KeyHolder-only routes refuse non-KeyHolder callers. In
   v0.1 where there is no per-route auth check, the dashboard
   MAY rely on the localhost / private-LAN boundary; in v1 with
   cert-handshake on `/ws/raw`, the routes **MUST** also be
   cert-gated.
3. Tokens are CSPRNG-generated, 5-minute-expiring, single-use,
   in-memory only (§3.3).
4. `/api/access/claim` is idempotent for the same `device_pk`
   within the original window and rejects every other
   `device_pk` after the first.
5. `/api/access/members` returns the persisted member list
   including revoked rows.
6. `/api/access/revoke` adds to the revocation G-Set, broadcasts
   `r2.dash.access.revoked`, and tears down the offending
   connection synchronously.
7. The KeyHolder cannot revoke itself (§4.4).

### 11.2 Webapp

A webapp build conforms when:

1. It detects `?join=<token>` on first load and POSTs
   `/api/access/claim` with the token, a freshly-generated
   `device_pk`, and an operator-supplied `device_name`.
2. It persists the cert + key + DEK + HK in IndexedDB under
   `r2-rocker-access > members > self` (§6).
3. On subsequent loads without `?join=`, it reads the IndexedDB
   record and uses it; it does not re-enrol.
4. It renders the "Access" tab per §8, including the modal,
   the device list, and the not-enrolled landing page.
5. It listens for `r2.dash.access.revoked` on `/ws/status` and,
   when the revoked `device_pk` matches its own, deletes the
   IndexedDB record and re-renders the not-enrolled landing
   page (§7.3).

### 11.3 Firmware

Sensor firmware in v0.1 holds a single `DeviceCertificate`
issued during BLE bootstrap; it is unaffected by this spec.

Future work: extend the firmware to verify peer certs on
inbound R2-WIRE frames when the bridge-fronted path becomes the
canonical transport (post-Phase-9-secure). The Access spec does
not require firmware changes for v0.1.

---

## 12. Cross-references

| Topic | Authoritative source |
|---|---|
| R2-TRUST primitives (cert, group, persist, revocation) | `crates/r2-trust/`, R2-TRUST spec |
| R2 relay wire protocol | `crates/r2-transport/`, R2-TRANSPORT spec |
| Two-TG topology + bridge policy | `SPEC-R2-ROCKER-BRIDGE.md` |
| Sensor BLE bootstrap + cert issuance | `SPEC-R2-ROCKER-SENSOR.md` §4 |
| KeyHolder private-key storage | `SECRETS-POLICY.md` |
| OTA-image signing | future Phase 9-secure spec (open task #24) |
| Wire frame format | `SPEC-R2-ROCKER-WIRE.md` |
| Dashboard HTTP / WS routes | `SPEC-R2-ROCKER-DASHBOARD.md` §5 |

---

## Appendix A. Notekeeper feature reconciliation

For each feature surfaced by the r2-notekeeper enrolment flow
study (see plan file `sleepy-snuggling-tome.md`), this spec
either includes it or marks it explicitly out of scope:

| Notekeeper feature | This spec |
|---|---|
| QR code | §3.2 — included. |
| Shareable URL | §3.2 — included, with `url_local` + `url_relay` variants. |
| 3-word code | §3.2 — included as optional display. |
| 5-minute expiry | §3.1 — included, server-side enforced. |
| Single-use token | §3.3 — included. |
| Browser keypair generation | §6 — included. |
| Cert + DEK + HK in localStorage | §6 — moved to IndexedDB. |
| Relay-mediated enrolment | §5.2 — included; relay wire protocol cross-referenced to R2-TRANSPORT. |
| Word-code → join-code relay lookup | OUT OF SCOPE — the 3-word code is recovered from the entropy locally, no relay round-trip needed. |
| Member list view | §8 (2) — included. |
| Revoke button | §4.4 + §8 (2) — included. |
| Revocation broadcast | §7 — INCLUDED; this closes notekeeper's sharp edge. |
| Persistent invitation records / audit log | OUT OF SCOPE for v0.1 — operator-visible audit trail is a v0.2 concern. |
| Relay HELLO HMAC | OUT OF SCOPE here — R2-TRANSPORT covers it. |

---

## Appendix B. Implementation tracking

The Phase-5 implementation slice that lands this spec is the
follow-on plan to `sleepy-snuggling-tome.md`. It is expected to
contain, at minimum:

* `dashboard/src/main.rs` — four new routes, in-memory token
  table, `/ws/status` broadcast hooks, revocation teardown.
* `webapp/index.html` — "Access" tab + invite modal + device
  list + not-enrolled landing page; IndexedDB layer; `?join=`
  handler.
* `crates/r2-wasm/` — if WASM bindings for
  `process_join_request` / `R2Member::from_join_response` are not
  already exposed, add them. (Notekeeper has working precedent.)
* No firmware changes for v0.1.

The implementation slice MAY be split further (e.g. invite +
claim first, members + revoke second) at the implementer's
discretion. The order of merge is implementation-defined; the
spec is the merge gate.
