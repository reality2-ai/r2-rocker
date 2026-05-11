---
title: r2-rocker — Hardware wiring (Seeed XIAO ESP32-S3)
status: Draft v0.1 — current default carrier per ADR-001
date: 2026-05-11
applies-to: Seeed XIAO ESP32-S3 (Pre-Soldered SKU) + EVAL-ADXL355-PMDZ + microSD-SPI + LiPo (removable, on-board charging)
related-carriers: HARDWARE-WIRING-DEVKITC.md (alternative, fully supported)
related: decisions/ADR-001-xiao-esp32-s3-carrier.md
trade-offs: On-board LiPo charger + buck regulator + USB-C, tiny form factor; only 11 exposed GPIOs, 8 MB flash, no on-board RGB LED (external WS2812 required)
---

# r2-rocker — Hardware wiring (Seeed XIAO ESP32-S3)

Soldering-ready wiring for the rocker-rig sensor node. Three phases, each
self-contained — you can stop after any phase and have a working sensor for
that phase's scope.

> **One of several supported carrier-board implementations.** This is
> the **current default** for new builds (see
> `decisions/ADR-001-xiao-esp32-s3-carrier.md` for the rationale). The
> ESP32-S3-DevKitC-1 build at `HARDWARE-WIRING-DEVKITC.md` is a fully-
> supported alternative — both wiring documents describe complete,
> working sensor implementations against the same firmware codebase
> and the same SPEC-R2-ROCKER-SENSOR contract. A future student or
> operator may legitimately choose either carrier depending on
> priorities (size, GPIO headroom, on-board peripherals, BOM).

| Phase | Adds | Purpose |
|---|---|---|
| 1 | ADXL355 SPI + external WS2812 LED | Prove SPI bring-up; UART-monitored sample stream; FSM status indication |
| 2 | microSD on shared SPI bus | Durable sample buffer (store-and-forward) |
| 3 | LiPo cell on BAT+/BAT- pads + battery sense divider | Battery operation + battery telemetry |

> **Power-on safety.** Disconnect USB before soldering. Verify each phase's
> wiring against the table below *before* re-applying power.

> **Difference from the DevKitC build.** Three things change with the XIAO
> carrier: (a) the on-board buck regulator handles the LiPo cell directly —
> no external buck-boost; (b) the cell connects to **BAT+ / BAT-** solder
> pads on the back of the board, not a JST-PH connector; (c) there is no
> on-board addressable RGB LED, so an **external WS2812 module** is wired
> for FSM status indication.

## 1. Bill of materials

| Qty | Item | Notes |
|---|---|---|
| 1 | Seeed XIAO ESP32-S3 (Pre-Soldered) | DigiKey/Seeed SKU 113991141 or similar; 8 MB flash, 8 MB PSRAM |
| 1 | EVAL-ADXL355-PMDZ | Analog Devices Pmod accelerometer (unchanged from DevKitC build) |
| 1 | microSD breakout (SPI, 3.3 V) | Adafruit #254 or generic equivalent (Phase 2) |
| 1 | microSD card | ≥ 4 GB, Class 10 (Phase 2) |
| 1 | WS2812 single-LED module | Jaycar XC4380 (Duinotech WS2812 RGB module) or a single WS2812 cell cut from a 5050 strip; powered from 3V3 (Phase 1) |
| 1 | LiPo cell, 18650 or similar | 3.7 V nominal, 2000–3000 mAh; **protected cell strongly recommended** (Phase 3) |
| 1 | 18650 holder (or solder tabs) | Holder with solder leads, or solder wires directly to the cell tabs |
| 2 | 100 kΩ ¼ W resistor | 1 % tolerance preferred (battery divider, Phase 3) |
| 1 | 100 nF ceramic capacitor | 0805 or through-hole (optional ADC decoupling, Phase 3) |
| — | Hookup wire | 24–28 AWG silicone, multiple colours |
| — | Tools | Soldering iron, flux, multimeter |

