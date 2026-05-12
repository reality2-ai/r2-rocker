---
title: ADR-001 — Adopt Seeed XIAO ESP32-S3 (Pre-Soldered) as the sensor carrier board
status: Accepted
date: 2026-05-11
supersedes: none
superseded-by: none
---

# ADR-001 — Seeed XIAO ESP32-S3 as the sensor carrier board

## Status

**Accepted** — 2026-05-11.

## Context

Phases 0–2 were built on an **ESP32-S3-DevKitC-1**. Phase 2 (real
ADXL355 driver) was completed and bench-verified on that board at
commits `8a389ed` and `8fb1c4f`. The DevKitC firmware tree is preserved
unchanged at `firmware/esp32-s3/` and remains the known-good fallback.

Phase 3 (battery integration) hit a parts-availability wall in NZ/AU.
A single LiPo cell (3.0–4.2 V) cannot drive an ESP32-S3 directly: it
overvolts the 3V3 rail at full charge and undervolts at end of life.
The clean solution is a buck-boost (or SEPIC) regulator — Pololu
S7V8F3 / TPS63020 — but Jaycar (the practically-reachable retailer)
stocks none of them. Locally-available alternatives
(LM2596 buck, LM2936 LDO, XL6009 boost) each fail one of three
sanity checks: input range, dropout, or output current. The lowest-
friction workaround using Jaycar parts is an MT3608 boost-to-5 V
feeding the DevKitC's on-board LDO — functional but inefficient and
adds a component to the BOM.

The user has several development boards on hand that integrate the
power-management problem onto the carrier:

* **FireBeetle 2 ESP32-C6 V1.0** (DFRobot DFR1075) — on-board CN3165
  charger, regulator, battery sense pin (IO0). **But:** ESP32-C6 is a
  different SoC family (RISC-V single-core vs Xtensa LX7 dual-core),
  requiring a full Rust toolchain retarget from `xtensa-esp32s3-espidf`
  to `riscv32imac-esp-espidf`, plus ESP-IDF target change, partition
  table re-sizing for 4 MB flash, and verification that all esp-idf-*
  crates work on C6.
* **Seeed XIAO ESP32-S3 (Pre-Soldered)** — on-board LiPo charger,
  3.3 V regulator, USB-C, **same ESP32-S3 silicon** as the DevKitC.
  8 MB flash, 8 MB PSRAM, 21 × 17.5 mm form factor, ~14 µA deep
  sleep. The "Pre-Soldered" SKU ships with the 2.54 mm pin headers
  already attached — functionally identical to the regular XIAO
  ESP32-S3, just one fewer soldering step. No built-in battery
  divider — user adds an external 100 kΩ/100 kΩ to an ADC pin
  (matches what was already planned for the DevKitC).

## Decision

Adopt **Seeed XIAO ESP32-S3** (Pre-Soldered SKU) as the **current
default** sensor carrier board for new builds going forward.

The **ESP32-S3-DevKitC-1 is retained as a fully-supported alternative
carrier**, not a deprecated predecessor. Both carriers are documented
as parallel implementations of the same sensor specification: the
DevKitC-1 wiring guide at `HARDWARE-WIRING-DEVKITC.md` describes a
complete working sensor and may be the right choice for a future
student or operator whose priorities differ (more GPIO headroom,
on-board WS2812 RGB LED, 16 MB flash, or access to an external
buck-boost regulator that solves the Phase 3 power-input problem
that motivated this ADR). See `HARDWARE-WIRING.md` (carrier index)
for the alternatives framework.

The XIAO firmware tree lives at `firmware/esp32-s3/xiao/`; the
DevKitC firmware tree at `firmware/esp32-s3/devkitc/`. Both share
the same Rust target, ESP-IDF target, and most of the driver code —
only the carrier-specific pin assignments and partition table differ.

The FireBeetle 2 ESP32-C6 was seriously considered (a draft of this
ADR proposed it) and remains a defensible future direction if/when
the project wants WiFi 6, 802.15.4 (Thread/Zigbee), or a mainline-
Rust handoff story. It is documented under "Alternatives considered"
below.

## Consequences

### What changes

