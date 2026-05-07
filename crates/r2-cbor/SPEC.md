# R2-CBOR: Constrained CBOR Encoding for Reality2

**Version:** 0.1.0
**Status:** Stable — encoding profile frozen

---

## 1. Purpose

R2-CBOR defines a constrained subset of CBOR (RFC 8949) for encoding event payloads in R2-WIRE messages. The constraints enable zero-allocation, single-pass encode/decode on MCUs with 2–8 KB SRAM.

## 2. Encoding Modes

| Mode | Transport | Max Payload | Key Type |
|------|-----------|-------------|----------|
| **Compact** | BLE, LoRa | 180 bytes | Unsigned integer only |
| **Standard** | TCP, WebSocket | 65535 bytes | Integer or text string |

On LoRa, the effective maximum is further constrained by the spreading factor (e.g., ~39 bytes at SF12/BW125).

## 3. Compact Mode Rules

1. **Integer keys only.** Map keys MUST be unsigned integers (major type 0). String keys MUST NOT be used.
2. **Definite length only.** All maps, arrays, text strings, and byte strings MUST use definite-length encoding.
3. **No tags.** CBOR tags (major type 6) MUST NOT be used.
4. **No undefined.** The `undefined` simple value (0xF7) MUST NOT be used.
5. **Supported types:** unsigned int, negative int, bool, null, float16, float32, float64, text string, byte string, array, map.
6. **Maximum payload: 180 bytes** after the R2-WIRE header.

## 4. Integer Encoding

Per RFC 8949 — preferred serialisation (smallest encoding):

| Value Range | Encoding | Bytes |
|-------------|----------|-------|
| 0–23 | Single byte: major type + value | 1 |
| 24–255 | Major type + AI 24 + 1 byte | 2 |
| 256–65535 | Major type + AI 25 + 2 bytes (big-endian) | 3 |
| 65536–4294967295 | Major type + AI 26 + 4 bytes (big-endian) | 5 |
| >4294967295 | Major type + AI 27 + 8 bytes (big-endian) | 9 |

Negative integers: encoded as major type 1 with value `(-1 - n)`.

## 5. Float Encoding

| Type | CBOR AI | Bytes |
|------|---------|-------|
| float16 | 25 (0xF9) | 3 |
| float32 | 26 (0xFA) | 5 |
| float64 | 27 (0xFB) | 9 |

**Preferred practice:** Use integer-scaled values (e.g., temperature 2550 = 25.50°C) on compact transports. Reserve floats for coordinates and values requiring IEEE 754 precision.

## 6. Conformance Vectors

All hex values below are the complete CBOR encoding of the payload.

| # | Description | CBOR Hex | Decoded |
|---|-------------|----------|---------|
| 1 | Empty map | `A0` | `{}` |
| 2 | Single uint | `A1 01 18 2A` | `{1: 42}` |
| 3 | Negative int | `A1 01 26` | `{1: -7}` |
| 4 | Float32 | `A1 01 FA 41CC0000` | `{1: 25.5}` |
| 5 | Float16 (1.0) | `A1 01 F9 3C00` | `{1: 1.0_f16}` |
| 6 | Booleans | `A2 01 F5 02 F4` | `{1: true, 2: false}` |
| 7 | Null | `A1 01 F6` | `{1: null}` |
| 8 | Text string | `A1 01 62 4869` | `{1: "Hi"}` |
| 9 | Byte string | `A1 01 44 DEADBEEF` | `{1: h'DEADBEEF'}` |
| 10 | Nested array | `A1 01 83 0A 14 18 1E` | `{1: [10, 20, 30]}` |
| 11 | Timestamp | `A1 03 1A 698FBB00` | `{3: 1771027200}` |
| 12 | Sensor (int-scaled) | `A2 01 19 09F6 02 19 1856` | `{1: 2550, 2: 6230}` |
| 13 | String key (standard) | `A1 61 74 16` | `{"t": 22}` |
| 14 | GPS (signed coords) | `A2 01 3A 15F6A287 02 1A 682AC368` | `{1: -368485000, 2: 1747633000}` |
| 15 | RGB colour | `A3 01 18FF 02 1880 03 00` | `{1: 255, 2: 128, 3: 0}` |

### Decoder Rejection Vectors

| Input | Expected |
|-------|----------|
| `F7` (undefined) | ERROR: DisallowedType |
| `C0` (tag) | ERROR: DisallowedType |
| `A2 01` (truncated) | ERROR: Truncated (after decoding Map(2) and UInt(1)) |
| `BF` (indefinite map) | ERROR: IndefiniteLength |
| `9F` (indefinite array) | ERROR: IndefiniteLength |

---

*This spec is self-contained. For the broader Reality2 protocol context, see the R2 specification suite.*