**Dropped from the DevKitC BOM** (no longer required with the XIAO):

| Item | Why dropped |
|---|---|
| Buck-boost 3.3 V converter (Pololu S7V8F3 / TPS63020 module) | XIAO has an on-board buck regulator that handles the LiPo cell directly across its full discharge curve |
| External LiPo charging dock | XIAO has an on-board charger; charging happens automatically when USB-C is plugged in |
| JST-PH 2-pin pigtail | Cell wires solder directly to the XIAO's BAT+ / BAT- back pads |

---

## 2. Phase 1 — ADXL355 over SPI + external WS2812 LED (minimum viable sensor)

### 2.0 How to read the connection tables

The XIAO ESP32-S3 has **two ways** to identify a pin. The tables in this
document give you both, so you can cross-check:

| Identifier | Where you see it | Example |
|---|---|---|
| **Silkscreen label** | Silkscreened on the board next to each pin | `D0`, `D8`, `3V3`, `GND` |
| **ESP32-S3 GPIO number** | From the Seeed pinout reference (linked below); not on the board itself | `GPIO1`, `GPIO7` |

**Trust the silkscreen.** The `D0`–`D10` labels are the authoritative
identifiers — that's what you wire to physically. The GPIO column is what
your firmware sets, via the mapping in this document.

