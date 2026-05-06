# EVAL-ADICUP360 — vendor reference material

This directory is a **snapshot copy** of Analog Devices' EVAL-ADICUP360
evaluation project repository, kept here for offline reference while
designing the r2-rocker sensor.

## Upstream

* Repository: <https://github.com/analogdevicesinc/EVAL-ADICUP360>
* License: see `LICENSE` in this directory.
* The `.git` history has been stripped. To work with upstream commits,
  re-clone fresh:
  ```bash
  git clone https://github.com/analogdevicesinc/EVAL-ADICUP360.git
  ```

## Why it's here

The EVAL-ADICUP360 is an ARM Cortex-M3 evaluation board with several
ADI-supplied example projects, including drivers and protocol examples
for ADXL accelerometers. We're using it for **reference only** — the
r2-rocker sensor runs on ESP32-S3 in Rust, not on the ADICUP360 in C —
but the example drivers contain useful register-level documentation
of the ADXL355 (initialisation sequences, calibration register reads,
typical SPI command framing) that informs our own Rust driver in
`firmware/esp32-s3/src/`.

When borrowing register layouts or initialisation values from this
material, cite the specific file path within this directory in the
relevant code comment so future readers can trace the lineage.

## What's NOT here

* No ADI proprietary code beyond what's in the public upstream.
* No build tools — the C SDK / Keil project files are present but you
  don't need to build any of it for the r2-rocker work.
