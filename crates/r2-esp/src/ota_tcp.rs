//! R2 OTA Receive — WiFi TCP transport for ESP32-S3 (DFR1195)
//!
//! Same binary protocol as Linux (r2-core/platforms/linux/src/ota_tcp_recv.rs):
//!   - Port 21043
//!   - CMD_START (0x01): 37-byte preamble (cmd + size_u32_le + sha256_32), then firmware stream
//!   - CMD_QUERY (0x02): respond with JSON build info
//!   - Response: status(1) + len(2 LE) + message
//!
//! Uses std::net::TcpListener (ESP-IDF provides POSIX sockets via lwIP).
//! Uses ESP-IDF's esp_ota_ops for dual-partition management.

use esp_idf_svc::sys;
use log::{error, info, warn};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// TCP port for OTA firmware transfer and device query.
/// 0x5233 — R2 ASCII port range + 1 (OTA)
const OTA_PORT: u16 = 21043;

// Protocol commands (must match Linux ota_tcp_push.rs)
const CMD_START: u8 = 0x01;
const CMD_QUERY: u8 = 0x02;

// Response status codes
const STATUS_OK: u8 = 0x00;
const STATUS_ERROR: u8 = 0x01;

/// Start the TCP OTA listener in a background thread.
///
/// Listens on 0.0.0.0:21043 for incoming connections.
/// Each connection is handled synchronously (one at a time — fine for OTA).
pub fn start_listener() {
    std::thread::Builder::new()
        .name("ota-tcp".into())
        .stack_size(16384) // 16KB — needs headroom for TCP + SHA-256 + 4KB read buffer
        .spawn(move || {
            listener_loop();
        })
        .expect("[OTA-TCP] Failed to spawn listener thread");
}

fn listener_loop() {
    let listener = match TcpListener::bind(("0.0.0.0", OTA_PORT)) {
        Ok(l) => l,
        Err(e) => {
            error!("[OTA-TCP] Failed to bind port {}: {}", OTA_PORT, e);
            return;
        }
    };

    info!("[OTA-TCP] Listening on port {}", OTA_PORT);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                info!("[OTA-TCP] Connection from {}", peer);
                handle_connection(stream);
            }
            Err(e) => {
                error!("[OTA-TCP] Accept failed: {}", e);
            }
        }
    }
}

fn handle_connection(mut stream: TcpStream) {
    // Read first byte — command
    let mut cmd = [0u8; 1];
    if let Err(e) = stream.read_exact(&mut cmd) {
        warn!("[OTA-TCP] Failed to read command byte: {}", e);
        return;
    }

    match cmd[0] {
        CMD_START => handle_start(&mut stream),
        CMD_QUERY => handle_query(&mut stream),
        _ => {
            warn!("[OTA-TCP] Unknown command: 0x{:02x}", cmd[0]);
            send_response(&mut stream, STATUS_ERROR, "unknown command");
        }
    }
}

// ---------------------------------------------------------------------------
// CMD_START — firmware OTA
// ---------------------------------------------------------------------------

