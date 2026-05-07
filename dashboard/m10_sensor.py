#!/usr/bin/env python3
"""M10 Sensor Bridge — reads accelerometer, sends R2-WIRE, shows live display.

Usage: python3 m10_sensor.py <gateway_ip> [gateway_port] [rate_hz]
Example: python3 m10_sensor.py 192.168.44.1 21042 10

Runs on a Unihiker M10. Reads the built-in accelerometer and gyroscope
via the GD32 co-processor (PinPong library), encodes readings as R2-WIRE
frames with CBOR payloads, sends over TCP to the dashboard gateway.
Shows live status on the M10's 2.8" touchscreen.

Deploy: scp to M10, or use m10_install.sh for systemd auto-start.
"""

import socket
import struct
import time
import sys
import os
import threading
import math


def get_device_name():
    """Read hostname from /etc/hostname, fallback to socket.gethostname()."""
    try:
        with open('/etc/hostname') as f:
            name = f.read().strip()
            if name:
                return name
    except Exception:
        pass
    return socket.gethostname()


def cbor_announce_payload(device_name):
    """Encode CBOR map: {"name": device_name} for the sensor announce frame."""
    return bytes([0xA1]) + cbor_text("name") + cbor_text(device_name)

# ── R2 protocol helpers ──

def fnv1a_32(data):
    """FNV-1a 32-bit hash (must match r2-fnv crate)."""
    h = 0x811c9dc5
    for b in data:
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h

# Precomputed event hashes
ACCELERATION = fnv1a_32(b"acceleration")
GYROSCOPE = fnv1a_32(b"gyroscope")
RUN_STATE = fnv1a_32(b"run_state")
CMD_START = fnv1a_32(b"cmd_start")
CMD_STOP = fnv1a_32(b"cmd_stop")
CMD_MARK = fnv1a_32(b"cmd_mark")
CMD_CALIBRATE = fnv1a_32(b"cmd_calibrate")
SENSOR_ANNOUNCE = fnv1a_32(b"r2.sensor.announce")

def cbor_float32(value):
    """Encode a single CBOR float32."""
    return b'\xfa' + struct.pack('>f', value)

def cbor_text(s):
    """Encode a CBOR text string."""
    b = s.encode('utf-8')
    if len(b) < 24:
        return bytes([0x60 | len(b)]) + b
    else:
        return bytes([0x78, len(b)]) + b

def cbor_map(pairs):
    """Encode a CBOR map from (key_str, float_value) pairs."""
    buf = bytearray()
    n = len(pairs)
    if n < 24:
        buf.append(0xA0 | n)
    else:
        buf.extend([0xB8, n])
    for key, value in pairs:
        buf.extend(cbor_text(key))
        buf.extend(cbor_float32(value))
    return bytes(buf)

def build_frame(event_hash, payload, msg_id=0):
    """Build R2-WIRE frame: [len:2][type:1][msg_id:2][hash:4][payload]."""
    body = struct.pack('>BHI', 0x01, msg_id & 0xFFFF, event_hash) + payload
    return struct.pack('>H', len(body)) + body

def parse_frame(buf):
    """
    Extract one complete R2-WIRE frame from buf.

    Returns (event_hash, payload_bytes, consumed_bytes) on success,
    or (None, b'', 0) if buf doesn't yet contain a complete frame.
    Advances past the frame if it is complete.
    """
    if len(buf) < 2:
        return None, b'', 0
    frame_len = struct.unpack('>H', buf[:2])[0]
    total = 2 + frame_len
    if len(buf) < total:
        return None, b'', 0
    if frame_len < 7:
        # Malformed frame — skip it
        return None, b'', total
    frame = buf[2:total]
    event_hash = struct.unpack('>I', frame[3:7])[0]
    payload = frame[7:]
    return event_hash, bytes(payload), total

