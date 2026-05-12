---
title: ADR-002 — Revert active default carrier to ESP32-S3-DevKitC-1
status: Accepted
date: 2026-05-13
supersedes: none
amends: ADR-001 (changes "current default" only; both carriers remain supported)
superseded-by: none
---

# ADR-002 — Revert active default carrier to ESP32-S3-DevKitC-1

## Status

**Accepted** — 2026-05-13.

## Context

ADR-001 (2026-05-11) adopted the Seeed XIAO ESP32-S3 as the current
default carrier. The decision was driven by a parts-availability
wall: Phase 3 needed a buck-boost regulator (Pololu S7V8F3 /
TPS63020) that no NZ/AU retailer reachable on the rig timeline
stocked, and the XIAO's on-board charger + buck regulator solved
that problem without an overseas order.

Three things have changed in the days since ADR-001:

1. **The buck-boost regulators arrived in the post** (the overseas
   order that ADR-001 considered "blocked by shipping wait" has
   landed). The original Phase 3 design — DevKitC + external
   buck-boost feeding the 3V3 pin — is now buildable from on-hand
   parts.

2. **SD card breakouts also arrived** (the same parcel). Phase 2
   (microSD on shared SPI bus) becomes buildable on the DevKitC
   without further sourcing.

3. **The XIAO build hit a hardware fault.** During XIAO bring-up, a
   solder bridge between 5V and GND was found and removed, but
   afterwards the board's 3V3 regulator stayed stuck at ~0.7 V
   (silicon-diode forward drop — the textbook "regulator pushed
   into shutdown" signature) and the 5V rail sagged to ~3.1 V (USB
   host overcurrent fold-back). The on-board power management IC
   appears permanently damaged. A spare XIAO would resolve this,
   but the experience underscored the broader trade-off: the
   DevKitC's discrete power-management exposure (you wire it, you
   debug it, you see what's happening on the rails) is genuinely
   better for an instructional / research-handoff context than the
   XIAO's "small, integrated, single-IC" power chain that's hard to
   diagnose when something goes wrong.

The decision context for ADR-001 therefore no longer holds: the
"can't buy a buck-boost" pressure is gone, and the "DevKitC is
worse for university handoff" framing reverses (more visible power
electronics is *better* for a teaching artifact).

## Decision

Revert the **current default** carrier from Seeed XIAO ESP32-S3 back
to **ESP32-S3-DevKitC-1**.

**ADR-001 is not rescinded.** The multi-carrier framework it
introduced (parallel firmware trees under
`firmware/esp32-s3/{devkitc,xiao}/`, parallel wiring documents
under `specifications/HARDWARE-WIRING-*.md`, the carrier-index file
`specifications/HARDWARE-WIRING.md`, the carrier-agnostic
`SPEC-R2-ROCKER-SENSOR.md`) remains the project's structural
convention. The XIAO ESP32-S3 build is **still a fully-supported
alternative carrier** — its firmware tree, wiring document, and
hardware support are retained verbatim. A future student or
operator may legitimately rebuild the sensor on a XIAO if their
priorities (form factor, on-board LiPo charging, USB-C convenience)
favour it.

This ADR only changes which carrier the project's documentation
labels as the **current default for new builds** — not the relative
support status of the two carriers.

## Consequences

### What changes

* `HARDWARE-WIRING.md` (carrier index) — the table re-orders to list
  DevKitC first, swaps which row carries the "Current default" status
  flag, and updates the "How to choose" guidance to reflect that
  external buck-boost availability is no longer a deciding factor
  against the DevKitC.
* `HARDWARE-WIRING-DEVKITC.md` — status field flips to
  "Current default per ADR-002".
* `HARDWARE-WIRING-XIAO.md` — status field flips to
  "Alternative carrier — fully supported".
* `firmware/esp32-s3/README.md` — recommends DevKitC for new builds,
  keeps XIAO listed as alternative.
* `firmware/esp32-s3/devkitc/main.rs` — gains the BLE-only ADXL355
  diagnostic thread that was added to `xiao/main.rs` during XIAO
  bring-up (so the same bench-debug capability exists on either
  carrier).

### What is preserved (explicitly not changing)

* All firmware code in `firmware/esp32-s3/xiao/` — kept intact, still
  builds, still flashes, still works on a healthy XIAO.
* `HARDWARE-WIRING-XIAO.md` — content unchanged apart from the status
  field.
* ADR-001 — accepted record of the prior decision and the
  alternatives considered at that time.
* The r2-esp `BLEDevice::take` race fix — landed in r2-esp regardless
  of carrier; the DevKitC always benefited from the fix even though it
  never observed the failure.
* `SPEC-R2-ROCKER-SENSOR.md` carrier-agnostic refactor — applies to
  either carrier.
* `tools/setup-firmware.sh` multi-tree support — applies to either
  carrier and to any future third carrier.

### Cost

Minimal:

* Spec updates: status fields in three wiring docs + the README +
  this ADR.
