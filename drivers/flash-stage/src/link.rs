//! The two things a stage talks to: the link the host's frames come in on, and the storage the chunks go
//! to. A chip implements both over its own controllers; the tests and the simulated device implement them
//! over queues.

/// Why a link or storage operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoError {
    /// The operation did not finish within its bounded wait.
    Timeout,
    /// The device reported a failure.
    Failed,
    /// The bus was reset, or the device was unconfigured, under the operation. The session must be
    /// enumerated again before it goes on.
    Reset,
}

/// The bulk pair the host's frames come in on and the replies go out on.
pub trait Link {
    /// Receives one frame into `buf`, returning its length. A frame shorter than `buf` returns its own
    /// length: the caller decides what a short frame means. A frame that does not fit is `Failed`.
    fn recv(&mut self, buf: &mut [u8], budget_us: u64) -> Result<usize, IoError>;
    /// Sends one reply, as one transfer, bounded by `budget_us`.
    fn send(&mut self, data: &[u8], budget_us: u64) -> Result<(), IoError>;
}

/// The storage the chunks go to, in 512-byte sectors.
pub trait Storage {
    fn write(&mut self, lba: u32, data: &[u8], budget_us: u64) -> Result<(), IoError>;
    fn read(&mut self, lba: u32, out: &mut [u8], budget_us: u64) -> Result<(), IoError>;
}
