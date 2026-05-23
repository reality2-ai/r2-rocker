# webapp

The browser-side r2-rocker viewer — a full R2 hive running in WebAssembly inside the page.

## What this is

`index.html` boots an `R2RockerHive` (a thin wrapper around `crates/r2-wasm`'s `R2Hive`) and registers a `DashboardViewerSentant` (`crates/r2-rocker-viewer-sentant`). The sentant owns peer state, capture state, alias map, LED phases, and access flow; the page renders from sentant state on every `requestAnimationFrame`.

UI events (button clicks, form submits) emit `r2.dash.cmd.*` events on the unified `/r2` WebSocket; status broadcasts from the controller arrive on the same socket as binary R2-WIRE frames and are routed through the sentant's event handlers.

Track D ("webapp runs `R2Hive`") and Tracks B+C ("operator plane → R2 events") landed in v0.2.0 — there is no longer any `/api/*` polling or `/ws/status` JSON channel from the webapp's side.

## Build

The WASM bundle is built from [`../crates/r2-wasm`](../crates/r2-wasm) via `wasm-pack`, with `--out-dir` pointing back here so the viewer is a self-contained deployable:

```bash
wasm-pack build crates/r2-wasm --target web --release --out-dir ../../webapp/pkg
```

Output lands at `webapp/pkg/`. `index.html` imports from `./pkg/r2_wasm.js`.

## Run

The page must be served over HTTP (browsers refuse to load WASM from `file://`). The onsite controller serves this directory at the root of the unified R2 port:

```
http://localhost:21042/
```

For standalone webapp development without a running controller, any static-file server in this directory works (you'll see the not-enrolled landing page; pairing requires a real controller):

```bash
python3 -m http.server 8090   # then http://localhost:8090/
```

## Tabs

| Tab | Purpose |
|---|---|
| **Live** | Real-time accelerometer traces per sensor (1 kHz capture, decimated 100× on the wire). |
| **Devices** | Fleet status: connection state, run state, firmware version, "needs update" dot, OTA push, Reset, Identify. |
| **Data** | Per-sensor capture file browser — list, download (CSV with device-stamped header), delete, merged-fleet export. |
| **Capture** | Start / Mark / Stop the calibration capture; markers carry an operator name and dashboard-stamped timestamp. |
| **Connections** | Bootstrap log + scan-quiet timer; "Connect Sensors" button emits `r2.dash.cmd.bootstrap`. |
| **Link** | Access management — onboard a visitor (QR), pending requests, paired devices, revoke. KeyHolder-only Approve/Deny buttons. |

The "Link" tab is the operator-visible name for ACCESS — the spec, code, and event family keep the canonical `access` name (per ACCESS §8).

## Deployment

The bundle ships byte-identical to two hosts:

| Host | Role |
|---|---|
| **Onsite controller** (`:21042/`) | Served from the dashboard process for closed-network LAN deployments — no internet required. |
| **GitHub Pages** (`reality2-ai.github.io/r2-rocker/`) | Public/internet host for off-network viewers using the relay path (ACCESS §5.2). |

A viewer enrols by typing a name and clicking "Ask to pair" — the webapp emits `r2.dash.cmd.access.request`, the KeyHolder approves on their Link tab, and the approved bundle (cert + key + DEK + HK + relay URL) is persisted in IndexedDB. Subsequent loads pick up the cert silently.
