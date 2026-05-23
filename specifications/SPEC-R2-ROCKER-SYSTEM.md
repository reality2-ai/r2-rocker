# SPEC-R2-ROCKER-SYSTEM: System Architecture

**Version:** 0.1 Draft
**Date:** 2026-05-07
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-WIRE, SPEC-R2-ROCKER-SENSOR, SPEC-R2-ROCKER-DASHBOARD, HARDWARE-WIRING (reference)

---

## 1. Introduction

This specification is the **top-level architectural document** for the
r2-rocker system. It names the components, defines the network and
trust boundaries, describes the end-to-end lifecycle from provisioning
through decommissioning, enumerates failure modes and recovery
behaviour, and sets system-level conformance criteria.

Component-level normative content lives in the per-component specs;
this document references them as the authoritative source for each
domain.

### 1.1 Scope

In scope:

* System composition (which components exist, where they run).
* Trust boundaries and identity flow.
* End-to-end lifecycles (provisioning, deployment, operation, update,
  decommissioning).
* Data flow across components.
* Failure modes and system-level recovery.
* Reference deployment topology.
* System-level conformance (end-to-end integration tests).

Out of scope:

* Wire-level message formats (`SPEC-R2-ROCKER-WIRE`).
* Sensor firmware behaviour (`SPEC-R2-ROCKER-SENSOR`).
* Dashboard internals (`SPEC-R2-ROCKER-DASHBOARD`).
* Hardware reference design (`HARDWARE-WIRING.md`).

### 1.2 Terminology

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHOULD**,
**MAY** are interpreted per RFC 2119.

* **System** — the whole r2-rocker deployment: dashboard + N sensors +
  the rig they instrument.
* **Deployment** — one physical installation. There is exactly one
  dashboard per deployment.
* **Cold start** — first power-up or post-power-loss restart of all
  components.
* **Steady state** — every sensor calibrated, streaming, and connected
  to the dashboard.
* **TG** — trust group, defined by the keypair generated during
  provisioning (see PLAN D-19).

---

## 2. System overview

### 2.1 Components

| Component | Multiplicity | Where it runs | Authoritative spec |
|---|---|---|---|
| Sensor node | N (≥ 1, target: dozens) | ESP32-S3-DevKitC-1 + ADXL355 + microSD + LiPo | `SPEC-R2-ROCKER-SENSOR` |
| Dashboard | exactly 1 | Linux laptop or Raspberry Pi | `SPEC-R2-ROCKER-DASHBOARD` |
| TG signer | exactly 1 | Same host as dashboard (key off-tree) | `SECRETS-POLICY` |
| Browser UI | 1 or more | Any device on the dashboard's LAN/hotspot | `SPEC-R2-ROCKER-DASHBOARD` §12 |
| OTA artefact server | 1 | Same host as dashboard, or any reachable HTTP server | `SPEC-R2-ROCKER-DASHBOARD` §13 |

### 2.2 Component diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                       Deployment site                           │
│                                                                 │
│  ┌──────────────┐   BLE adv          ┌──────────────────────┐   │
│  │ Sensor #1    │ ──────────────►    │  Dashboard host       │   │
│  │ ESP32-S3 +   │ ◄── L2CAP ───────  │   - dashboard process  │  │
│  │ ADXL355 +    │ ──── WiFi ───────► │   - TG signer (key off-tree)│  │
│  │ microSD +    │ ──── TCP ────────► │   - WiFi hotspot AP    │   │
│  │ LiPo         │ ◄── ACK ──────     │   - R2 port :21042     │   │
│  └──────────────┘                    │     (HTTP + WS + raw)  │   │
│                                      │   - state on disk      │   │
│  ┌──────────────┐                    └──────────┬─────────────┘   │
│  │ Sensor #2    │ ─── (same flow) ───►         │                   │
│  └──────────────┘                              │ HTTP+WS (:21042)  │
│         …                                      ▼                   │
│  ┌──────────────┐                    ┌──────────────┐              │
│  │ Sensor #N    │                    │ Browser UI   │              │
│  └──────────────┘                    └──────────────┘              │
└─────────────────────────────────────────────────────────────────┘

      Trust group (hardwired):
        - TG public key embedded in every sensor firmware build.
        - TG private key held only on the dashboard host (off-tree).
        - Per-device Ed25519 keypair generated on each sensor's first boot,
          persisted in encrypted NVS, never leaves the device.
