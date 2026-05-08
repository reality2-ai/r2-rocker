#!/usr/bin/env bash
# tools/build-firmware.sh — build the ESP32-S3 firmware, package the
# OTA-ready application image (.bin), AND archive a copy under
# `firmware/esp32-s3/releases/<fw_ver>.bin` for git-tracked posterity.
#
# `cargo espflash flash` does the ELF→app-image conversion internally
# when flashing over USB, but doesn't write the .bin to disk; the OTA
# receiver (r2-esp::ota_tcp) needs the same image format on the wire,
# so this script runs `espflash save-image` after the build.
#
# After this completes:
# * Latest build artifact (overwritten on each run) is at
#   `firmware/esp32-s3/target/xtensa-esp32s3-espidf/release/r2-rocker-firmware.bin`
#   — push this via /api/ota/{addr}.
# * Versioned archive copy lives at
#   `firmware/esp32-s3/releases/r2-rocker-firmware-<fw_ver>.bin`
#   — `git add` this when you want to record the release for posterity.
#   The filename matches the `fw_ver` string the firmware bakes into
#   `r2.sensor.announce`, so a sensor's reported version is searchable
#   directly against the releases directory.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW_DIR="${REPO_ROOT}/firmware/esp32-s3"
REL_DIR="${FW_DIR}/releases"

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

# Compute the same FW_VER string the firmware bakes in via build.rs:
#   <semver>-<YYYY-MM-DD-HH:MM>-sim+<git-short-sha>[-dirty]
# Same git + date inputs as build.rs, so within the same minute the
# script's filename matches the announce string exactly. (May drift by
# 1 minute in pathological build-races; close enough for archival.)
SEMVER=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "${FW_DIR}/Cargo.toml")
SHA=$(git -C "${REPO_ROOT}" rev-parse --short=8 HEAD 2>/dev/null || echo unknown)
DIRTY=""
if ! git -C "${REPO_ROOT}" diff-index --quiet HEAD -- 2>/dev/null; then DIRTY="-dirty"; fi
TS=$(date -u +%Y-%m-%d-%H:%M)
FW_VER="${SEMVER}-${TS}-sim+${SHA}${DIRTY}"

mkdir -p "${REL_DIR}"
ARCHIVE="${REL_DIR}/r2-rocker-firmware-${FW_VER}.bin"
cp "${BIN}" "${ARCHIVE}"

echo
echo "OTA-ready image (use this with /api/ota/{addr}):"
ls -la "${BIN}"
echo
echo "Versioned archive copy (git add to record the release):"
ls -la "${ARCHIVE}"
