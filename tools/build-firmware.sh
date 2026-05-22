#!/usr/bin/env bash
# tools/build-firmware.sh — build the ESP32-S3 firmware for a chosen
# carrier, package the OTA-ready application image (.bin), AND archive
# a copy under `firmware/esp32-s3/<carrier>/releases/<fw_ver>.bin` for
# git-tracked posterity.
#
# `cargo espflash flash` does the ELF→app-image conversion internally
# when flashing over USB, but doesn't write the .bin to disk; the OTA
# receiver (r2-esp::ota_tcp) needs the same image format on the wire,
# so this script runs `espflash save-image` after the build.
#
# Usage:
#   tools/build-firmware.sh                # defaults to devkitc
#   tools/build-firmware.sh devkitc
#   tools/build-firmware.sh xiao
#
# After this completes:
# * Latest build artifact (overwritten on each run) is at
#   `firmware/esp32-s3/<carrier>/target/xtensa-esp32s3-espidf/release/r2-rocker-firmware.bin`
#   — push this via /api/ota/{addr}.
# * Versioned archive copy lives at
#   `firmware/esp32-s3/<carrier>/releases/r2-rocker-firmware-<fw_ver>.bin`
#   — `git add` this when you want to record the release for posterity.
#   The filename matches the `fw_ver` string the firmware bakes into
#   `r2.sensor.announce`, so a sensor's reported version is searchable
#   directly against the releases directory.

set -euo pipefail

CARRIER="${1:-devkitc}"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW_DIR="${REPO_ROOT}/firmware/esp32-s3/${CARRIER}"
REL_DIR="${FW_DIR}/releases"

if [[ ! -f "${FW_DIR}/Cargo.toml" ]]; then
    echo "ERROR: no Cargo.toml at ${FW_DIR}" >&2
    echo "Available carriers:" >&2
    ls -1 "${REPO_ROOT}/firmware/esp32-s3" | grep -v -E '^(README|releases)' | sed 's/^/  /' >&2
    exit 1
fi

# Trust-Group key guard. The firmware embeds trust_keys/tg_pub.bin
# at compile time via include_bytes!. Two failure modes to catch
# before kicking off a 30 s xtensa build:
#
#   1. No tg_pub.bin / tg_cert.bin at all — fresh clone, no deployment
#      keys yet. The build would fail late with an include_bytes!
#      error; we'd rather print actionable setup instructions early.
#
#   2. tg_pub.bin is the canonical upstream demo key. Identifiable
#      by SHA-256 against a hash recorded under
#      `trust_keys/.tg_pub_demo_sha256`. Building against this key
#      means every lab that clones the public repo gets the same TG
#      embedded, which only works for the one deployment that owns
#      the matching tg_priv.bin. Per
#      audits/2026-05-23-architectural-gaps.md (post-handoff
#      recommendation), refuse this case so the new lab is forced
#      to keygen first.
TG_PUB="${REPO_ROOT}/trust_keys/tg_pub.bin"
TG_CERT="${REPO_ROOT}/trust_keys/tg_cert.bin"
KEYGEN_HINT=$(cat <<'EOF'
This deployment has no Trust Group keys, or is still on the upstream
demo keys. Each r2-rocker deployment needs its own keypair — sensors
verify their certs against the public key baked into the firmware at
build time, so cloning the repo and re-using the committed key would
mean every lab shares a TG identity (whoever holds the matching
private key is the only one who can sign certs).

Generate a fresh TG keypair for this deployment (one-time per lab):

    cd "$REPO_ROOT" && cargo run -p r2-rocker-tg --release -- init

That writes:
  trust_keys/tg_pub.bin            (committed; embedded into firmware)
  trust_keys/tg_cert.bin           (committed; self-signed KeyHolder cert)
  ~/.config/r2-rocker/tg_signer/tg_priv.bin   (off-tree; read by dashboard)

After it completes, re-run this script. See SECRETS-POLICY.md for the
full key-handling policy.
EOF
)

if [[ ! -s "${TG_PUB}" || ! -s "${TG_CERT}" ]]; then
    echo "ERROR: no Trust Group keys at trust_keys/tg_pub.bin (or tg_cert.bin)." >&2
    echo "" >&2
    echo "$KEYGEN_HINT" >&2
    exit 1
fi

DEMO_HASH_FILE="${REPO_ROOT}/trust_keys/.tg_pub_demo_sha256"
if [[ -s "${DEMO_HASH_FILE}" ]]; then
    DEMO_HASH=$(cat "${DEMO_HASH_FILE}")
    ACTUAL_HASH=$(sha256sum "${TG_PUB}" | awk '{print $1}')
    if [[ "${ACTUAL_HASH}" == "${DEMO_HASH}" ]]; then
        echo "ERROR: trust_keys/tg_pub.bin matches the upstream demo key SHA." >&2
        echo "" >&2
        echo "$KEYGEN_HINT" >&2
        exit 1
    fi
fi

# Pull in the ESP-IDF / xtensa toolchain if exported. Best-effort —
# users in a fresh shell still need to source `~/export-esp.sh` first.
if [[ -f "${HOME}/export-esp.sh" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/export-esp.sh" >/dev/null 2>&1 || true
fi

cd "${FW_DIR}"

echo "==> cargo build --release (xtensa-esp32s3-espidf) — carrier=${CARRIER}"
cargo build --release

ELF="target/xtensa-esp32s3-espidf/release/r2-rocker-firmware"
BIN="${ELF}.bin"

echo "==> espflash save-image  →  ${BIN}"
espflash save-image --chip esp32s3 "${ELF}" "${BIN}"

# Compute the same FW_VER string the firmware bakes in via build.rs:
#   <semver>-<YYYY-MM-DD-HH:MM>+<git-short-sha>[-dirty]
# Same git + date inputs as build.rs, so within the same minute the
# script's filename matches the announce string exactly. (May drift by
# 1 minute in pathological build-races; close enough for archival.)
SEMVER=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "${FW_DIR}/Cargo.toml")
SHA=$(git -C "${REPO_ROOT}" rev-parse --short=8 HEAD 2>/dev/null || echo unknown)
DIRTY=""
if ! git -C "${REPO_ROOT}" diff-index --quiet HEAD -- 2>/dev/null; then DIRTY="-dirty"; fi
TS=$(date -u +%Y-%m-%d-%H:%M)
FW_VER="${SEMVER}-${TS}+${SHA}${DIRTY}"

mkdir -p "${REL_DIR}"
ARCHIVE="${REL_DIR}/r2-rocker-firmware-${FW_VER}.bin"
cp "${BIN}" "${ARCHIVE}"

echo
echo "OTA-ready image (use this with /api/ota/{addr}):"
ls -la "${BIN}"
echo
echo "Versioned archive copy (git add to record the release):"
ls -la "${ARCHIVE}"
