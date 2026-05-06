# SPEC-R2-ROCKER-SENSOR: Sensor Firmware Behaviour

**Version:** 0.1 Draft
**Date:** 2026-05-07
**Status:** Normative Draft
**Depends on:** SPEC-R2-ROCKER-WIRE, R2-WIRE, R2-TRUST, R2-BLE, R2-BOOTSTRAP, R2-WIFI

---

## 1. Introduction

This specification defines the **runtime behaviour of an r2-rocker
sensor node**: boot and self-test, the state machine, sample
acquisition, SD-card store-and-forward semantics, network behaviour, OTA
update flow, NVS configuration, and error handling.

It builds on `SPEC-R2-ROCKER-WIRE` (which defines what is sent on the
wire) and is implemented against the reference hardware described in
`HARDWARE-WIRING.md`.

### 1.1 Scope

In scope:

* Boot sequence and self-test.
* The sensor state machine and LED indication.
* Sample acquisition pipeline (ADXL355 → SD → network).
* Store-and-forward semantics, including reconnect resume.
* Battery monitoring and low-power behaviour.
* Calibration request handling.
* Time-synchronisation reply behaviour.
* Persistent configuration in NVS.
* OTA update flow.
* Error codes and recovery.

Out of scope:

* Wire protocol (`SPEC-R2-ROCKER-WIRE`).
* Dashboard analytics, calibration math (`SPEC-R2-ROCKER-DASHBOARD`).
* Hardware reference design (`HARDWARE-WIRING.md`).

### 1.2 Terminology

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHOULD**,
**MAY** in this document are to be interpreted as in RFC 2119.

* **Sensor** — the assembled hardware unit (ESP32-S3 + ADXL355 + SD +
  battery) running the r2-rocker firmware.
* **Firmware** — the Rust binary executing on the ESP32-S3.
* **Sample** — one accelerometer triplet `(x, y, z)` from the ADXL355.
* **SD ring** — the on-card durable log of samples; conceptually a ring
  buffer though physically a set of segment files (§6).
* **Backlog** — `tail_seq − last_acked_seq`: the count of samples in the
  ring not yet acknowledged by the dashboard.
* **TG public key** — the trust-group public key, baked into the firmware
  at compile time (§3).

### 1.3 Notation

Multi-byte integers are stored on SD in little-endian (matches ESP32-S3
native), serialised on the wire in big-endian (per R2-WIRE §1.3). NVS
uses esp-idf's native key/value typing.

---

## 2. Boot and self-test

### 2.1 Boot sequence

On reset the firmware shall execute the following sequence in order. A
failure at any step shall transition to the `ERROR` state (§4.1) with
the corresponding error code (§13.1):

1. ESP-IDF bootloader (unchanged from default).
2. App entry: initialise logger, NVS namespace `"r2-rocker"`.
3. Initialise GPIO (LED pin per `HARDWARE-WIRING.md` §5).
4. Briefly drive LED **white** (boot indicator) for 100 ms.
5. Initialise SPI2 driver (FSPI defaults; see `HARDWARE-WIRING.md` §2.1).
6. Initialise ADXL355 over SPI2; verify WHO_AM_I (`DEVID_AD = 0xAD`,
   `DEVID_MST = 0x1D`, `PARTID = 0xED`). Mismatch → `SPI_FAULT`.
7. Mount SD card via FATFS over SPI2 (CS = GPIO9). Failure →
   `SD_MOUNT_FAIL`.
8. Initialise ADC1 oneshot driver, configure CH3 (GPIO4) at 12 dB
   attenuation, 12-bit width.
9. Load device key from NVS; if absent, generate a fresh Ed25519 keypair
   via `esp_fill_random()` and persist (§3.1). NVS errors → `NVS_FAIL`.
10. Load TG public key from compile-time constant (`include_bytes!`
    from `trust_keys/tg_pub.bin`). If the embedded bytes are
    syntactically invalid, the firmware shall refuse to boot — this is
    a build-time bug, not a runtime condition.
