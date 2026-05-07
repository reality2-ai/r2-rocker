#!/bin/sh
# Install R2 Notekeeper as a Linux desktop application
# Copies icons to XDG icon directories and installs .desktop file

set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ICON_DIR="$SCRIPT_DIR/icons"

echo "Installing R2 Notekeeper icons..."
for size in 16 24 32 48 64 96 128 256 512; do
    dest="$HOME/.local/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dest"
    cp "$ICON_DIR/icon-${size}.png" "$dest/r2-notekeeper.png"
done

# SVG for scalable
mkdir -p "$HOME/.local/share/icons/hicolor/scalable/apps"
cp "$ICON_DIR/notekeeper.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/r2-notekeeper.svg"

echo "Installing desktop entry..."
cp "$SCRIPT_DIR/r2-notekeeper.desktop" "$HOME/.local/share/applications/"

echo "Updating icon cache..."
gtk-update-icon-cache -f "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

echo "Done. R2 Notekeeper should appear in your application menu."
