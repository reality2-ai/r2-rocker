# R2-FNV: Event Name Hashing

**Version:** 0.1.0
**Status:** Stable — wire-format frozen

---

## 1. Purpose

R2-FNV maps human-readable event names to compact 32-bit identifiers for use in R2-WIRE message headers. The hash is computed once at registration time and used on every message.

## 2. Algorithm

FNV-1a 32-bit (Fowler–Noll–Vo, variant 1a).

```
offset_basis = 0x811C9DC5
prime        = 0x01000193

hash = offset_basis
for each byte b in canonicalised_name:
    hash = hash XOR b
    hash = hash × prime (mod 2³²)
```

This is the standard FNV-1a as defined by Landon Curt Noll. No modifications.

## 3. Canonicalisation

Before hashing, event names MUST be canonicalised:

1. Convert all ASCII uppercase (A–Z) to lowercase (a–z)
2. Strip all ASCII whitespace bytes: `{0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20}`
3. If the result is empty, reject with an error

After canonicalisation: `"  Set_Temp "` → `"set_temp"`, `"# PING"` → `"#ping"`.

Unicode whitespace (e.g., U+2003 EM SPACE) is NOT stripped — it is invalid in event names and MUST be rejected at registration time.

## 4. Naming Convention

| Prefix | Meaning | Examples |
|--------|---------|----------|
| `#` | Platform-reserved event | `#ping`, `#wifi_req`, `#ota_query` |
| *(none)* | Agent-defined event | `read_level`, `set_temp`, `alert` |

The `#` prefix is part of the hash input. `hash("#ping") ≠ hash("ping")`.

## 5. Reserved Values

| Hash | Meaning |
|------|---------|
| `0x00000000` | Reserved — broadcast target / null |
| `0xFFFFFFFF` | Reserved — future use |

If a canonicalised name hashes to either reserved value, it MUST be rejected.

## 6. Collision Properties

FNV-1a 32-bit has good distribution for short ASCII strings. Birthday-problem collision probability:

| Event count | Collision probability |
|-------------|----------------------|
| 100 | ~0.0001% |
| 1,000 | ~0.01% |
| 10,000 | ~1.2% |
| 50,000 | ~25% |

R2 deployments are expected to have <1,000 distinct event names. Implementations SHOULD check for collisions at registration time and reject duplicates.

## 7. Conformance Vectors

Any correct implementation MUST produce these exact values:

### Algorithm Vectors (no prefix)

| Input | Hash |
|-------|------|
| `""` (empty) | ERROR: empty name |
| `"read_level"` | `0x424D3E4C` |
| `"set_temp"` | `0x745B28B2` |
| `"water_level"` | `0x49B47465` |
| `"set_color"` | `0xC98950FB` |
| `"get_status"` | `0xA8E6815A` |
| `"display_image"` | `0x56179273` |
| `"clear"` | `0x5C6E1222` |
| `"calibrate"` | `0xBD2F7702` |
| `"alert"` | `0xB7C358F9` |
| `"evacuate"` | `0x79E8481B` |
| `"message"` | `0x24F208E4` |

### Platform Events (# prefix)

| Input | Hash |
|-------|------|
| `"#ping"` | `0x7CB36B0A` |
| `"#pong"` | `0x1130DCC8` |
| `"#wifi_req"` | `0xC85B18A8` |
| `"#wifi_offer"` | `0x01F77656` |
| `"#wifi_done"` | `0x89A70310` |
| `"#ota_query"` | `0x68F41803` |
| `"#ota_info"` | `0x610F43A5` |
| `"#heartbeat"` | `0x5D6B02AE` |
| `"#join"` | `0xFAD8225E` |
| `"#leave"` | `0x2FD66589` |
| `"#capability"` | `0xF24C0E88` |
| `"#ack"` | `0xB4A426EF` |
| `"#subscribe"` | `0xFB90EBEE` |
| `"#unsubscribe"` | `0x25174D77` |
| `"#broadcast"` | `0xFAECDE75` |

### Canonicalisation Vectors

All of these MUST produce the same hash as `"set_temp"` (`0x745B28B2`):

| Input |
|-------|
| `"Set_Temp"` |
| `"SET_TEMP"` |
| `"  set_temp  "` |
| `"\tset_temp\n"` |
| `"s e t _ t e m p"` |

---

*This spec is self-contained. For the broader Reality2 protocol context, see the R2 specification suite.*