```

### 2.3 Network topology

The dashboard host SHALL operate two distinct network roles
simultaneously:

1. **WiFi access point** — sensors join this hotspot. Default subnet
   `10.42.0.0/24` (NetworkManager default). DHCP from the host.
2. **BLE central** — scans for sensor beacons, initiates L2CAP
   connections.

The dashboard host MAY simultaneously be connected to an external
network (e.g. site WiFi for internet, ethernet for SSH access) on a
different interface. The hotspot is dedicated to sensors.

Sensors SHALL NOT be configured to join the site's main WiFi — only
the dashboard's hotspot. This isolates the deployment from external
network conditions.

### 2.4 Trust model

Per PLAN D-19:

* The **trust group** is established once at provisioning by generating
  a fresh Ed25519 keypair (`tg_priv.bin`, `tg_pub.bin`).
* `tg_pub.bin` is committed to the repo at `trust_keys/`. Every sensor
  firmware build embeds it via `include_bytes!`.
* `tg_priv.bin` is kept exclusively on the dashboard host, off-tree at
  `~/.config/r2-rocker/tg_signer/tg_priv.bin`. The dashboard signs
  `#wifi_offer` frames with it.
* Each sensor generates a per-device Ed25519 keypair on first boot
  (`device_priv` in encrypted NVS, `device_pub` derived). The
  `device_pub` is the sensor's stable identity across reboots and
  across firmware updates.
* On TCP connect, the sensor signs `r2.sensor.announce` with
  `device_priv`; the dashboard verifies the signature using
  `device_pub` (TOFU policy in v0.1; future versions migrate to
  TG-signed device certificates per R2-TRUST).

The system has **two trust assumptions**:

1. The TG private key remains exclusively on the dashboard host.
2. Sensors' NVS encryption keeps `device_priv` secret against
   moderate-effort physical attack (chip removal + flash dump).

Compromise of either invalidates the corresponding part of the trust
graph; recovery is via key rotation (`SECRETS-POLICY.md` §key-rotation
or sensor factory reset, respectively).

Member device lifecycle (how new devices join, who can invite
them, what their certs look like, how a KeyHolder removes a
device that has been lost / compromised / decommissioned) is
specified in **`SPEC-R2-ROCKER-ACCESS.md`**. That spec is the
authoritative source for the Access tab in the dashboard, the
QR/link enrolment flow, and revocation propagation across the
fleet. Subsequent sections of this document (Lifecycle §3,
Failure modes §5) reference ACCESS where relevant.

---

## 3. Lifecycle

### 3.1 Provisioning (one-time, off-tree)

Performed by the project lead before any deployment exists:

1. Generate the TG keypair on the lead's signing host:
   ```bash
   r2-rocker-tg keygen --name "rocker-rig-uoa-2026" \
                       --priv ~/.config/r2-rocker/tg_signer/tg_priv.bin \
                       --pub  /tmp/tg_pub.bin
   ```
2. Copy the public key into the repo and commit:
   ```bash
   cp /tmp/tg_pub.bin <repo>/trust_keys/tg_pub.bin
   git add trust_keys/tg_pub.bin && git commit
   ```
3. Build sensor firmware against the embedded `tg_pub.bin`.
4. Flash each physical sensor with the build.

`r2-rocker-tg` is a small CLI utility under `tools/` that wraps
ed25519-dalek; it is part of the deliverable but runs only on the
signer host.

### 3.2 Cold start

On a fresh deployment site:

1. Operator boots the dashboard host. The dashboard process auto-starts
   via systemd (or is launched manually).
