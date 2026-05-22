//! r2-rocker-tg — Trust-group keygen / verify / inspect utility.
//!
//! See `specifications/SECRETS-POLICY.md` for the policy this tool enforces
//! and `specifications/SPEC-R2-ROCKER-SYSTEM.md` §3.1 for the provisioning
//! flow it implements.

use anyhow::{anyhow, bail, Context, Result};
use ciborium::value::{Integer, Value};
use clap::{Args, Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const PRIV_MODE: u32 = 0o600;
const PUB_MODE: u32 = 0o644;

#[derive(Parser)]
#[command(
    name = "r2-rocker-tg",
    version,
    about = "Trust-group keygen / verify / inspect for r2-rocker"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// One-shot Trust Group setup for a new deployment.
    ///
    /// Generates a fresh keypair + self-signed cert and writes them to
    /// the canonical paths:
    ///   ~/.config/r2-rocker/tg_signer/tg_priv.bin   (mode 0600)
    ///   trust_keys/tg_pub.bin                       (relative to cwd)
    ///   trust_keys/tg_cert.bin                      (relative to cwd)
    ///
    /// Run this once per deployment from the repo root, before
    /// building firmware. After running, `tools/build-firmware.sh`
    /// will pick up the new TG public key via `include_bytes!`.
    Init(InitArgs),
    /// Generate a fresh Ed25519 trust-group keypair, optionally with a self-signed cert.
    Keygen(KeygenArgs),
    /// Verify the signature in a cert file.
    Verify {
        /// Path to the cert file.
        cert: PathBuf,
    },
    /// Print metadata of a 32-byte raw key file or a CBOR cert.
    Inspect {
        /// Path to the file (raw key or cert).
        path: PathBuf,
    },
}

#[derive(Args)]
struct InitArgs {
    /// TG name to embed in the self-signed cert. Defaults to
    /// `rocker-rig-<YYYY-MM-DD>` if omitted.
    #[arg(long)]
    name: Option<String>,
    /// Overwrite existing files. Without this, refuses if any of
    /// tg_priv.bin / tg_pub.bin / tg_cert.bin already exists.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct KeygenArgs {
    /// Output path for the 32-byte raw private key seed.
    #[arg(long = "priv")]
    private: PathBuf,
    /// Output path for the 32-byte raw public key.
    #[arg(long = "pub")]
    public: PathBuf,
    /// Output path for the self-signed cert (CBOR).
    #[arg(long)]
    cert: Option<PathBuf>,
    /// TG name to embed in the cert. Required if --cert is given.
    #[arg(long)]
    name: Option<String>,
    /// Overwrite existing files.
    #[arg(long)]
    force: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init(args) => init(args),
        Commands::Keygen(args) => keygen(args),
        Commands::Verify { cert } => verify_cert(&cert),
        Commands::Inspect { path } => inspect(&path),
    }
}

// ──────────────────────────────────────────────────────────────────────────
// init — one-shot setup for a new deployment

fn init(args: InitArgs) -> Result<()> {
    // Default paths per SECRETS-POLICY.md §1.
    let home = std::env::var("HOME").context("$HOME not set — required to resolve tg_priv.bin path")?;
    let priv_path = PathBuf::from(&home).join(".config/r2-rocker/tg_signer/tg_priv.bin");
    let pub_path = PathBuf::from("trust_keys/tg_pub.bin");
    let cert_path = PathBuf::from("trust_keys/tg_cert.bin");

    // Refuse early if cwd doesn't look like the repo root.
    if !PathBuf::from("trust_keys").is_dir() {
        bail!(
            "trust_keys/ not found in current directory — run `r2-rocker-tg init` \
             from the repo root (where trust_keys/ lives)"
        );
    }

    // Default name: rocker-rig-YYYY-MM-DD. Stable enough to recognise
    // later; doesn't leak any host-identifying info.
    let name = args.name.unwrap_or_else(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // YYYY-MM-DD from unix seconds, UTC. Avoids pulling in chrono.
        let days = now / 86_400;
        let (y, m, d) = unix_days_to_ymd(days as i64);
        format!("rocker-rig-{:04}-{:02}-{:02}", y, m, d)
    });

    eprintln!("r2-rocker-tg init — fresh Trust Group for this deployment");
    eprintln!("  name:    {}", name);
    eprintln!("  priv:    {}", priv_path.display());
    eprintln!("  pub:     {}", pub_path.display());
    eprintln!("  cert:    {}", cert_path.display());
    eprintln!();

    if let Some(parent) = priv_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    keygen(KeygenArgs {
        private: priv_path,
        public: pub_path,
        cert: Some(cert_path),
        name: Some(name),
        force: args.force,
    })?;

    eprintln!();
    eprintln!("Done. The next firmware build (e.g. `tools/build-firmware.sh devkitc`)");
    eprintln!("will embed the new tg_pub.bin via include_bytes!. The dashboard will");
    eprintln!("pick up the new tg_priv.bin on its next start.");
    eprintln!();
    eprintln!("If this deployment had earlier sensors flashed with a different TG,");
    eprintln!("re-flash them with firmware built from the new keys — their old certs");
    eprintln!("won't verify under the new TG_PUB_KEY.");
    Ok(())
}

