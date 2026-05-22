# SPEC-R2-ROCKER-ACCESS: Device-access lifecycle (enrolment, certs, revocation)

**Version:** 0.3 Draft
**Date:** 2026-05-22
**Status:** Normative Draft

> **v0.3 Δ from v0.2:** **Request/approve is the sole enrolment flow.**
> Operator-initiated invitation tokens (the `/api/access/invite` +
> `/api/access/claim` round-trip and the time-limited QR/URL token
> from old §3) are removed. A new viewer instead lands on the
> dashboard's not-enrolled page, submits a request with its public
> key + a chosen name, and waits for the operator to Approve or Deny
> on the Link tab. The cryptographic outcome is the same — the
> KeyHolder runs `process_join_request` at approve time and the
> viewer's polling `/api/access/check/{device_pk}` returns the cert +
> DEK + HK + relay_url bundle — but the operator UX is simpler (one
> button, no expiring tokens) and the protocol surface is smaller
> (no invite-token table, no 5-minute timeout, no QR-with-token).
> Two QRs (WiFi-join + dashboard URL) remain on the Link tab as a
> static "Onboard a visitor" helper, but neither carries a token.
> Sections affected: §3 (replaced — token format gone), §4 (routes
> table + handlers), §5 (unchanged, relay still post-enrolment),
> §8 (Link tab UX), Appendix A.

> **v0.2 Δ from v0.1:** Enrolment is **local-WiFi-only**. The relay
> URL is delivered as part of the claim response (now the approve
> response under v0.3), not embedded in the invitation token. The
> relay never sees enrolment material. Sections affected: §3.2
> (token representations), §3.4 (relay endpoint), §4 (routes table),
> §4.1 (invite), §4.2 (claim), §5 (post-enrolment paths). See §3.4
> *Rationale* for the threat-model motivation.
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
| **Production TG** | **Target state** — the Trust Group sensors and the controller belong to. Carries telemetry. SPEC-R2-ROCKER-BRIDGE §2. *In v0.1 this is the only TG; see §2.4.1.* |
| **Viewing TG** | **Target state** — the Trust Group viewers belong to. Receives a policy-filtered subset of production-TG traffic via the bridge. SPEC-R2-ROCKER-BRIDGE §2. The two TGs are bilaterally entangled at the KeyHolder layer. *In v0.1 viewers are members of the production TG with the `viewer` variant tag; the split lands in a follow-up slice — see §2.4.1.* |
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

### 2.4 Two TGs, one KeyHolder — target state

Per SPEC-R2-ROCKER-BRIDGE §2, the **target state** is two Trust
Groups (production + viewing) bilaterally entangled (R2-TRUST §7.5).
In that state:

* Member-Sensors belong to the **production TG** (enrolment path:
  SPEC-R2-ROCKER-SENSOR §4 BLE bootstrap).
* Member-Viewers belong to the **viewing TG** (enrolment path:
  this spec, §3–§5).
* The controller process is a member of the production TG with
  `DeviceRole::KeyHolder`, and **separately** a member of the
  viewing TG with `DeviceRole::KeyHolder`. Two device certs, two
  TG identities, one physical process holding both signing keys.
  This is consistent with R2-TRUST: a *device* may be a member of
  at most one TG at a time, but a *process* may hold several
  device identities (one per TG it participates in) just as a
  laptop can run two browser tabs paired to two different TGs.
* The bridge sentant (SPEC-R2-ROCKER-BRIDGE §2.1) is a
  Member-Bridge in the production TG only; the entanglement
  itself lives at the TG level, derived from both TGs'
  `TG_PK` / `TG_SK` per R2-TRUST §7.5.

### 2.4.1 v0.1 reality — single TG, role tag

In v0.1 (the current cut) the dashboard operates with **one** TG.
A single `tg_priv.bin` signs every cert, sensors and viewers
alike. The "two TGs" terminology above describes the design
target; on the wire there is one TG hash and one relay bucket.