2. The dashboard binds the unified R2 port :21042 (HTTP + WS + raw
   R2-WIRE multiplexed via peek-based protocol detection per WIRE
   §13.5), brings up the WiFi hotspot, and becomes operational. State
   directory is empty (no peers yet).
3. Operator powers on each sensor. Each sensor:
   * Self-tests (`SPEC-R2-ROCKER-SENSOR` §2).
   * Generates a device key on first boot, persists it.
   * Begins BLE advertising R2-BEACON.
4. Operator opens the browser UI at the dashboard's hotspot IP
   (typically `http://10.42.0.1:21042`).
5. Operator clicks **Discover**; the dashboard runs the bootstrap
   engine (`SPEC-R2-ROCKER-DASHBOARD` §6).
6. Each sensor receives `#wifi_offer`, joins the hotspot, opens TCP to
   the dashboard, sends `r2.sensor.announce`.
7. Each sensor appears as a peer card in the UI.

Time from power-on to all sensors visible: ≤ 90 s for typical 2-sensor
deployment. Per-sensor cold-boot latency follows the M10 demo's
observation in `r2-core/demos/rocker-rig/README.md`.

### 3.3 Calibration & deployment commissioning

After all sensors are visible:

1. Operator confirms each sensor's mounting role (default `rocker`,
   editable in UI).
2. Operator assigns each sensor to a logical joint
   (`SPEC-R2-ROCKER-DASHBOARD` §10.1).
3. Operator runs the calibration wizard per sensor:
   * Position the rig at end-stop A; capture sample.
   * Move rig to end-stop B; capture sample.
   * Dashboard computes R, persists per-device.
4. Operator clicks **Start streaming** (per peer or for all).
5. Sensors enter `STREAMING_LIVE`; dashboard cards begin updating in
   real time.

The deployment is now in **steady state**.

### 3.4 Steady-state operation

In steady state:

* Sensors sample at the configured rate (default 100 Hz), persist to
  SD, stream live frames to the dashboard.
* The dashboard ACKs every 200 ms or 100 samples; sensors free SD
  segments accordingly.
* Battery events arrive every 30 s per sensor; the UI updates widgets.
* The analytics task computes 1 Hz stress indicators per joint;
  threshold breaches alert.
* Daily trend summaries are persisted to disk.
* A network blip causes the affected sensor to switch to
  `STREAMING_CATCHUP` until drained; no data is lost provided SD
  retention is sufficient.

### 3.5 OTA updates

To roll out a new firmware build:

1. Operator builds the firmware against the current `tg_pub.bin`.
2. Operator publishes the binary to a reachable HTTP endpoint
   (typically the dashboard host itself).
3. From the UI, operator triggers OTA per sensor (or in bulk).
4. The dashboard sends `r2.dash.fw.update {url, sha256}` to each
   target sensor sequentially.
5. Each sensor fetches, verifies, swaps partition, reboots, reconnects.
6. The dashboard's first-boot rollback condition (`SPEC-R2-ROCKER-SENSOR`
   §12.2) ensures bricked binaries auto-revert.

Rollout time for 30 sensors at ~2 MB binary, sequential: ≈ 5 minutes
on a 5 Mbit/s hotspot.

### 3.6 Key rotation (rare)

Per `SECRETS-POLICY.md` §key-rotation. If the TG private key is
compromised:

1. Generate a fresh TG keypair off-tree.
2. Commit the new `tg_pub.bin` to the repo, build new firmware.
3. Use the **old** TG to OTA-push the new firmware to all sensors.
4. After confirming all sensors have updated, destroy the old
   `tg_priv.bin`.
5. The brief overlap window where both keys are valid is the
   unavoidable cost of remote rotation.

If rotation cannot reach a sensor (powered off, physically
inaccessible), that sensor shall be physically re-flashed before
re-deployment.

### 3.7 Decommissioning

To retire a deployment:

