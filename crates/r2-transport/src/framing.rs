//! Transport-agnostic framing helpers.
//!
//! Provides length-prefix encoding/decoding for stream transports (TCP,
//! BLE L2CAP stream mode) as specified in R2-WIRE §13.4.

/// Framing errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Buffer too small to hold the length prefix.
    BufferTooSmall,
    /// Incomplete length prefix (need more bytes).
    Incomplete,
    /// Frame payload exceeds maximum allowed size.
    PayloadTooLarge,
    /// Declared length does not match available data.
    LengthMismatch,
}

/// Maximum R2-WIRE frame size (64 KiB, matching extended format limits).
pub const MAX_FRAME_SIZE: usize = 65535;

// -------------------------------------------------------------------------
// TCP / big-endian length prefix (R2-WIRE §13.4)
// -------------------------------------------------------------------------

/// Write a 4-byte big-endian length prefix into `buf`.
///
/// Returns the number of bytes written (always 4), or an error if the
/// buffer is too small.
///
/// ```
/// # use r2_transport::framing::write_length_prefix;
/// let mut buf = [0u8; 4];
/// assert_eq!(write_length_prefix(&mut buf, 42), Ok(4));
/// assert_eq!(buf, [0, 0, 0, 42]);
/// ```
pub fn write_length_prefix(buf: &mut [u8], payload_len: u32) -> Result<usize, FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::BufferTooSmall);
    }
    buf[0] = (payload_len >> 24) as u8;
    buf[1] = (payload_len >> 16) as u8;
    buf[2] = (payload_len >> 8) as u8;
    buf[3] = payload_len as u8;
    Ok(4)
}

/// Read a 4-byte big-endian length prefix from `buf`.
///
/// Returns the payload length, or an error if the buffer is too short.
///
/// ```
/// # use r2_transport::framing::read_length_prefix;
/// let buf = [0u8, 0, 0, 42];
/// assert_eq!(read_length_prefix(&buf), Ok(42));
/// ```
pub fn read_length_prefix(buf: &[u8]) -> Result<u32, FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::Incomplete);
    }
    let len =
        (buf[0] as u32) << 24 | (buf[1] as u32) << 16 | (buf[2] as u32) << 8 | (buf[3] as u32);
    if len as usize > MAX_FRAME_SIZE {
        return Err(FrameError::PayloadTooLarge);
    }
    Ok(len)
}

// -------------------------------------------------------------------------
// BLE L2CAP stream mode / little-endian length prefix (R2-BLE §6.4)
// -------------------------------------------------------------------------

/// Write a 2-byte little-endian length prefix (BLE L2CAP stream mode).
///
/// Per R2-BLE §6.4, BLE stream mode uses LE byte order (differs from
/// R2-WIRE §13.4 TCP which uses big-endian).
pub fn write_ble_length_prefix(buf: &mut [u8], frame_len: u16) -> Result<usize, FrameError> {
    if buf.len() < 2 {
        return Err(FrameError::BufferTooSmall);
    }
    buf[0] = frame_len as u8;
    buf[1] = (frame_len >> 8) as u8;
    Ok(2)
}

/// Read a 2-byte little-endian length prefix (BLE L2CAP stream mode).
pub fn read_ble_length_prefix(buf: &[u8]) -> Result<u16, FrameError> {
    if buf.len() < 2 {
        return Err(FrameError::Incomplete);
    }
    Ok(buf[0] as u16 | ((buf[1] as u16) << 8))
}

// -------------------------------------------------------------------------
// Frame extraction from a stream buffer
// -------------------------------------------------------------------------

/// Try to extract one complete TCP frame from a byte buffer.
///
/// Returns `Ok(Some((frame_start, frame_len)))` if a complete frame is
/// available, `Ok(None)` if more data is needed, or `Err` on protocol
/// violation.
///
/// `frame_start` is the offset of the R2-WIRE payload (after the 4-byte
/// length prefix).  `frame_len` is the payload length.
pub fn try_extract_tcp_frame(buf: &[u8]) -> Result<Option<(usize, usize)>, FrameError> {
    if buf.len() < 4 {
        return Ok(None); // Need more data for the length prefix.
    }
    let payload_len = read_length_prefix(buf)? as usize;
    if payload_len == 0 {
        // Zero-length frame — skip (keepalive / no-op).
        return Ok(Some((4, 0)));
    }
    if buf.len() < 4 + payload_len {
        return Ok(None); // Have the prefix but not the full payload yet.
    }
    Ok(Some((4, payload_len)))
}

/// Try to extract one complete BLE L2CAP stream frame from a byte buffer.
///
/// Returns `Ok(Some((frame_start, frame_len)))` if a complete frame is
/// available.
pub fn try_extract_ble_frame(buf: &[u8]) -> Result<Option<(usize, usize)>, FrameError> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let frame_len = read_ble_length_prefix(buf)? as usize;
    if frame_len == 0 {
        return Ok(Some((2, 0)));
    }
    if buf.len() < 2 + frame_len {
        return Ok(None);
    }
    Ok(Some((2, frame_len)))
}
