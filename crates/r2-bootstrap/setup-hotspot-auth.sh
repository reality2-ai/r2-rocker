#!/bin/bash
# setup-hotspot-auth.sh — One-time setup: allow r2-bootstrap to create WiFi hotspots without a password.
# Run this once from the Tuxedo terminal (requires sudo password).
# After this, 'nmcli dev wifi hotspot ...' works without auth from SSH sessions.

set -e

SUDOERS_FILE="/etc/sudoers.d/r2-bootstrap-nmcli"
USER="${SUDO_USER:-$(whoami)}"

echo "[setup] Adding NOPASSWD sudoers rule for nmcli hotspot (user: $USER)..."

cat > "$SUDOERS_FILE" <<EOF
# Allow r2-bootstrap to create/destroy WiFi hotspots without a password.
# Added by setup-hotspot-auth.sh
$USER ALL=(ALL) NOPASSWD: /usr/bin/nmcli dev wifi hotspot *
$USER ALL=(ALL) NOPASSWD: /usr/bin/nmcli dev disconnect *
$USER ALL=(ALL) NOPASSWD: /usr/bin/nmcli con delete *
EOF

chmod 0440 "$SUDOERS_FILE"
visudo -c -f "$SUDOERS_FILE" && echo "[setup] ✓ Sudoers rule written to $SUDOERS_FILE" || (rm "$SUDOERS_FILE" && echo "[setup] ERROR: invalid sudoers syntax — rule NOT installed" && exit 1)

echo "[setup] Done. r2-bootstrap can now create hotspots via 'sudo nmcli dev wifi hotspot ...' without a password."
echo "[setup] Test with: sudo -n nmcli dev wifi hotspot ssid R2-test password testpass1234"