1. Operator triggers `r2.dash.reset {factory: true}` on each sensor —
   clears NVS (device key + last_acked_seq + calibration absent on
   sensor anyway).
2. Operator stops the dashboard, archives the state directory and
   trends to long-term storage.
3. Sensors may be repurposed (factory-reset state is equivalent to
   first-boot) or wiped and disposed.

Long-term archival of the analytics trend is the deliverable to the
research record (paper-grade, per project goals).

---

## 4. Data flow

### 4.1 Sensor → dashboard (telemetry)

Per `SPEC-R2-ROCKER-WIRE`:

```
Sensor                                     Dashboard
──────                                     ─────────
ADXL355 → SPI burst read
       → SD ring (durable)
       → network task
              │ live mode: r2.sensor.acceleration   ──►  decode, scale,
              │             (1 frame / sample)            rotate (if cal'd),
              │ catchup mode: r2.sensor.acceleration.batch ─► WS push to browser
              │             (50 samples / frame)
              │
              ├ r2.sensor.battery (every 30 s)      ──►  battery widget
              ├ r2.sensor.status (on demand)        ──►  status display
              ├ r2.sensor.event.log (notable)       ──►  event log panel
              └ r2.sensor.cal.sample.resp (cal)     ──►  calibration handler
```

### 4.2 Dashboard → sensor (control)

```
Dashboard                                   Sensor
─────────                                   ──────
peer_handler:                                │
  r2.dash.ack (every 200 ms / 100 samples)  ──►  free SD ring up to seq N
  r2.dash.sync_pulse (1 Hz then 30 s)       ──►  reply with sync_pong
  r2.dash.cal.sample.req (operator-driven)  ──►  enter CALIBRATING, average, reply
  r2.dash.stream.start / stop                ──►  start/stop live emission
  r2.dash.config.set                        ──►  persist NVS, apply
  r2.dash.fw.update                         ──►  enter OTA, fetch, verify, reboot
  r2.dash.reset                             ──►  soft / factory reset
```

### 4.3 Persistence map

| Where | What | Lifecycle |
|---|---|---|
| Sensor NVS (encrypted) | `device_priv`, `device_pub`, `hostname`, defaults, `last_acked_seq`, `boot_count` | Survives reboots; cleared by factory reset |
| Sensor SD | `/r2/log.NNNN.bin` segments + `meta.bin` | Persists indefinitely; ring overwrites oldest at full |
| Dashboard state dir | `peers.json`, `calibration.json`, `joints.json`, `high_water.json`, `events.log`, `trends/` | Survives dashboard restart; archived at decommission |
| TG signer dir (off-tree) | `tg_priv.bin` | Off-disk-of-repo; backed up out-of-band by operator |
| Repo `trust_keys/` | `tg_pub.bin`, `tg_cert.bin` | Committed; embedded in firmware at build |

---

## 5. Failure modes & recovery

### 5.1 Single sensor offline

* TCP closes (network or power loss).
* Dashboard observes `STALE` after 10 s, then `OFFLINE` after 30 s.
* UI marks the peer offline; analytics involving that peer pauses
  (single-sensor joint metrics) or degrades to single-sensor mode
  (pair joint loses its differential).
* On sensor return: announce + ACK exchange resumes streaming from
  `last_acked_seq + 1`. Catch-up mode drains the SD-buffered samples.
* No operator action required.

### 5.2 Dashboard offline

* All sensors observe TCP failure within 5 s, transition to
  `ADVERTISING`.
* Each sensor continues sampling and writing to SD locally, paused
  on streaming.
* On dashboard return: bootstrap engine re-discovers; sensors rejoin
  hotspot, reconnect TCP, resume streaming.
* Data captured during the dashboard outage is preserved on each
  sensor's SD and replayed via catch-up mode on resume — provided the
  outage is shorter than the SD ring's retention (default ~14 hours
  at 100 Hz, 96 MiB ring).