XIAO pin headers are 2.54 mm (0.1") pitch — standard breadboard / ribbon
cable friendly. The board is 21 × 17.5 mm, USB-C on one short edge.

### 2.1 Connection table — ADXL355

| XIAO header | GPIO | Wire colour (suggested) | ADXL355-PMDZ (Pmod 12-pin) | Pmod signal |
|---|---|---|---|---|
| **3V3**       | —      | Red    | **Pin 6** (or 12) | VDD |
| **GND**       | —      | Black  | **Pin 5** (or 11) | DGND |
| **D0**        | GPIO1  | Yellow | **Pin 1**         | CS |
| **D10**       | GPIO9  | Green  | **Pin 2**         | MOSI |
| **D9**        | GPIO8  | Blue   | **Pin 3**         | MISO |
| **D8**        | GPIO7  | White  | **Pin 4**         | SCLK |
| **D1**        | GPIO2  | Orange | **Pin 10**        | DRDY (optional) |

Total: **7 wires** (6 if DRDY is skipped — firmware polls in v0.1).

> **If you already soldered the ADXL355 Pmod ribbon for the DevKitC**, the
> wire colours above match that ribbon. You only need to move the
> XIAO-side ends of each coloured wire to the new pin. The ADXL355 end
> stays as-soldered.

> **Pmod pin 1** is marked on the EVAL-ADXL355-PMDZ silkscreen with a white
> square pad or `1` label. Pins 1–6 are one row; pins 7–12 the other.
> Source: <https://wiki.analog.com/resources/eval/user-guides/circuits-from-the-lab/eval-adxl355-pmdz>.

### 2.2 Connection table — external WS2812 LED

A WS2812 (or WS2812B, "NeoPixel") single-LED module provides the
firmware's status indicator. Three wires:

| XIAO header | GPIO | Wire colour (suggested) | WS2812 module pin | Signal |
|---|---|---|---|---|
| **3V3** | —     | Red    | VCC (or VDD / 5V) | Power (3.3 V tolerant; check module — most accept 3.0–5.5 V) |
| **GND** | —     | Black  | GND               | Ground |
| **D5**  | GPIO6 | Purple | DIN               | Serial data input |

> **WS2812 power.** Most WS2812 modules are spec'd for 5 V but work
> reliably at 3.3 V (slightly reduced brightness). The firmware caps
> brightness at 20 % anyway, so 3.3 V is plenty. If your module
> insists on 5 V (rare), tap from the XIAO's **5V** pin instead of
> **3V3**.

> **DOUT not connected.** A single LED has only DIN driven; DOUT is left
> floating. If you ever chain multiple WS2812s, DOUT of LED 1 wires to
> DIN of LED 2, etc.

### 2.3 Physical layout — XIAO ESP32-S3 pin header

The XIAO has **7 pins on each long edge**, 2.54 mm pitch. Top view, USB-C
at the top:

```
                      ┌─[USB-C]─┐
                      │   XIAO  │
                      │ ESP32-S3│
       left edge      │         │       right edge
   ┌────────────────┐ │         │ ┌────────────────┐
   │ 5V             │ │         │ │ D0  (GPIO1)    │← yellow ── ADXL CS
   │ GND            │←─ black     │ D1  (GPIO2)    │← orange ── ADXL DRDY
   │ 3V3            │←─ red       │ D2  (GPIO3) ⚠  │  strap pin (do not use)
   │ D10 (GPIO9)    │← green   ── │ D3  (GPIO4)    │← (Phase 3 batt sense)
   │ D9  (GPIO8)    │← blue    ── │ D4  (GPIO5)    │← (Phase 2 SD CS)
   │ D8  (GPIO7)    │← white   ── │ D5  (GPIO6)    │← purple ── WS2812 DIN
   │ D7  (GPIO44)   │  reserved   │ D6  (GPIO43)   │  reserved (UART TX)
   └────────────────┘ └─────────┘ └────────────────┘
                       BAT+/BAT-
                       on back
```

> **Physical orientation note.** The exact silkscreen layout (which side
> has D0, etc.) is verified against the Seeed pinout reference linked in
> §6. The diagram above is approximate — always check your specific
> board's silkscreen before soldering.

### 2.4 Power, decoupling, cable

* ADXL355-PMDZ has onboard decoupling (10 nF + 100 nF across VDD/GND) — no
  extra cap needed at this end.
* XIAO supplies 3.3 V at up to 700 mA from the onboard regulator. ADXL355
  draws < 200 µA and a single WS2812 at 20 % brightness draws < 20 mA — both
  well within budget.
* Keep wires < 15 cm if possible. SPI at 5 MHz on long unshielded jumpers
  can pick up noise; if you see CRC/WHO_AM_I errors, shorten or twist pairs
  (CLK with GND, MISO with GND).
* All signals are 3.3 V — **no level shifter required**.
* The WS2812's DIN line is somewhat tolerant of mismatched 3.3 V→5 V
  signalling, but if the LED looks erratic, try 3.3 V VCC (most modern
  WS2812Bs latch at 0.7 × VCC, so 3.3 V VCC means a 2.3 V threshold —
  easy from a 3.3 V GPIO).

### 2.5 Pre-power-up checklist

- [ ] No shorts: multimeter across 3V3 ↔ GND on the XIAO = open circuit.
- [ ] No shorts: 3V3 ↔ GND on the ADXL355-PMDZ = open circuit.
- [ ] No shorts: VCC ↔ GND on the WS2812 module = open circuit.
- [ ] Continuity: XIAO **3V3** ↔ Pmod pin 6 (red wire intact end-to-end).
- [ ] Continuity: XIAO **GND** ↔ Pmod pin 5 (black wire intact end-to-end).
- [ ] Pmod pin 1 (CS) goes to **D0 / GPIO1**, not **D1 / GPIO2** (common
      mistake — count from the marked end of the Pmod).
- [ ] WS2812 DIN goes to **D5 / GPIO6**, not DOUT.
- [ ] No connections to the strapping pin **D2 / GPIO3**.
- [ ] No connections to UART pins **D6 / GPIO43** (TX) and **D7 / GPIO44**
      (RX) unless you specifically want them for an external UART.

### 2.6 First power-up (smoke test, no firmware yet)

With USB-C plugged in:

1. The XIAO's small charge / power indicator LED should light (red, near
   the USB-C connector).
2. Measure with multimeter: Pmod pin 6 reads 3.3 V ± 0.1 V.
3. Measure: WS2812 module VCC pin reads 3.3 V ± 0.1 V.
4. ADXL355 idle current draw < 1 mA (rest of the board dominates).
5. The WS2812 may glow dimly white or its boot-up colour briefly; this
   is normal for an unwritten data line.