11. Read `last_acked_seq` from NVS (default 0 if absent).
12. Scan SD ring tail to determine `tail_seq` (§6.5); set the in-RAM
    `seq` counter to `tail_seq + 1`.
13. Initialise BLE stack (NimBLE) and WiFi STA (esp-idf-svc).
14. Decide initial state per §4.2 transition rules and enter that state.

### 2.2 Self-test acceptance

A device SHALL be considered "ready" only if steps 1–14 complete without
error. The self-test result MAY be reported via UART logging during
development; in production, the LED state (any colour other than steady
red) is the indicator that self-test passed.

### 2.3 Watchdog

The hardware watchdog SHALL be enabled with a 30-second timeout. The
main loop shall reset the watchdog at least once per second. A watchdog
expiry triggers a CPU reset; on the subsequent boot the bootloader's
OTA rollback path (§12.7) applies if the previous boot was a recently
flashed image.

---

## 3. Identity and trust

### 3.1 Device key generation

On first boot (no `device_priv` in NVS), the firmware shall generate a
fresh Ed25519 keypair using `esp_fill_random()` for the seed and persist
it to NVS in the `r2-rocker` namespace under keys `device_priv`
(blob, 64 bytes — 32-byte seed + 32-byte public, per ed25519-dalek's
`SigningKey::to_keypair_bytes()` layout) and `device_pub` (blob, 32
bytes).

NVS encryption SHALL be enabled (`CONFIG_NVS_ENCRYPTION=y` in
sdkconfig.defaults) so that physical removal of the flash does not
expose the device's private key.

The device key persists across reboots and across firmware updates. A
factory reset (§12.x via `r2.dash.reset {factory: true}`) erases it,
forcing a fresh identity on next boot.

### 3.2 TG public key

The trust-group public key (32 bytes, Ed25519) shall be embedded in the
firmware via:

```rust
const TG_PUB_KEY: [u8; 32] = *include_bytes!("../trust_keys/tg_pub.bin");
```

(Path relative to the firmware crate root.) The TG cert (if used) is
embedded similarly. Build fails if `trust_keys/tg_pub.bin` is missing.

### 3.3 Announce signature

On TCP connect to the dashboard, the firmware shall transmit
`r2.sensor.announce` (per WIRE §3.1) with `sig` computed as:

```
canonical = canonical_cbor_encode({
    0: device_pk,
    1: hostname,
    2: fw_ver,
    3: last_seq,
    4: boot_ts_ms,
    5: nonce
})
sig = ed25519_sign(device_priv, canonical)
```

The dashboard SHALL verify this signature against `TG_PUB_KEY` (i.e. the
device key is *not* directly checked; rather, the device proves
possession of a TG-signed device cert — see R2-TRUST). For the v0.1
deployment with hardwired TG, the device key itself doubles as a TG
member key; dashboard-side verification reduces to "is this signature
well-formed and is `device_pk` in the dashboard's accepted-peers list."
The dashboard's accept-list is initialised empty; new peers are
admitted on first contact and persisted (TOFU — trust on first use). A
later spec version SHALL formalise device-cert issuance.

---

## 4. State machine

### 4.1 States

| State | LED indication | Description |
|---|---|---|
| `IDLE` | white briefly, then dark | Boot complete; no networking active |
| `ADVERTISING` | blue, slow pulse (1 Hz) | BLE beacon active, awaiting bootstrap |
| `BLE_CONNECTED` | cyan, fast pulse | L2CAP up, awaiting `#wifi_offer` |
| `WIFI_CONNECTING` | cyan→yellow flicker | Joining hotspot, DHCP, TCP handshake |
| `STREAMING_LIVE` | green, heartbeat (60 bpm) | TCP up, sample-to-frame latency ≤ 2 periods |
| `STREAMING_CATCHUP` | yellow, heartbeat | TCP up, draining backlog ≥ 200 samples |
| `CALIBRATING` | purple, solid | Averaging samples for a `cal.sample.req` |
| `LOW_BATTERY` | orange, slow pulse (overlay) | Cell ≤ 3.3 V; overrides other state colour |
| `OTA` | white, fast strobe | Firmware update in progress |
| `ERROR` | red, fast pulse | Fatal init or runtime fault; manual reset required |