Role disambiguation between Member-Sensor and Member-Viewer is
carried in the cert metadata (`DeviceCertificate::role`, plus a
v0.1 `r2-rocker:variant` extension distinguishing
`"sensor"` / `"viewer"`); the bridge policy (BRIDGE §3–§4) is
enforced at the controller as an in-process filter, not as a TG
boundary. Implementations **MUST** persist the variant in the
cert and **MUST** reject any frame whose variant does not match
the source's known role.

The split into two TGs is **deferred** to a follow-up slice
(tracked as `task #52`). When that slice lands:

* A second TG keypair (`tg_priv_view.bin`) is generated alongside
  the production key. Existing viewer certs are reissued under
  the viewing TG via a one-shot migration the operator triggers
  from the Access tab.
* The relay session opens **two** WSS connections — one signed
  with each TG key — and viewers in the viewing-TG bucket can no
  longer see production-TG frames except those the bridge re-emits.
* Detection of "one device in two TGs" becomes meaningful and
  the dashboard MUST refuse to issue a viewer cert under
  production-TG (and vice versa).

Until that slice ships, the "one device / one TG" invariant is
**not** under threat: nobody is in two TGs, because there is only
one TG.

---

## 3. Enrolment via request-and-approve

### 3.0 The KeyHolder still gates every admission

A foundational R2-TRUST invariant, restated here because it
shapes the rest of this section:

* **Admission to a Trust Group is always an explicit KeyHolder
  action.** v0.3 changes *how* that action is initiated — the
  prospective member opens a request, the operator approves it
  — but the cryptographic outcome is unchanged: the KeyHolder
  runs `process_join_request` and signs the resulting cert. No
  cert is ever produced without an explicit operator click.
* **There is no anonymous self-enrolment.** A request that the
  operator never approves never becomes a cert. The request
  surface is open by design (anyone on the LAN can POST one)
  but the only thing it gets you without operator action is a
  spot in the pending queue.

The v0.1 / v0.2 invitation-token model (single-use entropy + QR
URL) is removed. Operators **MUST NOT** rely on `/api/access/invite`
or `/api/access/claim` — both are deleted in v0.3.

### 3.1 Lifecycle states

A request progresses through these states:

| State | Triggered by | Persisted between dashboard restarts? |
|---|---|---|
| `Pending` | viewer POSTs `/api/access/request` | NO — in-memory only |
| `Approved` | KeyHolder POSTs `/api/access/approve/{device_pk}` | partial — the issued cert + DEK + HK bundle is cached in the same pending record until the viewer's next `/check` poll consumes it, then the cert is also written to the persistent member set per [[r2-trust/persist.rs]]. |
| `Denied` | KeyHolder POSTs `/api/access/deny/{device_pk}` | NO — visible to the viewer's next `/check` poll, then the pending record is dropped. |
| `Consumed` | viewer's `/check` returns the approved bundle | n/a — record dropped |
| `Revoked` | KeyHolder POSTs `/api/access/revoke/{device_pk}` (§4.4) | YES — G-Set CRDT in [[r2-trust/revocation.rs]] |

Each state transition broadcasts an event on `/ws/status` so the
operator's Link tab can refresh without polling.

### 3.2 Request payload

The viewer's webapp POSTs `/api/access/request` with:

```json
{
  "device_pk": "<64 hex chars>",
  "name":      "<1..=64 chars, charset [A-Za-z0-9 ._-]>"
}
```

`device_pk` is the viewer's locally-generated Ed25519 public key
(from `wasmGenerateDeviceKeypair`). `name` is the operator-facing
label the requester picks — it appears in the operator's
pending-queue row so they know which physical device they're
approving. The dashboard MAY append a transport hint (the
requester's IP) to the row internally, but the spec does not
require it to be displayed.

The route is **open** — no auth header, no enrolment token. The
dashboard's only gate is a per-IP rate limit (MUST be at most one
new pending request per 5 seconds per source IP; over the limit
returns 429).

### 3.3 Server-side state

For each pending request, the dashboard keeps an in-memory record:

| Field | Type | Purpose |
|---|---|---|
| `device_pk` | 32 bytes | requester's Ed25519 public key — also the lookup key |
| `name` | UTF-8 string ≤ 64 bytes | requester's chosen device name |
| `hint` | UTF-8 string | dashboard-internal transport hint (e.g. source IP) |
| `submitted_at_ms` | i64 ms | wall-clock when the request arrived |
| `decided` | optional `(approved\|denied, decided_at_ms)` | set on operator action |
| `approved_bundle` | optional `{cert+DEK+HK+relay_url+tg_pk}` JSON | cached at approve time; consumed on the requester's first successful `/check` poll |

Records are dropped immediately on `/check` consumption (success
or denial) and on `/api/access/revoke/{device_pk}` (the operator
explicitly removing a pending entry). A dashboard restart drops
every pending request — the viewer must re-submit. There is no
expiry timer in v0.3; an unattended pending request stays in the
queue until the operator decides or the dashboard restarts.

### 3.4 Relay endpoint — delivered post-approve, not in the request

The relay URL plays no part in the enrolment handshake. After the
operator clicks Approve, the dashboard issues the cert and
includes its configured `--relay-url` (if any) in the
`approved_bundle` (§3.3, §4.2). The viewer reads it from the
`/api/access/check/{device_pk}` response and persists it
alongside the cert per §6.

When `--relay-url` is empty, the `relay_url` field **MUST** be
omitted from the approve response. The viewer is then LAN-only
on this deployment.

**Rationale.** Identical to v0.2: keeping the relay out of cert
issuance makes the relay a pure forwarding plane — it never has
opportunity to interpose during the moment the KeyHolder is
signing. The v0.3 simplification removes the last bit of relay
involvement in enrolment (the v0.2 invite-token QR could have
been transcribed off-LAN; v0.3 removes the token entirely so no
such path exists).

---

## 4. HTTP routes on the controller

The four routes below are added to `SPEC-R2-ROCKER-DASHBOARD §5.1`.
They formalise (and supersede) the stubs presently at
`dashboard/src/main.rs:642–645` (`/api/enrol-init`,
`/api/enrol-complete`).

| Route | Method | Body | Returns | Auth |
|---|---|---|---|---|
| `/api/access/request` | POST | `{device_pk: hex, name: str}` | `{ok: bool, device_pk: hex}` (202 on accept) | none — rate-limited per source IP |
| `/api/access/check/{device_pk}` | GET | — | 202 `{status:"pending"}`, 200 `{tg_pk_hex, encrypted_b64, paired_at_ms, relay_url?}`, 410 `{status:"denied"}`, 404 `{error:"no such request"}` | none — the device_pk is the lookup key |
| `/api/access/pending` | GET | — | `{requests: [{device_pk, name, hint, submitted_at_ms}]}` | KeyHolder-only |
| `/api/access/approve/{device_pk}` | POST | `{}` | `{ok: bool, device_pk: hex}` | KeyHolder-only |
| `/api/access/deny/{device_pk}` | POST | `{}` | `{ok: bool, device_pk: hex}` | KeyHolder-only |
| `/api/access/members` | GET | — | `{members: [{device_pk, name, role, paired_at, last_seen, revoked}]}` | KeyHolder-only |
| `/api/access/revoke/{device_pk}` | POST | `{}` | `{ok: bool, revoked_at}` | KeyHolder-only |

**v0.3 removes `/api/access/invite` and `/api/access/claim`.**
Implementations conforming to v0.3 **MUST NOT** expose those
routes (a stale 404 is the correct response; the routes do not
return 410-Gone aliases because there is no successor URL to
point at — the model is genuinely different, not renamed).

The legacy `/api/enrol-init` and `/api/enrol-complete` routes
remain as 501 stubs from earlier phases. They MAY be deleted at
any time; no implementation depends on them.

The existing `/api/keyholder/tg-pub` (`dashboard/src/main.rs:2754-2783`)
remains as-is — it returns the TG public key for any caller to
verify cert chains.