If 3.3 V is missing at the Pmod or WS2812 VCC, you have a wire/solder
issue — fix before flashing firmware.

---

## 3. Phase 2 — microSD on shared SPI bus

The SD card sits on the **same SPI bus** as the ADXL355 (different CS line —
the firmware bus driver multiplexes). Six new wires.

### 3.1 Typical microSD breakout pinout

Most generic SPI breakouts (Adafruit, HiLetgo, generic Chinese) expose:
`VCC, GND, MISO, MOSI, SCK, CS, [CD]`. Confirm against your specific module.

### 3.2 Connection table

| XIAO header | GPIO | microSD breakout pin | Signal | Note |
|---|---|---|---|---|
| **3V3**       | —     | VCC  | 3.3 V         | Most modules accept 3.3–5 V; 3.3 V is fine |
| **GND**       | —     | GND  | Ground        | Share with ADXL355 GND |
| **D9**        | GPIO8 | MISO | Shared        | **Same wire as ADXL355 MISO** (T-junction OK) |
| **D10**       | GPIO9 | MOSI | Shared        | **Same wire as ADXL355 MOSI** |
| **D8**        | GPIO7 | SCK  | Shared        | **Same wire as ADXL355 SCLK** |
| **D4**        | GPIO5 | CS   | SD-only       | Dedicated chip-select |

### 3.3 Notes

* SPI bus sharing: only the device whose CS is asserted responds. Both CS
  lines (**D4 / GPIO5** for SD, **D0 / GPIO1** for ADXL355) must be driven
  high (idle) when not in use — the firmware handles this automatically.
* SD cards typically want a 10 kΩ pull-up on CS (most breakouts include it).
* Bus speed reconciles per-device: ADXL355 ≤ 10 MHz, SD usually 25 MHz. The
  ESP-IDF SPI driver re-clocks per transaction.
* Keep the shared SPI wires bundled — long stub branches cause reflections.
* **Card-detect (CD) pin is not used** on the XIAO build — the GPIO budget
  is tighter than the DevKitC's, and the firmware can detect a missing
  card via mount failure with no functional loss.

---

## 4. Phase 3 — LiPo cell on BAT+/BAT- pads + battery sense

The XIAO ESP32-S3 has an on-board LiPo charger and a buck regulator that
takes the cell voltage directly. There is **no need for an external
buck-boost** — the regulator handles the cell's full discharge curve
(3.0 V – 4.2 V) and produces a stable 3.3 V output. Charging happens
automatically over USB-C; no external charging dock is required.

The **only Phase 3 external circuit** is the battery-voltage sense
divider, because the XIAO does not provide a built-in battery-sense
pin.

### 4.1 Topology

```
                                            (on-board buck regulator)
                                              ┌─────────────────────┐
   ┌─[ BAT+ pad ]─┐    ┌─────────────────────┤ XIAO ESP32-S3        │
   │  LiPo cell   ├────┤                     │                      │
   │ 3.0–4.2 V    ├────┤   on-board          │  3V3 ── (ADXL355,    │
   │              │    │   charger + buck    │         WS2812,      │
   └─[ BAT- pad ]─┘    │   regulator         │         SD)          │
            │           └─────────────────────┴─────────────────────┘
            │
            │  (cell voltage tap, before regulator)
            ├──── 100 kΩ ──┬─── D3 (GPIO4, ADC1_CH3, battery sense)
            │              │
            │            100 kΩ
            │              │
            └──── GND ─────┘

   USB-C charging:  Plug in USB-C → on-board charger (CC/CV, ~100 mA
   default) charges the cell. Cell continues to power the board when
   USB is unplugged.

   No external buck-boost.  No external charging dock.  No JST-PH
   pigtail.  Cell wires solder directly to the BAT+ and BAT- pads on
   the back of the XIAO.
```