The LED indication conforms to `HARDWARE-WIRING.md` §5 mapping. States
that overlay (only `LOW_BATTERY`) take precedence over the colour of the
underlying state but do not change the underlying state.

### 4.2 Transitions

```
                  POWER ON
                     │
                     ▼
                  ┌───────┐
                  │ IDLE  │
                  └───┬───┘
       ┌─────────────┴─────────────┐
       │ last_acked > 0 &&         │ otherwise
       │ gateway reachable in 3 s  │
       ▼                           ▼
  ┌─────────────┐              ┌──────────────┐
  │ STREAMING_  │              │ ADVERTISING  │
  │ LIVE        │              └──────┬───────┘
  └─────────────┘                     │ L2CAP connect
       ▲                              ▼
       │                       ┌──────────────┐
       │                       │ BLE_CONNECTED│
       │                       └──────┬───────┘
       │                              │ valid #wifi_offer
       │                              ▼
       │                       ┌──────────────────┐
       │       TCP up + announce│ WIFI_CONNECTING │
       └──────────────────────── └────────┬───────┘
                                          │ timeout / fail
                                          ▼
                                   (back to ADVERTISING)
```

Additional transitions:

* `STREAMING_LIVE` ↔ `STREAMING_CATCHUP` based on backlog (§7.3).
* `STREAMING_LIVE` → `CALIBRATING` on `r2.dash.cal.sample.req`; back
  after the response is sent.
* Any state → `LOW_BATTERY` (overlay) when battery voltage ≤ 3.3 V; the
  underlying state continues to operate. Cleared when voltage > 3.4 V
  (hysteresis).
* Any state → `OTA` on `r2.dash.fw.update`; transitions to a fresh boot
  on success or back to the prior state on rollback.
* `STREAMING_LIVE` → `ADVERTISING` when the network task observes
  3 consecutive TCP-write failures or 5 s of no `r2.dash.ack` reception
  during keep-alive (KeepAlive condition; analogous to the M10 demo
  behaviour — see `r2-core/demos/rocker-rig/README.md`).
* Any state at battery ≤ 3.0 V → safe shutdown (§8.4): persist
  `last_acked_seq`, flush SD, deep-sleep with a wake-on-charger condition.

---

## 5. Sample acquisition

### 5.1 ADXL355 driver

The firmware shall provide a driver module exposing:

| Function | Description |
|---|---|
| `init(spi, range, odr) -> Result<Driver>` | Soft reset, set `RANGE` (0x2C), set `FILTER` (0x28) ODR bits, clear `POWER_CTL.STANDBY` (0x2D bit 0). |
| `who_am_i() -> Result<(u8, u8, u8)>` | Read `DEVID_AD` (0x00), `DEVID_MST` (0x01), `PARTID` (0x02). Expected `(0xAD, 0x1D, 0xED)`. |
| `read_xyz() -> Result<(i32, i32, i32)>` | Burst-read 9 bytes from `XDATA3` (0x08); decode three 20-bit signed values, sign-extend to `i32`. |
| `set_range(r)`, `set_odr(o)` | Runtime reconfiguration. |

SPI command framing (per ADXL355 datasheet): byte 1 = `(addr << 1) | RW`,
where `RW = 1` for read and `0` for write; bytes 2..N are data. SCLK
polarity / phase: SPI mode 0 (CPOL = 0, CPHA = 0). Maximum SCLK 10 MHz.

20-bit sample reconstruction:

```
raw_unsigned = (xdata3 << 12) | (xdata2 << 4) | (xdata1 >> 4)
raw_signed   = sign_extend_20bit(raw_unsigned)
```