fn handle_start(stream: &mut TcpStream) {
    // Read preamble: size(4 LE) + sha256(32) = 36 bytes
    let mut preamble = [0u8; 36];
    if let Err(e) = stream.read_exact(&mut preamble) {
        error!("[OTA-TCP] Failed to read START preamble: {}", e);
        send_response(stream, STATUS_ERROR, "preamble read failed");
        return;
    }

    let expected_size = u32::from_le_bytes([preamble[0], preamble[1], preamble[2], preamble[3]]);
    let mut expected_sha256 = [0u8; 32];
    expected_sha256.copy_from_slice(&preamble[4..36]);

    info!(
        "[OTA-TCP] START: {} bytes, sha256={}",
        expected_size,
        hex_short(&expected_sha256)
    );

    // Find update partition
    let update_part = unsafe { sys::esp_ota_get_next_update_partition(std::ptr::null()) };
    if update_part.is_null() {
        error!("[OTA-TCP] No OTA update partition available");
        send_response(stream, STATUS_ERROR, "no OTA partition");
        return;
    }

    // Begin OTA
    let mut ota_handle: sys::esp_ota_handle_t = 0;
    let ret = unsafe { sys::esp_ota_begin(update_part, expected_size as usize, &mut ota_handle) };
    if ret != sys::ESP_OK {
        error!("[OTA-TCP] esp_ota_begin failed: {}", ret);
        send_response(stream, STATUS_ERROR, &format!("ota_begin failed: {}", ret));
        return;
    }

    // Stream firmware data
    let mut hasher = Sha256::new();
    let mut received: u32 = 0;
    let mut buf = [0u8; 4096]; // 4KB chunks — good balance for ESP32 memory

    loop {
        let remaining = expected_size - received;
        if remaining == 0 {
            break;
        }

        let to_read = std::cmp::min(remaining as usize, buf.len());
        match stream.read(&mut buf[..to_read]) {
            Ok(0) => {
                // Connection closed
                if received < expected_size {
                    error!(
                        "[OTA-TCP] Connection closed early: {}/{} bytes",
                        received, expected_size
                    );
                    unsafe { sys::esp_ota_abort(ota_handle); }
                    send_response(stream, STATUS_ERROR, "connection closed early");
                    return;
                }
                break;
            }
            Ok(n) => {
                // Write to flash
                let ret = unsafe {
                    sys::esp_ota_write(ota_handle, buf.as_ptr() as *const _, n)
                };
                if ret != sys::ESP_OK {
                    error!("[OTA-TCP] esp_ota_write failed: {}", ret);
                    unsafe { sys::esp_ota_abort(ota_handle); }
                    send_response(stream, STATUS_ERROR, &format!("ota_write failed: {}", ret));
                    return;
                }

                hasher.update(&buf[..n]);
                received += n as u32;

                // Progress every 64KB
                if received % 65536 < n as u32 && expected_size > 0 {
                    info!(
                        "[OTA-TCP] {:.0}% ({}/{} bytes)",
                        (received as f32 / expected_size as f32) * 100.0,
                        received,
                        expected_size
                    );
                }
            }
            Err(e) => {
                error!("[OTA-TCP] Read error: {}", e);
                unsafe { sys::esp_ota_abort(ota_handle); }
                send_response(stream, STATUS_ERROR, &format!("read error: {}", e));
                return;
            }
        }
    }

    info!("[OTA-TCP] Transfer complete: {} bytes", received);

    // Verify SHA-256
    let actual_sha256 = hasher.finalize();
    if actual_sha256.as_slice() != expected_sha256 {
        error!(
            "[OTA-TCP] SHA-256 mismatch: expected {}, got {}",
            hex_short(&expected_sha256),
            hex_short(actual_sha256.as_slice())
        );
        unsafe { sys::esp_ota_abort(ota_handle); }
        send_response(stream, STATUS_ERROR, "SHA-256 mismatch");
        return;
    }
    info!("[OTA-TCP] SHA-256 verified ✓");

    // Finalise OTA (validates ESP image header)
    let ret = unsafe { sys::esp_ota_end(ota_handle) };
    if ret != sys::ESP_OK {
        error!("[OTA-TCP] esp_ota_end failed: {}", ret);
        send_response(stream, STATUS_ERROR, &format!("ota_end failed: {}", ret));
        return;
    }

    // Set boot partition
    let ret = unsafe { sys::esp_ota_set_boot_partition(update_part) };
    if ret != sys::ESP_OK {
        error!("[OTA-TCP] esp_ota_set_boot_partition failed: {}", ret);
        send_response(stream, STATUS_ERROR, &format!("set_boot failed: {}", ret));
        return;
    }

    info!("[OTA-TCP] ✅ Firmware validated and staged ({} bytes)", received);
    send_response(stream, STATUS_OK, "OK");

    // Reboot after response is sent
    info!("[OTA-TCP] Rebooting in 2s...");
    std::thread::sleep(std::time::Duration::from_secs(2));
    info!("[OTA-TCP] Rebooting now!");
    unsafe { sys::esp_restart(); }
}

// ---------------------------------------------------------------------------
// CMD_QUERY — device build info
// ---------------------------------------------------------------------------

fn handle_query(stream: &mut TcpStream) {
    let version = env!("CARGO_PKG_VERSION");
    let timestamp = option_env!("R2_BUILD_TIMESTAMP").unwrap_or("unknown");
    let arch = "xtensa";

    // Compute SHA-256 of running firmware (from partition)
    // For now, report build-time info. Runtime SHA would require reading
    // the entire partition, which is slow. The build metadata is sufficient
    // for the management tool to verify what's running.
    let response = format!(
        r#"{{"version":"{}","built":"{}","arch":"{}","platform":"esp32s3","sha256":"build-time-only"}}"#,
        version, timestamp, arch
    );

    info!("[OTA-TCP] QUERY → {}", response);
    send_response(stream, STATUS_OK, &response);
}

// ---------------------------------------------------------------------------
// Protocol helpers
// ---------------------------------------------------------------------------

/// Send response: status(1) + len(2 LE) + message
fn send_response(stream: &mut TcpStream, status: u8, message: &str) {
    let msg_bytes = message.as_bytes();
    let len = msg_bytes.len() as u16;
    let len_bytes = len.to_le_bytes();

    let mut response = Vec::with_capacity(3 + msg_bytes.len());
    response.push(status);
    response.extend_from_slice(&len_bytes);
    response.extend_from_slice(msg_bytes);

    if let Err(e) = stream.write_all(&response) {
        error!("[OTA-TCP] Failed to send response: {}", e);
    }
    let _ = stream.flush();
}

fn hex_short(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{:02x}", b)).collect()
}
