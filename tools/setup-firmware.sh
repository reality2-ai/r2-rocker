#!/usr/bin/env bash
# tools/setup-firmware.sh — one-time firmware setup.
#
# Pre-stages partitions.csv into any existing esp-idf-sys CMake build
# directories so the FIRST `cargo build --release` of a fresh clone
# uses the custom OTA-slot partition table rather than the ESP-IDF
# default.
#
# Why this matters: ESP-IDF's CMake resolves
# CONFIG_PARTITION_TABLE_CUSTOM_FILENAME relative to esp-idf-sys's OUT
# dir, not our crate root. firmware/esp32-s3/build.rs copies the CSV
# there each build — but on a fresh checkout, build.rs runs AFTER
# esp-idf-sys's CMake configure (regular dep ordering), so the first
# build still uses the default table. This script bridges that gap.
#
# Same shape as r2-core/platforms/esp32-s3/build-and-flash-dfr1195-peripheral.sh.
# Idempotent — safe to re-run.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW_DIR="${REPO_ROOT}/firmware/esp32-s3"
SRC="${FW_DIR}/partitions.csv"

if [[ ! -f "${SRC}" ]]; then
    echo "FATAL: ${SRC} not found." >&2
    exit 1
fi

shopt -s nullglob
copied=0
for d in "${FW_DIR}"/target/xtensa-esp32s3-espidf/*/build/esp-idf-sys-*/out; do
    cp -f "${SRC}" "${d}/partitions.csv"
    echo "  staged → ${d}/partitions.csv"
    copied=$((copied + 1))
done
shopt -u nullglob

if (( copied == 0 )); then
    echo "No esp-idf-sys build dirs yet; run 'cargo build --release' once,"
    echo "then re-run this script and rebuild to pick up the custom partition table."
else
    echo
    echo "Pre-staged ${copied} build dir(s). Custom OTA-slot partition layout"
    echo "will take effect on the next 'cargo build --release'."
fi