### 5.2 Sample loop

The firmware shall sample at the configured `rate_hz` (default 100 Hz,
NVS-tunable). Two acquisition modes are permitted:

* **DRDY-triggered** (preferred at high rates): GPIO14 ISR triggers a
  task that performs the burst read.
* **Polled at fixed period** (acceptable for ≤ 200 Hz): a periodic
  FreeRTOS timer fires at `1/rate_hz`.

The implementation shall measure jitter and reject samples whose
inter-sample interval exceeds `2 / rate_hz` (i.e. dropped samples are
detected and counted; `r2.sensor.event.log {code: SAMPLE_DROP, …}` shall
be emitted on every 100 dropped samples).

### 5.3 Sequence number

Per WIRE §5.1, `seq` is a per-device monotonic 32-bit counter that
persists across reboots:

* On boot, `seq` is initialised to `tail_seq + 1` where `tail_seq` is
  the highest `seq` found in the SD ring (§6.5).
* The sample task increments `seq` by 1 per sample written to SD —
  *not* per sample sent on the wire.
* On wrap (every ~1.4 years at 100 Hz), the firmware shall emit
  `r2.sensor.event.log {code: SEQ_WRAP, …}` 24 hours before reaching
  `0xFFFFFFF0` and continue counting through 0.

### 5.4 Timestamp

`ts_ms` is a 32-bit monotonic uptime counter in milliseconds, captured
at sample-read time using `esp_timer_get_time() / 1000`. Wraps every
~49 days; the dashboard's per-device offset (WIRE §7) accommodates wraps
implicitly because the wrap manifests as a one-off backwards jump that
exceeds normal smoothing.

---

## 6. SD store-and-forward

### 6.1 Filesystem layout

The SD card shall be formatted FAT32 (or FATFS-compatible exFAT for
cards > 32 GB) with allocation-unit size 32 kB.

The firmware writes to a single directory `/r2/`:

```
/r2/
├─ log.0001.bin     ← sample segment 1 (oldest)
├─ log.0002.bin     ← sample segment 2
├─ log.0003.bin     ← sample segment 3 (current write target)
├─ meta.bin         ← head_seq, tail_seq, last_acked_seq snapshot
└─ fw.bak/          ← OTA rollback image (optional, §12.8)
```

Segments are named `log.NNNN.bin` with a 4-digit zero-padded counter,
incrementing forever (no reuse).

### 6.2 Record format

Each sample is appended as a fixed-size 20-byte record (per WIRE D-12):

```
offset 0..3   seq    u32 little-endian
offset 4..7   ts_ms  u32 little-endian
offset 8..11  x      i32 little-endian
offset 12..15 y      i32 little-endian
offset 16..19 z      i32 little-endian
```

No record framing or padding; segment files are pure concatenations of
20-byte records.

### 6.3 Segment rotation

A new segment shall be opened when the current segment reaches
`segment_size_bytes` (default 8 MiB = 419,430 samples ≈ 70 minutes at
100 Hz). The default is configurable via NVS key `segment_size_mb`
(u8, default 8).

The firmware shall retain at most `ring_segments` segments (default 12,
NVS-tunable). When opening a new segment causes the count to exceed
`ring_segments`, the **oldest** segment is deleted (overwrite-oldest,
per PLAN D-15). Default ring size: 12 × 8 MiB = 96 MiB ≈ 14 hours at
100 Hz.

### 6.4 ACK persistence

`last_acked_seq` is updated on every received `r2.dash.ack` but
persisted to NVS at most **once per second** (rate-limited). This bounds
NVS write wear at ≤ 86,400 writes/day. On any clean shutdown, the
firmware shall flush `last_acked_seq` to NVS.

A snapshot of `(head_seq, tail_seq, last_acked_seq)` is also written to
`/r2/meta.bin` once per minute as a fallback if NVS is corrupted.

### 6.5 Boot recovery