### 4.1 `/api/access/request`

The viewer's webapp POSTs to this route from the not-enrolled
landing page (§8). The dashboard:

1. Validates `device_pk` is 64 hex chars and decodes to a valid
   Ed25519 public key. On failure, returns 400.
2. Validates `name` (1..=64 chars, charset
   `[A-Za-z0-9 ._-]`, no leading/trailing whitespace). On
   failure, returns 400.
3. Enforces the per-IP rate limit (one new pending request per 5 s).
   Over the limit returns 429.
4. Inserts a `Pending` record per §3.3.
5. Broadcasts `{type:"access", event:"request_pending", device_pk,
   name, hint}` on `/ws/status` so the operator's Link tab
   refreshes immediately.
6. Returns `200 {ok: true, device_pk}`.

The route is **open by design**. Anyone on the LAN can POST a
request — they just end up in the operator's pending queue,
where the operator decides. There is no enrolment token to leak.

### 4.2 `/api/access/check/{device_pk}`

The viewer polls this every 2 seconds while the not-enrolled
landing page shows the "waiting for operator…" state. The
dashboard returns:

| Status | Body | Meaning |
|---|---|---|
| 202 | `{"status": "pending"}` | not yet decided — keep polling |
| 200 | `{tg_pk_hex, encrypted_b64, paired_at_ms, relay_url?}` | approved — viewer runs the join handshake (§6) |
| 410 | `{"status": "denied"}` | operator denied — landing page shows the denial state |
| 404 | `{"error": "no such request"}` | unknown device_pk (or already consumed; the dashboard treats these the same so a viewer that reloads after a successful enrol can detect "already done" and skip re-enrolment) |

Successful (200) consumption **MUST** drop the pending record
atomically so the second poll sees 404 — this is the spec's
single-use guarantee. The viewer's local `viewerIdentity` is the
authoritative copy from that point.

The 200 body includes `relay_url` if `--relay-url` was set when
the dashboard started, omitted otherwise (§3.4).

### 4.3 `/api/access/pending`

KeyHolder-only. Lists every currently-pending request so the
operator's Link tab can render the approve/deny row. Each entry
includes the requester's `device_pk`, `name`, `hint` (IP), and
`submitted_at_ms`. Items disappear from the list when the
operator decides (§4.4, §4.5) or when the requester consumes the
result (§4.2).

### 4.4 `/api/access/approve/{device_pk}`

KeyHolder-only. The dashboard:

1. Looks up the pending request by `device_pk`. If absent,
   returns 404.
2. Generates a transient `JoinCode` and runs
   `r2-trust::TrustGroup::process_join_request` with the
   requester's `device_pk` and the cached `name`. The cert is
   signed and the encrypted `(DEK, HK)` bundle is produced.
3. Caches the `(tg_pk_hex, encrypted_b64, paired_at_ms, relay_url?)`
   bundle in the same pending record (now in `Approved` state per
   §3.1) — the viewer's next `/check` poll consumes it.
4. Adds the new member to the persistent `TrustGroup::members`
   via `r2-trust/src/persist.rs`.
5. Broadcasts `{type:"access", event:"request_approved",
   device_pk}` on `/ws/status` so the operator's Link tab pulls
   the new row from `/api/access/members`.
6. Returns `200 {ok: true, device_pk}`.

If the request was already approved (re-click), returns 409.
If it was denied, returns 409. If `device_pk` decodes invalid,
returns 400.

### 4.5 `/api/access/deny/{device_pk}`

KeyHolder-only. Looks up the pending request, marks it `Denied`,
broadcasts `{type:"access", event:"request_denied", device_pk}`
on `/ws/status`. The next `/check` from the requester returns
410. The pending record is dropped on that next poll (or
immediately if the operator preferred to clear it via a future
"clear denied requests" affordance — out of scope for v0.3).

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

A viewer holds a `DeviceCertificate` (and optionally the relay
URL, per §4.2) after the operator has approved its request. It
can reach the dashboard in two ways. The spec defines the
*behaviour at the path boundary*; the wire protocol of each leg
lives elsewhere.

