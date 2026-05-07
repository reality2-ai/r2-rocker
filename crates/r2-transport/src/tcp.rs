//! TCP stream transport binding (R2-WIRE §13.4).
//!
//! R2-WIRE extended messages are length-prefixed on a TCP stream:
//!
//! ```text
//! [payload_length: 4 bytes, big-endian] [R2-WIRE extended message]
//! ```
//!
//! This module provides encode/decode helpers that work on byte slices
//! (`no_std` compatible).  For async TCP socket I/O, applications use
//! these helpers with their own socket abstraction (tokio, embassy, etc.).

use crate::framing::{self, FrameError, MAX_FRAME_SIZE};

/// Encode an R2-WIRE frame for TCP transmission.
///
/// Writes the 4-byte big-endian length prefix followed by the frame
/// bytes into `out_buf`.  Returns the total bytes written (4 + frame.len()).
///
/// ```
/// # use r2_transport::tcp::encode_tcp_frame;
/// let frame = [0x00, 0x53, 0xA1, 0xB2]; // minimal R2-WIRE header stub
/// let mut buf = [0u8; 64];
/// let n = encode_tcp_frame(&frame, &mut buf).unwrap();
/// assert_eq!(n, 8); // 4 prefix + 4 payload
/// assert_eq!(&buf[..4], &[0, 0, 0, 4]); // length = 4
/// assert_eq!(&buf[4..8], &frame);
/// ```
pub fn encode_tcp_frame(frame: &[u8], out_buf: &mut [u8]) -> Result<usize, FrameError> {
    let total = 4 + frame.len();
    if out_buf.len() < total {
        return Err(FrameError::BufferTooSmall);
    }
    if frame.len() > MAX_FRAME_SIZE {
        return Err(FrameError::PayloadTooLarge);
    }
    framing::write_length_prefix(out_buf, frame.len() as u32)?;
    out_buf[4..total].copy_from_slice(frame);
    Ok(total)
}

/// Decode the next R2-WIRE frame from a TCP stream buffer.
///
/// Returns `Ok(Some(payload_slice))` if a complete frame is available,
/// `Ok(None)` if more data is needed.  The caller should remove the
/// consumed bytes from the buffer.
///
/// ```
/// # use r2_transport::tcp::decode_tcp_frame;
/// let buf = [0, 0, 0, 4, 0x00, 0x53, 0xA1, 0xB2];
/// let (frame, consumed) = decode_tcp_frame(&buf).unwrap().unwrap();
/// assert_eq!(frame, &[0x00, 0x53, 0xA1, 0xB2]);
/// assert_eq!(consumed, 8);
/// ```
pub fn decode_tcp_frame(buf: &[u8]) -> Result<Option<(&[u8], usize)>, FrameError> {
    match framing::try_extract_tcp_frame(buf)? {
        Some((start, len)) => {
            if len == 0 {
                Ok(Some((&[], start)))
            } else {
                Ok(Some((&buf[start..start + len], start + len)))
            }
        }
        None => Ok(None),
    }
}

/// The R2-WIRE UDP/TCP port number (R2-WIRE §13.5).
///
/// Port 21042 = 0x5232 = "R2" in ASCII.
pub const R2_PORT: u16 = 21042;

/// The R2-WIRE GROUP_MGMT TCP port (same as events for simplicity).
pub const R2_MGMT_PORT: u16 = 21042;

/// The R2 OTA firmware delivery port (R2-DEPLOY).
pub const R2_OTA_PORT: u16 = 21043;

/// The R2 presence/discovery broadcast port (R2-WIFI §4.4).
pub const R2_PRESENCE_PORT: u16 = 21044;

/// The R2 console (GraphQL/WebSocket) port (R2-CONSOLE).
pub const R2_CONSOLE_PORT: u16 = 21045;
