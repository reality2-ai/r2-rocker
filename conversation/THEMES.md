---
title: r2-rocker — thematic conversation analysis
date_range: 2026-05-06 → 2026-05-24
status: thematic synthesis (replaces per-session raw transcript convention)
participants:
  - user: roy.c.davies@ieee.org
  - ai: Claude (Anthropic), Opus 4.7 (1M context)
purpose: |
  Primary research record for r2-rocker. Replaces the original per-session
  verbatim-transcript convention. Each theme draws from raw transcripts
  (where present — sessions 01 + 02), formal audits, binding decisions
  (PLAN.md §4), curated memory entries, and the git change log. The
  organising principle is *what the project learned*, not *what was said
  on what date*.

  Material for the paper / university handoff.
---

# r2-rocker — thematic conversation analysis

> **Reading note.** This document is a thematic synthesis of the
> entire r2-rocker development conversation, 2026-05-06 → 2026-05-24.
> The earlier convention was per-session verbatim transcripts in
> `conversation/YYYY-MM-DD-design-session-NN.md`; only sessions 01
> and 02 (2026-05-06–07) were ever archived in that form. From
> 2026-05-24 the convention is this single thematic document, kept
> up to date as new themes emerge, with the raw session-01/02 files
> preserved as primary source.
>
> Each section names the **decision or pattern**, then cites where
> in the repo the canonical record lives (binding decision, spec,
> audit, memory entry, or PLAN row), so a reader can trace from
> theme back to source.

## Source inventory

| Source | Span | Role |
|---|---|---|
| `conversation/2026-05-06-design-session-01.md` | 2026-05-06 | Raw transcript — initial scoping, hardware choices, Phase-0 specs. |
| `conversation/2026-05-07-design-session-02.md` | 2026-05-07 | Raw transcript — scaffolding through Phase 5a; first signed announce on the wire. |
| `audits/2026-05-07-conformance-audit.md` | 2026-05-07 | First conformance audit (wire ✅, architecture ⚠️). |
| `audits/2026-05-08-phase-6-9light-conformance.md` | 2026-05-08 | Phase 6 + 9-light conformance. |
| `audits/2026-05-18-post-v0.1.0-conformance.md` | 2026-05-18 | Post-v0.1.0 wire-level audit; hygiene findings F/G/H/I. |
| `audits/2026-05-23-architectural-gaps.md` | 2026-05-23 | Architectural-gaps audit (Tracks A/B/C/D). |
| `plan/PLAN.md` §4 + §8 | continuous | Binding decisions D-01…D-38 + change log. |
| Memory `/home/roycdavies/.claude/projects/-mnt-data-Development-R2-r2-rocker/memory/` | continuous | Curated per-session takeaways. |
| Git log on `hw/active-default-devkitc-v2` and `main` | continuous | What actually shipped. |

---

## 1. The arc in one paragraph

r2-rocker started (2026-05-06) as "instrument an ESP32-S3 with an
ADXL355 and stream into the existing Reality2 dashboard." Over
about three weeks it grew through hardware (DevKitC → XIAO →
DevKitC after parts availability shifted), through a wire-level
conformance pass (every R2-WIRE byte verified against
`r2-specifications`), through an SD-ring + capture + data-export
slice, through an ACCESS rewrite (invite/claim → request/approve),
through the v0.2.0 architectural-gaps closure (sensors as formal
TG members; status broadcasts as `r2.dash.*.progress`; operator
actions as `r2.dash.cmd.*`; webapp as a true R2 hive), and the
unified-port + peek-detect transport. The project is now a
working rig used for tyre-wear-test instrumentation, with a
university handoff target.

---

## 2. Theme: hardware path (carrier choices)

The hardware path tells a story of "field-availability beats
spec elegance" — twice.

**Initial choice (D-01 → D-06, session 01):** ESP32-S3-DevKitC-1
+ EVAL-ADXL355-PMDZ + microSD breakout + single-cell LiPo.
Reasons: ESP32-S3 is the R2-CORE-supported MCU; ADXL355 has the
noise floor (~25 µg/√Hz) the structural-health-monitoring task
needs; DevKitC has the GPIO header for hand-wiring.

**Carrier swap to XIAO (ADR-001, mid-arc):** Bench supply of
DevKitC ran short during a parts window; XIAO ESP32-S3 was
available, also ESP32-S3, mostly pin-compatible. Firmware tree
split into `firmware/esp32-s3/devkitc/` + `firmware/esp32-s3/xiao/`
— byte-identical drivers, parameterised pin literals. Single-
colour LED on the XIAO meant LED patterns had to carry state
without the RGB channel (memory: `xiao_led_single_color`).

