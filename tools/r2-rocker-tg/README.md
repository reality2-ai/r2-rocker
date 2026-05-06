# r2-rocker-tg

Trust-group keygen / verify / inspect utility for the r2-rocker project.

This is the only tool that touches the **trust-group private key**. Run it
once, off the repo working tree, on a host you trust. See
`specifications/SECRETS-POLICY.md` for the policy and
`specifications/SPEC-R2-ROCKER-SYSTEM.md` §3.1 for the provisioning flow.

## Build

```bash
cd tools/r2-rocker-tg
cargo build --release
# binary at: ../../target/release/r2-rocker-tg
```

Or install:

```bash
cargo install --path .
```

## Usage

### Generate a new trust group (one-time)

```bash
mkdir -p ~/.config/r2-rocker/tg_signer

r2-rocker-tg keygen \
    --priv ~/.config/r2-rocker/tg_signer/tg_priv.bin \
    --pub  /tmp/tg_pub.bin \
    --cert /tmp/tg_cert.bin \
    --name "rocker-rig-uoa-2026"
```

Output:

```
Wrote private key: /home/you/.config/r2-rocker/tg_signer/tg_priv.bin (mode 0600)
Wrote public key:  /tmp/tg_pub.bin
Wrote cert:        /tmp/tg_cert.bin

Public key (hex): 1a2b3c…
Fingerprint:      1a2b:3c4d:5e6f:7081:9293:a4b5:c6d7:e8f9

Next steps:
  1. Copy the public key (and cert if produced) into the repo at trust_keys/.
     The firmware build embeds tg_pub.bin via include_bytes!.
  2. Keep the private key OFF-tree per SECRETS-POLICY.md.
```

Then:

```bash
cp /tmp/tg_pub.bin   <repo>/trust_keys/tg_pub.bin
cp /tmp/tg_cert.bin  <repo>/trust_keys/tg_cert.bin
git -C <repo> add trust_keys/
git -C <repo> commit -m "trust_keys: rocker-rig-uoa-2026 TG"
```

**Never** copy `tg_priv.bin` into the repo. The `.gitignore` patterns
block `*_priv*` as a safety net, but the rule is *don't put it there in
the first place*.

### Verify a cert

```bash
r2-rocker-tg verify <repo>/trust_keys/tg_cert.bin
```

### Inspect a key or cert

```bash
r2-rocker-tg inspect <repo>/trust_keys/tg_pub.bin
r2-rocker-tg inspect <repo>/trust_keys/tg_cert.bin
```

## Cert format

CBOR map with integer keys. The signature at key 3 covers the canonical
CBOR encoding of keys 0..2 (the body):

| Key | Type      | Description |
|-----|-----------|-------------|
| 0   | text      | TG name (free-form, e.g. `"rocker-rig-uoa-2026"`) |
| 1   | uint      | Created-at, unix epoch seconds |
| 2   | bytes(32) | Ed25519 public key |
| 3   | bytes(64) | Ed25519 signature over canonical CBOR of {0,1,2} |

The cert is **self-signed** — verifying it proves the key holder agreed
to the name + creation date but says nothing about external authority.
That's appropriate for a closed-deployment trust group; no PKI here.

## File formats

| File           | Bytes | Permissions |
|----------------|-------|-------------|
| `tg_priv.bin`  | 32    | 0600        |
| `tg_pub.bin`   | 32    | 0644        |
| `tg_cert.bin`  | ~120  | 0644        |

`tg_priv.bin` is the raw 32-byte Ed25519 seed (compatible with
`ed25519_dalek::SigningKey::from_bytes`). `tg_pub.bin` is the raw 32-byte
public key.

## Safety rails

* The tool refuses to overwrite existing output files unless `--force`
  is passed.
* It warns if the private-key path is inside an `r2-rocker` working tree
  (heuristic; the canonical location is `~/.config/r2-rocker/tg_signer/`).
* It writes the private key with mode 0600 on Unix; on Windows the OS
  default ACLs apply (no equivalent enforced — keep the file off shared
  storage).

## Manual round-trip test

```bash
TMPDIR=$(mktemp -d)
cargo run --release -- keygen \
    --priv $TMPDIR/priv.bin \
    --pub  $TMPDIR/pub.bin \
    --cert $TMPDIR/cert.bin \
    --name "test-tg"
cargo run --release -- inspect $TMPDIR/pub.bin
cargo run --release -- verify  $TMPDIR/cert.bin
cargo run --release -- inspect $TMPDIR/cert.bin
rm -rf $TMPDIR
```

Expect each step to succeed.