**Phase order matters.** Enrolment (§3, §4.1–§4.5) happens
once, on the LAN, while the new viewer's browser is on the
controller's hotspot. The two paths below are post-enrolment
*re-connection* paths: a viewer with a cert can use either,
choosing whichever is reachable. A viewer **without** a cert has
no business on the relay path — it has nothing to authenticate
with — and a viewer that needs a cert MUST come back to the LAN
to submit a request.

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

### 5.2 Off-network (relay) — post-enrolment only

The viewer loaded the webapp from the static host
(`https://reality2-ai.github.io/r2-rocker/`) and is not on the
controller's hotspot. Its WASM hive connects to the operator's
configured R2 relay (from the relay URL the viewer received in
its claim response and persisted alongside its cert per §6) and
the dashboard's relay-side state forwards encrypted blobs.

A viewer that never enrolled has nothing to present here — the
relay session below requires a `DeviceCertificate` that only the
controller can issue, and the controller only issues certs over
the LAN-bound `/api/access/claim` route (§4.2). The static-host
landing page **MUST** detect "no persisted cert in IndexedDB" and
render the not-enrolled state, which tells the operator to come
back on the lab WiFi to enrol — not to keep trying the relay.

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
off-network path is not available in this deployment. Claim
responses (§4.2) omit `relay_url`; the operator and viewers are
expected to be co-located on the hotspot.

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

1. **An "Onboard a visitor" affordance**. Visible only when the
   local device is the KeyHolder. When clicked, the webapp opens
   a static modal containing two QR codes for the operator to
   show to the visitor:
   * **Top QR: WiFi-join** (`WIFI:S:<SSID>;T:WPA;P:<password>;;`
     format). Only rendered when the dashboard was started with
     `--wifi-config` pointing at a readable creds file (or
     NetworkManager exposes them); omitted otherwise.
   * **Bottom QR: dashboard URL** — a plain
     `http://<controller_lan_ip>:8080/` URL with **no token and
     no parameters**. Scanning it opens the not-enrolled landing
     page; the visitor enrols by typing a name and clicking
     "Ask to pair" (§4.1).
   * Neither QR has an expiry; they're operator helpers, not
     time-limited tokens. The same modal MAY be opened any
     number of times without state change on the dashboard.
   * A close button dismisses the modal. There is no countdown.

2. **A "Pending requests" panel** (visible only when there is at
   least one pending request, or always — implementation choice).
   Polls `/api/access/pending` on Link-tab open and refreshes on
   each `request_pending` / `request_approved` / `request_denied`
   `/ws/status` broadcast. Each row shows:
   * Requester name (chosen at request time).
   * `device_pk` short form (first 8 hex chars).
   * Source-IP hint, so the operator can correlate with a
     physical device when several requests arrive at once.
   * Two buttons — **Approve** and **Deny**. Single click; the
     `request_approved` / `request_denied` broadcast removes the
     row.

3. **A list of currently-paired devices**, one card per device:
   * Device name (operator-chosen at request time).
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

4. **No other elements in v0.3**. Specifically, the operator
   does NOT see TG signing key paths, cert byte-blobs, relay
   debug, R2-TRUST schema versions, or any other internal state.

Non-KeyHolder viewers visiting the Link tab see the device list
(read-only, no Revoke buttons, no Approve/Deny buttons, no
Onboard affordance) and their own card marked "this device". The
list is the same shape per §4.3 (members list), just rendered in
read-only mode.

A viewer that has not yet enrolled (no cert in IndexedDB) **MUST**
render a single landing page in place of the regular tabs:

> *"This device hasn't been paired yet. Choose a name the
> operator will recognise, then ask them to approve it on their
> Link tab."*

with a name input and an "Ask to pair" button. On click, the
webapp generates a device keypair (via WASM), POSTs
`/api/access/request` (§4.1), and transitions to a "⏳ Waiting
for the operator to approve <name>…" state. The page polls
`/api/access/check/{device_pk}` every ~2 seconds; on success
(200) it runs the join handshake (§6) and transitions to the
enrolled state.

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

