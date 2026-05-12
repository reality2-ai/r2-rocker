---
title: r2-rocker — Hardware wiring (index of carrier alternatives)
status: Index — points to carrier-specific wiring documents
date: 2026-05-13
---

# r2-rocker — Hardware wiring (carrier alternatives)

The r2-rocker sensor is **carrier-board agnostic at the protocol and
firmware-spec layer**. The same `SPEC-R2-ROCKER-SENSOR`,
`SPEC-R2-ROCKER-WIRE`, and `SPEC-R2-ROCKER-DASHBOARD` contracts hold
regardless of which ESP32-S3 carrier board the sensor is built on.

Each supported carrier has its own wiring document under this folder.
A future student or operator should pick the carrier that best
matches their priorities (form factor, GPIO headroom, on-board
peripherals, BOM, availability) and follow the corresponding wiring
guide. **None of these documents is "deprecated" — they describe
parallel implementations of the same sensor specification.**

## Supported carriers

| Carrier | Wiring document | Status | Strengths | Trade-offs |
|---|---|---|---|---|
| **ESP32-S3-DevKitC-1** | [`HARDWARE-WIRING-DEVKITC.md`](HARDWARE-WIRING-DEVKITC.md) | **Current default** per ADR-002 | 45 GPIO pins (lots of expansion); on-board WS2812 RGB LED; 16 MB flash (N16R8 variant); discrete and diagnosable power chain | No on-board LiPo charging; requires external buck-boost regulator for LiPo operation; ~52 × 27 mm footprint |
| **Seeed XIAO ESP32-S3** (Pre-Soldered) | [`HARDWARE-WIRING-XIAO.md`](HARDWARE-WIRING-XIAO.md) | Alternative — fully supported (was current default under ADR-001) | On-board LiPo charger + buck regulator + USB-C; tiny 21 × 17.5 mm footprint; ~14 µA deep sleep | 11 GPIO pins (tight if adding hats); 8 MB flash; no on-board RGB LED (external WS2812 required); integrated power IC harder to diagnose if a fault develops |

## How to choose

Pick the **ESP32-S3-DevKitC-1** (current default) if:

* You have a buck-boost regulator on hand (Pololu S7V8F3 / TPS63020 /
  similar) — or you're happy running from USB power for bench work.
* You want the on-board WS2812 RGB LED — one less component to wire.
* You're prototyping with multiple accessories (SD card, LoRa hat,
  RS485 breakout) that would exhaust the XIAO's 11-pin header.
* You're following a teaching / research-handoff path that
  emphasises explicit, diagnosable power-management circuitry as a
  learning artifact (the LiPo, buck-boost, divider resistors, and
  JST-PH disconnect are all individually visible).
* You may want to access more flash (16 MB on the N16R8 variant) for
  larger OTA slots or embedded assets.

Pick the **XIAO ESP32-S3** if:

* The sensor packaging needs to be small (e.g. a future sealed
  sensor pack for non-rig deployment).
* You want USB-C charging of the LiPo without external hardware.
* The external-buck-boost availability is a blocker on your timeline
  (this was the original reason ADR-001 picked the XIAO — see
  `decisions/ADR-001-xiao-esp32-s3-carrier.md`).
* You don't mind running an external WS2812 module on a single GPIO
  for status indication, and the 11-pin budget fits your accessory
  set.

## Adding a new carrier

If you want to add a third carrier (e.g. **FireBeetle 2 ESP32-S3**,
**XIAO ESP32-S3 Plus**, or a custom PCB):

1. Read `decisions/ADR-001-xiao-esp32-s3-carrier.md` for the structure
   of the carrier-choice rationale and the alternatives that have
   already been considered.
2. Decide whether the new carrier is a substitution (write a new ADR
   that supersedes ADR-001 with reasoning) or an additional supported
   alternative (extend this index and add a parallel
   `HARDWARE-WIRING-<NAME>.md` document).
3. If the carrier uses a different SoC family (e.g. ESP32-C6 RISC-V or
   RP2040 ARM), additionally document the firmware-toolchain
   implications. See the discussion of ESP32-C6 in ADR-001 §
   "Alternatives considered" for an example.
4. Update `SPEC-R2-ROCKER-SENSOR.md` only if the carrier change forces
   a protocol or behaviour change. In general, the protocol layer is
   carrier-agnostic — only the wiring document and the firmware
   pin-assignment file should change.

## Firmware structure

The firmware tree mirrors this same alternative-carrier pattern:

```
firmware/
  esp32-s3/
    devkitc/     ← firmware for HARDWARE-WIRING-DEVKITC.md
    xiao/       ← firmware for HARDWARE-WIRING-XIAO.md
    common/     (future: shared library crate, once worth doing)
```

Both firmware variants build against the same `xtensa-esp32s3-espidf`
Rust target and the same ESP-IDF version, sharing the R2 protocol
stack, the ADXL355 driver, and the FSM logic. Only the pin
assignments and partition table differ.

## See also

* `decisions/ADR-001-xiao-esp32-s3-carrier.md` — carrier-choice
  rationale and considered alternatives
* `SPEC-R2-ROCKER-SENSOR.md` — sensor firmware behaviour (carrier-
  agnostic)
* `SECRETS-POLICY.md` — key handling (carrier-agnostic)