* For outages longer than ring retention, the oldest samples are lost
  (overwrite-oldest policy per PLAN D-15). Operator SHOULD enlarge
  ring (NVS `ring_segments`) for longer expected outage tolerance.

### 5.3 SD card failure on a sensor

* Sensor enters RAM-buffered mode (≤ 1024 samples ≈ 10 s).
* If recovery within 30 s, samples flush to SD; no data loss.
* If no recovery: sensor enters `ERROR` state. Operator must replace
  the SD card and reboot.

### 5.4 Network partition (sensors split across two hotspots)

Out of scope for v0.1 — the design assumes one dashboard with one
hotspot. Multi-dashboard / federated deployment is future work.

### 5.5 Battery exhaustion

* At ≤ 3.3 V: `LOW_BATTERY` overlay, immediate battery event, continued
  streaming.
* At ≤ 3.1 V: sample rate reduced to 10 Hz.
* At ≤ 3.0 V: safe shutdown; CPU halts. The cell is **not charged on
  the board** — the operator unplugs the depleted cell at the JST-PH
  connector and connects a fresh charged one (per
  `HARDWARE-WIRING.md` §4).
* On cell replacement: full cold boot (§3.2), self-test, then resume
  per the resume path (`last_acked_seq + 1`).
* Charging happens **off-board** in a separate single-cell LiPo
  charging dock; depleted cells are rotated out and recharged out of
  band.

### 5.6 Time-sync degradation

* If `r2.sensor.sync_pong` round-trips become irregular (RTT variance
  high), the dashboard's offset estimate becomes noisy.
* Differential analysis on paired joints accepts up to 5 ms misalignment
  (half a sample period at 100 Hz); beyond that, `Δa` is dominated by
  alignment error, not real signal.
* If the dashboard observes alignment error consistently > 10 ms, it
  shall flag the affected joint and pause its differential metric
  (revert to single-sensor view) until sync recovers.

---

## 6. Security

Per `SECRETS-POLICY.md` and PLAN D-19. System-level summary:

### 6.1 Threat model

| Threat | Mitigation |
|---|---|
| External attacker on the LAN | Sensors connect only to the dashboard hotspot; the hotspot is private with WPA2-PSK |
| Rogue sensor (impersonation) | Announce signature verified against TG; v1.0 will require TG-signed device cert |
| Compromised dashboard host | TG private key compromise → key rotation flow (§3.6) |
| Stolen sensor (physical) | NVS encrypted; factory reset on dashboard side denies the device's prior identity |
| MITM on TCP between sensor and dashboard | Within hotspot only; not currently encrypted at the application layer (acceptable per PLAN scope; HMAC envelope future) |
| OTA-pushed malicious firmware | Hash verify (mandatory v0.1), TG sig verify (mandatory v1.0) |

### 6.2 Out-of-scope threats

* Side-channel attacks against the ESP32-S3.
* Sustained physical access to a sensor with chip-level analysis.
* Adversaries who can intercept the dashboard's hotspot RF.

These are deemed acceptable for the lab/test-rig deployment context.

---

## 7. Reference deployment topology

### 7.1 Recommended hardware

| Role | Recommended hardware | Notes |
|---|---|---|
| Sensor (×N) | ESP32-S3-DevKitC-1 + ADXL355-PMDZ + microSD breakout + 2000 mAh LiPo + TP4056 module | Per `HARDWARE-WIRING.md` |
| Dashboard host | Raspberry Pi 4 (4 GB) or Linux laptop | RPi: 64-bit Raspberry Pi OS Lite; laptop: Ubuntu 22.04+ |
| Dashboard WiFi | Internal RPi adapter for hotspot, OR USB WiFi for hotspot + internal for site network | Two adapters strongly recommended for SSH access during deployment |
| Operator browser | Any modern browser (Chrome / Firefox / Safari ≥ 2024) | Mobile responsive |

### 7.2 Operational requirements

* Dashboard host SHOULD run on AC power for steady-state monitoring;
  laptop battery is acceptable for short test sessions.