**Revert to DevKitC (ADR-002):** Buck-boost regulator + SD
breakout arrived; DevKitC restored as the active default. Both
trees retained as parallel alternatives — the spec-and-build
machinery now treats "carrier" as a first-class axis.

**Lesson (D-37):** Heterogeneous-fleet support has to be a
deliberate spec slice. Today's "Pull → N sensor(s)" assumes one
carrier + one sensor type; mixed deployments need announce-time
variant tag, dashboard fleet-routing by variant, GH-release `.bin`
naming convention.

Canonical record: ADRs in `specifications/decisions/`,
HARDWARE-WIRING-{DEVKITC,XIAO}.md, memory
`project_heterogeneous_fleet_open_question`.

---

## 3. Theme: trust model maturation (TG → TOFU → cert chain)

The trust model walked from "embed the TG pub" to "every
sensor announce carries a KeyHolder-signed certificate".

**Phase 5a (D-19, session 02):** TG keypair generated by
`r2-rocker-tg`; `trust_keys/tg_pub.bin` + `tg_cert.bin` committed;
firmware embeds TG pub via `include_bytes!`. Per-device Ed25519
keypair generated in NVS on first boot; Ed25519 signature on the
announce body. End-to-end verified on MAC `1c:db:d4:41:28:3c`.

**Phase 5b (Track A part 1, v0.2.0):** Dashboard verifies the
announce signature against the device's pubkey from the announce
itself — TOFU. Adequate but not conformant with R2-TRUST's cert
model.

**Track A (architectural-gaps closure, v0.2.0):** Cert minted by
the KeyHolder at end-of-bootstrap, persisted in NVS, carried on
every announce; dashboard verifies cert chain under embedded TG
pk + announce signature under cert's `device_public_key`. Every
announce on `/r2` now shows `sig=ValidWithCert`. Legacy TOFU
branch removed. Closes Gap D in `audits/2026-05-23-architectural-
gaps.md`.

**Still pending — Phase 5c-hmac:** Per-frame HMAC envelope (DEK
+ HK derivation via HKDF-SHA256 from TG SK, per R2-TRUST §3).
Cert is on the channel; the per-frame integrity tag is the next
slice.

Canonical record: D-19, D-33, audits/2026-05-23-architectural-
gaps.md, SPEC-R2-ROCKER-ACCESS.md, SPEC-R2-ROCKER-WIRE.md §3.2.

---

## 4. Theme: webapp evolution (Axum-served HTML → R2 hive)

The webapp's identity changed twice.

**Original scope (D-21, session 01):** Vendor R2 crates into
`crates/`. Browser viewer eventually becomes a WASM hive
(r2-notekeeper / anthill shape). For v0.1 the dashboard process
serves HTML and decodes everything; the browser is a dumb pane.

**Reframe — "the webapp IS an R2 hive" (memory:
`project_webapp_is_a_hive`):** Each browser instance runs a TG-
member hive; role-from-cert chooses which sentants instantiate.
Not a UI client of the controller — a peer.

**Reframe — "the dashboard process is a plugin to the hive"
(memory: `project_dashboard_is_a_plugin`):** The web-server / HTTP
surface is one transport plugin under the hive. Default new
logic to sentants, not routes.

**Track D (v0.2.0):** `R2RockerHive` + `DashboardViewerSentant`
shipped. The sentant owns peer state, capture state, alias map,
LED phases, access flow; the page renders from sentant state on
every requestAnimationFrame.

**Tracks B+C (v0.2.0):** Operator actions migrated off ~17 `POST
/api/*` routes onto `r2.dash.cmd.*` events on `/r2`; status
broadcasts off the ad-hoc `/ws/status` JSON channel onto
`r2.dash.*.progress` events on the same socket. The surviving
HTTP routes are operator-helpers (multi-MB OTA push, capture-file
export, version check, alias bulk-fetch, ACCESS UI helpers).

**Track F (v0.2.0):** Single TCP listener on `:21042` with peek-
based protocol detection per R2-WIRE §13.5; first byte routes
raw R2-WIRE (`0x00…`) vs HTTP (`[A-Z]…`).

Canonical record: D-24, D-34, audits/2026-05-23-architectural-
gaps.md, SPEC-R2-ROCKER-WIRE.md §13.5, SPEC-R2-ROCKER-VIEWER-
SENTANT.md, dashboard/README.md, webapp/README.md.

---

## 5. Theme: ACCESS — invite/claim vs request/approve

