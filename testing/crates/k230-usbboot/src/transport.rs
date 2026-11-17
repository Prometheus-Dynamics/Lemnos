//! The USB operations the host uses. A real transport implements [`UsbIo`] over the operating
//! system's USB stack; the simulator implements it over a simulated board.

/// A USB operation that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsbError {
    /// The device left the bus. A reset is this error, not a failure of the request.
    Disconnected,
    /// The device stalled the request.
    Stall,
    /// No answer in time, after this many milliseconds.
    Timeout(u32),
    /// The device is still on the bus, but its boot ROM has stopped answering (EPROTO, not a stall). It
    /// stays enumerated, so only a load within the window after its re-enumeration can use it.
    Deaf,
    /// The device answered, but not what the protocol says.
    Protocol(String),
    /// Anything else, with the reason as text.
    Other(String),
}

impl std::fmt::Display for UsbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "the device disconnected"),
            Self::Stall => write!(f, "the device stalled the request"),
            Self::Deaf => write!(
                f,
                "the boot ROM stopped answering (EPROTO) and stays enumerated: a load must come within its window"
            ),
            Self::Timeout(ms) => write!(f, "no answer after {ms} ms"),
            Self::Protocol(text) | Self::Other(text) => write!(f, "{text}"),
        }
    }
}

impl std::error::Error for UsbError {}

/// The vendor tool's transfer timeout for the boot ROM and the stage's commands (`USB_TIMEOUT`,
/// `kburn_k230.h:11`).
pub const ROM_TIMEOUT_MS: u32 = 1_000;
/// The timeout the vendor tool raises to for a VERIFY, which hashes a whole payload on the board
/// (`burner_uboot.cpp:392-396`, `max(old, 300000)`).
pub const VERIFY_TIMEOUT_MS: u32 = 300_000;

/// The USB operations: vendor control requests on endpoint 0, and bulk transfers.
pub trait UsbIo {
    /// Sets the timeout of the transfers that follow, in milliseconds. A simulator has none, so the
    /// default does nothing; a real transport honours it.
    fn set_timeout_ms(&mut self, _ms: u32) {}
    /// A vendor IN control request; returns the `len` bytes the device sends.
    fn control_in(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
    ) -> Result<Vec<u8>, UsbError>;
    /// A vendor OUT control request with `data` (possibly empty).
    fn control_out(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), UsbError>;
    /// A bulk OUT transfer.
    fn bulk_out(&mut self, data: &[u8]) -> Result<(), UsbError>;
    /// A bulk IN transfer of up to `len` bytes.
    fn bulk_in(&mut self, len: usize) -> Result<Vec<u8>, UsbError>;
    /// The largest packet the OUT endpoint takes in one transfer.
    fn max_packet_out(&self) -> usize;
    /// Waits up to `timeout_ms` for a device to be on the bus at this port, and returns its vendor
    /// and product IDs. A device that does not come back in time is [`UsbError::Timeout`].
    fn wait_for_device(&mut self, timeout_ms: u32) -> Result<(u16, u16), UsbError>;
}
