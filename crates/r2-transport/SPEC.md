# R2-TRANSPORT: Reality2 Transport Binding Specification

**Version:** 0.1 Draft
**Date:** 2026-03-25
**Status:** Draft
**Depends on:** R2-WIRE (framing), R2-ROUTE (transport selection), R2-TRUST (authentication)
**Series:** Reality2 Transient Networking Stack

---

## 1. Introduction

R2-TRANSPORT defines the transport binding abstraction for Reality2's transient networking stack.  A transport binding injects R2-WIRE frames into and extracts them from a physical or logical medium (BLE, WiFi/UDP, LoRa, TCP/IP).

### 1.1 Scope: Events and Parameters Only

Transports carry **R2-WIRE frames** — events, heartbeats, capabilities, and GROUP_MGMT messages.  These are small (typically 16–222 bytes) and fire-and-forget.

Bulk data transfer (firmware updates, file sync, chat messages, AI responses) is a **plugin** concern.  Plugins use the connectivity that transports provide (e.g. a TCP socket over a WiFi SoftAP) but have their own reliable-delivery protocols.  Plugin traffic does NOT flow through the transport binding.

This separation is defined in R2-WIRE §1.1.1:

> As a **mesh transport**, TCP carries R2-WIRE events — identical framing, fire-and-forget semantics.  As a **plugin transport**, it is used by plugins that require reliable bulk delivery.  The plugin use of TCP is NOT a mesh concern.

### 1.2 Design Principles

1. **Medium-agnostic.**  The core abstraction (§2) is the same for BLE, WiFi, LoRa, and TCP/IP.  No transport is primary (R2-WIRE §1.1.1).
2. **Frames pass through unchanged.**  A transport MUST NOT alter R2-WIRE frame bytes.  It only wraps them for the medium (length prefix, datagram boundary, radio frame) and unwraps on receipt.
3. **Fire-and-forget.**  No delivery guarantees.  GROUP_MGMT achieves reliability through idempotent retransmission (R2-WIRE §8.5), not transport-level ACKs.
4. **Simple interface.**  The routing layer asks "can you send this?" and "what can you see?"  Connection management, pool scheduling, duty cycles, and radio configuration are internal to each transport.

### 1.3 Terminology

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in RFC 2119.

| Term | Definition |
|------|-----------|
| **Hive** | A single R2 device |
| **Transport** | A physical or logical medium that carries R2-WIRE frames |
| **Binding** | The code that adapts R2-WIRE frames to a specific transport |
| **Plugin** | An application-layer capability that uses transport connectivity for its own protocol |

### 1.4 Relationship to Other Specifications

| Spec | Relationship |
|------|-------------|
| R2-WIRE | Defines the frames that transports carry (§4, §13) |
| R2-ROUTE | Consumes transport metrics, selects transport per-message (§5) |
| R2-BLE | BLE transport binding (L2CAP CoC, GATT) |
| R2-WIFI | WiFi transport binding (UDP events, SoftAP) |
| R2-LORA | LoRa transport binding (raw radio frames) |
| R2-TRUST | Trust group authentication (§3); HMAC envelope (R2-WIRE §10) |

---

## 2. Transport Abstraction

### 2.1 The Transport Interface

Every transport MUST provide the following to the R2 routing layer (R2-ROUTE §1.4.4):

**Upward (transport → routing layer):**

| Information | Description |
|------------|-------------|
| Reachability | Which hives are reachable via this transport |
| Link quality | Per-neighbour: quality score [0.0, 1.0], RSSI, SNR, latency |
| Transport state | Available, existing-only, unavailable, or failed (§2.3) |
| Current MTU | Maximum R2-WIRE frame size the transport can carry right now |

**Downward (routing layer → transport):**

| Operation | Description |
|-----------|-------------|
| Send | Deliver an R2-WIRE frame to a target hive (by hive_id hash) |

### 2.2 Transport Identification

| ID | Transport | Wire Format | Max Payload | Bitmask |
|----|-----------|-------------|-------------|---------|
| 0 | BLE | Compact | 200 bytes | 0x01 |
| 1 | WiFi | Extended | 65535 bytes | 0x02 |
| 2 | LoRa | Compact | 222 bytes | 0x04 |
| 3 | Internet | Extended | 65535 bytes | 0x08 |

The bitmask values correspond to the `transports` bitfield in the neighbour table (R2-ROUTE §2.2).

### 2.3 Transport State

| State | Meaning | Routing behaviour |
|-------|---------|-------------------|
| Available | Accept new and existing peers | Normal |
| Existing-only | Existing peers usable, new peers rejected | Use for known neighbours only (R2-ROUTE §5.6) |
| Unavailable | Temporarily cannot send | Skip this transport for selection |
| Failed | Transport error, requires recovery | Skip until recovered |