On boot, the firmware shall:

1. Enumerate `/r2/log.*.bin` segments, sort by segment number.
2. Open the highest-numbered segment, seek to end, divide by 20 to
   determine the sample count, read the last record to obtain `tail_seq`
   (the highest `seq` written).
3. Set the in-RAM `seq` counter to `tail_seq + 1`.
4. Read `last_acked_seq` from NVS; if absent, fall back to
   `/r2/meta.bin`; if both absent, treat as 0 (full retransmission of
   the ring on next connect).

### 6.6 Reconnect replay

When the network task (re)connects to the dashboard, it shall resume
sending from `last_acked_seq + 1`. The dashboard's `r2.dash.ack` after
`r2.sensor.announce` (WIRE §3.1) MAY override this with a different
`through_seq` — the firmware shall accept this and adjust its ring
freeing accordingly.

### 6.7 SD failure handling

If a write to SD fails (write error, card removed), the firmware shall:

1. Emit `r2.sensor.event.log {level: ERROR, code: SD_WRITE_FAIL, …}` if
   network is up.
2. Continue sampling into a small in-RAM bounded queue (default 1024
   samples ≈ 10 s at 100 Hz) and retry SD writes every 100 ms.
3. If SD recovery does not occur within 30 s, transition to `ERROR`
   state — durability is the primary contract; running without
   durability is not acceptable.

---

## 7. Network task

### 7.1 Connection management

The network task is started after `WIFI_CONNECTING` succeeds (DHCP
complete). It performs:

1. TCP connect to the gateway IP (received in `#wifi_offer`) on port
   21042. Connection failure: retry with exponential backoff (1 s, 2 s,
   4 s, 8 s, 16 s, capped at 30 s).
2. Send `r2.sensor.announce` (WIRE §3.1).
3. Await `r2.dash.ack` from the dashboard within 5 s — failure to
   receive transitions back to `ADVERTISING`.
4. Begin draining the SD ring from `last_acked_seq + 1`.

### 7.2 Live mode

In `STREAMING_LIVE`, each sample written to SD by the sample task is
also (effectively concurrently) emitted as a single
`r2.sensor.acceleration` frame (WIRE §3.2). The implementation MAY
batch up to 4 samples in a single TCP write call to reduce syscall
overhead, provided each sample remains in its own R2-WIRE frame.

### 7.3 Catch-up mode

The network task shall switch to `STREAMING_CATCHUP` when:

```
backlog = tail_seq − last_acked_seq ≥ 200
```

In catch-up mode, it shall emit `r2.sensor.acceleration.batch` (WIRE
§3.3) with up to 50 samples per frame.

The network task shall return to `STREAMING_LIVE` when:

```
backlog ≤ 50
```

The hysteresis (200 enter, 50 exit) prevents thrashing.

### 7.4 ACK reception

The network task shall continuously read incoming dashboard frames in a
non-blocking manner. On `r2.dash.ack`:

* Update `last_acked_seq` in RAM.
* Schedule a rate-limited NVS write (§6.4).
* Free SD segments where every record's `seq ≤ last_acked_seq` —
  release-by-deletion, atomic per segment.

On `r2.dash.cal.sample.req`, `r2.dash.stream.start`, `r2.dash.stream.stop`,
`r2.dash.sync_pulse`, `r2.dash.config.set`, `r2.dash.fw.update`,
`r2.dash.reset`: dispatch to the appropriate handler (§9, §11, §10, §12).

### 7.5 KeepAlive

If no `r2.dash.ack` is received within 5 s while streaming, the network
task shall send a `r2.sensor.status` frame as a keep-alive probe. If
no traffic returns within a further 5 s, the task closes the TCP
session and transitions to `ADVERTISING` (the M10 demo's KeepAlive
pattern).

---

## 8. Battery monitoring

### 8.1 Sampling

ADC1_CH3 (GPIO4) shall be sampled at 12-bit resolution with 12 dB
attenuation (full-scale ≈ 3.1 V). Each battery reading shall be the
median of 16 successive samples to reject ADC noise.