| Layer | Change |
|---|---|
| Carrier board | ESP32-S3-DevKitC-1 → Seeed XIAO ESP32-S3 (Pre-Soldered) |
| SoC | ESP32-S3 → **same** ESP32-S3 (no SoC change) |
| Flash | 16 MB (DevKitC N16R8) → 8 MB (XIAO) |
| PSRAM | 8 MB (N16R8) → 8 MB (XIAO) |
| Rust target | `xtensa-esp32s3-espidf` → **unchanged** |
| ESP-IDF target | `esp32s3` → **unchanged** |
| Form factor | ~52 × 27 mm (DevKitC) → 21 × 17.5 mm (XIAO) |
| GPIO exposure | 45 (DevKitC) → 11 (XIAO D0–D10) |
| ADXL355 GPIOs | GPIO10/11/12/13 (CS/MOSI/SCLK/MISO) → GPIO1/9/7/8 (CS/MOSI/SCLK/MISO), matching XIAO's SPI defaults; see `HARDWARE-WIRING.md` §2.1 |
| Battery sense | External 100 kΩ/100 kΩ divider on GPIO4 (D3 on XIAO) — channel unchanged from the planned DevKitC design |
| Power-input topology | Phase 3 was to add an external buck-boost → XIAO's on-board regulator handles the full cell range; no external regulator |
| Charging | Out of scope ("no charging in circuit") → handled by the XIAO's on-board charger over USB-C; cell still removable for swap |
| WS2812 LED | DevKitC has on-board WS2812 → XIAO has none; external WS2812 module wired to a free GPIO |
| Partition table | Sized for 16 MB flash → resized for 8 MB flash (~3 MB per app slot is plenty) |
| Battery connector | JST-PH pigtail → wires soldered to XIAO's BAT+/BAT− back-pads (less swappable, more reliable joint) |

### What stays the same

* All R2 protocol code (R2-WIRE, R2-FNV, R2-CBOR, R2-BLE,
  R2-BOOTSTRAP, R2-WIFI, R2-TRUST)
* The ADXL355 driver in `adxl355.rs` — already carrier-agnostic,
  takes `Peripheral<P = AnyIOPin>` handles
* The sample-acquisition pipeline (`sender.rs`)
* The LED FSM state-machine logic (only the GPIO routing changes)
* OTA flow over WiFi
* Wire vectors and conformance audits
* The Rust toolchain — same `xtensa-esp32s3-espidf` target, no
  `espup` re-install, no new compiler

### Cost

* Move ADXL355 jumpers from DevKitC pin header to XIAO pin header.
* Pin reassignment in `main.rs`: ~4 lines for SPI pins, 1 for the
  external WS2812 LED, 1 for the battery-sense ADC (same channel as
  planned for the DevKitC).
* External WS2812 module: a single WS2812 LED on a breakout (or a
  3-LED strip with the others unused) wired to a chosen GPIO.
* Partition table resize: change `partitions.csv` to fit 8 MB.
* Spec updates: `SPEC-R2-ROCKER-SENSOR.md` hardware section,
  `HARDWARE-WIRING.md` §§2.1 and 4.
* Verification: full Phase 1 → Phase 2 → Phase 6 → Phase 7 → Phase 9
  smoke test on the new board.
* **Estimated effort: half a day.**

### Benefits

* **No external regulator** — solves the Phase 3 parts problem.
* **Same toolchain** — no `espup` change, no Rust target change,
  university handoff instructions unaffected.
* **Tiny form factor** — much better for sensor packaging on the
  rocker rig.
* **Lower BOM** — fewer parts, smaller enclosure footprint.
* **Native USB-Serial-JTAG** — same flashing workflow as the DevKitC.
* **On-board LiPo charger** — USB-C plug → cell charges; unplug →
  cell powers the board.
* **Comfortable SD path** — XIAO's SPI bus can be shared with the
  ADXL355 (3-wire shared, CS-per-device) without exhausting GPIOs.

### What we give up

* **Flash halves** — 16 MB → 8 MB. Still comfortable for current
  firmware (~600 KB) with two OTA app slots and headroom.
* **GPIO count drops** — 45 → 11. Adequate for the planned phases
  but no spare pins for ad-hoc expansion. A third SPI peripheral
  would be tight.
* **WS2812 is external** — small additional wiring. The on-board user
  LED on GPIO21 is single-colour and would not carry the FSM's
  6-state colour palette.
* **Battery sense divider is external** — same as the original
  DevKitC plan; not a regression.