### 4.2 Battery voltage divider

Sample VBATT (3.0–4.2 V at the cell) into the ADC range (0–3.3 V):

```
   BAT+ ──┬── R1 (100 kΩ) ──┬── ADC node ──── D3 (GPIO4)
          │                  │
          │                 R2 (100 kΩ)
          │                  │
          │                 GND
          │
         [decoupling: 100 nF from ADC node to GND, optional]
```

Divider ratio = R2 / (R1 + R2) = 0.5. Cell voltage maps:

| Cell | ADC input | ADC reading (12-bit, 12 dB att.) |
|---|---|---|
| 4.20 V (full)   | 2.10 V | ≈ 2620 |
| 3.70 V (nominal)| 1.85 V | ≈ 2310 |
| 3.00 V (cut-off)| 1.50 V | ≈ 1870 |

Quiescent current: 4.2 V / 200 kΩ ≈ 21 µA continuous. Acceptable for a
2000 mAh cell (months of standby loss). For lower quiescent, gate the
high side with a MOSFET driven from a spare GPIO — defer to a v2 board.

### 4.3 Connection table

| Function                          | XIAO header | GPIO | Notes |
|---|---|---|---|
| Battery sense (divider mid-point) | **D3**      | GPIO4 (ADC1_CH3) | Must be ADC1; all D0–D10 pins are ADC1-capable on the XIAO |
| Battery + (raw cell, divider top) | **BAT+ pad** (on back) | — | Solder cell positive lead here |
| Battery − (ground, divider bottom)| **BAT− pad** (on back) | — | Solder cell negative lead here |
| Common GND                        | **GND** (header)       | — | Internally tied to BAT−; either is fine for divider bottom |

> **Disable USB while running on battery only?** No — leave USB power
> rules to the user. The XIAO is designed so USB-C and the cell can
> coexist: USB-C charges the cell while powering the board. To run
> battery-only, simply unplug USB-C.

### 4.4 Cell safety — unprotected vs. protected 18650

* The XIAO's on-board charger uses CC/CV charging (safe charging
  behaviour).
* The XIAO does **NOT** provide over-discharge protection on the cell.
  If the cell is left connected to the XIAO while the board sleeps for
  long periods, the XIAO's quiescent draw can take the cell below
  2.5 V — at which point the cell is damaged (capacity loss, possible
  internal short).
* **A protected 18650** (with PCM/BMS built into the cell) eliminates
  this risk. Strongly recommended for any v1 hardware that ships to the
  university. The Naccon 18650 2600 mAh "plain" cell currently on hand
  is **unprotected** — fine for bench work where the cell can be
  disconnected when idle, not fine for long-term unattended deployment.
* If using an unprotected cell, **physically disconnect it** (unsolder
  one lead, or use an in-line switch) when not in use for > 24 hours.

### 4.5 Hot-swap behaviour

Unlike the DevKitC build (JST-PH connector for easy swap), the XIAO
build solders the cell directly. Hot-swapping is not the intended
workflow:

* For **bench work**: keep USB-C plugged in while flashing/debugging;
  this charges the cell continuously.
* For **field runs**: plug USB-C to start a charge → unplug USB-C →
  run from cell → re-plug USB-C to recharge. The cell stays soldered.
* **If the cell does need to be replaced** (capacity faded, damage,
  upgrade to a protected cell): de-solder BAT+ and BAT− leads, install
  the new cell. Treat as a hardware-revision event, not a routine
  swap.

---

## 5. Pin reservation — full board (final)

Everything on the XIAO ESP32-S3 after all three phases:

| XIAO header | GPIO | Used by | Notes |
|---|---|---|---|
| D0          | GPIO1 | ADXL355 CS | |
| D1          | GPIO2 | ADXL355 DRDY (optional, polled v0.1) | |
| D3          | GPIO4 | Battery sense | ADC1_CH3 |
| D4          | GPIO5 | SD CS | |
| D5          | GPIO6 | External WS2812 DIN | RMT-driven |
| D8          | GPIO7 | ADXL355 / SD SCK (shared) | XIAO SPI default |
| D9          | GPIO8 | ADXL355 / SD MISO (shared) | XIAO SPI default |
| D10         | GPIO9 | ADXL355 / SD MOSI (shared) | XIAO SPI default |

