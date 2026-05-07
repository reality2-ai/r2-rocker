//! Plugin trait — hardware abstraction for sentant actions.
//!
//! Plugins provide platform-specific capabilities (SPI, SD, ADC, GPIO,
//! networking) that sentants invoke via [`Action::PluginCall`].
//!
//! Each plugin has a unique ID within a hive and handles commands
//! identified by a `u8` command byte. The command/response protocol
//! is plugin-specific — the engine just routes the bytes.
//!
//! # Compiled vs Runtime
//!
//! - **R2-COMPILE**: plugins are selected at compile time and linked
//!   statically. The compiler generates the glue code.
//! - **Elixir engine**: plugins are loaded dynamically (NIFs or ports).
//! - Both implement the same capabilities — a sentant definition that
//!   uses `spi_write` works on either engine if the SPI plugin is available.

/// Plugin identifier — unique within a hive.
pub type PluginId = u8;

/// Plugin command — identifies the operation within a plugin.
///
/// Each plugin defines its own command set. For example, an SPI plugin
/// might define: 0x01 = write_register, 0x02 = burst_read, etc.
pub type PluginCommand = u8;

/// Result of a plugin command execution.
#[derive(Debug, Clone)]
pub enum PluginResult {
    /// Command succeeded, optional response data.
    Ok(PluginResponse),
    /// Command failed with an error message.
    Error(PluginError),
}

/// Successful plugin response.
#[derive(Debug, Clone)]
pub struct PluginResponse {
    /// Response data (may be empty).
    data: [u8; 128],
    len: u8,
}

impl PluginResponse {
    /// Empty response (command succeeded, no data).
    pub const fn empty() -> Self {
        Self {
            data: [0u8; 128],
            len: 0,
        }
    }

    /// Response with data.
    pub fn with_data(data: &[u8]) -> Self {
        let mut buf = [0u8; 128];
        let len = data.len().min(128);
        buf[..len].copy_from_slice(&data[..len]);
        Self {
            data: buf,
            len: len as u8,
        }
    }

    /// Get response bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

/// Plugin error.
#[derive(Debug, Clone)]
pub struct PluginError {
    /// Error code (plugin-specific).
    pub code: u8,
    /// Human-readable description (may be empty on constrained targets).
    desc: [u8; 64],
    desc_len: u8,
}

impl PluginError {
    /// Create an error with a code and description.
    pub fn new(code: u8, desc: &str) -> Self {
        let mut buf = [0u8; 64];
        let bytes = desc.as_bytes();
        let len = bytes.len().min(64);
        buf[..len].copy_from_slice(&bytes[..len]);
        Self {
            code,
            desc: buf,
            desc_len: len as u8,
        }
    }

    /// Get the description string.
    pub fn description(&self) -> &str {
        core::str::from_utf8(&self.desc[..self.desc_len as usize]).unwrap_or("?")
    }
}

/// The plugin trait.
///
/// Implemented by platform-specific hardware drivers. Plugins are
/// registered with the engine at startup and invoked via
/// [`Action::PluginCall`](crate::Action::PluginCall).
///
/// # Thread Safety
///
/// On single-threaded targets (ESP32 without FreeRTOS threading),
/// plugins are called from the main loop — no locking needed.
/// On multi-threaded targets, the engine ensures exclusive access.
pub trait Plugin {
    /// Handle a command from a sentant.
    ///
    /// `command` and `data` are plugin-specific. The plugin executes
    /// the command and returns a result.
    fn execute(&mut self, command: PluginCommand, data: &[u8]) -> PluginResult;

    /// Plugin name (for logging/debug).
    fn name(&self) -> &str;

    /// Plugin ID (set during registration).
    fn id(&self) -> PluginId;

    /// Called once at startup for hardware initialisation.
    fn init(&mut self) -> PluginResult {
        PluginResult::Ok(PluginResponse::empty())
    }

    /// Called periodically by the engine (optional polling).
    ///
    /// Return events to inject into the bus (e.g., ISR batch ready,
    /// timer expired). Default: do nothing.
    fn poll(&mut self) -> Option<(u32, &[u8])> {
        None
    }
}
