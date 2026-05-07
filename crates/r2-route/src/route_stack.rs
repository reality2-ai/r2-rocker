//! Route stack manipulation for compact and extended formats (SPEC.md §5).
//!
//! The route stack records the path a message has taken, enabling reply routing.
//! Compact entries are 16-bit (upper half of FNV hash); extended are full 32-bit.

use r2_wire::types::{CompactRouteStack, ExtendedRouteStack};

/// Route stack operation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteStackError {
    /// Route stack option is `None`.
    Missing,
    /// Route stack has reached maximum capacity (8 entries).
    Full,
    /// Route stack is empty (cannot pop).
    Empty,
    /// Top of stack doesn't match local ID (reply routing error).
    MismatchedHop,
}

/// Compress a 32-bit hive ID to 16-bit compact route entry (upper 16 bits).
pub fn compress_hive_id_16(hive_id: u32) -> u16 {
    (hive_id >> 16) as u16
}

/// Append a hive ID to a compact route stack.
pub fn append_compact(
    route: &mut Option<CompactRouteStack>,
    hive_id: u32,
) -> Result<(), RouteStackError> {
    let stack = route.as_mut().ok_or(RouteStackError::Missing)?;
    if stack.len >= 8 {
        return Err(RouteStackError::Full);
    }
    stack.entries[stack.len as usize] = compress_hive_id_16(hive_id);
    stack.len += 1;
    Ok(())
}

/// Append a hive ID to an extended route stack.
pub fn append_extended(
    route: &mut Option<ExtendedRouteStack>,
    hive_id: u32,
) -> Result<(), RouteStackError> {
    let stack = route.as_mut().ok_or(RouteStackError::Missing)?;
    if stack.len >= 8 {
        return Err(RouteStackError::Full);
    }
    stack.entries[stack.len as usize] = hive_id;
    stack.len += 1;
    Ok(())
}

/// Pop the top entry for reply routing (compact). Returns the next hop, or `None` if origin.
///
/// The top entry must match `local_id` (this node). After popping, the new top
/// is the next hop toward the originator.
pub fn pop_for_reply_compact(
    stack: &mut CompactRouteStack,
    local_id: u16,
) -> Result<Option<u16>, RouteStackError> {
    if stack.len == 0 {
        return Err(RouteStackError::Empty);
    }
    if stack.entries[(stack.len - 1) as usize] != local_id {
        return Err(RouteStackError::MismatchedHop);
    }
    stack.len -= 1;
    if stack.len == 0 {
        Ok(None)
    } else {
        Ok(Some(stack.entries[(stack.len - 1) as usize]))
    }
}

/// Pop the top entry for reply routing (extended). Returns the next hop, or `None` if origin.
pub fn pop_for_reply_extended(
    stack: &mut ExtendedRouteStack,
    local_id: u32,
) -> Result<Option<u32>, RouteStackError> {
    if stack.len == 0 {
        return Err(RouteStackError::Empty);
    }
    if stack.entries[(stack.len - 1) as usize] != local_id {
        return Err(RouteStackError::MismatchedHop);
    }
    stack.len -= 1;
    if stack.len == 0 {
        Ok(None)
    } else {
        Ok(Some(stack.entries[(stack.len - 1) as usize]))
    }
}

/// Peek at the top entry without modifying the stack (compact).
pub fn peek_next_hop_compact(route: &CompactRouteStack) -> Option<u16> {
    if route.len == 0 {
        None
    } else {
        Some(route.entries[(route.len - 1) as usize])
    }
}

/// Peek at the top entry without modifying the stack (extended).
pub fn peek_next_hop_extended(route: &ExtendedRouteStack) -> Option<u32> {
    if route.len == 0 {
        None
    } else {
        Some(route.entries[(route.len - 1) as usize])
    }
}
