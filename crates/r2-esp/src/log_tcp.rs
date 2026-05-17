//! R2 dev log server — telnet-style TCP fan-out of `log!()` output.
//!
//! Wraps `esp_idf_svc::log::EspLogger` so:
//!   * UART / USB-Serial-JTAG console output is preserved (the inner
//!     EspLogger is called for every record).
//!   * Each formatted log line is also broadcast to every connected
//!     TCP client on port 21046.
//!
//! Connect from a workstation on the hotspot:
//!     `nc <sensor-ip> 21046`  →  live tail
//!
//! Used by the dashboard's per-sensor "Logs" panel via a WS proxy.
//! Per-client queue is bounded; the log path can never block on a
//! slow reader (overflowing lines are dropped silently).
//!
//! Port choice: 21045 was the original allocation but it collides
//! with the canonical R2 Console / GraphQL port (R2-TRANSPORT §5
//! port table, R2-CONSOLE §3.2). 21046 is the first port above
//! the canonical R2 21042..21045 block and remains within the
//! rocker's contiguous allocation. Per
//! `audits/2026-05-18-post-v0.1.0-conformance.md` Finding F.

use esp_idf_svc::log::EspLogger;
use log::{Log, Metadata, Record};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Mutex;

const LOG_PORT: u16 = 21046;
/// Per-subscriber bounded queue. Lines beyond this drop silently — the
/// log path must never backpressure the writer (which could deadlock
/// the system if a slow client is sluggish).
const SUBSCRIBER_QUEUE: usize = 128;

static SUBSCRIBERS: Mutex<Vec<SyncSender<String>>> = Mutex::new(Vec::new());

struct CapturingLogger {
    inner: EspLogger,
}

impl Log for CapturingLogger {
    fn enabled(&self, m: &Metadata) -> bool {
        self.inner.enabled(m)
    }

    fn log(&self, record: &Record) {
        // UART path — unchanged.
        self.inner.log(record);

        // Capture path. Single line, terminated with \n so `nc` /
        // browser `\n`-splitting display it cleanly.
        let line = format!(
            "{:>5} {} {}\n",
            record.level(),
            record.target(),
            record.args(),
        );

        let mut subs = match SUBSCRIBERS.lock() {
            Ok(g) => g,
            Err(_) => return, // poisoned — give up; better than panic
        };
        subs.retain(|tx| match tx.try_send(line.clone()) {
            Ok(_) => true,
            // Full: keep the subscriber, drop the line. Slow client
            // won't take down the log path.
            Err(TrySendError::Full(_)) => true,
            // Disconnected: receiver was dropped (client closed).
            // Remove the dead sender so future log() calls don't hit
            // it.
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

static LOGGER: CapturingLogger = CapturingLogger {
    inner: EspLogger::new(),
};

static LISTENER_STARTED: AtomicBool = AtomicBool::new(false);

/// Install the capturing logger as the global `log` sink. Safe to call
/// early in `main()` — does not touch the network stack. Replaces a
/// call to `EspLogger::initialize_default()`; do not call both.
///
/// After WiFi / lwIP is up, call `start_listener()` to bring the TCP
/// fan-out online. Splitting the two avoids a deadlock observed when
/// `TcpListener::bind` was called before lwIP was initialised — the
/// listener thread would never return from `bind` and no `[log-tcp]`
/// activity ever appeared on UART.
pub fn install_logger() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// Spawn the TCP listener thread. Call once WiFi is up so the bind
/// against `0.0.0.0:21046` actually succeeds. Idempotent — subsequent
/// calls are no-ops.
pub fn start_listener() {
    if LISTENER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    log::info!("[log-tcp] spawning listener thread");
    if let Err(e) = std::thread::Builder::new()
        .name("log-tcp".into())
        .stack_size(8192)
        .spawn(listener_loop)
    {
        log::error!("[log-tcp] failed to spawn listener thread: {} — log fan-out disabled this boot", e);
        LISTENER_STARTED.store(false, Ordering::SeqCst);
    }
}

fn listener_loop() {
    // `start_listener` runs early in main() — before lwIP / WiFi are
    // up. `TcpListener::bind` will fail with ENETDOWN until the stack
    // is ready, so retry until it succeeds rather than dying silently.
    // 1 s × 60 retries covers normal boot + a worst-case WiFi
    // provisioning round trip; after that we log once and stop.
    let mut listener = None;
    for attempt in 1..=60 {
        match TcpListener::bind(("0.0.0.0", LOG_PORT)) {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(e) => {
                if attempt == 1 || attempt % 10 == 0 {
                    log::warn!(
                        "[log-tcp] bind attempt {} on port {} failed: {} — retrying",
                        attempt, LOG_PORT, e,
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
    let listener = match listener {
        Some(l) => l,
        None => {
            log::error!("[log-tcp] giving up after 60 s — log fan-out disabled this boot");
            return;
        }
    };
    log::info!("[log-tcp] listening on port {}", LOG_PORT);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => spawn_client(s),
            Err(e) => log::warn!("[log-tcp] accept failed: {}", e),
        }
    }
}

fn spawn_client(mut stream: TcpStream) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    log::info!("[log-tcp] client connected from {}", peer);

    std::thread::Builder::new()
        .name("log-tcp-cli".into())
        .stack_size(4096)
        .spawn(move || {
            let (tx, rx) = mpsc::sync_channel::<String>(SUBSCRIBER_QUEUE);
            {
                if let Ok(mut subs) = SUBSCRIBERS.lock() {
                    subs.push(tx);
                }
            }
            let _ = stream.write_all(b"-- r2-rocker log stream --\n");
            // Receive loop. Exits when the socket dies (write error) OR
            // when our tx is dropped during SUBSCRIBERS prune. After
            // exit, the tx implicitly drops; the log path then prunes
            // it next call.
            for line in rx.iter() {
                if stream.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
            log::info!("[log-tcp] client {} disconnected", peer);
        })
        .ok();
}
