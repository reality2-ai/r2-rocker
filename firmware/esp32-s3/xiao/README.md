# r2-rocker firmware — Seeed XIAO ESP32-S3

Sensor firmware for the r2-rocker structural-health-monitoring rig,
built for the **Seeed XIAO ESP32-S3 (Pre-Soldered)** carrier per
ADR-001. The DevKitC-1 carrier remains a fully-supported alternative
at `../devkitc/`; see `../../specifications/HARDWARE-WIRING.md` for
the carrier-choice framework.

The R2 protocol stack, ADXL355 driver, sender pipeline, LED FSM, and
OTA logic are shared with the DevKitC build — only pin assignments,
sdkconfig comments, and the partition-table contextual notes differ.

## What this firmware does

This crate carries the same firmware behaviour as the DevKitC build
through Phase 9-light:

* Phase 0–1: boot, SPI bring-up, ADXL355 enumeration, simulated /
  real sample stream
* Phase 5L: WS2812 RGB LED state-machine indicator
* Phase 6: BLE-bootstrap FSM with R2-BEACON advertising and L2CAP
  `#wifi_offer` reception
* Phase 7–8: WiFi STA association, sample streaming to the dashboard
* Phase 9-light: OTA over WiFi with bootloader rollback

See `SPEC-R2-ROCKER-SENSOR.md` for the behavioural contract.

## XIAO-specific pin assignments

| Function | XIAO silkscreen | GPIO |
|---|---|---|
| ADXL355 SPI CS | D0 | GPIO1 |
| ADXL355 DRDY (optional) | D1 | GPIO2 |
| Battery sense ADC | D3 | GPIO4 |
| SD CS (Phase 2) | D4 | GPIO5 |
| External WS2812 DIN | D5 | GPIO6 |
| SPI SCK (shared) | D8 | GPIO7 |
| SPI MISO (shared) | D9 | GPIO8 |
| SPI MOSI (shared) | D10 | GPIO9 |

Wiring reference: `../../specifications/HARDWARE-WIRING-XIAO.md` §2.1.

## Toolchain prerequisites

Same as the DevKitC build — Xtensa Rust toolchain via `espup`:

```bash
espup install
source ~/export-esp.sh    # or add the source line to your shell rc
```

The XIAO and the DevKitC both run on `xtensa-esp32s3-espidf` — no
toolchain change.

## Build & flash

Plug the XIAO into the **USB-C** connector and run:

```bash
cd firmware/esp32-s3/xiao
cargo run --release
```

`cargo run` invokes `espflash flash --monitor` (per `.cargo/config.toml`),
which cross-compiles for `xtensa-esp32s3-espidf`, flashes the binary, and
opens a serial monitor over the XIAO's native USB-Serial-JTAG.

> **First build takes 15–30 minutes.** `esp-idf-svc` builds the entire
> ESP-IDF C SDK on first build. Subsequent builds are fast.

> **Bootloader mode:** if `espflash` cannot detect the chip, hold the
> small `BOOT` button on the XIAO (or short the BOOT pad if your SKU
> has no button) while you press / release `RESET`, then retry the
> flash. Most of the time this is not needed — the XIAO drops into
> bootloader automatically when `espflash` resets it over USB-Serial-
> JTAG.

## Expected UART output

Same as the DevKitC build — boot banner, identity load, beacon start,
WiFi association attempt, sender thread spawn.

## Board-variant notes

`sdkconfig.defaults` is tuned for the **XIAO ESP32-S3 (Pre-Soldered)**:

* 8 MB flash (`CONFIG_ESPTOOLPY_FLASHSIZE_8MB=y`)
* 8 MB octal SPI PSRAM (the ESP32-S3R8 die)
* Native USB-Serial-JTAG for the console
* NimBLE host stack

For the **XIAO ESP32-S3 Plus** (16 MB flash, additional GPIOs D11–D18):

* Change `CONFIG_ESPTOOLPY_FLASHSIZE_8MB=y` to
  `CONFIG_ESPTOOLPY_FLASHSIZE_16MB=y`.
* Optionally extend `partitions.csv` to use the extra space (larger
  OTA slots or larger storage partition).
* GPIO map differs slightly — verify in `HARDWARE-WIRING-XIAO.md` (or
  a future `HARDWARE-WIRING-XIAO-PLUS.md`).

## Common issues

* **`error: linker `ldproxy` not found`** — `espup install` not run,
  or `~/export-esp.sh` not sourced in this shell.
* **`unable to find a chip`** — XIAO not in bootloader mode. Hold
  BOOT, press RESET, release BOOT, then retry. Or unplug / replug the
  USB-C cable.
* **WS2812 doesn't light up** — verify D5 (GPIO6) is wired to the
  WS2812's DIN, not DOUT. Confirm the WS2812 module accepts 3.3 V VCC
  (most do).
* **ADXL355 WHO_AM_I mismatch** — verify the four SPI signals are on
  the right XIAO pins (D8 SCK, D9 MISO, D10 MOSI, D0 CS) and that
  3V3/GND reach the Pmod's pin 6 / pin 5.

## Exit the serial monitor

Press `Ctrl-C` in the terminal where `cargo run` is attached.

## Relationship to the DevKitC build

This XIAO build is the **current default** carrier for new sensor
units (per ADR-001). The DevKitC build at `../devkitc/` is preserved
as a fully-supported alternative — both build against the same Rust
target, same dependencies, same ESP-IDF version, and produce
functionally-equivalent firmware. The only differences are in
`main.rs` (pin literals) and the carrier-specific comments in
`Cargo.toml`, `sdkconfig.defaults`, and `partitions.csv`.

A Cargo-workspace consolidation into a shared library crate (with
both binaries pulling from a common `r2-rocker-firmware-lib`) is a
plausible future cleanup — out of scope for v0.1, see ADR-001
"Repository layout".
