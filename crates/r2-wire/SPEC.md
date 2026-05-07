# R2-WIRE: Wire Protocol Framing for Reality2

**Version:** 0.1.0
**Status:** Stable — wire format frozen

---

## 1. Purpose

R2-WIRE defines the binary message format for Reality2 mesh communication. Two formats serve different transports:

| Format | Transport | Header | Fields |
|--------|-----------|--------|--------|
| **Compact** | BLE, LoRa | 12 bytes fixed | 16-bit msg_id, 32-bit target |
| **Extended** | TCP, WebSocket | 22 bytes fixed | 32-bit msg_id, dual 32-bit target |

Both formats are transport-agnostic and self-describing via byte 0.

## 2. Byte Order

All multi-byte integer fields are **big-endian** (network byte order). This applies to R2-WIRE header fields only — transport-layer framing (e.g., BLE L2CAP length prefix) follows transport conventions.

## 3. Compact Format (12-byte header)

```
Byte 0:   [VV][TTT][RHM]     version(2) | msg_type(3) | flags(3)
Byte 1:   [TTTT][KKKK]       ttl(4) | k(4)
Byte 2-3: msg_id              uint16 BE
Byte 4-7: event_hash          uint32 BE (FNV-1a of event name)
Byte 8-11: target             uint32 BE
--- variable ---
[1 + 2×N]: route_stack        if R flag: len(1) + N×uint16 BE entries (max 8)
[N]:        payload            CBOR-encoded (R2-CBOR compact mode)
[8]:        hmac_tag           if H flag: truncated HMAC-SHA256
```

### 3.1 Byte 0 — Header

| Bits | Field | Values |
|------|-------|--------|
| [7:6] | Version | `0b00` = v0 (current) |
| [5:3] | Message type | 0=EVENT, 1=reserved, 2=REPLY, 3=CAPABILITY, 4=GROUP_MGMT, 5=HEARTBEAT, 6-7=reserved |
| [2] | R (has_route) | 1 = route stack present |
| [1] | H (has_hmac) | 1 = HMAC tag appended |
| [0] | M (mcu_origin) | 1 = originated from constrained device |

### 3.2 Byte 1 — TTL and K

| Bits | Field | Range |
|------|-------|-------|
| [7:4] | TTL | 0–15 (decremented at each hop) |
| [3:0] | K | 0–15 (spray-and-wait budget) |

### 3.3 Route Stack

Each entry is bits [31:16] of FNV-1a 32-bit of the hive's device UUID string (same string as target addressing). First entry = originator. Max 8 entries; messages exceeding this MUST be dropped.

### 3.4 Special Targets

| Target | Value | Meaning |
|--------|-------|---------|
| `@all` | `0x00000000` | Broadcast |
| `@local` | TTL=0 | Local delivery only |
| `@sender` | Route stack reply | Reply to originator |
| `@group` | Via trust group routing | All group members |

## 4. Extended Format (22-byte header)

```
Byte 0:    [VV][TTT][RHM]     (same as compact)
Byte 1:    [TTTT][KKKK]       (same as compact)
Byte 2-5:  msg_id              uint32 BE
Byte 6-9:  event_hash          uint32 BE
Byte 10-13: payload_len        uint32 BE (authoritative)
Byte 14-17: target_group       uint32 BE
Byte 18-21: target_hive        uint32 BE
--- variable ---
[1 + 4×N]: route_stack         if R flag: len(1) + N×uint32 BE entries (max 8)
[N]:        payload             CBOR-encoded (payload_len bytes)
[32]:       hmac_tag            if H flag: full HMAC-SHA256
```

`payload_len` is authoritative: decoders MUST validate that remaining data matches.

## 5. Transcoding

Compact ↔ Extended transcoding rules:

| Compact → Extended | Extended → Compact |
|---|---|
| `msg_id` zero-extended to 32 bits | `msg_id` truncated to 16 bits |
| `target` → `target_group`, `target_hive` = 0 | `target_group` if nonzero, else `target_hive` |
| Route entries: `(entry as u32) << 16` | Route entries: `(entry >> 16) as u16` |
| HMAC 8 bytes → 32 bytes (zero-padded) | HMAC 32 bytes → first 8 bytes |

Note: HMAC zero-padding is lossy — the extended-side tag will not verify.

## 6. Deduplication

Hives maintain a `(msg_id, source)` dedup cache with 60-second TTL. Source is the first route stack entry, or the transport-layer address if no route.

## 7. Conformance Vectors

### TV1 — Minimal EVENT (no route, no HMAC)

```
Hex: 00 53 A1B2 424D3E4C 1A2B3C4D A10018EA
```

| Field | Value |
|-------|-------|
| version | 0 |
| msg_type | EVENT (0) |
| flags | R=0, H=0, M=0 |
| TTL | 5 |
| K | 3 |
| msg_id | 0xA1B2 |
| event_hash | 0x424D3E4C (`read_level`) |
| target | 0x1A2B3C4D |
| payload | `A1 00 18 EA` = `{0: 234}` |

### TV2 — EVENT with route stack

```
Hex: 04 53 A1B2 424D3E4C 1A2B3C4D 02 AABB CCDD A10018EA
```

Route: 2 entries — `0xAABB`, `0xCCDD`.

### TV3 — EVENT with HMAC

```
Hex: 02 53 A1B2 424D3E4C 1A2B3C4D A10018EA 0102030405060708
```

HMAC tag: `01 02 03 04 05 06 07 08` (8 bytes, truncated).

## 8. HMAC Envelope (`hmac.rs`)

The `HmacProvider` trait abstracts HMAC computation. r2-wire defines *what* to authenticate; the caller (r2-trust) supplies *how*.

### 8.1 Authenticated Bytes

Only **immutable** fields are authenticated. TTL, K, msg_id, and route stack are mutable (relay nodes change them) and excluded.

**Compact:** `msg_type(1) || event_hash(4) || target(4) || payload(N)`

**Extended:** `msg_type(1) || event_hash(4) || target_group(4) || target_hive(4) || payload(N)`

`msg_type` is the 3-bit value (0x00–0x05) zero-extended to one byte — NOT byte 0 of the wire header.

### 8.2 Tag Sizes

| Format | Tag | Algorithm |
|--------|-----|-----------|
| Compact | 8 bytes (truncated) | First 8 bytes of HMAC-SHA256 |
| Extended | 32 bytes (full) | Full HMAC-SHA256 output |

### 8.3 Frame Classification (R2-TRUST §6.3)

| HMAC present? | Key available? | Result |
|--------------|---------------|--------|
| No (H=0) | — | `Unauthenticated` |
| Yes | No | `Relay` (forward opaquely) |
| Yes | Yes, valid | `SameGroup` |
| Yes | Yes, invalid | Drop frame (`None`) |

Verification uses constant-time comparison.

### 8.4 HmacProvider Trait

```rust
pub trait HmacProvider {
    fn mac_compact(&self, authenticated_bytes: &[u8]) -> [u8; 8];
    fn mac_extended(&self, authenticated_bytes: &[u8]) -> [u8; 32];
}
```

Concrete implementations: `r2_trust::GroupHmac` (intra-group) and `r2_trust::PeeringHmac` (bilateral entanglement).

---

*For HMAC key derivation, see R2-TRUST. For payload encoding, see r2-cbor SPEC.md.*
