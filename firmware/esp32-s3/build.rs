use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=partitions.csv");
    println!("cargo:rerun-if-changed=sdkconfig.defaults");
    println!("cargo:rerun-if-changed=build.rs");

    // ESP-IDF's CMake resolves `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME`
    // relative to its own auto-generated build directory, not our crate
    // root. Workaround: physically copy partitions.csv into esp-idf-sys's
    // OUT directory each time our build.rs runs, so a relative path of
    // "partitions.csv" in sdkconfig.defaults resolves correctly.
    //
    // Same trick as r2-core/platforms/esp32-s3/build.rs (the source of
    // this idea). Note: the FIRST build of a fresh checkout still uses
    // the ESP-IDF default partition table, because esp-idf-sys's CMake
    // configure runs BEFORE our build.rs places the file. The second
    // build picks up our custom table. To avoid that, run
    // `tools/setup-firmware.sh` once after a fresh clone — it pre-stages
    // partitions.csv before the first cargo invocation.
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let src = PathBuf::from(&manifest_dir).join("partitions.csv");

    if src.exists() {
        // Copy to our own OUT_DIR (belt).
        let _ = fs::copy(&src, Path::new(&out_dir).join("partitions.csv"));

        // Walk up two parents (out_dir → build/<our-pkg-hash> → build/)
        // and locate any esp-idf-sys-*/out directory.
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

    embuild::espidf::sysenv::output();
}
