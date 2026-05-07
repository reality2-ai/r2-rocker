//! # L2CAP CoC (Connection-Oriented Channel) transport for R2 on ESP32
//!
//! ## Multi-channel support
//!
//! Supports up to `L2CAP_COC_MAX_NUM` simultaneous L2CAP CoC channels
//! (set via sdkconfig). Each channel tracks its peer address, connection
//! handle, and per-channel accumulation buffer for reassembling
//! length-prefixed frames across NimBLE callbacks.
//!
//! ## Wire format
//!
//! Per R2-BLE §6.4 (little-endian per BLE convention; differs from
//! R2-WIRE TCP framing which is big-endian):
//!
//! ```text
//! [len_lo, len_hi, R2-WIRE payload...]
//! ```
//!
//! ## Threading model
//!
//! NimBLE callbacks fire on its host task thread. We use Mutex-protected
//! statics for cross-thread access. The main loop drains received frames
//! periodically via `drain_received()`.

use esp_idf_svc::sys::*;

// NimBLE OS abstraction functions have different prefixes per target:
//   ESP32-S3 (Xtensa): os_mempool_init, os_mbuf_*, etc.
//   ESP32-C6 (RISC-V): r_os_mempool_init, r_os_mbuf_*, etc.
// We alias the RISC-V prefixed names to the unprefixed form so the
// rest of this module uses a single consistent set of names.
#[cfg(target_arch = "riscv32")]
use esp_idf_svc::sys::{
    r_os_mempool_init as os_mempool_init,
    r_os_mbuf_pool_init as os_mbuf_pool_init,
    r_os_mbuf_append as os_mbuf_append,
    r_os_mbuf_free_chain as os_mbuf_free_chain,
    r_os_mbuf_get_pkthdr as os_mbuf_get_pkthdr,
};
use ::log::{debug, error, info, warn};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const R2_PSM: u16 = 0x00D2;
const L2CAP_MTU: u16 = 512;
const MBUF_POOL_COUNT: u16 = 80;
const MBUF_BUF_SIZE: u16 = L2CAP_MTU;
const MAX_FRAME_SIZE: usize = 4096;
const RX_QUEUE_CAPACITY: usize = 16;

// ---------------------------------------------------------------------------
// Static memory for mbuf pool
// ---------------------------------------------------------------------------

static mut MBUF_MEM: [os_membuf_t; ((MBUF_BUF_SIZE as usize + 4)
    / core::mem::size_of::<os_membuf_t>()
    + 1)
    * MBUF_POOL_COUNT as usize] = [0; ((MBUF_BUF_SIZE as usize + 4)
    / core::mem::size_of::<os_membuf_t>()
    + 1)
    * MBUF_POOL_COUNT as usize];

static mut MEMPOOL: os_mempool = unsafe { core::mem::zeroed() };
static mut MBUF_POOL: os_mbuf_pool = unsafe { core::mem::zeroed() };

// ---------------------------------------------------------------------------
// Channel tracking
// ---------------------------------------------------------------------------

/// A single L2CAP CoC channel with per-peer state.
struct PeerChannel {
    /// Raw NimBLE channel pointer (valid between CONNECTED and DISCONNECTED)
    chan: *mut ble_l2cap_chan,
    /// GAP connection handle
    conn_handle: u16,
    /// Peer BLE address (6 bytes, from ble_gap_conn_find)
    peer_addr: [u8; 6],
    /// Per-channel accumulation buffer for length-prefixed frame reassembly
    accum: Vec<u8>,
    /// Last mbuf allocated for recv_ready — must be freed on disconnect if unused
    pending_rx_mbuf: *mut os_mbuf,
}

// Safety: PeerChannel contains a raw pointer to ble_l2cap_chan. This pointer
// is only dereferenced while NimBLE is running and the channel is valid.
// Access is guarded by the CHANNELS Mutex.
unsafe impl Send for PeerChannel {}
unsafe impl Sync for PeerChannel {}

/// All active L2CAP CoC channels. Protected by Mutex.
static CHANNELS: Mutex<Option<Vec<PeerChannel>>> = Mutex::new(None);