* Firmware: ~40 lines copied from `xiao/main.rs` to `devkitc/main.rs`
  (BLE-only ADXL355 diagnostic thread).
* Hardware bring-up on the bench: physically move the ADXL355 Pmod
  wires from the (damaged) XIAO end back to the DevKitC's J1 header
  per `HARDWARE-WIRING-DEVKITC.md` §2.1. Then wire the buck-boost +
  battery sense per §4, and (if doing Phase 2) the microSD per §3.
* No protocol, wire-format, dashboard, or trust-group changes.

### Benefits

* **Phase 3 power-input becomes buildable today** with the now-on-hand
  buck-boost regulator. No more "BLE-only fallback because no
  regulator."
* **Phase 2 SD card becomes buildable today** with the on-hand SD
  breakout. Black-box recorder for catastrophic-failure events (see
  `project_catastrophic_joint_failures.md` memory) is in reach.
* **Hardware diagnosability** is better on the DevKitC for the
  research / handoff context. The 5V↔GND solder bridge on the XIAO
  that started a several-hour debugging chain was eventually found
  by eye in better light — but the cascade of "everything looks
  right but the rail is at 0.7 V" effects of an integrated power IC
  going into shutdown was much harder to diagnose than a discrete
  regulator going open or short would have been.
* **45 exposed GPIO** on the DevKitC vs 11 on the XIAO — leaves
  comfortable room for SD card, future LoRa hat, RS485 breakout, or
  other expansion without GPIO pressure.
* **On-board WS2812 RGB LED** on the DevKitC means the FSM colour
  indication works out of the box (no external WS2812 module
  needed).
* **Teaching artifact quality**: a student or new contributor
  looking at the rig can SEE the LiPo, the buck-boost board, the
  divider resistors, the JST-PH disconnect, the SD breakout — all
  discrete and labelled. On the XIAO, those are an opaque chip and a
  pair of solder pads on the back. Discrete is more pedagogical.

### What we give up (relative to the XIAO)

* **Form factor** — DevKitC is ~52 × 27 mm; XIAO is ~21 × 17.5 mm.
  For the rocker rig itself this doesn't matter; for a future
  sensor-pack deployment it might.
* **On-board LiPo charging via USB-C** — DevKitC has no charger;
  cell unplugs at the JST-PH and is recharged in an external dock.
  That convention is documented in
  `HARDWARE-WIRING-DEVKITC.md` §4 and matches the operator-
  supervised operational mode (see
  `project_operational_supervised.md` memory).
* **The "no charging in circuit" decision** returns to its original
  form: no charge IC on the board at all. (ADR-001 noted that the
  XIAO's vendor-validated charger was a different shape of the
  same principle; with the revert, the original strict form
  applies.)

### Risks

Low. The only real failure mode here is misreading the framing —
treating ADR-002 as a deprecation of the XIAO build. It is not.

### Reversibility

Same as ADR-001: very high. The branch / ADR pattern means that if
the parts situation changes again, or a new carrier becomes
preferred, a future ADR-003 can flip the active default once more
with the same scope of changes (status fields + README guidance +
maybe an additional cross-port of bench helpers).

## Alternatives considered

| Option | Why not chosen |
|---|---|
| Keep XIAO as current default, swap to spare XIAO at home tonight, proceed | Doesn't use the buck-boost / SD parts that just arrived; doesn't address the diagnosability concern. Future students / collaborators would still find an integrated-power XIAO harder to debug. |
| Multi-default — say "either is fine, pick what's on your bench" | Ambiguity in spec-driven projects is a tax. A single clear default makes the path-of-least-resistance correct without removing optionality for those who want it. |
| Rescind ADR-001 entirely and remove the XIAO tree | Throws away validated, working firmware. Loses the multi-carrier framework. Goes against the project's "alternative implementations" philosophy. |
| Wait until both carriers have been independently bench-validated end-to-end before flipping | Slow. The DevKitC build *was* bench-validated end-to-end before the carrier swap (commits `8a389ed` and `8fb1c4f`); a build-and-flash from `firmware/esp32-s3/devkitc/` is expected to come up first try once the wires are re-attached. |

## References

* `firmware/esp32-s3/devkitc/` — preserved DevKitC firmware tree (current default after this ADR)
* `firmware/esp32-s3/xiao/` — XIAO firmware tree (alternative, preserved verbatim)
* `specifications/HARDWARE-WIRING.md` — carrier index (status flags updated by this ADR)
* `specifications/HARDWARE-WIRING-DEVKITC.md` — DevKitC wiring (current default after this ADR)
* `specifications/HARDWARE-WIRING-XIAO.md` — XIAO wiring (alternative, preserved verbatim)
* `decisions/ADR-001-xiao-esp32-s3-carrier.md` — prior carrier decision
* `~/.../memory/project_operational_supervised.md` — supervised-operation context that informs the no-on-board-charging principle
* `~/.../memory/project_catastrophic_joint_failures.md` — research goal informing the SD-as-black-box-recorder value
* Branch: `hw/active-default-devkitc-v2`
