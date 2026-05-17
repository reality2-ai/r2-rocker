use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=partitions.csv");
    println!("cargo:rerun-if-changed=sdkconfig.defaults");
    println!("cargo:rerun-if-changed=wifi_config.toml");
    println!("cargo:rerun-if-changed=build.rs");
    // Re-run the build script when the source tree or git state changes,
    // so R2_GIT_SHA / R2_BUILD_TIMESTAMP track the current build (and
    // the ESP-IDF `PROJECT_VER` string baked into the boot banner stays
    // in sync with the actual binary). Without these, build.rs ran once
    // on first build, baked stale values into env vars, and never re-ran
    // — every subsequent `cargo build` produced a fresh `.bin` whose
    // announce/boot strings pointed at the *previous* commit.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let manifest_dir_for_git = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    track_git_state(&manifest_dir_for_git);

    let manifest_dir = manifest_dir_for_git.clone();

    stage_partitions_csv(&manifest_dir);
    load_wifi_config(&manifest_dir);
    stamp_build_metadata();

    embuild::espidf::sysenv::output();
}

/// Emit `cargo:rerun-if-changed` directives for the git files whose
/// contents the stamped env vars depend on: `HEAD` (branch switch),
/// the branch ref the current HEAD points at (commits on this branch),
/// and `index` (staged-vs-tree status — affects the `-dirty` suffix).
/// Repo root is three levels above `firmware/esp32-s3/<carrier>/`.
fn track_git_state(manifest_dir: &str) {
    let repo_root = PathBuf::from(manifest_dir)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let Some(repo_root) = repo_root else { return };

    let git_dir = repo_root.join(".git");
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
    }
    let index = git_dir.join("index");
    if index.exists() {
        println!("cargo:rerun-if-changed={}", index.display());
    }
    if let Ok(content) = fs::read_to_string(&head) {
        if let Some(ref_path) = content.strip_prefix("ref: ").map(str::trim) {
            let ref_file = git_dir.join(ref_path);
            if ref_file.exists() {
                println!("cargo:rerun-if-changed={}", ref_file.display());
            }
        }
    }
}

/// Stamp git short SHA + build timestamp as compile-time env vars so the
/// firmware's announce frame can carry an unambiguous version string.
/// Falls back to "unknown" outside a git checkout.
///
/// Release mode (`R2_RELEASE=1` in the env): the build must be on a
/// clean checkout AND on an exact git tag. The fw_ver string is then
/// just the tag (e.g. `v0.2.0`) — no timestamp, no SHA — so it matches
/// the GitHub Release tag for "needs update?" comparison in the
/// dashboard. Refusing dirty / non-tagged release builds keeps the
/// archive honest.
fn stamp_build_metadata() {
    let dirty = std::process::Command::new("git")
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);

    let release_mode = env::var("R2_RELEASE").ok().as_deref() == Some("1");

    if release_mode {
        if dirty {
            panic!(
                "R2_RELEASE=1 but working tree is dirty — refusing to bake an unverifiable release version. \
                 Commit your changes or unset R2_RELEASE."
            );
        }
        let tag = std::process::Command::new("git")
            .args(["describe", "--tags", "--exact-match"])
            .output()
            .ok()
            .and_then(|o| if o.status.success() { Some(o.stdout) } else { None })
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim().to_owned());
        let tag = match tag {
            Some(t) if !t.is_empty() => t,
            _ => panic!(
                "R2_RELEASE=1 but HEAD is not on a tag — refusing to bake a release version. \
                 `git tag vX.Y.Z` first."
            ),
        };
        println!("cargo:rustc-env=R2_GIT_SHA={tag}");
        println!("cargo:rustc-env=R2_BUILD_TIMESTAMP=");
        println!("cargo:rustc-env=R2_FW_VER={tag}");
        return;
    }

    // Dev-mode: short-sha + dirty marker + per-build UTC timestamp.
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() { Some(o.stdout) } else { None })
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    let sha_full = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=R2_GIT_SHA={sha_full}");

    let ts = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%d-%H:%M"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=R2_BUILD_TIMESTAMP={ts}");

    // Compose the full dev-mode fw_ver here so the sender can emit it
    // verbatim. Format: `<semver>-<UTC-yyyy-mm-dd-HH:MM>+<sha>[-dirty]`.
    let semver = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=R2_FW_VER={semver}-{ts}+{sha_full}");
}

/// ESP-IDF resolves `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME` relative to
/// esp-idf-sys's auto-generated build directory. Workaround: copy our
/// partitions.csv there so the relative path resolves. Same trick as
/// `r2-core/platforms/esp32-s3/build.rs`. On a fresh checkout the FIRST
/// build still gets the default table — run `tools/setup-firmware.sh`
/// to pre-stage, or rebuild a second time.
fn stage_partitions_csv(manifest_dir: &str) {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let src = PathBuf::from(manifest_dir).join("partitions.csv");
    if !src.exists() {
        return;
    }

    let _ = fs::copy(&src, Path::new(&out_dir).join("partitions.csv"));

    if let Some(build_dir) = Path::new(&out_dir).parent().and_then(Path::parent) {
        if let Ok(entries) = fs::read_dir(build_dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("esp-idf-sys-") {
                    let espidf_out = entry.path().join("out");
                    if espidf_out.is_dir() {
                        let _ = fs::copy(&src, espidf_out.join("partitions.csv"));
                    }
                }
            }
        }
    }
}

/// Load WiFi credentials from wifi_config.toml (gitignored), falling back
/// to env vars. Output is `cargo:rustc-env` so main.rs can `env!()` them
/// at compile time. Empty strings are emitted if neither source is set —
/// the firmware will warn on boot.
///
/// Adapted from `r2-core/platforms/esp32-s3/build.rs`.
fn load_wifi_config(manifest_dir: &str) {
    let config_path = format!("{manifest_dir}/wifi_config.toml");

    let mut ssid = env::var("R2_WIFI_SSID").unwrap_or_default();
    let mut pass = env::var("R2_WIFI_PASS").unwrap_or_default();
    let mut gw   = env::var("R2_GATEWAY_IP").unwrap_or_default();

    if Path::new(&config_path).exists() {
        if let Ok(content) = fs::read_to_string(&config_path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"');
                    match key {
                        "ssid"       => ssid = value.to_string(),
                        "password"   => pass = value.to_string(),
                        "gateway_ip" => gw   = value.to_string(),
                        _ => {}
                    }
                }
            }
        }
    }

    println!("cargo:rustc-env=R2_WIFI_SSID={ssid}");
    println!("cargo:rustc-env=R2_WIFI_PASS={pass}");
    println!("cargo:rustc-env=R2_GATEWAY_IP={gw}");

    if ssid.is_empty() {
        println!("cargo:warning=WiFi not configured — copy wifi_config.toml.example to wifi_config.toml");
    } else {
        println!(
            "cargo:warning=WiFi: SSID=\"{ssid}\" gateway={gw} (password {})",
            if pass.is_empty() { "not set" } else { "set" }
        );
    }
}