/// Received frame queue: (payload, peer_addr).
/// NimBLE callback pushes; main task drains.
static RX_QUEUE: Mutex<Option<Vec<(Vec<u8>, [u8; 6])>>> = Mutex::new(None);

static INIT_DONE: Mutex<bool> = Mutex::new(false);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialise the L2CAP CoC subsystem.
pub fn init() {
    let mut done = INIT_DONE.lock().unwrap();
    if *done {
        warn!("[L2CAP] Already initialised, skipping");
        return;
    }

    info!("[L2CAP] Initialising L2CAP CoC subsystem (PSM=0x{:04X}, MTU={})", R2_PSM, L2CAP_MTU);

    {
        let mut q = RX_QUEUE.lock().unwrap();
        *q = Some(Vec::with_capacity(RX_QUEUE_CAPACITY));
    }
    {
        let mut ch = CHANNELS.lock().unwrap();
        *ch = Some(Vec::with_capacity(4));
    }

    unsafe {
        let rc = os_mempool_init(
            &mut MEMPOOL as *mut _,
            MBUF_POOL_COUNT,
            MBUF_BUF_SIZE as u32,
            MBUF_MEM.as_mut_ptr() as *mut _,
            b"r2_l2cap_pool\0".as_ptr() as *const _,
        );
        assert!(rc == 0, "[L2CAP] os_mempool_init failed: {}", rc);

        let rc = os_mbuf_pool_init(
            &mut MBUF_POOL as *mut _,
            &mut MEMPOOL as *mut _,
            MBUF_BUF_SIZE,
            MBUF_POOL_COUNT,
        );
        assert!(rc == 0, "[L2CAP] os_mbuf_pool_init failed: {}", rc);
    }

    info!("[L2CAP] Memory pool ready ({} × {} bytes)", MBUF_POOL_COUNT, MBUF_BUF_SIZE);

    unsafe {
        let rc = ble_l2cap_create_server(R2_PSM, L2CAP_MTU, Some(l2cap_event_callback), core::ptr::null_mut());
        assert!(rc == 0, "[L2CAP] ble_l2cap_create_server failed: {}", rc);
    }

    info!("[L2CAP] Server listening on PSM 0x{:04X}", R2_PSM);
    *done = true;
}

/// Send a length-prefixed frame to a specific peer by BLE address.
///
/// `peer_addr` is the 6-byte BLE address. `payload` is the data to send
/// (will be wrapped with u16 BE length prefix).
pub fn send_to(peer_addr: &[u8; 6], payload: &[u8]) -> Result<(), i32> {
    let chan = {
        let guard = CHANNELS.lock().unwrap();
        match guard.as_ref() {
            Some(channels) => {
                match channels.iter().find(|c| c.peer_addr == *peer_addr) {
                    Some(pc) => pc.chan,
                    None => {
                        warn!("[L2CAP] No channel for peer {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                            peer_addr[0], peer_addr[1], peer_addr[2],
                            peer_addr[3], peer_addr[4], peer_addr[5]);
                        return Err(-1);
                    }
                }
            }
            None => {
                warn!("[L2CAP] Not initialised");
                return Err(-1);
            }
        }
    };

    send_on_chan(chan, payload)
}

/// Send a length-prefixed frame on the first available channel.
/// Backward compatible — use `send_to()` for targeted sends.
pub fn send(payload: &[u8]) -> Result<(), i32> {
    let chan = {
        let guard = CHANNELS.lock().unwrap();
        match guard.as_ref() {
            Some(channels) => {
                match channels.first() {
                    Some(pc) => pc.chan,
                    None => {
                        warn!("[L2CAP] No active channels");
                        return Err(-1);
                    }
                }
            }
            None => return Err(-1),
        }
    };

    send_on_chan(chan, payload)
}

/// Drain all received frames. Returns `Vec<(payload, peer_addr)>`.
pub fn drain_received() -> Vec<(Vec<u8>, [u8; 6])> {
    let mut guard = RX_QUEUE.lock().unwrap();
    match guard.as_mut() {
        Some(q) => {
            let mut drained = Vec::with_capacity(q.len());
            drained.append(q);
            drained
        }
        None => Vec::new(),
    }
}