ADC calibration via esp-idf's two-point calibration scheme is REQUIRED
to remove the ADC's manufacturing offset (`esp_adc_cal_characterize` or
`adc_cali_create_scheme_*`).

### 8.2 Voltage reconstruction

Cell voltage in millivolts:

```
v_cell_mv = adc_calibrated_mv × 2     # divider ratio = 2 (100k / 100k)
```

### 8.3 State of charge

Percentage shall be computed via piecewise-linear interpolation of:

| Cell mV | Percent |
|---|---|
| 4200 | 100 |
| 4100 | 90 |
| 4000 | 80 |
| 3900 | 65 |
| 3800 | 50 |
| 3700 | 35 |
| 3600 | 20 |
| 3500 | 10 |
| 3400 | 5 |
| 3300 | 0 |

This curve is approximate; refine empirically once the chosen LiPo cell
is in hand. Implementations SHOULD treat the curve as a config-time
constant, not hard-coded.

### 8.4 Low-battery behaviour

| Cell mV | Action |
|---|---|
| ≤ 3300 | Enter `LOW_BATTERY` overlay (LED orange). Continue streaming. Emit `r2.sensor.battery` immediately, then every 10 s. |
| ≤ 3100 | Reduce sample rate to 10 Hz to extend runtime. |
| ≤ 3000 | Safe shutdown: flush `last_acked_seq` and `meta.bin`, send a final `r2.sensor.event.log {code: BATTERY_CRITICAL}` if network up, halt the CPU. The operator unplugs the depleted cell and connects a fresh charged cell; this triggers a cold boot per §2.1. There is no on-board charging — the sensor does not "wake from sleep on charger connect." |
| ≥ 3400 | Clear `LOW_BATTERY` overlay (hysteresis). |

### 8.5 Reporting cadence

Per WIRE §3.4: every 30 s in `STREAMING_*` states, every 5 minutes
otherwise. Plus immediate transmission on entering `LOW_BATTERY`.

---

## 9. Calibration handling

On `r2.dash.cal.sample.req` (WIRE §4.2):

1. The firmware enters `CALIBRATING` state if currently in
   `STREAMING_LIVE`. If in any other state, it replies with
   `r2.sensor.status {error_code: CAL_INVALID_STATE}` and remains
   unchanged.
2. The firmware continues sampling at the configured rate; in parallel
   it accumulates `(x, y, z)` triplets into running sums for `req.ms`
   milliseconds.
3. Streaming MAY pause during the averaging window if SD throughput is
   limited; the implementation SHOULD prefer durability over streaming
   here (the dashboard tolerates a brief gap, marked via `seq`).
4. After the window closes, compute arithmetic means
   `(gx, gy, gz)` per axis.
5. Emit `r2.sensor.cal.sample.resp` (WIRE §3.6) with the means and the
   actual `n_samples` counted.
6. Transition back to `STREAMING_LIVE`.

The firmware does not store the calibration result; the dashboard owns
the calibration matrix per PLAN D-16.

---

## 10. Time synchronisation

On `r2.dash.sync_pulse` (WIRE §4.5), the firmware shall:

1. Capture `sensor_ts_ms = esp_timer_get_time() / 1000` **immediately**
   on frame receipt (before any other processing).
2. Reply with `r2.sensor.sync_pong {req_id, sensor_ts_ms}` (WIRE §3.7).

The sensor's clock is never adjusted. The dashboard maintains the
per-device offset and applies it on its side (WIRE §7).

---

## 11. NVS configuration

### 11.1 Persistent items

Namespace: `"r2-rocker"`. All keys are ASCII; encryption per §3.1.