* Sensors MUST have charged batteries (>50%) at start of a measurement
  campaign; expected runtime at 100 Hz with active streaming is
  ~6–10 hours per 2000 mAh cell (firmware power consumption to be
  measured in Phase 5).
* The dashboard host SHOULD have at least 16 GB of free disk for
  long-term trend storage (assumes years of daily summaries +
  decimated raw archives).

### 7.3 Site survey checklist

Before each deployment:

- [ ] Confirm BLE adapter on the dashboard host functions (`bluetoothctl scan on`).
- [ ] Confirm hotspot adapter brings up via NetworkManager (`nmcli device status`).
- [ ] Confirm the rig's two end-stop positions are accessible for calibration.
- [ ] Confirm SD cards in all sensors are empty or known-good.
- [ ] Confirm all sensors' batteries are charged.
- [ ] Confirm the dashboard host has the correct `tg_priv.bin` for the
      firmware build flashed on the sensors.

---

## 8. Conformance

A system conforms to this specification when **all** of the following
end-to-end tests pass with reference hardware, reference firmware, and
reference dashboard:

### 8.1 Cold start to steady state (single sensor)

1. With one freshly flashed sensor and a fresh dashboard host, power on
   both. Open the UI.
2. Click Discover.
3. Within 90 s, the sensor card appears, calibration is offered, and
   on completing calibration + Stream Start, live samples appear in
   the chart.

### 8.2 Cold start to steady state (two sensors)

1. As §8.1 with two sensors.
2. Both sensors complete bootstrap in parallel within 90 s.
3. Per-joint stress indicators appear (single-sensor topology by
   default); UI shows two cards in the grid.

### 8.3 Resilience: dashboard restart

1. From a steady-state two-sensor deployment, kill the dashboard
   process.
2. Wait 30 s.
3. Restart the dashboard.
4. Both sensors reconnect within 60 s; no data is lost (verify by
   comparing pre- and post-restart `seq` continuity).

### 8.4 Resilience: sensor power-cycle

1. From steady state, power-cycle one sensor.
2. The sensor self-tests, advertises, is re-bootstrapped (without
   operator action: reuse path, since the prior hotspot is still up
   and credentials are in NVS), reconnects, resumes from
   `last_acked_seq + 1`.
3. Calibration is preserved (it lives on the dashboard, not the
   sensor).

### 8.5 OTA roll-out

1. Build firmware version N+1.
2. Trigger OTA on each sensor; all complete within expected time.
3. Each sensor's `r2.sensor.announce` after reboot reports `fw_ver` =
   N+1.
4. Calibration and `last_acked_seq` are preserved through the update.

### 8.6 Calibration math integrity

1. With a calibrated sensor placed level on the bench, the dashboard's
   computed `a_vertical` shall be ~ 1 g (within 5 %), `a_main` and
   `a_sideways` ~ 0 g.
2. With the sensor rotated 90° about the rig's main axis, `a_main`
   moves to ~ 1 g, `a_vertical` to ~ 0 g.

### 8.7 Differential analysis

1. With two sensors mounted on a rigid coupling, agitated together,
   the pair's `Δa_lateral` RMS shall be small relative to either
   sensor's individual `a_sideways` RMS (common-mode rejection).
2. With one sensor manually displaced, `Δa_lateral` shall increase
   visibly in the joint card.

### 8.8 Decommissioning

1. Trigger `factory reset` on each sensor.
2. Each sensor reboots, generates a fresh device key, advertises a
   fresh `device_pk`. Prior peer entries on the dashboard remain in
   `peers.json` with `last_seen` not updated (i.e. the dashboard
   knows the old identity is gone).

---

## 9. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-07 | 0.1 | Initial draft. Components, topology, trust model, lifecycle, data flow, failure modes, deployment, system-level conformance. |
| 2026-05-07 | 0.1.1 | §5.5 corrected: cell is removable (JST-PH); recharged off-board in a separate dock; recovery is cold boot on cell swap, not deep-sleep wake. |
