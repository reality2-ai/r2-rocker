# wasm-viewer

The browser-side r2-rocker WebApp — a full R2 hive running in WebAssembly.

This directory is being built up incrementally as Phase 5d lands (see
[`plan/PLAN.md`](../plan/PLAN.md)). Currently it contains a smoke-test
page that confirms the WASM pipeline works end-to-end.

## Status

* ✅ `index.html` — smoke test for `r2-wasm` (load, version, FNV-1a hash,
  encode/decode an R2-WIRE compact frame, generate a device keypair).
  Confirms the WASM build pipeline is alive.
* ⏳ Replace the smoke test with the actual viewer (live charts,
  Devices view, calibration wizard, joint-group editor, sessions).
* ⏳ Enrolment flow (QR / link / one-time token) per
  `AI-CONTEXT.md` § "Browser enrolment via QR / link".
* ⏳ Wire up to a relay-compatible WSS endpoint (initially the onsite
  controller's; later the r2-relay or our own combined relay+archive).

## Build

The WASM bundle is built from [`../crates/r2-wasm`](../crates/r2-wasm)
via `wasm-pack`:

```bash
wasm-pack build crates/r2-wasm --target web --release
```

Output lands at `crates/r2-wasm/pkg/`. The HTML in this directory
imports from there directly (`../crates/r2-wasm/pkg/r2_wasm.js`).

## Run the smoke test

The page must be served over HTTP (browsers refuse to load WASM from
`file://` URLs by default). Any static-file server in this repo's
root will do:

```bash
# Python's built-in works fine for testing:
python3 -m http.server 8090

# Then open:
#   http://localhost:8090/wasm-viewer/
```

You should see five rows of green ticks: load status, version,
`fnv1a_32` of `r2.sensor.acceleration`, a round-trip encode/decode of
a synthetic R2-WIRE frame, and a fresh device keypair.

## Deployment (eventual)

Per `plan/PLAN.md` Phase 5d, this directory ships the static viewer
bundle to **two hosts** — byte-identical:

| Host | Role |
|---|---|
| GitHub Pages | Public/internet WebApp host for remote viewers |
| Onsite controller | Same bundle on the local hotspot for closed-network deployments — no internet required |

Browser scans a QR (or follows a shared link) from the onsite
dashboard's Enrol-Device UI; the same WebApp opens and auto-enrols
the browser into the trust group.
