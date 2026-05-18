//! R2 capture-file TCP server — per SPEC-R2-ROCKER-CAPTURE §6.
//!
//! Dedicated listener on port 21047 that lets the dashboard enumerate,
//! fetch, and delete the named-capture CSV files on this sensor's SD
//! card. Uses simple binary framing — no CBOR — so wire vectors are
//! easy to inspect with `xxd` / `nc` and the implementation stays
//! tight on a small heap.
//!
//! ```text
//! Request : [opcode u8][body…]
//! Response: [status u8][body…]
//! status  : 0x00 OK | 0x01 ERROR | 0x02 BUSY (capture is writing the file)
//! ```
//!
//! Opcodes
//! -------
//!
//! - `0x01 LIST`
//!   - request body: (none)
//!   - response (OK): `[u32 BE count]` then `count` × `[u16 BE
//!     name_len][name utf-8][u64 BE size][i64 BE mtime_ms]`
//!
//! - `0x02 GET`
//!   - request body: `[u16 BE name_len][name utf-8]`
//!   - response (OK): `[u64 BE size][size bytes]`
//!   - response (ERROR / BUSY): `[u16 BE msg_len][msg utf-8]`
//!
//! - `0x03 DEL`
//!   - request body: `[u16 BE name_len][name utf-8]`
//!   - response (OK): (empty)
//!   - response (ERROR / BUSY): `[u16 BE msg_len][msg utf-8]`
//!
//! - `0x04 DEL_ALL`
//!   - request body: (none)
//!   - response (OK): `[u32 BE deleted_count]`
//!
//! Listener accepts ONE client at a time — SD bandwidth stays
//! exclusive to one consumer. Subsequent connect attempts wait at
//! the accept queue.

use log::{error, info, warn};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const DATA_PORT: u16 = 21047;

const OP_LIST:    u8 = 0x01;
const OP_GET:     u8 = 0x02;
const OP_DEL:     u8 = 0x03;
const OP_DEL_ALL: u8 = 0x04;

const ST_OK:    u8 = 0x00;
const ST_ERROR: u8 = 0x01;
const ST_BUSY:  u8 = 0x02;

const CAPTURES_SUBDIR: &str = "captures";
const CAP_PREFIX:      &str = "cap-";

const NAME_MAX: usize = 64; // bigger than CAPTURE_NAME_MAX in capture.rs to allow the `<ts16>-<name>.csv` envelope

/// Snapshot of the capture sentant's currently-open file (if any).
/// Sender thread updates this; data_tcp consults it before honouring
/// GET / DEL on the same filename.
pub type CurrentRecording = Arc<Mutex<Option<String>>>;

/// Build a fresh shared handle.
pub fn new_current_recording() -> CurrentRecording {
    Arc::new(Mutex::new(None))
}

static LISTENER_STARTED: AtomicBool = AtomicBool::new(false);

/// Start the data_tcp listener thread. Idempotent. Spawn AFTER WiFi
/// is up — bind to `0.0.0.0:21047` blocks indefinitely before lwIP
/// is initialised on ESP-IDF (same `install_logger` / `start_listener`
/// split rationale as `log_tcp`).
pub fn start_listener(mount_point: &'static str, current: CurrentRecording) {
    if LISTENER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    log::info!("[data-tcp] spawning listener thread");
    if let Err(e) = std::thread::Builder::new()
        .name("data-tcp".into())
        // 16 KiB: FATFS LFN allocates ~512 B per directory entry on
        // the stack, and a LIST traverses every file in two
        // directories. 8 KiB overflowed and reset the socket
        // mid-LIST; 16 KiB matches `ota_tcp` and tested clean
        // across captures/ + the ring's logNNNN.csv files.
        .stack_size(16384)
        .spawn(move || listener_loop(mount_point, current))
    {
        error!("[data-tcp] failed to spawn listener: {} — capture files unreachable", e);
        LISTENER_STARTED.store(false, Ordering::SeqCst);
    }
}

