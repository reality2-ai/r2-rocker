---
title: r2-rocker — architectural conformance audit (gaps open since 2026-05-07)
date: 2026-05-23
auditor: Claude Opus 4.7 (1M context), commissioned by roy.c.davies@ieee.org
session: design session 05 (Phase 5 access + relay + cross-origin work)
status: open; closure roadmap in `/home/roycdavies/.claude/plans/sleepy-snuggling-tome.md`
scope: |
  Four architectural conformance gaps surfaced during a one-line review
  of the rig at v0.1.5. Orthogonal to the 2026-05-18 audit, which
  covered *wire-level* deltas (time-sync, ACK, SD ring, log_tcp,
  firmware HTTP, cycle_hotspot). That audit concluded
  "v0.1.5 wire-conformant"; this one says "v0.1.5 *architecturally*
  partial." Specifically:

    A. sensor ↔ controller events
    B. controller ↔ browser events
    C. who serves the webapp / where the WASM hive lives
    D. trust-group membership shape

  Findings A/B/C/D have been carried since the 2026-05-07 audit. They
  are not new in v0.1.5 — but they are the only architectural gaps
  remaining, so it is now cheap to enumerate them in one place.
---

## 1. Statement of scope

This audit is **orthogonal** to `2026-05-18-post-v0.1.0-conformance.md`.
That audit checked whether each new event introduced since v0.1.0
matched its spec at the byte level. It concluded: **v0.1.5 is
wire-conformant.** This audit checks four older architectural claims
about how the system is *shaped* — what travels over R2 events vs.
HTTP/JSON, who is a Trust Group member, where the hive abstraction
lives. Those questions have been open since 2026-05-07.

The findings here are not regressions and they do not affect the
research goal (D-07 / D-08 differential lateral motion across
joints, per `PROJECT.md`). They affect the project's ability to
claim "r2-rocker is a worked R2 hive ensemble." The university
handoff cohort will read both audits in order; this one tells them
which architectural decisions are still load-bearing as TODOs.

---

## 2. Findings

### Finding A — sensor ↔ controller is fully R2-WIRE

**Status:** ✓ conformant.

The sensor side emits R2-WIRE compact frames over TCP:21042 carrying
FNV-1a-32 event hashes + deterministic-CBOR payloads:

* `firmware/esp32-s3/devkitc/src/wire.rs:36-55` — event-hash table
  (`r2.sensor.announce`, `.acceleration`, `.battery`, `.status`,
  `.event.log`, `.sync_pong`, `.capture.state`).
* `firmware/esp32-s3/devkitc/src/sender.rs::send_announce` — CBOR
  encoder + Ed25519 signature over keys 0–5.
* `dashboard/src/main.rs::run_event_listener` —
  consumes the same frame format on the controller side.

Dashboard-originated events to sensors use the same wire format:
`r2.dash.ack`, `r2.dash.sync_pulse`, `r2.dash.set_clock_offset`,
`r2.dash.capture.{start,mark,stop}`, `r2.dash.identify_set`. See
`dashboard/src/main.rs::build_dash_frame` and the firmware's
`sender.rs::dispatch_inbound`.

Wholly R2 on this leg. No gap.

### Finding B — controller ↔ browser is mixed (≈30 non-R2 paths)

**Status:** ✗ architecturally non-conformant for the operator plane;
the data plane (`/ws/raw`) is fine.

| Channel | Conformance | Notes |
|---|---|---|
| `/ws/raw` | ✓ R2-WIRE | Carries the same R2-WIRE binary frames the sensors send, wrapped in `encode_raw_frame_envelope` (`dashboard/src/main.rs:3178`). Decoded by the webapp via `wasmDecodeFrame` + `handleEvent`. |
| `/ws/status` | ✗ ad-hoc JSON | ~7 message types invented per-feature: `bootstrap`, `ota`, `reset`, `capture`, `access`, `device_alias`, `peer_disconnected`. See `dashboard/src/main.rs` (9× `ws_broadcast_tx.send(serde_json::json!(...))`). Webapp dispatches on `type` in `statusWs.onmessage`. |
| `/api/*` | ✗ HTTP REST | ~25 routes. Operator actions (capture start/mark/stop, identify, reset, OTA push, access approve/deny/revoke, device-alias set/get, bootstrap, etc.) ride HTTP. The dashboard's Rust handler often *translates* into R2-WIRE downstream — e.g. `POST /api/capture/start` → `build_dash_frame(DASH_CAPTURE_START)` fan-out — but the operator↔controller leg is HTTP. |

**Concrete count:** ~25 HTTP routes + 7 `/ws/status` message types ≈
**~32 non-R2 operator-plane paths.** Enumeration available at
the time of writing in the conversation archive
(`2026-05-23` session, "Enumerate dashboard HTTP/WS control plane"
agent run).

