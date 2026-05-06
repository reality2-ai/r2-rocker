# r2-rocker firmware (ESP32-S3)

Sensor firmware for the r2-rocker structural-health-monitoring rig.
Currently at **Phase 0.5** — pre-soldering smoke test.

## What this firmware does (Phase 0.5)

* Boots.
* Prints `r2-rocker firmware v0.1.0` and the device's WiFi MAC over UART.
* Loops, printing one state name per second from the FSM in
  `SPEC-R2-ROCKER-SENSOR.md` §4.1.

Nothing else. It does not read the ADXL355, talk to the dashboard,
write to SD, or sample the battery — those land in Phases 1–5 once the
hardware is soldered and the protocol stack is in place.

The **intended partition layout is documented in `partitions.csv`** —
two 3 MB OTA slots + 1.875 MB `storage` data partition, no factory
slot. Phase 0.5 uses ESP-IDF's default single-app table for now; the
custom layout is wired in when OTA is implemented in Phase 9. The
supported path with `esp-idf-sys` is via
`[package.metadata.esp-idf-sys.extra_components]` in `Cargo.toml`,
not a simple sdkconfig path setting (the file path doesn't survive
into the auto-generated build directory).

This deferral does mean a one-time re-flash of the partition table
when OTA lands. That's an acceptable cost — a partition-table swap
takes seconds.

## Toolchain prerequisites

This crate uses `esp-idf-svc` and the Xtensa Rust toolchain. One-time
setup:

```bash
espup install
source ~/export-esp.sh    # or add the source line to your shell rc
```

`espup install` brings in the Xtensa LLVM, the `esp` Rust toolchain,
and the ESP-IDF SDK source.

## Build & flash

Plug the DevKitC-1 into the **USB-to-UART** port (the Micro-USB labelled
`UART` on the silkscreen — not the native USB-OTG one).

```bash
cd firmware/esp32-s3
cargo run --release
```

`cargo run` invokes `espflash flash --monitor` (per `.cargo/config.toml`),
which cross-compiles for `xtensa-esp32s3-espidf`, flashes the binary, and
opens a serial monitor.

> **First build takes 15–30 minutes.** `esp-idf-svc` builds the entire
> ESP-IDF C SDK on first build. Subsequent builds are fast.

## Expected UART output

```
I (315) r2_rocker_firmware: ================================================
I (315) r2_rocker_firmware: r2-rocker firmware v0.1.0
I (325) r2_rocker_firmware: Phase 0.5 — pre-soldering smoke test
I (325) r2_rocker_firmware: ================================================
I (335) r2_rocker_firmware:
I (335) r2_rocker_firmware: This firmware confirms the build, flash, and boot path.
I (345) r2_rocker_firmware: It does not yet read sensors or talk to the network.
I (355) r2_rocker_firmware:
I (355) r2_rocker_firmware: Device MAC: 7c:df:a1:b2:c3:d4
I (365) r2_rocker_firmware:
I (365) r2_rocker_firmware: Beginning FSM-state heartbeat (1 Hz).
I (375) r2_rocker_firmware: Press Ctrl-C in the monitor to exit.
I (385) r2_rocker_firmware:
I (1385) r2_rocker_firmware: [t=    0s] FSM-demo state: BOOT
I (2385) r2_rocker_firmware: [t=    1s] FSM-demo state: ADVERTISING
I (3385) r2_rocker_firmware: [t=    2s] FSM-demo state: BLE_CONNECTED
I (4385) r2_rocker_firmware: [t=    3s] FSM-demo state: WIFI_CONNECTING
…
```

If you see this, the toolchain works and the board is alive — proceed
to Phase 1 once the ADXL355 is soldered per `HARDWARE-WIRING.md`.

## Board-variant notes

`sdkconfig.defaults` is tuned for **ESP32-S3-DevKitC-1-N8R8** (8 MB
flash, 8 MB octal PSRAM). For other variants:

* **N32R16V**: change `CONFIG_ESPTOOLPY_FLASHSIZE_8MB=y` to
  `CONFIG_ESPTOOLPY_FLASHSIZE_32MB=y`. Note: this variant has **1.8 V SPI**
  — GPIO47/48 are 1.8 V signal levels, not 3.3 V. The on-board RGB LED
  (when we add it in a later firmware version) will need different drive.
* **WROOM-1U-N8R8** (external antenna): same settings as N8R8.
* **No-PSRAM** modules: comment out the `CONFIG_SPIRAM*` lines.

## Common issues

* **`error: linker `ldproxy` not found`** — `espup install` not run, or
  `~/export-esp.sh` not sourced in this shell.
* **`unable to find a chip`** — DevKitC plugged into wrong USB port, or
  not in download mode. Hold BOOT, press RESET, release BOOT, then retry
  flashing.
* **Build fails with PSRAM-related error** — your module has no PSRAM;
  comment out the `CONFIG_SPIRAM*` lines in `sdkconfig.defaults`.

## Exit the serial monitor

Press `Ctrl-C` in the terminal where `cargo run` is attached.