fn listener_loop(mount_point: &str, current: CurrentRecording) {
    let listener = match TcpListener::bind(("0.0.0.0", DATA_PORT)) {
        Ok(l) => l,
        Err(e) => {
            error!("[data-tcp] bind failed on {}: {}", DATA_PORT, e);
            return;
        }
    };
    info!("[data-tcp] listening on port {}", DATA_PORT);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                info!("[data-tcp] client {}", peer);
                if let Err(e) = handle_connection(s, mount_point, &current) {
                    warn!("[data-tcp] {} handler error: {}", peer, e);
                }
            }
            Err(e) => error!("[data-tcp] accept failed: {}", e),
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    mount_point: &str,
    current: &CurrentRecording,
) -> std::io::Result<()> {
    let mut opbuf = [0u8; 1];
    stream.read_exact(&mut opbuf)?;
    let op = opbuf[0];
    match op {
        OP_LIST => handle_list(&mut stream, mount_point),
        OP_GET  => handle_get(&mut stream, mount_point, current),
        OP_DEL  => handle_del(&mut stream, mount_point, current),
        OP_DEL_ALL => handle_del_all(&mut stream, mount_point, current),
        other => {
            warn!("[data-tcp] unknown opcode 0x{:02X}", other);
            write_err(&mut stream, "unknown opcode")
        }
    }
}

// ── LIST ───────────────────────────────────────────────────────────────

fn handle_list(stream: &mut TcpStream, mount_point: &str) -> std::io::Result<()> {
    let entries = list_captures(mount_point);
    stream.write_all(&[ST_OK])?;
    stream.write_all(&(entries.len() as u32).to_be_bytes())?;
    for (name, size, mtime) in entries {
        let nb = name.as_bytes();
        stream.write_all(&(nb.len() as u16).to_be_bytes())?;
        stream.write_all(nb)?;
        stream.write_all(&size.to_be_bytes())?;
        stream.write_all(&mtime.to_be_bytes())?;
    }
    Ok(())
}

/// Enumerate every capture file under `<mount>/captures/` AND every
/// `<mount>/cap-*.csv` fallback file. Returns `(name, size, mtime_ms)`.
/// `name` is the basename only — clients pass it back verbatim on
/// GET / DEL and the server resolves it through `resolve_path`.
fn list_captures(mount_point: &str) -> Vec<(String, u64, i64)> {
    let mut out = Vec::new();

    let primary = std::path::PathBuf::from(mount_point).join(CAPTURES_SUBDIR);
    if let Ok(dir) = fs::read_dir(&primary) {
        for entry in dir.flatten() {
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else { continue };
            if !name.ends_with(".csv") { continue; }
            push_entry(&mut out, &entry, name);
        }
    }

    // Mount-root fallback: cap-<ts16>-<name>.csv per SPEC §4 last paragraph.
    if let Ok(dir) = fs::read_dir(mount_point) {
        for entry in dir.flatten() {
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else { continue };
            if !name.starts_with(CAP_PREFIX) || !name.ends_with(".csv") { continue; }
            push_entry(&mut out, &entry, name);
        }
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn push_entry(out: &mut Vec<(String, u64, i64)>, entry: &fs::DirEntry, name: String) {
    let Ok(meta) = entry.metadata() else { return };
    let size = meta.len();
    let mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    out.push((name, size, mtime));
}

// ── GET ────────────────────────────────────────────────────────────────

fn handle_get(
    stream: &mut TcpStream,
    mount_point: &str,
    current: &CurrentRecording,
) -> std::io::Result<()> {
    let name = match read_name(stream)? {
        Some(n) => n,
        None => return write_err(stream, "bad name length"),
    };

    if is_recording(current, &name) {
        return write_status_msg(stream, ST_BUSY, "file is currently being recorded");
    }

    let Some(path) = resolve_path(mount_point, &name) else {
        return write_err(stream, "not found");
    };

    let mut f = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => return write_err(stream, &format!("open: {}", e)),
    };
    let size = match f.metadata() { Ok(m) => m.len(), Err(_) => 0 };

    stream.write_all(&[ST_OK])?;
    stream.write_all(&size.to_be_bytes())?;

    // Stream the file body in chunks. 4 KB matches the ring's
    // segment-page granularity comfortably and keeps stack usage
    // bounded.
    let mut buf = [0u8; 4096];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        stream.write_all(&buf[..n])?;
    }
    Ok(())
}