**What is NOT a gap** (per the 2026-05-18 audit Pass 5):

* `/api/firmware/available`, `/api/firmware/{carrier}/binary` —
  GitHub-releases proxy. Dashboard-internal helper. No R2 spec
  governs it.
* `/ws/logs/{addr}` — per-sensor log-TCP proxy (port 21045 → WS).
  Dashboard-internal helper; the *underlying* log_tcp wire is not
  R2 either, and that's accepted per the 2026-05-18 audit.
* `/api/version`, `/api/devices/aliases` (GET only) — read-only
  metadata queries. No R2 analogue is required.
* `/api/enrol-init`, `/api/enrol-complete` — stubs from a prior
  phase, return 501. Will be deleted as dead code.

Excluding the carve-outs, **~20 routes + 7 status types ≈ 27 paths
that would need to migrate** to close this gap.

### Finding C — the WASM hive does not serve the webapp

**Status:** ✗ the claim "the WASM hive in the browser serves the
webpages" is wrong-as-stated. The reality is more nuanced.

What's actually true:

* **The dashboard process serves the webapp.** `dashboard/src/main.rs:845`
  mounts `webapp/` via `tower_http::services::ServeDir`. The Rust
  + axum process is the HTTP host. The browser fetches
  `index.html` from it.
* **The browser receives the static files + a WASM module**
  (`webapp/pkg/r2_wasm_bg.wasm`). The WASM module is a *library* —
  it exposes FNV, CBOR, frame decode, signature verify, cert
  ops, the `R2Member` type, and an `R2Hive` struct.
* **The `R2Hive` struct exists** (`crates/r2-wasm/src/hive.rs:20-49`)
  with `new`, `send_event`, `drain_outbound`, `tick`. It wraps
  `r2-engine::EventBus`. It is the right abstraction for "browser
  tab is an R2 hive."
* **The rocker webapp does not yet drive it.** `webapp/index.html`
  uses r2-wasm *primitives* (frame decode, FNV, etc.) but does
  not instantiate `R2Hive` or run its `tick()` loop. JS owns the
  peer table, capture-state aggregation, and event dispatch.
  `r2-notekeeper` is the reference deployment of `R2Hive`;
  r2-rocker is "Phase 5d, in progress" (task #20).

**Gap classification:** half is documentation (the claim
"WASM hive *serves* webpages" is a category error — file serving
and hive eventing are different roles), half is implementation
(the browser hive abstraction exists but isn't deployed in the
rocker webapp yet).

### Finding D — sensors are not formal Trust Group members

**Status:** ✗ partial. Controller + paired browser viewers are
formal members; sensors are cryptographically self-asserting under
TOFU.

| Participant | Cert? | Source |
|---|---|---|
| Controller (dashboard process) | KeyHolder | `dashboard/src/access.rs::Access::tg_signing_key` loaded from `tg_priv.bin` per `SECRETS-POLICY.md`. Formal TG member. |
| Browser viewers (paired) | Member | KeyHolder-signed `DeviceCertificate` issued via `/api/access/request → approve` flow, persisted in IndexedDB. Formal TG members. |
| Sensors | **none** | `firmware/esp32-s3/devkitc/src/identity.rs:73` generates a per-device Ed25519 keypair on first boot, persists in NVS. `sender.rs::send_announce` signs the announce. The dashboard verifies the signature in `dashboard/src/main.rs::verify_announce_signature` under **TOFU** — every valid-signed announce is accepted, regardless of pk-pinning history. |

**Concrete impact today:** a compromised sensor could announce
itself as any device_pk it wants, signing each announce with the
matching key. The dashboard would accept all of them. There is no
TG-anchored authority that says "this device_pk is the real
sensor-47." This is what AI-CONTEXT.md §3 calls "TOFU policy in
v0.1: log-only" and explicitly defers until "all sensors are
r2-rocker-spec firmware" (`dashboard/src/main.rs:1536`).

The cryptographic primitive to close this gap already exists:
`crates/r2-trust/src/lifecycle.rs:257-313` (`process_join_request`)
is transport-agnostic, has no browser-specific assumptions, and
produces the same encrypted bundle a sensor could consume over
BLE L2CAP that a browser consumes over HTTP. See the
`2026-05-23` "sensor cert + BLE bootstrap gap" agent run for the
detailed feasibility assessment: ~195 firmware + ~80 dashboard
LoC, no RAM/flash blocker.

---

## 3. Wire conformance vs. architectural conformance

The 2026-05-18 audit established that every R2-WIRE event flowing
on the wire today matches its spec. **v0.1.5 is wire-conformant.**

This audit's findings are about *architectural* shape — what
travels over R2 events vs. ad-hoc JSON / REST, who is a TG member,
where the hive abstraction lives. These are real gaps but they do
not break interoperability with other R2 implementations on the
wires that exist. They affect:

1. Future R2 deployments that want to reuse r2-rocker pieces (e.g.
   would need to write their own operator-plane glue rather than
   reusing event handlers).
2. The claim "everything is in the same TG" (Finding D).
3. The claim "the browser tab is an R2 hive" (Finding C).
4. Audit traceability: which message a particular UI change
   corresponds to. Today it's a function name and a string in
   `serde_json::json!`. Tomorrow (post-B+C) it would be an
   FNV-1a-32 hash with a canonical event-name table entry.

---

## 4. Cross-reference table — what closes what

| Finding | Closed by | Effort | Status |
|---|---|---|---|
| A — sensor↔controller | already conformant | — | ✓ closed |
| B — `/ws/status` JSON + `/api/*` REST | Tracks B+C (one continuous arc) | ~2 weeks | open |
| C — WASM hive doesn't drive the webapp | Track D (browser runs `R2Hive`) | 3–5 days | open |
| D — sensors not TG members | Track A (cert issuance at BLE bootstrap) | 2–3 days | open |

Tracks A/B/C/D map to deliverables A–D in
`/home/roycdavies/.claude/plans/sleepy-snuggling-tome.md`. Track E
(BRIDGE §10 two-TG split + bridge sentant) is **deferred** — see
that plan's *Out of scope* section. Track F is hygiene drain from
the 2026-05-18 audit (port 21045 collision, 200 ms status ack,
`ts_ms`-is-uptime drift, status-enum drift); F is unrelated to
A/B/C/D but rolls into the same release boundary.

Recommended sequence: A0 (this audit) → F → A → D → B+C.

---

## 5. Cross-links

This audit replaces three TODO-shaped paragraphs scattered across
the spec tree. Those rows are left in place but cross-link here:

* `PLAN.md` row 5d (WASM webapp transition) — references this
  audit's Finding C.
* `specifications/SPEC-R2-ROCKER-BRIDGE.md` §1.4 v0.1-reality
  paragraph — references this audit's Finding D (single-TG with
  role tags is the carried debt; Track E in the plan would close
  it; deferred).