def parse_cbor_text_map_f32(data):
    """Parse a CBOR map of text-key → float32 pairs."""
    if not data or (data[0] & 0xE0) != 0xA0:
        return {}
    n = data[0] & 0x1F
    result = {}
    i = 1
    try:
        for _ in range(n):
            if (data[i] & 0xE0) != 0x60:
                break
            key_len = data[i] & 0x1F
            i += 1
            key = data[i:i+key_len].decode('utf-8')
            i += key_len
            if data[i] == 0xfa:
                val = struct.unpack('>f', data[i+1:i+5])[0]
                i += 5
            else:
                break
            result[key] = val
    except Exception:
        pass
    return result


# ── Display (runs in separate thread) ──

class SensorDisplay(object):
    """Live display on the Unihiker M10's 2.8" touchscreen (240x320)."""

    def __init__(self):
        self.ax = 0.0
        self.ay = 0.0
        self.az = 0.0
        self.state = 'idle'
        self.connected = False
        self.gateway = ''
        self.device_name = ''
        self.msg_count = 0
        self.rate_hz = 0.0
        self.running = True
        self._thread = None
        # History for mini chart (last 60 samples)
        self.history_z = []
        self.max_history = 60
        self.offset_x = 0.0
        self.offset_y = 0.0
        self.offset_z = 0.0

    def set_calibration_offset(self, ox, oy, oz):
        self.offset_x = ox
        self.offset_y = oy
        self.offset_z = oz

    def start(self):
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def update(self, ax, ay, az, connected, state, msg_count, rate_hz):
        self.ax = ax
        self.ay = ay
        self.az = az
        self.connected = connected
        self.state = state
        self.msg_count = msg_count
        self.rate_hz = rate_hz
        self.history_z.append(az)
        if len(self.history_z) > self.max_history:
            self.history_z.pop(0)

    def stop(self):
        self.running = False

    def _run(self):
        """Tkinter display loop."""
        try:
            import tkinter as tk
        except ImportError:
            print("[display] tkinter not available, running headless")
            return

        root = tk.Tk()
        root.title("R2 Sensor")
        root.geometry("240x320+0+0")
        root.configure(bg='#0a0e17')
        root.overrideredirect(True)  # Fullscreen on Unihiker

        canvas = tk.Canvas(root, width=240, height=320, bg='#0a0e17',
                          highlightthickness=0)
        canvas.pack()

        STATE_COLORS = {
            'idle': '#64748b',
            'calibrating': '#fbbf24',
            'rocking': '#34d399',
        }

        def draw():
            if not self.running:
                root.destroy()
                return

            canvas.delete('all')

            # ── Header ──
            header_text = self.device_name if self.device_name else "R2 SENSOR"
            canvas.create_text(120, 16, text=header_text,
                             fill='#4a9eff', font=('Helvetica', 14, 'bold'))

            # ── Connection status ──
            dot_color = '#44ff88' if self.connected else '#ffaa44'
            canvas.create_oval(16, 36, 26, 46, fill=dot_color, outline='')
            if self.connected:
                status_text = 'Connected  ' + self.gateway
            else:
                status_text = 'Finding ' + self.gateway + '...'
            canvas.create_text(32, 41, text=status_text, anchor='w',
                             fill='#94a3b8', font=('Helvetica', 9))

            # ── State indicator ──
            state_color = STATE_COLORS.get(self.state, '#64748b')
            canvas.create_rectangle(16, 54, 224, 82, fill='#111827', outline=state_color)
            canvas.create_text(120, 68, text=self.state.upper(),
                             fill=state_color, font=('Helvetica', 16, 'bold'))

            # ── Acceleration values ──
            y_start = 96
            labels = [
                ('X', self.ax - self.offset_x, '#ff6384'),
                ('Y', self.ay - self.offset_y, '#36a2eb'),
                ('Z', self.az - self.offset_z, '#4bc0c0'),
            ]
            for i, (label, val, color) in enumerate(labels):
                y = y_start + i * 28
                canvas.create_text(24, y, text=label, anchor='w',
                                 fill=color, font=('Helvetica', 12, 'bold'))
                canvas.create_text(50, y, text='%.3f g' % val, anchor='w',
                                 fill='#e0e0e0', font=('Courier', 12))
                # Bar graph
                bar_w = min(abs(val) * 100, 100)
                bar_x = 150
                canvas.create_rectangle(bar_x, y-6, bar_x + bar_w, y+6,
                                      fill=color, outline='')

            # ── Magnitude ──
            mag = math.sqrt(self.ax**2 + self.ay**2 + self.az**2)
            canvas.create_text(24, y_start + 90, text='|A| = %.3f g' % mag,
                             anchor='w', fill='#94a3b8', font=('Courier', 10))

            # ── Mini Z-axis chart ──
            chart_y = 210
            chart_h = 60
            chart_x = 16
            chart_w = 208

            # Background
            canvas.create_rectangle(chart_x, chart_y, chart_x + chart_w,
                                  chart_y + chart_h, fill='#111827', outline='#1e293b')

            # Grid line at z=1.0
            mid_y = chart_y + chart_h // 2
            canvas.create_line(chart_x, mid_y, chart_x + chart_w, mid_y,
                             fill='#1e293b', dash=(2, 4))

            # Plot Z history
            if len(self.history_z) > 1:
                points = []
                for i, z in enumerate(self.history_z):
                    px = chart_x + (i * chart_w) // self.max_history
                    # Scale: 0.8-1.2g maps to chart height
                    py = chart_y + chart_h - int((z - 0.8) / 0.4 * chart_h)
                    py = max(chart_y, min(chart_y + chart_h, py))
                    points.append(px)
                    points.append(py)
                if len(points) >= 4:
                    canvas.create_line(points, fill='#4bc0c0', width=2)

            canvas.create_text(chart_x + 2, chart_y + 2, text='Z',
                             anchor='nw', fill='#4bc0c0', font=('Helvetica', 8))

            # ── Stats ──
            canvas.create_text(24, 288, text='Events: %d' % self.msg_count,
                             anchor='w', fill='#64748b', font=('Helvetica', 9))
            canvas.create_text(24, 304, text='Rate: %.1f Hz' % self.rate_hz,
                             anchor='w', fill='#64748b', font=('Helvetica', 9))

            # ── TX indicator (blink) ──
            if self.connected and self.msg_count % 2 == 0:
                canvas.create_text(210, 304, text='TX', anchor='w',
                                 fill='#4a9eff', font=('Helvetica', 9, 'bold'))

            root.after(100, draw)  # Redraw at ~10 FPS

        draw()
        try:
            root.mainloop()
        except Exception:
            pass


