#!/bin/bash
# Install m10_sensor.py as a systemd service on a Unihiker M10.
#
# Usage: ./m10_install.sh <gateway_ip> [gateway_port] [rate_hz]
# Example: ./m10_install.sh 192.168.2.78 21042 10
#
# Run this ON the M10 (or via SSH).

set -e

GATEWAY_IP="${1:?Usage: $0 <gateway_ip> [port] [rate_hz]}"
GATEWAY_PORT="${2:-21042}"
RATE_HZ="${3:-10}"

INSTALL_DIR="/opt/r2"
SCRIPT="m10_sensor.py"
SERVICE="r2-sensor"

echo "Installing R2 sensor service..."
echo "  Gateway: ${GATEWAY_IP}:${GATEWAY_PORT}"
echo "  Rate: ${RATE_HZ} Hz"

# Copy script
mkdir -p "$INSTALL_DIR"
cp "$(dirname "$0")/$SCRIPT" "$INSTALL_DIR/$SCRIPT"
chmod +x "$INSTALL_DIR/$SCRIPT"

# Write config (easy to change gateway IP later)
cat > "$INSTALL_DIR/sensor.conf" << EOF
GATEWAY_IP=${GATEWAY_IP}
GATEWAY_PORT=${GATEWAY_PORT}
RATE_HZ=${RATE_HZ}
EOF

# Create systemd service
cat > /etc/systemd/system/${SERVICE}.service << EOF
[Unit]
Description=R2 Sensor Bridge (M10 accelerometer → R2-WIRE)
After=network.target
# Wait for PinPong/GD32 to be ready
After=multi-user.target

[Service]
Type=simple
Environment=DISPLAY=:0
EnvironmentFile=${INSTALL_DIR}/sensor.conf
ExecStartPre=/bin/sleep 5
ExecStart=/usr/bin/python3 ${INSTALL_DIR}/${SCRIPT} \${GATEWAY_IP} \${GATEWAY_PORT} \${RATE_HZ}
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

# Fix RTL8723DS: disable WiFi power save (causes TCP drops after ~15s)
echo "options rtl8723ds rtw_power_mgnt=0" > /etc/modprobe.d/rtl8723ds.conf
echo 0 > /sys/module/rtl8723ds/parameters/rtw_power_mgnt 2>/dev/null || true

# Fix NM: prevent p2p0 (WiFi Direct virtual iface) being selected for connections
# Without this, NM picks p2p0 over wlan0 and the sensor can never reach the gateway
mkdir -p /etc/NetworkManager/conf.d
cat > /etc/NetworkManager/conf.d/99-r2.conf << 'NMEOF'
[device-p2p0-unmanaged]
match-device=interface-name:p2p0
managed=false
NMEOF
systemctl reload NetworkManager 2>/dev/null || true

# Enable and start
systemctl daemon-reload
systemctl enable ${SERVICE}.service
systemctl start ${SERVICE}.service

echo ""
echo "✅ R2 sensor service installed and running."
echo ""
echo "  Status:  systemctl status ${SERVICE}"
echo "  Logs:    journalctl -u ${SERVICE} -f"
echo "  Config:  ${INSTALL_DIR}/sensor.conf"
echo "  Stop:    systemctl stop ${SERVICE}"
echo ""
echo "To change gateway IP:"
echo "  Edit ${INSTALL_DIR}/sensor.conf"
echo "  systemctl restart ${SERVICE}"