/// Convert days-since-1970-01-01 to (year, month, day). Algorithm
/// from "Hinnant's Date Algorithms" — handles all dates within
/// i64 range, no chrono dependency.
fn unix_days_to_ymd(z: i64) -> (i32, u8, u8) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

// ──────────────────────────────────────────────────────────────────────────
// keygen

fn keygen(args: KeygenArgs) -> Result<()> {
    if args.cert.is_some() && args.name.is_none() {
        bail!("--name is required when --cert is given");
    }

    // Refuse to clobber existing files unless --force.
    let mut paths: Vec<&Path> = vec![args.private.as_path(), args.public.as_path()];
    if let Some(c) = args.cert.as_ref() {
        paths.push(c);
    }
    for p in &paths {
        if p.exists() && !args.force {
            bail!(
                "refusing to overwrite existing file {} — pass --force to override",
                p.display()
            );
        }
    }

    if path_inside_repo(&args.private) {
        eprintln!("WARNING: private key path appears to be inside a repo working tree.");
        eprintln!("         If this gets committed, the trust group is compromised.");
        eprintln!("         Recommended: ~/.config/r2-rocker/tg_signer/tg_priv.bin");
        eprintln!();
    }

    let mut csprng = OsRng;
    let signing = SigningKey::generate(&mut csprng);
    let verifying = signing.verifying_key();

    write_with_mode(&args.private, signing.to_bytes().as_slice(), PRIV_MODE)
        .with_context(|| format!("writing {}", args.private.display()))?;
    write_with_mode(&args.public, verifying.to_bytes().as_slice(), PUB_MODE)
        .with_context(|| format!("writing {}", args.public.display()))?;

    println!("Wrote private key: {} (mode 0600)", args.private.display());
    println!("Wrote public key:  {}", args.public.display());

    if let Some(cert_path) = args.cert.as_ref() {
        let name = args.name.as_deref().unwrap();
        let cert_bytes = build_cert(name, &signing, &verifying)?;
        write_with_mode(cert_path, &cert_bytes, PUB_MODE)
            .with_context(|| format!("writing {}", cert_path.display()))?;
        println!("Wrote cert:        {}", cert_path.display());
    }

    println!();
    println!("Public key (hex): {}", hex_encode(verifying.to_bytes().as_slice()));
    println!("Fingerprint:      {}", fingerprint(verifying.to_bytes().as_slice()));
    println!();
    println!("Next steps:");
    println!("  1. Copy the public key (and cert if produced) into the repo at trust_keys/.");
    println!("     The firmware build embeds tg_pub.bin via include_bytes!.");
    println!("  2. Keep the private key OFF-tree per SECRETS-POLICY.md.");

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// cert build / verify

fn build_cert(name: &str, signing: &SigningKey, verifying: &VerifyingKey) -> Result<Vec<u8>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs();

    // Canonical body to sign: { 0: name, 1: created_at, 2: pub_key }
    let body = body_value(name, now, verifying.to_bytes().as_slice());
    let mut body_bytes = Vec::new();
    ciborium::ser::into_writer(&body, &mut body_bytes).context("encoding cert body")?;

    let sig = signing.sign(&body_bytes);

    // Cert: body + sig at key 3.
    let cert = Value::Map(vec![
        (Value::Integer(Integer::from(0_u64)), Value::Text(name.into())),
        (
            Value::Integer(Integer::from(1_u64)),
            Value::Integer(Integer::from(now)),
        ),
        (
            Value::Integer(Integer::from(2_u64)),
            Value::Bytes(verifying.to_bytes().to_vec()),
        ),
        (
            Value::Integer(Integer::from(3_u64)),
            Value::Bytes(sig.to_bytes().to_vec()),
        ),
    ]);
    let mut cert_bytes = Vec::new();
    ciborium::ser::into_writer(&cert, &mut cert_bytes).context("encoding cert")?;
    Ok(cert_bytes)
}

fn body_value(name: &str, created_at: u64, pub_key: &[u8]) -> Value {
    Value::Map(vec![
        (Value::Integer(Integer::from(0_u64)), Value::Text(name.into())),
        (
            Value::Integer(Integer::from(1_u64)),
            Value::Integer(Integer::from(created_at)),
        ),
        (
            Value::Integer(Integer::from(2_u64)),
            Value::Bytes(pub_key.to_vec()),
        ),
    ])
}

