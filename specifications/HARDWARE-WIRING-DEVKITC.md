---
title: r2-rocker — Hardware wiring (ESP32-S3-DevKitC-1)
status: Current default per ADR-002 (was alternative under ADR-001)
date: 2026-05-07 (last revised 2026-05-13)
applies-to: ESP32-S3-DevKitC-1 + EVAL-ADXL355-PMDZ + microSD-SPI + LiPo (removable, externally charged)
related-carriers: HARDWARE-WIRING-XIAO.md (alternative — fully supported)
trade-offs: More GPIO headroom (45 pins vs 11), on-board RGB LED, 16 MB flash, discrete + diagnosable power chain; requires external buck-boost regulator for LiPo operation
---

# r2-rocker — Hardware wiring (ESP32-S3-DevKitC-1)

> **Current default carrier for new builds** (per
> `decisions/ADR-002-revert-active-default-to-devkitc.md`). The
> ESP32-S3-DevKitC-1 was the original carrier (ADR-001 briefly moved
> to the XIAO during a parts-availability window; ADR-002 reverted
> after the buck-boost regulator and SD breakout arrived). The
> **Seeed XIAO ESP32-S3** build at `HARDWARE-WIRING-XIAO.md` remains
> a fully-supported alternative — a future student or operator who
> wants the on-board LiPo charging, USB-C convenience, or the tiny
> form factor may legitimately choose it. The DevKitC firmware tree
> at `firmware/esp32-s3/devkitc/` is the active build target for
> this carrier.

Soldering-ready wiring for the rocker-rig sensor node. Three phases, each
self-contained — you can stop after any phase and have a working sensor for
that phase's scope.

| Phase | Adds | Purpose |
|---|---|---|
| 1 | ADXL355 SPI only | Prove SPI bring-up; UART-monitored sample stream |
| 2 | microSD on shared SPI bus | Durable sample buffer (store-and-forward) |
| 3 | LiPo cell + charger + voltage divider | Battery operation + battery telemetry |

> **Power-on safety.** Disconnect USB before soldering. Verify each phase's
> wiring against the table below *before* re-applying power.

## 1. Bill of materials

| Qty | Item | Notes |
|---|---|---|
| 1 | ESP32-S3-DevKitC-1 (N8R8 or N32R16V) | Per `docs/esp-dev-kits-en-master-esp32s3.pdf` §1.1 |
| 1 | EVAL-ADXL355-PMDZ | Analog Devices Pmod accelerometer, per `docs/ADXL355.md` link |
| 1 | microSD breakout (SPI, 3.3 V) | Adafruit #254 or any generic equivalent |
| 1 | microSD card | ≥4 GB, Class 10 |
| 1 | LiPo cell | 3.7 V nominal, 1000–2000 mAh; with JST-PH 2-pin connector |
| 1 | Mating JST-PH 2-pin pigtail | In-line disconnect; lets the cell be removed for external charging |
| 1 | Buck-boost 3.3 V converter | LiPo (3.0–4.2 V) → stable 3.3 V; e.g. Pololu S7V8F3, TPS63020 module |
| 1 | External LiPo charging dock | Single-cell, JST-PH input — **off-board**, not part of the sensor |
| 2 | 100 kΩ ¼ W resistor | 1 % tolerance preferred (battery divider) |
| 1 | 100 nF ceramic capacitor | 0805 or through-hole (ADC decoupling) |
| — | Hookup wire | 24–28 AWG silicone, multiple colours |
| — | Tools | Soldering iron, flux, multimeter |

---

## 2. Phase 1 — ADXL355 over SPI (minimum viable sensor)

### 2.0 How to read the connection tables

The DevKitC-1 has **two ways** to identify a pin. The tables in this
document give you both, so you can cross-check:

| Identifier | Where you see it | Example |
|---|---|---|
| **GPIO number** | Silkscreened on the board itself, next to each pin | `10`, `11`, `G`, `3V3` |
| **Header pin position** | Datasheet only (not on the board); count sequentially from the top of the connector (USB end) | `J1 pin 16` = 16th pin down on the left header |

**Trust the silkscreen.** The GPIO number printed on the board is the
authoritative identifier — that's what your firmware sets. The "J1 pin N"
column is a counting aid when you can't read tiny silkscreen, or when the
silkscreen is hidden under a connector.

The numbering is **not sequential GPIO order** because the GPIO matrix is
flexible — Espressif put the pins where it suited the PCB layout, not in
numerical order. So GPIO9 and GPIO46 sit next to each other on the
silkscreen even though the numbers look out of order. That's normal.

If you're unsure, the labelled photo on PDF page 8
(`docs/esp-dev-kits-en-master-esp32s3.pdf`) shows every pin with its GPIO
number clearly visible.

### 2.1 Connection table

