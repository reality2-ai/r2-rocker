# firmware/esp32-s3/releases/

Versioned archive of every firmware `.bin` we want to keep around for
posterity — rollback fodder, audit trail, and a reverse lookup from
the version string a sensor reports in `r2.sensor.announce` back to
the exact bytes it's running.

## Filename format

```
r2-rocker-firmware-<semver>-<YYYY-MM-DD-HH:MM>-<tag>+<git-sha>[-dirty].bin
```

Matches the `fw_ver` string the firmware bakes into its
`r2.sensor.announce` payload (see `firmware/esp32-s3/src/sender.rs`).
A sensor's reported version is therefore searchable directly against
this directory:

```
$ ls releases/r2-rocker-firmware-0.1.0-2026-05-08-14:55-sim+abc12345.bin
```

The `-dirty` suffix means the working tree had uncommitted changes at
build time — useful for diagnostic builds, but should not be the
version of record for a deployment.

## Workflow

1. `tools/build-firmware.sh` compiles + packages, copying the build
   artifact here automatically.
2. Operator decides which builds are release-of-record by `git add`-
   ing them. Diagnostic / dirty builds typically aren't committed.
3. Push to a sensor via the dashboard's `/api/ota/{addr}` endpoint
   (Phase 9-light) using the bin file at
   `target/xtensa-esp32s3-espidf/release/r2-rocker-firmware.bin` —
   that path is overwritten on every build, but the same content
   exists here as a stable archive copy.
4. Once Phase 9-fwreg lands (PLAN row 9-fwreg), the controller will
   compare each sensor's announced `fw_ver` against a designated
   "current target" version (probably one of the files in this
   directory) and surface mismatches as an out-of-date badge.

## Size note

Each .bin is ~1.3 MB. At one release per day for a year that's about
0.5 GB. Manageable for now; if it ever bloats, move to git LFS rather
than abandoning the archive.