* `specifications/SPEC-R2-ROCKER-ACCESS.md` §2.4.1 (single-TG
  reality) — references Finding D; also references Track A as
  the partial closure (sensors gain certs; viewers stay in the
  same TG until Track E).

The cross-links should be added when the next commit touches
those files — not retroactively as a churn.

---

## 6. Auditor's recommendation

* **Land A0 (this audit)** as the canonical citation target for
  every later commit. Cheapest deliverable in the roadmap.
* **Land F** (2026-05-18 hygiene findings) next; the 2026-05-18
  audit explicitly recommends closing them before university
  handoff and they are unrelated to A/B/C/D.
* **Land Track A** next. Highest legitimacy-per-LoC gain.
  Closes Finding D for sensors. Additive on the wire.
* **Land Track D** next. No prereqs, sets up B+C, no wire break.
* **Land B+C as one continuous arc.** Versioning boundary
  (v0.2.0). Begin with an event-name design call against
  BRIDGE §3.1 so a future Track E doesn't force a second wire
  break.

The roadmap (`sleepy-snuggling-tome.md`) carries the detailed
deliverables, file paths, verification steps, and an honest
assessment of the "research-goal-sufficient" subset vs. the
"full R2 reference deployment" subset.

---

## 7. What is NOT a gap

For completeness, recording the carve-outs so future audits don't
re-litigate them:

* `dashboard/src/main.rs::ServeDir` mount of `webapp/` is not a
  conformance gap. R2 specifies the event protocol, not the
  webapp transport. HTTP-served static assets are fine.
* The relay (`relay.reality2.ai`) and its WSS API are governed
  by R2-TRANSPORT, not r2-rocker. r2-rocker uses the relay
  unchanged.
* `/api/firmware/*`, `/api/version`, `/api/devices/aliases` (GET),
  `/ws/logs/{addr}` are dashboard-internal helpers without R2
  spec governance. They stay as HTTP/WS-JSON by design.
* The R2-WIRE compact frame format itself (12-byte header + CBOR
  payload + optional Ed25519/HMAC signature) is unchanged by
  this audit and remains the canonical wire format.
* `log_tcp` on port 21045 — flagged as Finding F by 2026-05-18,
  not by this audit; it is project-local debug, not R2 spec.

---

*Findings recorded by Claude Opus 4.7 (1M context) during the
design session 05 review. Closure tracked in
`/home/roycdavies/.claude/plans/sleepy-snuggling-tome.md` and in
the conversation archive of session 05.*