| ESP32-S3-DevKitC-1 (J1) | GPIO | Wire colour (suggested) | ADXL355-PMDZ (Pmod 12-pin) | Pmod signal |
|---|---|---|---|---|
| **Pin 1**  (3V3)  | —      | Red    | **Pin 6** (or 12) | VDD |
| **Pin 22** (GND)  | —      | Black  | **Pin 5** (or 11) | DGND |
| **Pin 16** | GPIO10 | Yellow | **Pin 1**  | CS |
| **Pin 17** | GPIO11 | Green  | **Pin 2**  | MOSI |
| **Pin 19** | GPIO13 | Blue   | **Pin 3**  | MISO |
| **Pin 18** | GPIO12 | White  | **Pin 4**  | SCLK |
| **Pin 20** | GPIO14 | Orange | **Pin 10** | DRDY |

Total: **7 wires.**

> **Pmod pin 1** is marked on the EVAL-ADXL355-PMDZ silkscreen with a white
> square pad or `1` label. Pins 1–6 are one row; pins 7–12 the other.
> Source: <https://wiki.analog.com/resources/eval/user-guides/circuits-from-the-lab/eval-adxl355-pmdz>.

### 2.2 Physical layout — DevKitC-1 J1 header

The six SPI signals fall on **adjacent pins** on the J1 (left) header — easy
ribbon-cable territory. View is from the top of the board, USB at the top:

```
                        ┌─[USB-UART]─[USB]─┐
                        │    DevKitC-1     │
                        │                  │
  J1 (left)             │                  │           J3 (right)
  ┌──────────────────┐  │                  │   ┌──────────────────┐
  │ pin 1   3V3      │←─ red                    │ pin 1   GND      │
  │ pin 2   3V3      │                          │ pin 2   TX (43)  │
  │ pin 3   RST      │                          │ pin 3   RX (44)  │
  │ pin 4   GPIO4    │← (Phase 3 batt sense)    │ pin 4   GPIO1    │
  │ pin 5   GPIO5    │                          │ ...              │
  │ pin 6   GPIO6    │                          │                  │
  │ pin 7   GPIO7    │                          │                  │
  │ pin 8   GPIO15   │← (Phase 2 SD CD opt.)    │                  │
  │ pin 9   GPIO16   │                          │                  │
  │ pin 10  GPIO17   │                          │                  │
  │ pin 11  GPIO18   │                          │                  │
  │ pin 12  GPIO8    │                          │                  │
  │ pin 13  GPIO3 ⚠  │  strapping (do not use)  │                  │
  │ pin 14  GPIO46 ⚠ │  strapping (do not use)  │                  │
  │ pin 15  GPIO9    │← (Phase 2 SD CS)         │                  │
  │ pin 16  GPIO10   │← yellow  ── ADXL CS      │                  │
  │ pin 17  GPIO11   │← green   ── ADXL MOSI    │                  │
  │ pin 18  GPIO12   │← white   ── ADXL SCLK    │                  │
  │ pin 19  GPIO13   │← blue    ── ADXL MISO    │                  │
  │ pin 20  GPIO14   │← orange  ── ADXL DRDY    │                  │
  │ pin 21  5V       │                          │                  │
  │ pin 22  GND      │← black                   │ pin 21  GND      │
  └──────────────────┘                          │ pin 22  GND      │
                                                └──────────────────┘
```

### 2.3 Power, decoupling, cable

* ADXL355-PMDZ has onboard decoupling (10 nF + 100 nF across VDD/GND) — no
  extra cap needed at this end.
* DevKitC supplies 3.3 V at up to 100 mA from the onboard LDO. ADXL355 draws
  <200 µA — well within budget.
* Keep wires <15 cm if possible. SPI at 10 MHz on long unshielded jumpers can
  pick up noise; if you see CRC/WHO_AM_I errors, shorten or twist pairs (CLK
  with GND, MISO with GND).
* All signals are 3.3 V — **no level shifter required**.

### 2.4 Pre-power-up checklist

- [ ] No shorts: multimeter across 3V3↔GND on the DevKitC = open circuit.
- [ ] No shorts: 3V3↔GND on the ADXL355-PMDZ = open circuit.
- [ ] Continuity: J1 pin 1 ↔ Pmod pin 6 (red wire intact end-to-end).
- [ ] Continuity: J1 pin 22 ↔ Pmod pin 5 (black wire intact end-to-end).
- [ ] Pmod pin 1 (CS) goes to GPIO10, not GPIO11 (common mistake — count from the marked end).
- [ ] No connections to **strapping pins** GPIO0, GPIO3, GPIO45, GPIO46.
- [ ] No connections to USB pins GPIO19, GPIO20.
- [ ] No connections to UART0 pins GPIO43 (TX), GPIO44 (RX).

### 2.5 First power-up (smoke test, no firmware yet)