| Key | Type | Default | Description |
|---|---|---|---|
| `device_priv` | blob(64) | (gen first boot) | Ed25519 keypair bytes |
| `device_pub` | blob(32) | (derived) | Ed25519 public key |
| `hostname` | string | `"rocker-{6-hex of device_pk[..6]}"` | Friendly device name |
| `default_rate_hz` | u16 | 100 | Sample rate when streaming starts |
| `default_range` | u8 | 0 | 0=±2 g, 1=±4 g, 2=±8 g |
| `mounting_role` | u8 | 1 | 1=rocker, 2=bed, 3=other |
| `last_acked_seq` | u32 | 0 | ACK pointer (rate-limited writes) |
| `segment_size_mb` | u8 | 8 | SD ring segment size |
| `ring_segments` | u8 | 12 | Number of segments retained |
| `boot_count` | u32 | 0 | Incremented every boot — diagnostic |

### 11.2 Updates via `r2.dash.config.set`

On receipt of `r2.dash.config.set` (WIRE §4.6), the firmware shall
update each present field in NVS and apply the change:

* `default_rate_hz`, `default_range` — apply on next `stream.start`.
* `hostname`, `mounting_role` — apply immediately; the change is
  visible to the dashboard on the next frame's metadata or status.

The firmware shall reply with a `r2.sensor.status` confirming the new
values.

---

## 12. OTA

### 12.1 Trigger

On `r2.dash.fw.update` (WIRE §4.7), the firmware shall transition to
`OTA` state and:

1. Fetch the binary from the URL via TCP. Bytes-as-they-arrive are fed
   into `esp_ota_write` against the `OTA_NEXT` partition.
2. Compute SHA-256 streamingly during the fetch.
3. On EOF, compare the computed SHA-256 against `req.sha256`; mismatch
   → abort, free the partition, return to prior state, emit
   `r2.sensor.event.log {code: OTA_VERIFY_FAIL}`.
4. If `req.tg_sig` is present, verify it (Ed25519 over `(url || sha256)`)
   against `TG_PUB_KEY`; failure → abort as above. In v0.1, absence
   of `tg_sig` SHALL emit a warning log but is not fatal.
5. Mark the new partition as boot via `esp_ota_set_boot_partition`.
6. Reboot.

### 12.2 First-boot rollback

On the first boot of a freshly flashed image, the firmware shall
remain in a "tentative" state (`esp_ota_mark_app_valid_cancel_rollback`
deferred) until:

* Self-test (§2) passes.
* TCP connection to the dashboard succeeds.
* At least one `r2.dash.ack` is received.

Only then does the firmware mark the new partition as valid. If any
of these conditions fails within 60 s of first boot, the bootloader's
rollback path returns to the previous partition.

### 12.3 SD-backed backup (informative, v1.0)

A future version SHOULD copy the running firmware image to `/r2/fw.bak/`
during the OTA flow and provide a manual rollback path via
`r2.dash.reset` with a flag, for cases where automatic rollback fails.

---

## 13. Errors

### 13.1 Codes

The firmware emits `r2.sensor.event.log` with one of the codes below;
codes ≥ 0xF0 trigger `ERROR` state.

| Code | Name | Severity | Recovery |
|---|---|---|---|
| 0x00 | NONE | — | — |
| 0x10 | SAMPLE_DROP | warn | Continue; logged once per 100 drops |
| 0x11 | DRDY_TIMEOUT | warn | Re-init ADXL355; retry 3× then 0xF1 |
| 0x20 | SD_WRITE_FAIL | error | RAM buffer + retry; 0xF2 if 30 s no recovery |
| 0x21 | SD_RING_DELETE_FAIL | warn | Continue; ring may grow until next success |
| 0x30 | NVS_WRITE_FAIL | error | Continue (cached in RAM); 0xF3 on read fault |
| 0x40 | TG_SIG_FAIL | warn | Dashboard rejected announce; continue advertising |
| 0x50 | OTA_FETCH_FAIL | warn | Return to prior state |
| 0x51 | OTA_VERIFY_FAIL | warn | Return to prior state |
| 0x60 | BATTERY_LOW | warn | LOW_BATTERY overlay |
| 0x61 | BATTERY_CRITICAL | error | Safe shutdown |
| 0x70 | SEQ_WRAP_IMMINENT | info | None — informational, 24 h pre-wrap |
| 0xF1 | SPI_FAULT | fatal | ERROR state |
| 0xF2 | SD_FATAL | fatal | ERROR state |
| 0xF3 | NVS_FATAL | fatal | ERROR state |
| 0xFF | UNKNOWN | fatal | ERROR state |