/// Get list of connected peer addresses.
pub fn connected_peers() -> Vec<[u8; 6]> {
    let guard = CHANNELS.lock().unwrap();
    match guard.as_ref() {
        Some(channels) => channels.iter().map(|c| c.peer_addr).collect(),
        None => Vec::new(),
    }
}

/// Check whether any L2CAP CoC channel is connected.
pub fn is_connected() -> bool {
    let guard = CHANNELS.lock().unwrap();
    guard.as_ref().map_or(false, |ch| !ch.is_empty())
}

/// Disconnect all active channels.
pub fn disconnect_all() {
    let chans: Vec<*mut ble_l2cap_chan> = {
        let guard = CHANNELS.lock().unwrap();
        guard.as_ref().map_or(Vec::new(), |ch| ch.iter().map(|c| c.chan).collect())
    };
    for chan in chans {
        unsafe { ble_l2cap_disconnect(chan); }
    }
}

// ---------------------------------------------------------------------------
// Internal: send helper
// ---------------------------------------------------------------------------

fn send_on_chan(chan: *mut ble_l2cap_chan, payload: &[u8]) -> Result<(), i32> {
    if payload.len() > (u16::MAX as usize) {
        error!("[L2CAP] Payload too large: {} bytes", payload.len());
        return Err(-2);
    }

    let frame_len = payload.len() as u16;
    let len_bytes = frame_len.to_le_bytes();

    let mbuf = alloc_mbuf();
    if mbuf.is_null() {
        error!("[L2CAP] Failed to allocate mbuf for send");
        return Err(-3);
    }

    unsafe {
        let rc = os_mbuf_append(mbuf, len_bytes.as_ptr() as *const _, 2 as _);
        if rc != 0 {
            error!("[L2CAP] os_mbuf_append (length) failed: {}", rc);
            os_mbuf_free_chain(mbuf);
            return Err(rc);
        }

        let rc = os_mbuf_append(mbuf, payload.as_ptr() as *const _, payload.len() as _);
        if rc != 0 {
            error!("[L2CAP] os_mbuf_append (payload) failed: {}", rc);
            os_mbuf_free_chain(mbuf);
            return Err(rc);
        }

        let rc = ble_l2cap_send(chan, mbuf);
        if rc != 0 {
            if rc != BLE_HS_ESTALLED as i32 {
                os_mbuf_free_chain(mbuf);
            }
            if rc == BLE_HS_ESTALLED as i32 {
                debug!("[L2CAP] Send stalled (no credits)");
            } else {
                error!("[L2CAP] ble_l2cap_send failed: {}", rc);
            }
            return Err(rc);
        }
    }

    debug!("[L2CAP] Sent {} bytes (+ 2 byte header)", payload.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal: mbuf allocation
// ---------------------------------------------------------------------------

fn alloc_mbuf() -> *mut os_mbuf {
    unsafe { os_mbuf_get_pkthdr(&mut MBUF_POOL as *mut _, 0) }
}

// ---------------------------------------------------------------------------
// Internal: look up peer address from GAP connection handle
// ---------------------------------------------------------------------------

fn lookup_peer_addr(conn_handle: u16) -> Option<[u8; 6]> {
    let mut desc: ble_gap_conn_desc = unsafe { core::mem::zeroed() };
    let rc = unsafe { ble_gap_conn_find(conn_handle, &mut desc as *mut _) };
    if rc != 0 {
        warn!("[L2CAP] ble_gap_conn_find failed for handle {}: {}", conn_handle, rc);
        return None;
    }
    // NimBLE stores addresses LSB-first, but BLE convention displays MSB-first
    // (e.g., FC:B3:AA:27:B9:3C). Reverse to match scan/display order.
    let mut addr = [0u8; 6];
    for i in 0..6 {
        addr[i] = desc.peer_ota_addr.val[5 - i];
    }
    Some(addr)
}

// ---------------------------------------------------------------------------
// NimBLE L2CAP event callback
// ---------------------------------------------------------------------------

unsafe extern "C" fn l2cap_event_callback(
    event: *mut ble_l2cap_event,
    _arg: *mut core::ffi::c_void,
) -> i32 {
    if event.is_null() {
        error!("[L2CAP-CB] Null event pointer!");
        return 0;
    }

    let evt = &*event;

    match evt.type_ as u32 {
        BLE_L2CAP_EVENT_COC_CONNECTED => handle_connected(evt),
        BLE_L2CAP_EVENT_COC_DISCONNECTED => handle_disconnected(evt),
        BLE_L2CAP_EVENT_COC_ACCEPT => handle_accept(evt),
        BLE_L2CAP_EVENT_COC_DATA_RECEIVED => handle_data_received(evt),
        BLE_L2CAP_EVENT_COC_TX_UNSTALLED => {
            debug!("[L2CAP-CB] TX unstalled — credits available");
            0
        }
        other => {
            warn!("[L2CAP-CB] Unknown event type: {}", other);
            0
        }
    }
}

unsafe fn handle_accept(evt: &ble_l2cap_event) -> i32 {
    info!("[L2CAP-CB] Incoming CoC connection request (ACCEPT)");

    let conn_handle = evt.__bindgen_anon_1.accept.conn_handle;
    let peer_sdu_size = evt.__bindgen_anon_1.accept.peer_sdu_size;
    info!("[L2CAP-CB] conn_handle={}, peer_sdu_size={}", conn_handle, peer_sdu_size);

    let sdu_rx = alloc_mbuf();
    if sdu_rx.is_null() {
        error!("[L2CAP-CB] Failed to alloc mbuf for accept — rejecting");
        return BLE_HS_ENOMEM as i32;
    }

    let chan = evt.__bindgen_anon_1.accept.chan;
    if !chan.is_null() {
        let rc = ble_l2cap_recv_ready(chan, sdu_rx);
        if rc != 0 {
            error!("[L2CAP-CB] ble_l2cap_recv_ready on accept failed: {}", rc);
            os_mbuf_free_chain(sdu_rx);
            return rc;
        }
    }

    0
}

unsafe fn handle_connected(evt: &ble_l2cap_event) -> i32 {
    let status = evt.__bindgen_anon_1.connect.status;
    let chan = evt.__bindgen_anon_1.connect.chan;

    if status != 0 {
        warn!("[L2CAP-CB] CoC connection failed, status={}", status);
        return 0;
    }

    let conn_handle = ble_l2cap_get_conn_handle(chan);
    let peer_addr = lookup_peer_addr(conn_handle).unwrap_or([0; 6]);

    info!(
        "[L2CAP-CB] CoC channel CONNECTED (conn_handle={}, peer={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
        conn_handle,
        peer_addr[0], peer_addr[1], peer_addr[2],
        peer_addr[3], peer_addr[4], peer_addr[5]
    );

    let mut guard = CHANNELS.lock().unwrap();
    if let Some(channels) = guard.as_mut() {
        // Remove any stale entry for this conn_handle
        channels.retain(|c| c.conn_handle != conn_handle);
        channels.push(PeerChannel {
            chan,
            conn_handle,
            peer_addr,
            accum: Vec::with_capacity(256),
            pending_rx_mbuf: core::ptr::null_mut(),
        });
        info!("[L2CAP-CB] Active channels: {}", channels.len());
    }

    0
}

unsafe fn handle_disconnected(evt: &ble_l2cap_event) -> i32 {
    let chan = evt.__bindgen_anon_1.disconnect.chan;
    let conn_handle = ble_l2cap_get_conn_handle(chan);

    info!("[L2CAP-CB] CoC channel DISCONNECTED (conn_handle={})", conn_handle);

    let mut guard = CHANNELS.lock().unwrap();
    if let Some(channels) = guard.as_mut() {
        // Free any pending rx mbuf before removing the channel
        for pc in channels.iter() {
            if pc.conn_handle == conn_handle && !pc.pending_rx_mbuf.is_null() {
                debug!("[L2CAP-CB] Freeing pending rx mbuf for conn_handle={}", conn_handle);
                os_mbuf_free_chain(pc.pending_rx_mbuf);
            }
        }
        channels.retain(|c| c.conn_handle != conn_handle);
        info!("[L2CAP-CB] Active channels: {}", channels.len());
    }

    0
}

unsafe fn handle_data_received(evt: &ble_l2cap_event) -> i32 {
    let chan = evt.__bindgen_anon_1.receive.chan;
    let sdu_rx = evt.__bindgen_anon_1.receive.sdu_rx;

    if sdu_rx.is_null() {
        error!("[L2CAP-CB] DATA_RECEIVED with null sdu_rx");
        replenish_credit(chan);
        return 0;
    }

    let om_len = (*sdu_rx).om_len;
    let om_data = (*sdu_rx).om_data;

    if om_data.is_null() || om_len == 0 {
        debug!("[L2CAP-CB] Empty DATA_RECEIVED");
        replenish_credit(chan);
        return 0;
    }

    let chunk = core::slice::from_raw_parts(om_data, om_len as usize);
    let conn_handle = ble_l2cap_get_conn_handle(chan);

    // Find the channel's peer_addr and accumulate data
    let mut guard = CHANNELS.lock().unwrap();
    if let Some(channels) = guard.as_mut() {
        if let Some(pc) = channels.iter_mut().find(|c| c.conn_handle == conn_handle) {
            let peer_addr = pc.peer_addr;
            pc.accum.extend_from_slice(chunk);

            // Extract complete frames
            while pc.accum.len() >= 2 {
                let frame_len = u16::from_le_bytes([pc.accum[0], pc.accum[1]]) as usize;

                if frame_len == 0 || frame_len > MAX_FRAME_SIZE {
                    warn!("[L2CAP-CB] Invalid frame length {}, flushing", frame_len);
                    pc.accum.clear();
                    break;
                }

                if pc.accum.len() < 2 + frame_len {
                    debug!("[L2CAP-CB] Accumulating: have {}/{} bytes", pc.accum.len() - 2, frame_len);
                    break;
                }

                let payload = pc.accum[2..2 + frame_len].to_vec();
                pc.accum.drain(..2 + frame_len);

                info!("[L2CAP-CB] Received R2-WIRE frame ({} bytes) from {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    payload.len(),
                    peer_addr[0], peer_addr[1], peer_addr[2],
                    peer_addr[3], peer_addr[4], peer_addr[5]);

                // Push to receive queue with peer address
                {
                    let mut q_guard = RX_QUEUE.lock().unwrap();
                    if let Some(q) = q_guard.as_mut() {
                        if q.len() >= RX_QUEUE_CAPACITY {
                            warn!("[L2CAP-CB] RX queue full, dropping oldest");
                            q.remove(0);
                        }
                        q.push((payload, peer_addr));
                    }
                }
            }
        } else {
            warn!("[L2CAP-CB] DATA_RECEIVED for unknown conn_handle={}", conn_handle);
        }
    }

    drop(guard);

    // Free the consumed sdu_rx mbuf — NimBLE gave it to us, we must return it.
    // Without this, every received frame leaks one mbuf from the pool.
    os_mbuf_free_chain(sdu_rx);

    replenish_credit(chan);
    0
}

unsafe fn replenish_credit(chan: *mut ble_l2cap_chan) {
    let sdu_rx = alloc_mbuf();
    if sdu_rx.is_null() {
        error!("[L2CAP] Failed to alloc mbuf for recv_ready — channel will stall!");
        return;
    }

    // Track this mbuf so we can free it on disconnect
    let conn_handle = ble_l2cap_get_conn_handle(chan);
    {
        let mut guard = CHANNELS.lock().unwrap();
        if let Some(channels) = guard.as_mut() {
            if let Some(pc) = channels.iter_mut().find(|c| c.conn_handle == conn_handle) {
                pc.pending_rx_mbuf = sdu_rx;
            }
        }
    }

    let rc = ble_l2cap_recv_ready(chan, sdu_rx);
    if rc != 0 {
        error!("[L2CAP] ble_l2cap_recv_ready failed: {}", rc);
        os_mbuf_free_chain(sdu_rx);
        // Clear the tracked pointer since we freed it
        let mut guard = CHANNELS.lock().unwrap();
        if let Some(channels) = guard.as_mut() {
            if let Some(pc) = channels.iter_mut().find(|c| c.conn_handle == conn_handle) {
                pc.pending_rx_mbuf = core::ptr::null_mut();
            }
        }
    }
}