With USB plugged in:

1. The 3V3 power-on LED on the DevKitC should light.
2. Measure with multimeter: Pmod pin 6 reads 3.3 V ± 0.1 V.
3. ADXL355 idle current draw <1 mA (rest of the board dominates).

If 3.3 V is missing at the Pmod, you have a wire/solder issue — fix before
flashing firmware.

---

## 3. Phase 2 — microSD on shared SPI bus

The SD card sits on the **same SPI bus** as the ADXL355 (different CS line —
the firmware bus driver multiplexes). Six new wires.

### 3.1 Typical microSD breakout pinout

Most generic SPI breakouts (Adafruit, HiLetgo, generic Chinese) expose:
`VCC, GND, MISO, MOSI, SCK, CS, [CD]`. Confirm against your specific module.

### 3.2 Connection table

| ESP32-S3-DevKitC-1 (J1) | GPIO | microSD breakout pin | Signal | Note |
|---|---|---|---|---|
| **Pin 1**  (3V3)  | —      | VCC  | 3.3 V         | Most modules accept 3.3–5 V; 3.3 V is fine |
| **Pin 22** (GND)  | —      | GND  | Ground        | Share with ADXL355 GND |
| **Pin 19** | GPIO13 | MISO | Shared        | **Same wire as ADXL355 MISO** (T-junction OK) |
| **Pin 17** | GPIO11 | MOSI | Shared        | **Same wire as ADXL355 MOSI** |
| **Pin 18** | GPIO12 | SCK  | Shared        | **Same wire as ADXL355 SCLK** |
| **Pin 15** | GPIO9  | CS   | SD-only       | Dedicated chip-select |
| **Pin 8**  | GPIO15 | CD   | Card-detect (optional) | Skip if your module has no CD pin |

### 3.3 Notes

* SPI bus sharing: only the device whose CS is asserted responds. Both CS
  lines (GPIO9 for SD, GPIO10 for ADXL355) must be driven high (idle) when
  not in use — the firmware handles this automatically.
* SD cards typically want a 10 kΩ pull-up on CS (most breakouts include it).
* Bus speed reconciles per-device: ADXL355 ≤10 MHz, SD usually 25 MHz. The
  ESP-IDF SPI driver re-clocks per transaction.
* Keep the shared SPI wires bundled — long stub branches cause reflections.

---

## 4. Phase 3 — LiPo power input + battery sense