# ── Main ──

def main():
    if len(sys.argv) < 2:
        print("Usage: python3 m10_sensor.py <gateway_ip> [port] [rate_hz]")
        print("Example: python3 m10_sensor.py 192.168.44.1 21042 10")
        sys.exit(1)

    gateway_host = sys.argv[1]
    gateway_port = int(sys.argv[2]) if len(sys.argv) > 2 else 21042
    rate_hz = float(sys.argv[3]) if len(sys.argv) > 3 else 10.0

    # ── Determine device name ──
    device_name = get_device_name()
    print("Device name: %s" % device_name)

    # ── Start display ──
    display = SensorDisplay()
    display.gateway = '%s:%d' % (gateway_host, gateway_port)
    display.device_name = device_name
    if os.environ.get('DISPLAY') or os.path.exists('/tmp/.X11-unix/X0'):
        display.start()
        print("[display] started")
    else:
        print("[display] no X11 display, running headless")

    # ── Initialise PinPong ──
    print("Initialising PinPong...")
    from pinpong.board import Board
    from pinpong.extension.unihiker import accelerometer, gyroscope
    Board().begin()
    time.sleep(1)
    print("PinPong ready.")

    # ── Connect to gateway (with auto-reconnect) ──
    # Catch all exceptions here — PinPong errors, network errors, anything.
    # The sensor loop must keep running; systemd also restarts on full crash,
    # but we want faster recovery (~5s) without losing the PinPong session.
    while True:
        try:
            _run_sensor_loop(gateway_host, gateway_port, rate_hz,
                           accelerometer, display, device_name)
        except KeyboardInterrupt:
            print("\nShutting down.")
            display.stop()
            break
        except Exception as e:
            # Covers: BrokenPipeError, ConnectionRefusedError, OSError,
            # PinPong RuntimeError/IOError, and anything else.
            print("[sensor] loop exited: %s (%s) — restarting in 5s..." % (
                type(e).__name__, e), flush=True)
            display.connected = False
            time.sleep(5)