// ── DEL ────────────────────────────────────────────────────────────────

fn handle_del(
    stream: &mut TcpStream,
    mount_point: &str,
    current: &CurrentRecording,
) -> std::io::Result<()> {
    let name = match read_name(stream)? {
        Some(n) => n,
        None => return write_err(stream, "bad name length"),
    };

    if is_recording(current, &name) {
        return write_status_msg(stream, ST_BUSY, "file is currently being recorded");
    }

    let Some(path) = resolve_path(mount_point, &name) else {
        return write_err(stream, "not found");
    };
    match fs::remove_file(&path) {
        Ok(()) => {
            info!("[data-tcp] del {:?}", path);
            stream.write_all(&[ST_OK])
        }
        Err(e) => write_err(stream, &format!("unlink: {}", e)),
    }
}

// ── DEL_ALL ────────────────────────────────────────────────────────────

fn handle_del_all(
    stream: &mut TcpStream,
    mount_point: &str,
    current: &CurrentRecording,
) -> std::io::Result<()> {
    let recording = current.lock().ok().and_then(|g| g.clone());
    let mut deleted: u32 = 0;
    for (name, _, _) in list_captures(mount_point) {
        if recording.as_deref() == Some(name.as_str()) { continue; }
        if let Some(path) = resolve_path(mount_point, &name) {
            match fs::remove_file(&path) {
                Ok(()) => { deleted = deleted.saturating_add(1); }
                Err(e) => warn!("[data-tcp] del_all {:?} failed: {}", path, e),
            }
        }
    }
    info!("[data-tcp] del_all — removed {} capture(s)", deleted);
    stream.write_all(&[ST_OK])?;
    stream.write_all(&deleted.to_be_bytes())?;
    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────

fn read_name(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut nl = [0u8; 2];
    stream.read_exact(&mut nl)?;
    let len = u16::from_be_bytes(nl) as usize;
    if len == 0 || len > NAME_MAX { return Ok(None); }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let Ok(s) = String::from_utf8(buf) else { return Ok(None); };
    if !is_valid_basename(&s) { return Ok(None); }
    Ok(Some(s))
}

/// Reject anything that isn't a plain `<allowed-chars>.csv` filename
/// at the mount root or one directory level deep — guards against
/// path traversal (`..`, leading `/`, ...). The match is deliberately
/// strict; the client only ever needs the basename `list_captures`
/// returned.
fn is_valid_basename(name: &str) -> bool {
    if name.is_empty() || name.len() > NAME_MAX { return false; }
    if !name.ends_with(".csv") { return false; }
    name.bytes().all(|b| matches!(b,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'
    ))
}

/// Look up `<mount>/captures/<name>` first, then `<mount>/<name>` for
/// the `cap-` fallback layout. Returns `None` if neither exists.
fn resolve_path(mount_point: &str, name: &str) -> Option<std::path::PathBuf> {
    let primary = std::path::PathBuf::from(mount_point).join(CAPTURES_SUBDIR).join(name);
    if primary.is_file() { return Some(primary); }
    if name.starts_with(CAP_PREFIX) {
        let fallback = std::path::PathBuf::from(mount_point).join(name);
        if fallback.is_file() { return Some(fallback); }
    }
    None
}

fn is_recording(current: &CurrentRecording, name: &str) -> bool {
    current.lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|s| s == name)
        .unwrap_or(false)
}

fn write_status_msg(stream: &mut TcpStream, status: u8, msg: &str) -> std::io::Result<()> {
    let mb = msg.as_bytes();
    stream.write_all(&[status])?;
    stream.write_all(&(mb.len() as u16).to_be_bytes())?;
    stream.write_all(mb)?;
    Ok(())
}

fn write_err(stream: &mut TcpStream, msg: &str) -> std::io::Result<()> {
    write_status_msg(stream, ST_ERROR, msg)
}
