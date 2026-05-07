#!/usr/bin/env python3
"""Test sender — simulates a sensor sending R2-WIRE acceleration events.

Usage: python3 test_sender.py [gateway_host] [gateway_port]
Default: localhost:21042

Generates sine-wave acceleration data with noise to simulate
a rocking motion sensor. Useful for testing the dashboard without
real hardware.
"""

import socket
import struct
import time
import math
import sys
import random

# R2 event hashes (FNV-1a 32-bit, must match r2-fnv)
def fnv1a_32(data: bytes) -> int:
    h = 0x811c9dc5
    for b in data:
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h

ACCELERATION = fnv1a_32(b"acceleration")
RUN_STATE = fnv1a_32(b"run_state")
BATTERY_STATUS = fnv1a_32(b"battery_status")

# Verify hashes match the Rust implementation
assert ACCELERATION == 0x2FA0BA9D, f"acceleration hash mismatch: 0x{ACCELERATION:08X}"

def cbor_encode_map(pairs: list[tuple]) -> bytes:
    """Encode a simple CBOR map with string keys and float32 values."""
    buf = bytearray()
    # Map header
    n = len(pairs)
    if n < 24:
        buf.append(0xA0 | n)
    else:
        buf.append(0xB8)
        buf.append(n)
    
    for key, value in pairs:
        # Text string key
        key_bytes = key.encode('utf-8')
        klen = len(key_bytes)
        if klen < 24:
            buf.append(0x60 | klen)
        else:
            buf.append(0x78)
            buf.append(klen)
        buf.extend(key_bytes)
        
        # Float32 value (CBOR major type 7, additional info 26)
        buf.append(0xFA)  # float32
        buf.extend(struct.pack('>f', value))
    
    return bytes(buf)

def build_frame(event_hash: int, payload: bytes, msg_id: int = 0) -> bytes:
    """Build an R2-WIRE-compatible frame with length prefix."""
    body = struct.pack('>BHI', 0x01, msg_id, event_hash) + payload
    frame = struct.pack('>H', len(body)) + body
    return frame

def main():
    host = sys.argv[1] if len(sys.argv) > 1 else 'localhost'
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 21042
    rate = float(sys.argv[3]) if len(sys.argv) > 3 else 10.0  # Hz

    print(f"Connecting to {host}:{port}...")
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.connect((host, port))
    print(f"Connected! Sending at {rate} Hz")
    print(f"  acceleration hash: 0x{ACCELERATION:08X}")

    msg_id = 0
    t0 = time.time()

    try:
        while True:
            t = time.time() - t0
            
            # Simulate rocking motion:
            # - Primary frequency ~0.5 Hz (typical structural sway)
            # - Secondary harmonic at 1.5 Hz
            # - Random noise (sensor noise floor)
            x = 0.02 * math.sin(2 * math.pi * 0.5 * t) + 0.005 * random.gauss(0, 1)
            y = 0.03 * math.sin(2 * math.pi * 0.5 * t + 1.2) + 0.005 * random.gauss(0, 1)
            z = 0.98 + 0.01 * math.sin(2 * math.pi * 1.5 * t) + 0.003 * random.gauss(0, 1)

            # CBOR payload: {"x": float, "y": float, "z": float}
            payload = cbor_encode_map([("x", x), ("y", y), ("z", z)])
            
            frame = build_frame(ACCELERATION, payload, msg_id & 0xFFFF)
            sock.sendall(frame)
            
            msg_id += 1
            if msg_id % 50 == 0:
                print(f"  [{msg_id}] x={x:.4f} y={y:.4f} z={z:.4f}")

            time.sleep(1.0 / rate)

    except KeyboardInterrupt:
        print(f"\nSent {msg_id} events in {time.time()-t0:.1f}s")
    finally:
        sock.close()

if __name__ == '__main__':
    main()
