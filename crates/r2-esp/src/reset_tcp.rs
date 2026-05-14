//! R2 Remote-Reset — WiFi TCP transport for ESP32-S3
//!
//! Per SPEC-R2-ROCKER-SENSOR-REMOTE-RESET:
//!   - Port 21044 (one above OTA's 21043)
//!   - CMD_RESET (0x10): no payload; sensor responds with status,
//!     sleeps 100 ms, then calls esp_restart()
//!   - Response shape mirrors ota_tcp: status(1) + len_le(2) + utf-8 message
//!
//! Mirrors `ota_tcp.rs` in structure. Sequential one-at-a-time accept
//! loop; each connection handled synchronously on the listener thread.

use esp_idf_svc::sys;
use log::{error, info, warn};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use crate::ota_tcp::ota_in_progress;

/// TCP port for remote-reset commands.
const RESET_PORT: u16 = 21044;

/// Protocol commands.
const CMD_RESET: u8 = 0x10;

/// Response status codes.
const STATUS_OK: u8 = 0x00;
const STATUS_ERROR: u8 = 0x01;

/// Start the reset listener in a background thread.
///
/// Listens on 0.0.0.0:21044. One connection at a time.
pub fn start_listener() {
    std::thread::Builder::new()
        .name("reset-tcp".into())
        .stack_size(4096) // tiny: no buffers, no crypto — just a command byte + a response
        .spawn(move || {
            listener_loop();
        })
        .expect("[RESET-TCP] failed to spawn listener thread");
}

fn listener_loop() {
    let listener = match TcpListener::bind(("0.0.0.0", RESET_PORT)) {
        Ok(l) => l,
        Err(e) => {
            error!("[RESET-TCP] failed to bind port {}: {}", RESET_PORT, e);
            return;
        }
    };

    info!("[RESET-TCP] listening on port {}", RESET_PORT);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                info!("[RESET-TCP] connection from {}", peer);
                handle_connection(stream);
            }
            Err(e) => {
                error!("[RESET-TCP] accept failed: {}", e);
            }
        }
    }
}

fn handle_connection(mut stream: TcpStream) {
    // Bounded read: don't block forever on a half-open socket.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    let mut cmd = [0u8; 1];
    if let Err(e) = stream.read_exact(&mut cmd) {
        warn!("[RESET-TCP] failed to read command byte: {}", e);
        return;
    }

    match cmd[0] {
        CMD_RESET => handle_reset(&mut stream),
        _ => {
            warn!("[RESET-TCP] unknown command: 0x{:02x}", cmd[0]);
            send_response(&mut stream, STATUS_ERROR, "unknown command");
        }
    }
}

fn handle_reset(stream: &mut TcpStream) {
    // Refuse while an OTA is in flight — rebooting in the middle of
    // ota_write would leave the inactive partition half-written. The
    // bootloader rollback would recover, but it's cleaner to just say no.
    if ota_in_progress() {
        warn!("[RESET-TCP] reset refused — OTA in progress");
        send_response(stream, STATUS_ERROR, "OTA in progress");
        return;
    }

    info!("[RESET-TCP] CMD_RESET accepted — rebooting in 100 ms");
    send_response(stream, STATUS_OK, "reboot scheduled");
    // Give the OK response time to clear the socket before esp_restart
    // forcibly closes everything.
    let _ = stream.flush();
    thread::sleep(Duration::from_millis(100));
    unsafe {
        sys::esp_restart();
    }
}

fn send_response(stream: &mut TcpStream, status: u8, message: &str) {
    let msg_bytes = message.as_bytes();
    let len = msg_bytes.len() as u16;
    let len_bytes = len.to_le_bytes();

    let mut response = Vec::with_capacity(3 + msg_bytes.len());
    response.push(status);
    response.extend_from_slice(&len_bytes);
    response.extend_from_slice(msg_bytes);

    if let Err(e) = stream.write_all(&response) {
        error!("[RESET-TCP] failed to send response: {}", e);
    }
}
