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
# dir, not our crate root. firmware/esp32-s3/<carrier>/build.rs copies
# the CSV there each build — but on a fresh checkout, build.rs runs
# AFTER esp-idf-sys's CMake configure (regular dep ordering), so the
# first build fails to find partitions.csv at all (ninja errors with
# "missing and no known rule to make it"). This script bridges that
# gap.
#
# As of ADR-001 (2026-05-11) the firmware tree supports multiple
# parallel carrier-board implementations under
# `firmware/esp32-s3/<carrier>/`. This script iterates over every
# carrier directory present.
#
# Idempotent — safe to re-run.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW_BASE="${REPO_ROOT}/firmware/esp32-s3"

if [[ ! -d "${FW_BASE}" ]]; then
    echo "FATAL: ${FW_BASE} not found." >&2
    exit 1
fi

shopt -s nullglob
total_staged=0

for carrier_dir in "${FW_BASE}"/*/; do
    carrier="$(basename "${carrier_dir}")"
    src="${carrier_dir}partitions.csv"

    # Skip non-carrier directories (releases/, etc.) — must have a
    # partitions.csv at the carrier root to be a valid build target.
    if [[ ! -f "${src}" ]]; then
        continue
    fi

    echo "Carrier: ${carrier}"

    carrier_staged=0
    for d in "${carrier_dir}"target/xtensa-esp32s3-espidf/*/build/esp-idf-sys-*/out; do
        cp -f "${src}" "${d}/partitions.csv"
        echo "  staged → ${d#${REPO_ROOT}/}/partitions.csv"
        carrier_staged=$((carrier_staged + 1))
    done

    if (( carrier_staged == 0 )); then
        echo "  (no esp-idf-sys build dirs yet — run 'cargo build --release' once first)"
    fi

    total_staged=$((total_staged + carrier_staged))
done

shopt -u nullglob

echo
if (( total_staged == 0 )); then
    echo "Nothing staged. On a fresh checkout: run 'cargo build --release'"
    echo "in your chosen carrier directory first, then re-run this script,"
    echo "then rebuild to pick up the custom partition table."
else
    echo "Pre-staged partitions.csv in ${total_staged} build dir(s)."
    echo "Custom OTA-slot partition layout will take effect on the next"
    echo "'cargo build --release' in each affected carrier directory."
fi