### 13.2 ERROR state

In `ERROR` state, the firmware shall:

* Set LED to red, fast pulse.
* Stop sampling, stop streaming, stop networking.
* Continue UART logging.
* Wait for manual reset; the watchdog will not save us here, since the
  main loop is intentionally idle.

A future version MAY support remote `r2.dash.reset` from `ERROR` state
to allow remote recovery without site visit; v0.1 requires physical
power-cycle.

---

## 14. Conformance

A firmware build conforms to this specification when the following
acceptance tests pass on the reference hardware:

### 14.1 Self-test acceptance

1. Cold boot completes within 5 s.
2. WHO_AM_I read returns `(0xAD, 0x1D, 0xED)`.
3. SD card mount succeeds with a known-good FAT32 card.
4. NVS namespace `r2-rocker` is readable and writable.
5. Device generates a valid Ed25519 keypair on first boot and reuses it
   on subsequent boots.
6. LED transitions from white (boot) to the appropriate steady-state
   colour within 1 s of boot completion.

### 14.2 Sample-loop acceptance

1. With ADXL355 stationary and level on bench, mean `(x, y, z)` over
   5 s reads `(0, 0, ≈ 256000)` LSB ± 10% at ±2 g range (gravity = 1 g
   on z-axis).
2. Sample-rate jitter (max inter-sample interval / nominal) ≤ 1.5×
   under unloaded conditions.
3. `seq` increments by 1 per SD record.
4. After power cycle, the new `seq` equals `tail_seq + 1` of the
   pre-shutdown ring.

### 14.3 Network acceptance

1. Connect to a dummy dashboard simulator on port 21042; complete
   announce + first ACK within 1 s of WiFi up.
2. Live mode: emitted-frame `seq` matches written-record `seq` for
   contiguous samples; latency ≤ 2 sample periods.
3. Catch-up mode: with a 1000-sample backlog injected, drain to live
   mode within 10 s on a 5 Mbit/s link.
4. ACK reception frees SD segments idempotently — no
   `SD_RING_DELETE_FAIL` errors over 1 hour of continuous operation.

### 14.4 Calibration acceptance

1. On `cal.sample.req {position: A, ms: 1000}`, response received
   within 1.2 s with `n_samples` ≥ 90 at 100 Hz.
2. Mean `(gx, gy, gz)` reproducible within 1% across 5 successive
   requests with the device static.

### 14.5 Battery acceptance

1. Reported `voltage_mv` is within ±50 mV of voltmeter-measured cell
   voltage across the 3.0–4.2 V range.
2. `LOW_BATTERY` overlay engages within 1 s of cell ≤ 3.3 V.
3. Safe shutdown sequence completes (NVS flushed, deep sleep entered)
   within 3 s of cell ≤ 3.0 V.

### 14.6 OTA acceptance

1. Successfully OTA-update from build N to build N+1 with no data loss
   in `last_acked_seq` or SD ring.
2. Deliberate corruption of the binary causes verification failure
   without overwriting the running partition.
3. First-boot rollback returns to build N if build N+1 fails to connect
   to the dashboard within 60 s.

---

## 15. Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-07 | 0.1 | Initial draft. Boot, FSM, sample pipeline, SD ring, network, battery, calibration, OTA, conformance. |
| 2026-05-07 | 0.1.1 | §8.4 corrected: no on-board charging — depleted cell is unplugged and replaced with a charged one; this is a cold boot, not a deep-sleep wake. |