* **No JST-PH connector** — the XIAO has BAT+ / BAT− solder pads on
  the back. Cell wires solder directly to the board (less convenient
  to swap cells, but the soldered joint is more reliable than a cheap
  connector).

### Risks

* **Unprotected 18650 + XIAO charger** — the XIAO's on-board charger
  uses CC/CV charging (safe), but the XIAO does **not** provide
  over-discharge protection on the cell. If the cell is left
  connected with the board's quiescent current drawing it past
  2.5 V, the cell is damaged. **Mitigation:** disconnect the cell
  (de-solder, or switch in-line) when not in use for extended
  periods. A protected cell would eliminate this risk and is the
  recommended choice for any v1 hardware build that ships to the
  university.
* **GPIO budget** — adding the SD card on shared SPI consumes one
  more GPIO (CS). Watch this when scoping later phases.
* **Partition table** — 8 MB is comfortable, but every embedded
  asset added to flash competes with the second OTA slot. If
  firmware grows past ~3 MB per slot, revisit.

### Reversibility / carrier choice as user-facing decision

The XIAO is the **default** for new builds; it is not a one-way door.
The DevKitC firmware tree at `firmware/esp32-s3/devkitc/` and the
DevKitC wiring guide at `HARDWARE-WIRING-DEVKITC.md` together
describe a complete, working sensor. A future student or operator
may legitimately:

* Build a new sensor on the DevKitC carrier following the DevKitC
  wiring guide and firmware tree — both remain first-class supported
  implementations.
* Add a third carrier (e.g. FireBeetle 2 ESP32-S3, XIAO ESP32-S3
  Plus, custom PCB) by following the process documented in
  `HARDWARE-WIRING.md` (carrier index) and either writing a new ADR
  or extending the alternatives list.

The protocol, wire vectors, and data layer are carrier-agnostic and
shared across all variants.

## Alternatives considered

| Option | Why not chosen |
|---|---|
| Stay on DevKitC, source a Pololu S7V8F3 / TPS63020 buck-boost | Requires overseas order; slows the rig timeline |
| Stay on DevKitC, MT3608 boost → 5 V → on-board LDO | Works with local parts but adds a component, hurts efficiency, doesn't help BOM for university handoff |
| **FireBeetle 2 ESP32-C6 V1.0** | Different SoC (RISC-V vs Xtensa) → 1–2 days of toolchain retarget work for no protocol or feature benefit at this stage. Useful future direction if WiFi 6 / 802.15.4 / mainline-Rust handoff story becomes a priority. Boards retained for future projects. |
| XIAO ESP32-S3 **Plus** | Larger form factor, more pins, 16 MB flash. The user has the regular Pre-Soldered SKU on hand; the Plus would require ordering. Defensible upgrade if pin budget becomes painful. |
| Build a discrete buck-boost from components | Significant engineering work, EMI risk, no BOM or handoff benefit |
| Order a FireBeetle 2 ESP32-S3 (DFR0975) | Best of both worlds but requires shipping wait; XIAO is on-hand |

## Repository layout

The current `firmware/esp32-s3/` will be re-organised:

```
firmware/
  esp32-s3/
    devkitc/                   ← original DevKitC firmware, preserved
    xiao/                      ← new XIAO firmware
    common/                    (future: shared library crate once worth doing)
```

For v0.1 the `xiao/` and `devkitc/` trees may carry duplicated source.
A Cargo-workspace consolidation into a shared library crate is a
future cleanup, not a blocker for this ADR.

## References

* `firmware/esp32-s3/devkitc/` — preserved DevKitC-1 firmware tree (do not delete)
* `firmware/esp32-s3/xiao/` — new XIAO firmware tree (added in this branch)
* `specifications/SPEC-R2-ROCKER-SENSOR.md` §2.1 — boot sequence to be updated for XIAO pinout
* `specifications/HARDWARE-WIRING.md` §§2.1, 4 — wiring tables to be updated for XIAO
* Seeed wiki: <https://wiki.seeedstudio.com/xiao_esp32s3_getting_started/>
* Seeed product page (Pre-Soldered SKU): <https://www.seeedstudio.com/Seeed-Studio-XIAO-ESP32S3-Pre-Soldered-p-6334.html>
* DFRobot wiki (C6, considered alternative): <https://wiki.dfrobot.com/SKU_DFR1075_FireBeetle_2_Board_ESP32_C6>
* Branch: `hw/xiao-esp32-s3`