def _run_sensor_loop(gateway_host, gateway_port, rate_hz, accelerometer, display, device_name=""):
    """Main sensor loop — reads accel, sends R2-WIRE, handles commands."""
    print("Connecting to gateway at %s:%d..." % (gateway_host, gateway_port))

    # Use a blocking socket for connect; switch to non-blocking after.
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(5.0)
    sock.connect((gateway_host, gateway_port))

    # Switch to non-blocking mode so recv never hangs.
    # IMPORTANT: also switch sends to blocking so sendall doesn't time out.
    # We achieve this by using the raw socket as non-blocking for recv,
    # and wrapping sends in a try. Actually, settimeout(None) would block
    # sends indefinitely which is also bad. Use a dedicated send timeout.
    #
    # Strategy: use select() to poll for incoming data (no recv timeout
    # needed), and keep the socket in blocking mode with a generous timeout
    # so sendall doesn't spuriously fail when the gateway is briefly slow.
    sock.settimeout(2.0)  # Applies to both recv and send; select() avoids blocking on recv.

    print("Connected! Sending at %.1f Hz" % rate_hz)

    # ── Send sensor announce frame ──
    if device_name:
        announce_payload = cbor_announce_payload(device_name)
        announce_frame = build_frame(SENSOR_ANNOUNCE, announce_payload, 0)
        sock.sendall(announce_frame)
        print("Announced as: %s" % device_name)

    # ── Broadcast initial state (always idle on connect/reconnect) ──
    # The browser FSM listens for run_state events to know when a sensor
    # has reset. Without this, a browser that was mid-session before the
    # sensor crashed has no way to know the sensor is now in idle state.
    msg_id = 0
    initial_state_payload = cbor_map([("state", 0.0)])  # 0.0 = idle
    sock.sendall(build_frame(RUN_STATE, initial_state_payload, msg_id))
    msg_id += 1
    print("Initial state broadcast: idle")

    display.connected = True
    state = 'idle'
    t0 = time.time()
    sample_times = []
    calib_samples = []   # raw readings during calibration phase
    recent_samples = []  # rolling buffer of last 60 readings (always populated)
    RECENT_MAX = 60

    # ── Incoming frame reassembly buffer ──
    # TCP is a byte stream; command frames may arrive split across multiple
    # recv() calls or batched together. We accumulate into cmd_buf and parse
    # complete frames out of it — same approach as the Rust gateway.
    cmd_buf = bytearray()

    import select as _select

    def drain_commands():
        """Read all pending command frames from the socket (non-blocking)."""
        nonlocal state, cmd_buf
        commands = []
        while True:
            # Check if data is available without blocking
            r, _, _ = _select.select([sock], [], [], 0)
            if not r:
                break
            try:
                chunk = sock.recv(512)
                if not chunk:
                    # Connection closed by gateway
                    raise ConnectionResetError("gateway closed connection")
                cmd_buf.extend(chunk)
            except BlockingIOError:
                break
            except socket.timeout:
                break
            # Parse all complete frames out of cmd_buf
            while True:
                ev_hash, payload, consumed = parse_frame(cmd_buf)
                if consumed == 0:
                    break  # Need more data
                cmd_buf = cmd_buf[consumed:]
                if ev_hash is not None:
                    commands.append((ev_hash, payload))
        return commands

    def send_state():
        nonlocal msg_id
        state_val = {'idle': 0, 'calibrating': 1, 'rocking': 2}.get(state, 0)
        payload = cbor_map([("state", float(state_val))])
        sock.sendall(build_frame(RUN_STATE, payload, msg_id))
        msg_id += 1

    try:
        while True:
            loop_start = time.time()

            # ── Drain all pending commands from gateway ──
            for cmd_hash, payload in drain_commands():
                if cmd_hash == CMD_START:
                    state = 'calibrating'
                    calib_samples.clear()
                    display.set_calibration_offset(0.0, 0.0, 0.0)
                    print("[%.1fs] -> CALIBRATING" % (time.time() - t0), flush=True)
                    send_state()
                elif cmd_hash == CMD_MARK:
                    # Compute offset — prefer calib_samples, fall back to recent_samples
                    pool = calib_samples if calib_samples else recent_samples
                    if pool:
                        n = len(pool)
                        ox = sum(s[0] for s in pool) / n
                        oy = sum(s[1] for s in pool) / n
                        oz = sum(s[2] for s in pool) / n
                        display.set_calibration_offset(ox, oy, oz)
                        print("[calibrate] offset x=%.3f y=%.3f z=%.3f (%d samples, state=%s)" % (
                            ox, oy, oz, n, state), flush=True)
                    else:
                        print("[calibrate] no samples available!", flush=True)
                    calib_samples.clear()
                    state = 'rocking'
                    print("[%.1fs] -> ROCKING" % (time.time() - t0), flush=True)
                    send_state()
                elif cmd_hash == CMD_STOP and state != 'idle':
                    state = 'idle'
                    print("[%.1fs] -> IDLE" % (time.time() - t0), flush=True)
                    send_state()

            # ── Read accelerometer (with per-read error recovery) ──
            try:
                ax = accelerometer.get_x()
                ay = accelerometer.get_y()
                az = accelerometer.get_z()
            except Exception as e:
                # PinPong/GD32 read errors are transient — log and skip this sample.
                # The outer loop will catch repeated failures and reconnect.
                print("[accel] read error: %s — skipping sample" % e, flush=True)
                # Brief pause to let the GD32 co-processor recover
                time.sleep(0.1)
                continue

            # Rolling buffer — always keep last RECENT_MAX samples
            recent_samples.append((ax, ay, az))
            if len(recent_samples) > RECENT_MAX:
                recent_samples.pop(0)

            # Collect samples specifically during calibration phase
            if state == 'calibrating':
                calib_samples.append((ax, ay, az))

            # ── Send acceleration event (only when active) ──
            if state != 'idle':
                payload = cbor_map([("x", ax), ("y", ay), ("z", az)])
                frame = build_frame(ACCELERATION, payload, msg_id)
                sock.sendall(frame)
                msg_id += 1

            # ── Calculate rate ──
            now = time.time()
            sample_times.append(now)
            if len(sample_times) > 50:
                sample_times.pop(0)
            if len(sample_times) > 1:
                dt = (sample_times[-1] - sample_times[0]) / (len(sample_times) - 1)
                actual_rate = 1.0 / dt if dt > 0 else 0
            else:
                actual_rate = 0

            # ── Update display ──
            display.update(ax, ay, az, True, state, msg_id, actual_rate)

            if msg_id % 50 == 0:
                elapsed = time.time() - t0
                print("  [%d] x=%.3f y=%.3f z=%.3f (%.1f Hz)" % (
                    msg_id, ax, ay, az, actual_rate), flush=True)

            # ── Rate limit ──
            elapsed = time.time() - loop_start
            sleep_time = (1.0 / rate_hz) - elapsed
            if sleep_time > 0:
                time.sleep(sleep_time)

    finally:
        sock.close()
        display.connected = False


if __name__ == '__main__':
    main()
