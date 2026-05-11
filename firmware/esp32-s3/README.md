# r2-rocker firmware — ESP32-S3 carriers

The r2-rocker sensor firmware runs on an ESP32-S3 carrier board.
**Two carriers are supported as parallel alternative implementations**,
each with its own crate under this directory.

## Choose your carrier

| Carrier | Tree | Wiring | Status |
|---|---|---|---|
| **Seeed XIAO ESP32-S3** (Pre-Soldered) | [`xiao/`](xiao/) | [`HARDWARE-WIRING-XIAO.md`](../../specifications/HARDWARE-WIRING-XIAO.md) | **Current default** (ADR-001) |
| ESP32-S3-DevKitC-1 | [`devkitc/`](devkitc/) | [`HARDWARE-WIRING-DEVKITC.md`](../../specifications/HARDWARE-WIRING-DEVKITC.md) | Alternative — fully supported |

See [`../../specifications/HARDWARE-WIRING.md`](../../specifications/HARDWARE-WIRING.md)
for the carrier-choice framework and the trade-offs that drove ADR-001
([`../../specifications/decisions/ADR-001-xiao-esp32-s3-carrier.md`](../../specifications/decisions/ADR-001-xiao-esp32-s3-carrier.md)).

## What's shared between the two trees

Both carriers run on the same **ESP32-S3** silicon with the same Rust
target (`xtensa-esp32s3-espidf`), the same ESP-IDF version, the same
dependency set, and functionally-equivalent firmware. The driver code
(`adxl355.rs`), the sender pipeline (`sender.rs`), the LED state
machine (`led.rs`), the identity (`identity.rs`), the wire helpers
(`wire.rs`), and the simulator (`sim.rs`) are byte-identical across
the two trees today.

## What differs

| File | What differs |
|---|---|
| `src/main.rs` | Pin literals — XIAO uses D0/D5/D8/D9/D10/D1 (GPIO1/6/7/8/9/2); DevKitC uses GPIO10/38/12/13/11/14 |
| `Cargo.toml` | Description string and one comment about the WS2812 driver context |
| `sdkconfig.defaults` | Comments only — both carriers have 8 MB flash + 8 MB octal PSRAM + USB-Serial-JTAG console |
| `partitions.csv` | Comments only — partition layout is identical |
| `README.md` | Build-and-flash instructions specific to each carrier |
| `releases/` | Per-carrier built `.bin` artifacts (gitignored) |

## Choosing a build

```bash
# Build for the XIAO carrier:
cd firmware/esp32-s3/xiao && cargo run --release

# Build for the DevKitC carrier:
cd firmware/esp32-s3/devkitc && cargo run --release
```

Each crate has its own `target/` and `.embuild/` — first build of
either tree clones the ESP-IDF and takes 15–30 minutes; subsequent
builds are fast.

## Adding a third carrier

If you want to add a different ESP32-S3 board (FireBeetle 2 ESP32-S3,
XIAO ESP32-S3 Plus, or a custom PCB):

1. Copy one of the existing trees (`xiao/` or `devkitc/`) to a new
   directory under this folder.
2. Adjust `src/main.rs` pin literals to match the new board's GPIO map.
3. Update `Cargo.toml` description and the WS2812 comment.
4. Adjust `partitions.csv` if the new board has a different flash
   size.
5. Update `sdkconfig.defaults` for any board-specific options (e.g.
   PSRAM presence, flash size, console routing).
6. Add a corresponding `HARDWARE-WIRING-<NAME>.md` under
   `specifications/` and extend `HARDWARE-WIRING.md` (the carrier
   index).
7. If the carrier uses a **different SoC family** (e.g. ESP32-C6
   RISC-V, RP2040 ARM), write a new ADR that captures the toolchain
   and protocol-stack implications — that is a much larger
   undertaking than a same-SoC carrier swap. See ADR-001
   §"Alternatives considered" for an example of the ESP32-C6
   analysis.

A Cargo-workspace consolidation that pulls the shared sources into a
library crate (with each carrier providing only its `main.rs` + build
config) is a plausible future cleanup — out of scope for v0.1.