fn verify_cert(path: &Path) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let cert: Value = ciborium::de::from_reader(bytes.as_slice()).context("parsing CBOR")?;
    let map = match &cert {
        Value::Map(m) => m,
        _ => bail!("cert is not a CBOR map"),
    };

    let mut name: Option<String> = None;
    let mut created_at: Option<u64> = None;
    let mut pub_key: Option<Vec<u8>> = None;
    let mut sig: Option<Vec<u8>> = None;
    for (k, v) in map {
        let key = match k {
            Value::Integer(i) => {
                let raw: i128 = (*i).into();
                raw
            }
            _ => continue,
        };
        match (key, v) {
            (0, Value::Text(s)) => name = Some(s.clone()),
            (1, Value::Integer(i)) => {
                let raw: i128 = (*i).into();
                if raw < 0 {
                    bail!("created_at is negative");
                }
                created_at = Some(raw as u64);
            }
            (2, Value::Bytes(b)) => pub_key = Some(b.clone()),
            (3, Value::Bytes(b)) => sig = Some(b.clone()),
            _ => {}
        }
    }
    let name = name.ok_or_else(|| anyhow!("cert missing key 0 (name)"))?;
    let created_at = created_at.ok_or_else(|| anyhow!("cert missing key 1 (created_at)"))?;
    let pub_key = pub_key.ok_or_else(|| anyhow!("cert missing key 2 (pub_key)"))?;
    let sig = sig.ok_or_else(|| anyhow!("cert missing key 3 (sig)"))?;

    if pub_key.len() != 32 {
        bail!("pub_key length {} != 32", pub_key.len());
    }
    if sig.len() != 64 {
        bail!("sig length {} != 64", sig.len());
    }
    let pub_arr: [u8; 32] = pub_key.as_slice().try_into().unwrap();
    let sig_arr: [u8; 64] = sig.as_slice().try_into().unwrap();

    let body = body_value(&name, created_at, &pub_arr);
    let mut body_bytes = Vec::new();
    ciborium::ser::into_writer(&body, &mut body_bytes)?;

    let verifying = VerifyingKey::from_bytes(&pub_arr).context("invalid Ed25519 public key")?;
    let signature = Signature::from_bytes(&sig_arr);
    verifying
        .verify(&body_bytes, &signature)
        .context("signature verification failed")?;

    println!("Cert OK: {}", path.display());
    println!("  name:          {}", name);
    println!("  created_at:    {} (unix seconds)", created_at);
    println!("  pub key (hex): {}", hex_encode(&pub_arr));
    println!("  fingerprint:   {}", fingerprint(&pub_arr));

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// inspect

fn inspect(path: &Path) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() == 32 {
        println!("32-byte raw Ed25519 key: {}", path.display());
        println!("  hex:         {}", hex_encode(&bytes));
        println!("  fingerprint: {}", fingerprint(&bytes));
        println!();
        println!("Note: this tool cannot tell whether this is a public or private key from");
        println!("the bytes alone. If the file mode is 0600 it is most likely the private");
        println!("key — do not share its hex.");
    } else {
        verify_cert(path)?;
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// helpers

fn write_with_mode(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
    }
    fs::write(path, data)?;

    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    let _ = mode;

    Ok(())
}

fn path_inside_repo(p: &Path) -> bool {
    let s = p.to_string_lossy();
    // Heuristic: warn if writing inside any r2-rocker working tree, but allow
    // .config paths (the canonical off-tree location).
    s.contains("/r2-rocker/") && !s.contains("/.config/")
}

fn hex_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(data.len() * 2);
    for b in data {
        let _ = write!(out, "{:02x}", b);
    }
    out
}

fn fingerprint(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let h = hasher.finalize();
    h[..16]
        .chunks(2)
        .map(|c| format!("{:02x}{:02x}", c[0], c[1]))
        .collect::<Vec<_>>()
        .join(":")
}

// ──────────────────────────────────────────────────────────────────────────
// tests

#[cfg(test)]
mod tests {
    use super::*;

    // A keygen round-trip is documented in the README as a manual test.
    // Add an integration test (with tempfile, etc.) once the crate is
    // part of a workspace and dev-dependencies are wired up.
    #[test]
    fn fingerprint_format() {
        let bytes = [0u8; 32];
        let fp = fingerprint(&bytes);
        // 16 bytes of digest → 8 four-char groups separated by 7 colons.
        assert_eq!(fp.split(':').count(), 8);
        assert_eq!(fp.len(), 8 * 4 + 7);
    }

    #[test]
    fn hex_format() {
        assert_eq!(hex_encode(&[0xab, 0xcd, 0x01]), "abcd01");
    }
}