**Reserved / avoided** (do not wire anything to these):

| XIAO header | GPIO | Reason |
|---|---|---|
| D2 | GPIO3  | Strapping pin (JTAG signal source select on ESP32-S3) |
| D6 | GPIO43 | UART0 TX — reserved for an external UART console if ever needed |
| D7 | GPIO44 | UART0 RX — reserved for an external UART console if ever needed |

(Strapping pins **GPIO0, GPIO45, GPIO46** and USB pins **GPIO19, GPIO20**
are not exposed on the XIAO header — no risk of accidental wiring.)

**On-board indicators**:

| GPIO | Function | Notes |
|---|---|---|
| GPIO21 | On-board user LED | Single-colour (yellow); not used by the FSM — colour info would be lost. External WS2812 on D5 provides FSM status indication. |

The external WS2812 is the primary user-facing status indicator. Suggested
state → colour mapping (finalised in the firmware spec):

| Sensor state | LED |
|---|---|
| Boot / init | White, brief flash |
| Advertising (BLE beacon) | Blue, slow pulse (1 Hz) |
| BLE connected, WiFi joining | Cyan, fast pulse |
| Calibrating | Purple, solid |
| Streaming (live) | Green, heartbeat (60 bpm) |
| Streaming (catching up from SD) | Yellow, heartbeat |
| Low battery (<3.3 V) | Orange, slow pulse — overrides other states |
| Error / no SD / SPI fault | Red, fast pulse |
| OTA in progress | White, fast strobe |

**Free for future expansion**: D2/GPIO3 (with strap-pin care), D6/GPIO43,
D7/GPIO44 — plus the option to drop DRDY (D1/GPIO2) and reclaim that pin
if polling proves sufficient (it does in v0.1).

If a future phase needs more pins than the XIAO header exposes (e.g.
to add a LoRa hat alongside the ADXL355 and SD card), the
XIAO ESP32-S3 **Plus** SKU adds D11–D18 (GPIO40, 39, 38, 10, 13, 12, 11)
without changing the SoC, toolchain, or footprint of this build. See
ADR-001 §"Alternatives considered".

---

## 6. References

| Source | Where | What |
|---|---|---|
| Seeed XIAO ESP32-S3 wiki | <https://wiki.seeedstudio.com/xiao_esp32s3_getting_started/> | Pinout table, electrical specs, power-management notes |
| Seeed product page (Pre-Soldered SKU) | <https://www.seeedstudio.com/Seeed-Studio-XIAO-ESP32S3-Pre-Soldered-p-6334.html> | SKU details |
| `docs/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf` | p.10–11 (Pin Definitions) | ESP32-S3 GPIO function multiplexing (applies to any ESP32-S3 carrier, including the XIAO module which uses the ESP32-S3R8 die) |
| `docs/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf` | p.13 (Boot Configurations) | Strapping-pin behaviour |
| `docs/ADXL355.md` | (link) | EVAL-ADXL355-PMDZ user guide |
| EVAL-ADXL355-PMDZ wiki | <https://wiki.analog.com/resources/eval/user-guides/circuits-from-the-lab/eval-adxl355-pmdz> | Pmod pinout |
| `decisions/ADR-001-xiao-esp32-s3-carrier.md` | (this repo) | Carrier swap rationale and alternatives considered |
| `HARDWARE-WIRING-DEVKITC.md` | (this repo) | Preserved DevKitC-1 wiring reference for sensor units built on that carrier |

---

## 7. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-11 | 0.1 | Initial draft. XIAO ESP32-S3 carrier. Phase 1/2/3 pin assignments. Carrier-swap from DevKitC per ADR-001. |
