# r2-engine

Sentant runtime primitives for R2. Defines the `Sentant` trait, the
`Plugin` interface, the `Event` and `Action` types, and the dynamic
`EventBus` used by hand-written code and tests.

This crate is the foundation that every sentant — whether
hand-written Rust, R2-COMPILE generated, or interpreted from YAML — is
built on. It is `no_std` compatible (with optional `alloc` and `std`
features) so the same code paths compile for MCU targets and Linux
hosts.

---

## What's in here

```text
┌─────────────────────────────────────────────────────┐
│                    r2-engine                         │
│                                                      │
│   trait Sentant                                      │
│     fn handle_event(&mut self, &Event, &mut ActionBuf)│
│     fn state() -> StateId                             │
│     fn class_hash() -> u32                            │
│     fn name() -> &str                                 │
│     fn subscriptions() -> &[u32]    (default = &[])   │
│     fn init(&mut self, &mut ActionBuf)  (default no-op)│
│                                                      │
│   trait Plugin                                       │
│     fn handle_command(&mut self, …) -> Result<…>      │
│                                                      │
│   enum Action                                         │
│     Send / Transition / PluginCall / DelayedSend / Log │
│                                                      │
│   ActionBuf      — fixed-capacity, no_alloc           │
│   EventQueue     — ring buffer, no_alloc              │
│   EventBus       — dynamic dispatch (alloc only)      │
│   Timer registry — delayed sends (alloc only)         │
└─────────────────────────────────────────────────────┘
```

---

## The `Sentant` trait

The primary contract every R2 agent implements:

```rust
pub trait Sentant {
    fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf);
    fn state(&self) -> StateId;
    fn class_hash(&self) -> u32;
    fn name(&self) -> &str;

    fn subscriptions(&self) -> &[u32] { &[] }
    fn init(&mut self, _actions: &mut ActionBuf) {}
}
```

The signature is what gives R2 its IPUCOD determinism guarantee:

- `&mut self` — sole ownership; no shared mutable state
- `&Event` — borrowed input; nothing to mutate behind the sentant's back
- `&mut ActionBuf` — outputs go into a buffer the engine drains afterwards
- no `async`, no I/O return type — the handler runs to completion synchronously

Sentants do not perform I/O. They emit `Action`s, which the engine
executes. This separation is what makes a sentant testable: feed it an
event, inspect the resulting actions, no mocks.

---

## Actions

```rust
pub enum Action {
    Send         { target, event_hash, payload },
    Transition   ( StateId ),
    PluginCall   { plugin_id, command, data },
    DelayedSend  { delay_ms, target, event_hash, payload },
    Log          { level, message },
}
```

`Target` resolves to one of `Sentant(local id)`, `Local`, `TrustGroup`,
`Sender`, or `Broadcast`. The hosting runtime (e.g. `r2-ensemble` on
Linux, the dispatch table on an MCU) maps each `Target` to a transport
or local fanout.

`PayloadBuf` is inline (`MAX_ACTION_PAYLOAD = 256` bytes), so emitting
actions never allocates.

---

## Where sentants come from

Three different production paths, all implementing the same trait:

1. **Hand-written.** A Rust struct with a `Sentant` impl. The cleanest
   option for protocol/platform sentants and tests.
2. **R2-COMPILE generated.** A YAML score is run through the compiler
   which emits a Rust file with the trait implemented as a static
   dispatch `match` block. Identical wire behaviour, tightest binary.
3. **YAML interpreted.** A separate runtime crate (planned Phase 2
   follow-up) walks `SentantDef.automations` and emits actions
   dynamically. Trades compile-time strictness for instant deploy from
   a score file.

A registry like `r2-ensemble` consumes any of the three transparently
through a `SentantFactory` pluggable construction surface.

---

## R2 crates this crate uses

| Crate | Role |
|---|---|
| [`r2-fnv`](../r2-fnv/) | FNV-1a 32-bit hashes for event names and class strings |
| [`r2-cbor`](../r2-cbor/) | CBOR codec for event payloads (`no_std`) |
| [`r2-wire`](../r2-wire/) | Wire-frame types referenced in tests |

No external runtime dependencies in `no_std` mode. With `alloc` enabled,
adds `Vec`-backed buffers; with `std` enabled, adds the dynamic
`EventBus`.

---

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `alloc` | on | enables `EventBus`, `Timer` registry, `Vec` payloads |
| `std` | off | enables `std`-only convenience (test harnesses) |

For MCU targets, build with `--no-default-features` to drop alloc.

---

## Examples

```rust
use r2_engine::*;

struct PingSentant { state: StateId }

impl Sentant for PingSentant {
    fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
        if event.hash == PING_HASH {
            self.state = 1;
            actions.push(Action::send(Target::Sender, PONG_HASH, &[]));
        }
    }
    fn state(&self) -> StateId { self.state }
    fn class_hash(&self) -> u32 { CLASS_HASH }
    fn name(&self) -> &str { "ping" }
}
```

---

## License

Reality2 follows an **open-core** model
(`r2-specifications/specs/thurisaz/TH-ESG.md §8`):

- The R2 protocol suite — including this crate — is open source.
- The Mariko marketplace and vertical-market services (TH-MARKET) are
  licensed commercially and live elsewhere.

This crate is dual-licensed under either of:

- **Apache License, Version 2.0** ([`LICENSE-APACHE`](../../LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- **MIT License** ([`LICENSE-MIT`](../../LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option — the standard permissive Rust ecosystem dual license.
No copyleft obligation.

Contributions are accepted under the same dual license unless you say
otherwise, per the Apache-2.0 contribution clause.
