---
title: r2-rocker — Secrets policy
status: Draft v0.1
date: 2026-05-06
---

# r2-rocker — Secrets policy

This repo is destined for a **private** GitHub repository. Even so, we
treat secret-handling as if the repo could be public tomorrow:

* GitHub's secret-scanning runs against private repos under some org
  policies and pushes alerts to the repo owner.
* Private repos get cloned to laptops, mirrored to backups, and forked for
  collaborators. A leak surface is wider than "the GitHub web UI."
* The university handoff at the end of the project may convert the repo
  status, change owners, or be republished as appendix material to the
  paper. We don't want a key rotation forced by a clean-up oversight.

The rule: **no private key material in the working tree, ever**. Public
material only.

## What's a secret

| Item | Sensitivity | Where it lives | In repo? |
|---|---|---|---|
| **TG private key** (Ed25519 signing key for the trust group) | High — compromise enables fake sensors | `~/.config/r2-rocker/tg_signer/tg_priv.bin` on the *signing host* (one machine, normally the lead developer's laptop) | **No** |
| **TG public key** + cert | Public by design | Committed at `trust_keys/tg_pub.bin` and embedded in firmware via `include_bytes!` | **Yes** |
| **Per-device Ed25519 keys** | Medium — compromise impersonates one sensor | NVS partition on each ESP32 (`device_priv.bin` namespace), generated on first boot via `esp_fill_random()` | **No** |
| **Per-device public keys** | Public by design | Persisted by dashboard as `dashboard/peers.json` (gitignored — we keep the *list* private but the *contents* aren't secret) | No (gitignored, not for confidentiality but for hygiene) |
| **WiFi credentials** | Medium — local hotspot SSID/PSK | `firmware/esp32-s3/wifi_config.toml` at build time, or NVS at runtime | **No** (only `.example` committed) |
| **Calibration matrices** | Not a secret — but local state | `dashboard/calibration.json` | No (gitignored — local runtime state) |
| **Sample data captures** | Research data; check IP / NDA before publishing | `dashboard/.state/` | No (gitignored by default; export selectively) |

## What gets committed

The repo contains, intentionally:

* `trust_keys/tg_pub.bin` — TG public key, ~32 bytes, suitable for
  embedding in firmware.
* `trust_keys/tg_cert.bin` — TG self-signed cert (or commitment) if we
  use one. Public.
* `firmware/**/wifi_config.toml.example` — placeholder values only.
* All source code, specs, plans, conversation archives.

Anything else with key-shaped bytes is a bug.

## TG provisioning (one-time)

The trust group is generated **once** by the project lead on a single
machine, off-tree:

```bash
# off-tree, in ~/.config/r2-rocker/tg_signer/  (NOT inside the repo)
mkdir -p ~/.config/r2-rocker/tg_signer
cd ~/.config/r2-rocker/tg_signer
# Generate keypair (helper tool to be written in tools/r2-rocker-tg/)
r2-rocker-tg keygen --name "rocker-rig-uoa-2026" \
                    --priv tg_priv.bin \
                    --pub  tg_pub.bin \
                    --cert tg_cert.bin

# Then copy ONLY the public material into the repo:
cp tg_pub.bin  ~/Development/R2/r2-rocker/trust_keys/
cp tg_cert.bin ~/Development/R2/r2-rocker/trust_keys/
```

Key invariant: `tg_priv.bin` never enters the repo working tree. The
`.gitignore` patterns block `*_priv*`, `*.priv`, etc. as belt-and-braces.

## Dashboard signing flow

The dashboard signs `#wifi_offer` frames with the TG private key when
bootstrapping new sensors. The dashboard reads the private key from a
configured path, default `~/.config/r2-rocker/tg_signer/tg_priv.bin`.

If the dashboard is run on a different machine (e.g. handoff to the
university), the new operator must provision their own signer — i.e. they
either:

(a) receive the existing `tg_priv.bin` out-of-band (USB key, encrypted
    archive); or

(b) generate a fresh TG, reflash all sensor firmware to embed the new
    `tg_pub.bin`. (This is the "key rotation" path; see below.)

## Key rotation

If the TG private key leaks (lost laptop, accidental commit, etc.):

1. Generate a fresh TG keypair.
2. Update `trust_keys/tg_pub.bin` and `trust_keys/tg_cert.bin` in the
   repo, commit, push.
3. Rebuild firmware against the new keys.
4. OTA-push the new firmware to all deployed sensors using the **old**
   trust group (last legitimate use of the old key).
5. Confirm all sensors have updated, then destroy the old `tg_priv.bin`.
6. The brief overlap window where both keys are valid is the unavoidable
   cost of remote rotation; minimise it.

If the leak window is unbounded (key was published months ago), assume
all data signed with the old key is compromised; physical re-flashing of
each sensor is required.

## Per-device key handling

ESP32-S3 generates `device_priv` on first boot via `esp_fill_random()`,
stores it in encrypted NVS (esp-idf NVS encryption flag in
`sdkconfig.defaults`). The key never leaves the device. The dashboard
collects `device_pub` on `SENSOR_ANNOUNCE` and that's the only material
that ever crosses the wire.

For factory reset / re-pairing, an OTA command erases the NVS namespace;
the next boot generates a fresh device key. This is also the recovery
path for a stolen sensor.

## What to do before any public release

If any portion of this repo is ever republished (paper appendix, open
source release, etc.):

1. Audit `conversation/` for PII — author emails, real-name attribution
   that the contributor didn't sign off on.
2. Audit `dashboard/.state/` exports — sample captures may contain
   identifiable rig configuration data.
3. Audit `peers.json` if exported — device public keys are not secret,
   but the *list* is rig topology metadata.
4. Run the secret scanner one more time:
   ```bash
   gitleaks detect --source . --no-banner
   trufflehog filesystem --no-update .
   ```
5. Tag the release commit; future history can change without re-auditing.

## References

* GitHub secret scanning patterns:
  <https://docs.github.com/en/code-security/secret-scanning/secret-scanning-patterns>
* esp-idf NVS encryption:
  <https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-reference/storage/nvs_encryption.html>
* Existing R2 trust scheme: see `r2-core/crates/r2-trust/` for cert and
  HKDF derivation patterns we'll vendor selectively.

## Change log

| Date | Version | Change |
|---|---|---|
| 2026-05-06 | 0.1 | Initial draft. Established before any keys exist on disk. |
