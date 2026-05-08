#!/usr/bin/env bash
# tools/build-firmware.sh — build the ESP32-S3 firmware AND package the
# OTA-ready application image (.bin) alongside the ELF.
#
# `cargo espflash flash` does the ELF→app-image conversion internally
# when flashing over USB, but doesn't write the .bin to disk; the OTA
# receiver (r2-esp::ota_tcp) needs the same image format on the wire,
# so this script runs `espflash save-image` after the build.
#
# After this completes the image to push via the dashboard's
# /api/ota/{addr} endpoint is at:
#   firmware/esp32-s3/target/xtensa-esp32s3-espidf/release/r2-rocker-firmware.bin

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW_DIR="${REPO_ROOT}/firmware/esp32-s3"

# Pull in the ESP-IDF / xtensa toolchain if exported. Best-effort —
# users in a fresh shell still need to source `~/export-esp.sh` first.
if [[ -f "${HOME}/export-esp.sh" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/export-esp.sh" >/dev/null 2>&1 || true
fi

cd "${FW_DIR}"

echo "==> cargo build --release (xtensa-esp32s3-espidf)"
cargo build --release

ELF="target/xtensa-esp32s3-espidf/release/r2-rocker-firmware"
BIN="${ELF}.bin"

echo "==> espflash save-image  →  ${BIN}"
espflash save-image --chip esp32s3 "${ELF}" "${BIN}"

echo
echo "OTA-ready image:"
ls -la "${BIN}"
