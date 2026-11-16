//! The two things a stage talks to: the link the host's frames come in on, and the storage the chunks
//! go to. A chip implements both over its own controllers; the tests and the simulated device implement
//! them over queues.

/// Why a link or storage operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoError {
    /// The operation did not finish within its bounded wait.
    Timeout,
    /// The device reported a failure, or the frame was not the length the protocol makes it.
    Failed,
}

/// The bulk endpoint the host sends frames through and reads replies from.
pub trait Link {
    /// Receives one frame into `buf`, returning its length. A frame longer than `buf` is `Failed`.
    fn recv(&mut self, buf: &mut [u8], budget_us: u64) -> Result<usize, IoError>;
    /// Acknowledges a verified chunk. A chip whose link acknowledges by its reply does nothing here.
    fn ack(&mut self) -> Result<(), IoError>;
    /// Sends `data` to the host, bounded by `budget_us`. The send is done when the link has taken it.
    fn send(&mut self, data: &[u8], budget_us: u64) -> Result<(), IoError>;
}

/// The storage the chunks go to, in 512-byte sectors.
pub trait Storage {
    fn write(&mut self, lba: u32, data: &[u8], budget_us: u64) -> Result<(), IoError>;
    fn read(&mut self, lba: u32, out: &mut [u8], budget_us: u64) -> Result<(), IoError>;
}