1. The seven `/api/access/*` routes in §4 (request, check,
   pending, approve, deny, members, revoke) are implemented with
   the payload shapes shown.
2. The routes `/api/access/invite` and `/api/access/claim` are
   **not** exposed.
3. The KeyHolder-only routes refuse non-KeyHolder callers. In
   v0.1 where there is no per-route auth check, the dashboard
   MAY rely on the localhost / private-LAN boundary; in v1 with
   cert-handshake on `/ws/raw`, the routes **MUST** also be
   cert-gated.
4. `/api/access/request` is rate-limited per source IP per §4.1.
5. `/api/access/check/{device_pk}` returns the approved bundle
   exactly once: the second poll for the same `device_pk` after
   a successful 200 MUST return 404.
6. `/api/access/approve/{device_pk}` runs `process_join_request`
   synchronously and includes the configured `relay_url` (if any)
   in the cached approved bundle, per §3.4.
7. `/api/access/members` returns the persisted member list
   including revoked rows.
8. `/api/access/revoke` adds to the revocation G-Set, broadcasts
   `r2.dash.access.revoked`, and tears down the offending
   connection synchronously.
9. The KeyHolder cannot revoke itself (§4.4).

### 11.2 Webapp

A webapp build conforms when:

1. On first load, if no cert exists in IndexedDB, it renders the
   not-enrolled landing page (§8) and accepts a name + an "Ask
   to pair" click that POSTs `/api/access/request` with a
   freshly-generated `device_pk`.
2. After submitting the request, it polls
   `/api/access/check/{device_pk}` every ~2 seconds until the
   response is 200 (approved), 410 (denied), or 404 (lost).
3. On 200, it runs the join handshake (§6), persists the cert +
   key + DEK + HK + `relay_url` in IndexedDB under
   `r2-rocker-access > members > self`, and transitions to the
   enrolled view.
4. On subsequent loads with a valid IndexedDB record, it uses
   the persisted identity and does not re-enrol.
5. It renders the "Link" tab per §8, including the Onboard
   modal, the pending-requests panel, the device list, and (when
   applicable) the not-enrolled landing page.
6. It listens for `r2.dash.access.revoked` on `/ws/status` and,
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
| QR code carrying enrolment token | REMOVED in v0.3. Earlier drafts had it; v0.3 §3 replaces it with request/approve. The Onboard modal still shows two QRs (WiFi join + plain dashboard URL) but neither carries a token. |
| Shareable URL with token | REMOVED in v0.3 — see above. |
| 3-word code | REMOVED in v0.3 — no token, nothing to derive words from. |
| 5-minute token expiry | REMOVED in v0.3 — no token, no expiry. Pending requests live until the operator decides or the dashboard restarts. |
| Single-use guarantee | §4.2 — included as "single-use cert delivery": the `/check` response is dropped from the pending record on first successful (200) poll. |
| Browser keypair generation | §6 — included. |
| Cert + DEK + HK in localStorage | §6 — moved to IndexedDB. |
| Operator-initiated invite | REMOVED in v0.3 — replaced by viewer-initiated request + operator approve. |
| Viewer-initiated request | §4.1 — **new in v0.3**, sole enrolment entry point. |
| Operator approve/deny buttons | §4.4, §4.5, §8 (2) — **new in v0.3**. |
| Relay-mediated **enrolment** | DEFERRED / OUT OF SCOPE. Notekeeper has it; r2-rocker v0.2+ explicitly does not (§3.4 *Rationale*). |
| Relay-mediated **post-enrolment connection** | §5.2 — included; relay wire protocol cross-referenced to R2-TRANSPORT. |
| Word-code → join-code relay lookup | OUT OF SCOPE — no word-code in v0.3. |
| Member list view | §8 (3) — included. |
| Revoke button | §4.4 + §8 (3) — included. |
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