The sensor runs from a removable LiPo cell that is **never charged
in-circuit**. To recharge, the operator unplugs the cell at the JST-PH
connector and places it in an off-board charging dock; a freshly charged
cell is plugged back in. This keeps the sensor PCB free of charging
circuitry and lets the rig run continuously by hot-swapping cells (each
swap forces a cold boot — acceptable per the firmware FSM's resume path).

### 4.1 Topology

```
                                         3V3 (regulated)
   ┌──[JST-PH plug]──┐   ┌──────────────┐    ───────► DevKitC pin 1 (3V3)
   │   LiPo cell     ├───┤ buck-boost   │
   │ 3.0–4.2 V       │   │ 3.3 V out    ├──── GND  ── DevKitC pin 22
   └─────────────────┘   └──────────────┘
            │
            │  (raw cell voltage, before regulator)
            ├──── 100 kΩ ──┬─── GPIO4 (ADC1_CH3, battery sense)
            │              │
            │            100 kΩ
            │              │
            └──── GND ─────┘

   External: cell unplugs at JST-PH and is recharged in a separate
   single-cell LiPo charging dock. Charging is OUT OF SCOPE for this
   board — no charge controller, no charge inhibit, no thermistor.
```

The buck-boost regulator is REQUIRED because the LiPo cell ranges
3.0–4.2 V across its discharge curve and the DevKitC's onboard LDO
expects ≥ 4.5 V on the 5V pin. Feeding the 3V3 pin directly with a
regulated 3.3 V from a buck-boost is the cleanest path and bypasses the
onboard LDO. (An alternative is a 5 V step-up into the DevKitC's 5V
pin, which works but wastes power across the onboard LDO.)

The battery sense divider taps the **raw cell** (before the regulator)
so that the ADC reading reflects true cell voltage, not the regulated
output.

### 4.2 Battery voltage divider

Sample VBATT (3.0–4.2 V at the cell) into the ADC range (0–3.3 V):

```
   VBATT ──┬── R1 (100 kΩ) ──┬── ADC node ──── GPIO4 (J1 pin 4)
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
2000 mAh cell (months of standby loss). For lower quiescent, gate the high
side with a MOSFET driven from a spare GPIO — defer to a v2 board.

### 4.3 Connection table

| Function                          | ESP32-S3-DevKitC-1 (J1) | GPIO | Notes |
|---|---|---|---|
| Battery sense (divider mid-point) | **Pin 4** | GPIO4 (ADC1_CH3) | Must be ADC1 — ADC2 is unusable while WiFi active |
| 3V3 power input (from buck-boost) | **Pin 1** or **Pin 2** | — | Regulated 3.3 V; bypasses onboard LDO |
| GND (shared)                      | **Pin 22** | — | Common GND between cell, regulator, divider, DevKitC |

> **Power source rule.** Power the DevKitC from **either** USB **or** the
> battery's buck-boost output — never both at once. To flash while a
> battery is fitted, unplug the cell at the JST-PH first.

> **Hot-swap rule.** Cell swaps power-cycle the sensor. The firmware's
> FSM treats this as a cold boot and resumes streaming from
> `last_acked_seq + 1` once WiFi reconnects (per
> `SPEC-R2-ROCKER-SENSOR` §6.6). No data is lost provided the SD ring
> retention exceeds the swap interval.

---

## 5. Pin reservation — full board (final)

Everything on the DevKitC after all three phases:

| GPIO | DevKitC pin | Used by | Notes |
|---|---|---|---|
| GPIO4  | J1 pin 4  | Battery sense | ADC1_CH3 |
| GPIO9  | J1 pin 15 | SD CS | |
| GPIO10 | J1 pin 16 | ADXL355 CS | FSPICS0 default |
| GPIO11 | J1 pin 17 | ADXL355/SD MOSI | FSPID default |
| GPIO12 | J1 pin 18 | ADXL355/SD SCLK | FSPICLK default |
| GPIO13 | J1 pin 19 | ADXL355/SD MISO | FSPIQ default |
| GPIO14 | J1 pin 20 | ADXL355 DRDY | input |
| GPIO15 | J1 pin 8  | SD CD (optional) | input w/ pull-up |

**Reserved / avoided** (do not wire anything to these):

| GPIO | DevKitC pin | Reason |
|---|---|---|
| GPIO0  | J3 pin 14 | Strapping (boot mode) |
| GPIO3  | J1 pin 13 | Strapping (JTAG select) |
| GPIO19 | J3 pin 20 | USB-D− |
| GPIO20 | J3 pin 19 | USB-D+ |
| GPIO35 | J3 pin 13 | Octal PSRAM (R8/R16V variants) |
| GPIO36 | J3 pin 12 | Octal PSRAM |
| GPIO37 | J3 pin 11 | Octal PSRAM |
| GPIO43 | J3 pin 2  | UART0 TX (serial console) |
| GPIO44 | J3 pin 3  | UART0 RX (serial console) |
| GPIO45 | J3 pin 15 | Strapping (VDD_SPI voltage) |
| GPIO46 | J1 pin 14 | Strapping (ROM print) |

**On-board indicators** (no soldering — already wired by Espressif):

| GPIO | Function | Notes |
|---|---|---|
| GPIO38 *or* GPIO48 | Addressable RGB LED (WS2812-style) | **GPIO38 on DevKitC-1 v1.1**, **GPIO48 on v1.0** — check silkscreen near the LED, or assume v1.1 (current production). Per PDF p.8 & p.10. |

The RGB LED is the primary user-facing status indicator. Reserved for
firmware use; no external wiring needed. Suggested state→colour mapping
(finalised in the firmware spec):

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

**Free for future expansion** (Phase 4+ headroom):
GPIO1, GPIO2, GPIO5, GPIO6, GPIO7, GPIO8, GPIO16, GPIO17, GPIO18, GPIO21,
GPIO39, GPIO40, GPIO41, GPIO42, GPIO47.

---

## 6. References

| Source | Where | What |
|---|---|---|
| `docs/esp-dev-kits-en-master-esp32s3.pdf` | p.7  (Header Block) | DevKitC-1 J1/J3 pin tables |
| `docs/esp-dev-kits-en-master-esp32s3.pdf` | p.8  (Pin Layout) | Annotated DevKitC-1 photo |
| `docs/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf` | p.10–11 (Pin Definitions) | ESP32-S3-WROOM-1 pin functions |
| `docs/esp32-s3-wroom-1_wroom-1u_datasheet_en.pdf` | p.13 (Boot Configurations) | Strapping-pin behaviour |
| `docs/ADXL355.md` | (link) | EVAL-ADXL355-PMDZ user guide |
| EVAL-ADXL355-PMDZ wiki | <https://wiki.analog.com/resources/eval/user-guides/circuits-from-the-lab/eval-adxl355-pmdz> | Pmod pinout |

---

## 7. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-06 | 0.1 | Initial draft. Phase 1/2/3 pin assignments. |
| 2026-05-07 | 0.2 | §1 BoM and §4 (Phase 3) revised: removed on-board charger; battery is power-in only via JST-PH disconnect, recharged externally in a separate dock. Added buck-boost converter requirement and hot-swap rule. |
