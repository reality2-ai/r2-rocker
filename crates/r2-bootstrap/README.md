# r2-bootstrap

BLE-based sensor bootstrap tool for R2 networks.

Scans for nearby sensors advertising an R2-BEACON, offers WiFi credentials via L2CAP CoC, and validates the TCP R2-WIRE session once the sensor connects.

## What it does

1. **Creates a WiFi hotspot** (via nmcli) — or uses `--ssid`/`--psk` for an existing network
2. **Scans BLE** for devices advertising `ai.reality2.device.sensor` class (or any target class)
3. **Connects L2CAP CoC** to each discovered sensor (PSM 0x00D2)
4. **Sends `#wifi_offer`** with SSID, PSK, gateway IP, and port
5. **Waits for UDP presence** broadcast from the sensor (confirms WiFi join)
6. **Validates TCP session** with `test.ping` / `test.pong`

## Usage

```bash
# Hotspot mode (recommended for field use — no external WiFi needed)
r2-bootstrap

# Existing network
r2-bootstrap --ssid MyNetwork --psk MyPassword

# Tune scan window
r2-bootstrap --scan-secs 20

# Target a different sensor class
r2-bootstrap --class ai.reality2.device.sensor
```

## One-time setup (hotspot auth)

On Linux with NetworkManager, hotspot creation requires elevated privileges. Run once:

```bash
sudo bash tools/r2-bootstrap/setup-hotspot-auth.sh
```

This adds a NOPASSWD sudoers rule for `nmcli dev wifi hotspot` only.

## Spec compliance

Implements:
- **R2-BOOTSTRAP** — full bootstrap sequence (both scan and offer phases)
- **R2-BEACON** — parses manufacturer data, extracts RBID + class hash
- **R2-BLE** — L2CAP CoC on PSM 0x00D2, LE-prefixed length framing (§6.4)
- **R2-WIFI** — `#wifi_offer` frame encoding (CBOR: ssid, psk, gateway_ip, port, ttl)
- **R2-WIRE** — compact frame encoding/decoding for test.ping / test.pong

## Field-proven

Tested on:
- **Gateway**: Tuxedo laptop (Intel/MediaTek WiFi, x86_64 Linux)
- **Sensor**: Unihiker M10 (RTL8723DS, aarch64 Debian Buster)
- **Result**: Full wireless bootstrap in ~28s, 9ms ping RTT, 10Hz sensor stream to dashboard

## Known limitations

- **RTL8723DS one-shot**: The M10's BLE chip only accepts one L2CAP connection per `r2-sensor` process start. Restart `r2-bootstrap.service` on the sensor before re-running bootstrap.
- **RBID rotation**: Currently time-based random; HMAC-SHA256 rotation (R2-BEACON §6.1) deferred.