### 2.4 Wire Format Selection

Per R2-WIRE §4.3.5, the wire format is determined by transport context, not by inspecting the message:

- **BLE, LoRa** → compact format (12-byte header, 16-bit msg_id, 8-byte HMAC)
- **WiFi, Internet** → extended format (22-byte header, 32-bit msg_id, 32-byte HMAC)

A hive relaying between transports with different formats MUST transcode per R2-WIRE §4.3.5.

---

## 3. Transport Bindings

### 3.1 BLE (R2-BLE §6)

R2-WIRE compact frames carried over BLE L2CAP CoC or GATT characteristics.

- **L2CAP SeqPacket**: one frame per SDU (message boundaries preserved)
- **L2CAP Stream**: 2-byte little-endian length prefix per frame (R2-BLE §6.4)
- **GATT**: frame header byte + R2-WIRE compact message (R2-BLE §3.3)

Defined in R2-BLE.  Implementation lives in `r2-ble` crate (hardware-dependent).

### 3.2 WiFi / UDP (R2-WIFI §4)

R2-WIRE extended frames carried as UDP datagrams.

- One R2-WIRE message per datagram.  No additional framing.
- Port 21042 (R2-WIRE §13.3).
- Broadcast: subnet broadcast address or multicast group `239.82.50.0`.

### 3.3 LoRa (R2-LORA §5)

R2-WIRE compact frames are the LoRa payload directly.

- No additional framing.  LoRa radio provides PHY-level framing (preamble, CRC).
- Maximum payload varies by SF/BW (51–222 bytes).
- No fragmentation — messages exceeding MTU are rejected.

Defined in R2-LORA.  Implementation lives in `r2-lora` crate (hardware-dependent).

### 3.4 TCP / IP (R2-WIRE §13.4)

R2-WIRE extended frames length-prefixed on a TCP stream.

```
[payload_length: 4 bytes, big-endian] [R2-WIRE extended message]
```

TCP connections are ephemeral for GROUP_MGMT (connect, send, disconnect).  For ongoing event relay between remote hives, the connection MAY be long-lived with periodic R2-WIRE HEARTBEAT messages (type 0x5) as keepalive.

TCP is the internet transport — the "long-distance radio" for reaching hives beyond local radio range (R2-ROUTE §1.4.1).  It carries the same fire-and-forget R2-WIRE events as any other transport.

---

## 4. Framing Helpers

This crate provides encoding/decoding helpers for transports that require explicit framing:

### 4.1 TCP Length Prefix (Big-Endian)

Per R2-WIRE §13.4:

```
Byte 0–3: payload_length (uint32, big-endian, network byte order)
Byte 4+:  R2-WIRE extended message (payload_length bytes)
```

### 4.2 BLE L2CAP Stream Length Prefix (Little-Endian)

Per R2-BLE §6.4:

```
Byte 0–1: frame_length (uint16, little-endian, per BLE convention)
Byte 2:   frame header
Byte 3+:  R2-WIRE compact message
```

Note: the byte order differs from TCP.  TCP uses big-endian per R2-WIRE §1.3.  BLE uses little-endian per BLE convention.

### 4.3 No Framing Required

- **UDP**: datagram boundaries provide framing.
- **LoRa**: radio frame boundaries provide framing.
- **BLE L2CAP SeqPacket**: SDU boundaries provide framing.

---

## 5. Port Allocation

All R2 port assignments (R2-WIFI §4, R2-WIRE §13.5):

| Port | Protocol | Purpose |
|------|----------|---------|
| 21042 | UDP | R2-WIRE events |
| 21042 | TCP | R2-WIRE events + GROUP_MGMT (length-prefixed stream) |
| 21043 | TCP | OTA firmware delivery (plugin, R2-DEPLOY) |
| 21044 | UDP | Presence/discovery broadcast (R2-WIFI §4.4) |
| 21045 | TCP/WS | Console / GraphQL (plugin, R2-CONSOLE) |

Port 21042 = 0x5232 = "R2" in ASCII.

---

## 6. Conjectures

| ID | Conjecture | Falsification |
|----|-----------|---------------|
| TRANS-001 | The Transport trait is sufficient for all four transport types without medium-specific extensions | Implement BLE, WiFi, LoRa, TCP bindings; identify any operation that cannot be expressed through the trait |
| TRANS-002 | TCP length-prefix framing adds negligible overhead (<1%) for typical R2-WIRE event sizes | Measure framing overhead for 16, 50, 200 byte payloads |
| TRANS-003 | A bridge node can transcode and relay a compact→extended frame in <1ms on a Cortex-A53 | Benchmark transcoding on Raspberry Pi / UNO-Q |