The pairing flow was rewritten once.

**v0.2 design (initial 5d-enrol scope):** Operator clicks "Enrol
device" → dashboard mints a one-time, ≤5-min-expiry, KeyHolder-
signed join token → QR + shareable link `?join=<token>`. The
webapp on first load reads the URL, generates a keypair, presents
public_key + token to the KeyHolder via the relay, gets a cert.

**Why it was rewritten (D-30, ACCESS v0.3 — task #58):** Tokens
added complexity that the request/approve flow doesn't need. The
operator's approve click *is* the authorisation. Token minting,
expiry tracking, single-use enforcement, and the "what if the QR
is photographed off the operator's screen" attack surface all
fall away.

**v0.3 design (shipped v0.2.0):** Visitor opens the webapp on
the LAN, types a name, clicks "Ask to pair", which emits
`r2.dash.cmd.access.request`. The KeyHolder sees the request on
their Link tab and approves; the webapp polls
`r2.dash.cmd.access.check` every ~2s, picks up the approved
bundle (cert + key + DEK + HK + relay URL, JSON-stringified in
response key 5), persists in IndexedDB, becomes a TG member.

**Onboard QR helper retained** as a hand-friendly way to give
visitors the dashboard URL — no token, just the URL.

Canonical record: D-30, SPEC-R2-ROCKER-ACCESS.md, task #58.

---

## 6. Theme: capture / SD ring / data export

The data-flow design has three layers, deliberately decoupled.

**Layer 1 — SD ring (Phase 3, audit 2026-05-08):** Every sensor
writes 20-byte fixed records (`seq:u32, ts_ms:u32, x:i32, y:i32,
z:i32`) to a FAT-mounted microSD as ring segments. Boot recovery
hooks resume from the last-known `last_acked_seq` in NVS. SD is
the primary durable log (D-11); TCP is the near-real-time tap.
Ring overwrites oldest at full (D-15).

**Layer 2 — sender refactor (task #42):** Firmware writes to SD
first, sends from SD second, no ACK-loop coupling. Cumulative
ACK protocol every 200 ms or 100 samples (D-14); decouples
network jitter from sample integrity.

**Layer 3 — capture spec (SPEC-R2-ROCKER-CAPTURE.md, task #46):**
Operator-initiated lifecycle (start / mark / stop) coordinated
fleet-wide. `r2.dash.cmd.capture.start` (controller emits a
sync_pulse to align fleet clocks, then fans out
`r2.dash.capture.start` per sensor); `r2.dash.cmd.capture.mark`
carries an operator name and the dashboard-stamped `ts_ms`, so
every sensor uses the same canonical filename
`<ts16>-<name>.csv`; `r2.dash.cmd.capture.stop` closes the file.

**Data export:** `data_tcp` protocol on each sensor's port 21047
(LIST / GET / DEL / DEL_ALL); dashboard composes per-sensor
exports + a merged-fleet wide-format export
(`ts_ms, <dev>_x, <dev>_y, <dev>_z` per sensor in IP-sorted
order, blanks for missing samples). Filenames + CSV column
headers carry device-name stamps so multi-sensor exports stay
distinguishable.

**Lesson — operator-supervised, not unattended (D-29):** The
durability burden is lighter than the SPEC implies. Weight
"operator intervenes" over "system recovers autonomously". The
dashboard is the durable record; an offline operator can still
do the right thing.

Canonical record: D-11..D-15, D-29, SPEC-R2-ROCKER-CAPTURE.md,
audits/2026-05-08-phase-6-9light-conformance.md.

---

## 7. Theme: bugs found and the patterns behind them

Five field-bugs taught patterns worth recording — each became a
memory entry.

**(a) lwIP bind hangs pre-init** (`feedback_lwip_bind_pre_init_
hangs`). `TcpListener::bind` blocks indefinitely before WiFi/lwIP
is up. Pattern: spawn TCP listeners only after `wifi_sta::connect`
returns. Showed up during Phase 0L bring-up.

**(b) Keepalive via SockRef, not FD round-trip** (`feedback_
keepalive_via_sockref`). Setting TCP sockopts on a tokio stream
via `tokio → into_std → socket2 → from_std → tokio` corrupts the
stream after a preceding `stream.peek()`. Use
`socket2::SockRef::from(&stream)` — sockopt-on-borrowed-FD, no
ownership transfer. Showed up during port unification (Track F)
as sensors cycling every ~20s.

**(c) build.rs `rerun-if-changed` disables default** (`feedback_
build_rs_rerun_disables_default`). Emitting any
`cargo:rerun-if-changed` silently turns off cargo's per-package
change detection; must also list `src` + `.git/HEAD`/`index`/ref
or env-var stamps go stale. Surfaced during release-build version
stamping.

**(d) Pi5 acceleration rate-limit** (`project_pi5_acceleration_
rate_limit`). `/r2` fires every accel sample; Pi5 deployment
chokes. Pattern: server-side decimation BEFORE the broadcast
channel, not after — once it's in the channel it's too late.
1 kHz capture → 10 Hz wire.

**(e) Loopback as "streaming sensor"** (no memory; recorded in
this document). After port unification (Track F), `get_active_
sensor_ips` greps for r2-dashboard connections on `:21042` —
browser WS from `127.0.0.1` counted as "streaming sensor",
holding bootstrap in `scan-quiet 300s` mode. Pattern: filter
loopback in any "is there a peer streaming?" probe.

**Meta-pattern:** Each bug appeared at a *transition* (post-init
ordering, post-peek socket state, post-decoration cargo
detection, post-broadcast channel rate, post-listener
unification). Transitions are where invariants leak.

---

## 8. Theme: UX philosophy (three load-bearing rules)

Three UI rules emerged early and held:

**Rule A — calm-tech security UX** (`feedback_calm_tech_security`,
D-31). TG / cert / OTA / enrolment machinery is invisible by
default. One button equals one thing. Status arrives via ambient
signals (LED state, status dot). Expert mode is opt-in, not
default.

**Rule B — no R2 protocol jargon in user-visible strings**
(`feedback_ui_no_protocol_jargon`, D-38). The webapp uses task
language ("Connect Sensors", "Connection Log", "Link") — never
"TG" / "Bootstrap" / "Enrol" / "Trust Group" / "KeyHolder" on a
button. Code + specs keep canonical R2 terms.

**Rule C — accessible visual indicators** (`feedback_a11y_
indicators`, D-32). Pattern carries info alongside colour. Every
red / yellow / green state has a textual or iconographic
counterpart. Sub-errors use rhythm (LED flash count, blink
period) rather than shade. Validated by the XIAO single-colour
LED carrying the same patterns minus the RGB channel.

**Cosmetic consistency matters** (`feedback_cosmetic_
consistency`). Match toolbar chrome, padding, naming, layout
across tabs without being asked; small visual deltas add up
under operator cognitive load.

Canonical record: D-31, D-32, D-38, memory entries above,
SPEC-R2-ROCKER-VIEWER-SENTANT.md.

---

## 9. Theme: spec-first, conversation-as-research-data, secrets-out-of-repo

The three procedural conventions that have held since session 01
(D-22).

**Spec-first development.** Every code change has a driving spec
in `specifications/` first. The audits enforce this — when code
shipped ahead of spec (it did, occasionally), the next audit
flagged the divergence and the spec was updated retrospectively
or the code was reverted to match.

**Conversation as research data.** Originally: per-session
verbatim transcripts in `conversation/`. The intent was
defensible primary source for the eventual paper / university
handoff. Two sessions (01 + 02) were archived this way before
the convention broke; this thematic document is the replacement,
chosen 2026-05-24 because thematic synthesis serves the
"reconstruct the design arc" use case better than chronological
transcripts that no one will read.

**Secrets policy.** TG private key never enters the working
tree. `.gitignore` blocks `*_priv*`, `*.priv`, `*.key` as belt-
and-braces. Default tg_priv path is
`~/.config/r2-rocker/tg_signer/tg_priv.bin`.

Canonical record: D-22, PROCESS.md, SECRETS-POLICY.md.

---

## 10. Theme: operational reality (operator-supervised, controller-fixed)

Two framings that shifted scope significantly once recognised:

**Controller is fixed per experiment (D-28).** Chosen at setup;
if it dies the experiment restarts. Design for simple, not
seamless failover. This deletes a lot of HA scaffolding from the
"what if the controller dies mid-run?" hypothetical.

**Operator-supervised, not unattended (D-29).** Durability
burden lighter than the spec implies. The dashboard is the
durable record. Weight "operator intervenes" over "system
recovers autonomously". This justified, for example, leaving
some reconnection logic best-effort rather than building exhaust-
ive retry-with-backoff state machines.

**Battery test result (2026-05-18):** Streaming-only (no SD
writes), survived the night with ~30% left. SD+stream profile
not yet measured. Backs up the operator-supervised framing — the
rig isn't deployed for weeks unattended.

Canonical record: D-28, D-29, memory `project_operational_
supervised` + `project_battery_test_2026_05_18_result`.

---

## 11. Theme: the rocker exercises three R2 layers

Recognised mid-arc (memory: `project_rocker_exercises_r2_layers`)
as the project's contribution back to the broader Reality2 stack.

The rig is:

* The **first R2 entanglement deployment** — production TG
  (sensors + controller) + viewing TG (monitors), bilateral
  entanglement per R2-TRUST §7.5 (D-27).
* A **Transient-Networking real-world test** — sensors as TG
  members joining and leaving over BLE bootstrap → WiFi → cert
  chain.
* A **sentant-as-chokepoint pattern demonstration** — the
  `DashboardViewerSentant` owns operator-plane state for the
  webapp; the controller-side capture/access/bootstrap dispatch
  is a controller-side analogue. Gaps surfaced here may surface
  r2-core spec work.

Two-TG entanglement (Track E) is reserved for future
implementation. KeyHolder is a separable role from controller;
multi-KeyHolder + save/restore is operator-managed.

Canonical record: D-27, SPEC-R2-ROCKER-BRIDGE.md, memory entries
above.

---

## 12. Theme: research goal — capture training data, then on-line warning

The research framing (memory:
`project_catastrophic_joint_failures`).

v0.1 + v0.2 are the **data-collection phase** of a longer arc that
ends with a classifier warning the operator before joint failure.
v0.1+v0.2 priorities:

* **Sample fidelity** — clean, calibrated, time-synced data.
* **Low latency** — operator-loop responsiveness.
* **Black-box capture of failure events** — every failure becomes
  a training example.

This explains a lot of decisions:

* SD ring as primary durable log (D-11) — never lose a sample.
* Time-sync via Cristian's algorithm + calibration baseline + sync
  pulse refinement (D-20, SPEC-R2-ROCKER-TIMESYNC.md) — multi-
  sensor alignment matters for differential-motion metrics.
* Per-sensor calibration as Phase 7 (still pending) — classifier
  training on uncalibrated samples is training noise.
* Measurement sessions (Phase 8d, pending) — labelled-and-bounded
  data records for the eventual training set.

Canonical record: project memory above, SPEC-R2-ROCKER-TIMESYNC.md.

---

## 13. Theme: open questions carried

These don't have a resolution yet, deliberately. They're stored
as theme entries rather than open decisions because the rig
hasn't generated enough data to decide.

* **Sample-rate ceiling** — keep 100 Hz or bump to 200/500 Hz
  (Q-01).
* **Stress-indicator threshold per joint** — needs deployment
  data to baseline (Q-02).
* **OTA signing scheme** — TG group key vs per-device key (Q-03,
  Phase 9-secure).
* **When to add a second sensor per joint** (topology B → A)
  (Q-04).
* **Bed-sensor deployment** (mounting role = `bed`) (Q-05).
* **License** — paper / open source / private (Q-06).
* **Heterogeneous-fleet support** — needs a spec slice before the
  LoRa-bridged sensor arrives (D-37, task #57).
* **Pi5 + LoRa gateway architecture** — Pi 5 + RAK2287 vs Arduino
  Uno-Q + SX1302-USB (Phase 13).
* **Phase 5 relay-pairing bug carried** from 2026-05-18 — Anywhere
  pairing sends but doesn't reach the Link tab; cached webapp
  prime suspect; pick up next viewer-relay-leg session (task #62,
  memory `project_phase5_relay_pairing_blocker_2026_05_18`).

Canonical record: PLAN.md §5 + memory entries above.

---

## 14. Maintaining this document

* **When to update.** Whenever a session produces a new theme,
  retires an old one, or substantively changes an existing one.
  Not for incremental within-theme work — that goes in commit
  messages.
* **Convention for new themes.** Number sequentially; lead with
  the headline finding; cite the canonical record (binding
  decision, spec, audit, memory entry, or PLAN row).
* **Convention for retiring themes.** Mark with `~~strikethrough~~`
  and a `**Superseded:**` note pointing at what replaced it —
  don't delete; the paper / handoff reader needs to see what was
  *considered* and *retired*, not just what landed.
* **Relationship to memory.** Memory entries are operator-pace
  short-form takeaways; this document is the long-form synthesis
  built on top. Memory comes first; themes are reorganised memory.
* **Relationship to raw transcripts.** The session-01 + session-02
  files remain primary source. If a future session needs verbatim
  archival (e.g. an external review demands it), use the original
  `YYYY-MM-DD-design-session-NN.md` convention for that session
  only, and cross-link from this document.
