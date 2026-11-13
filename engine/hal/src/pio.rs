//! A PIO state machine: a small program a driver supplies, run on one GPIO line and fed 32-bit
//! words through its transmit FIFO.
//!
//! The driver states the whole job ([`PioJob`]): the instructions, where the program wraps, the
//! machine's clock, and how many bits each word supplies. The backend places the program in
//! instruction memory and relocates its jumps; the driver writes them relative to the program's
//! first instruction. The pin is the one the board file names, and it is the program's
//! side-set pin: each instruction's side-set bit drives it.
//!
//! [`PioOperation::Write`] pushes words in order, the first one shifted out first, most significant
//! bit first. Words are left-justified: a job with `out_bits` 24 takes bits 31 to 8 of each word.
//! [`PioOperation::DelayNs`] is the time the driver asks to pass after the last word has been
//! handed to the machine. Words already queued can still be shifting out then, so a driver that
//! needs a line low for a set time after its last bit adds the time those words take.

/// One instruction-memory program a state machine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PioJob<'a> {
    /// The instructions, at most 32. JMP targets and [`wrap_bottom`](Self::wrap_bottom) and
    /// [`wrap_top`](Self::wrap_top) are relative to the first instruction.
    pub program: &'a [u16],
    /// The first instruction of the wrap window (relative).
    pub wrap_bottom: u8,
    /// The last instruction of the wrap window (relative).
    pub wrap_top: u8,
    /// The machine's clock in hertz. The backend picks the nearest divider it can make from the
    /// block's clock.
    pub clock_hz: u32,
    /// Bits shifted out of each word, 1 to 32. The machine pulls the next word when that many
    /// bits have gone out.
    pub out_bits: u8,
}

/// One step of a PIO transaction.
#[derive(Debug, PartialEq, Eq)]
pub enum PioOperation<'a> {
    /// Words pushed into the machine's transmit FIFO, in order.
    Write(&'a [u32]),
    /// Time the driver asks to pass after the last word has been handed over, in nanoseconds.
    DelayNs(u32),
}

/// A state machine claimed for one pin. Load a program with [`load`](Self::load) before the first
/// [`transaction`](Self::transaction); the machine keeps the program until it is loaded again or
/// the machine is dropped.
pub trait PioMachine {
    /// Places `job` in instruction memory, sets the clock and pin, and starts the machine. Loading
    /// again replaces the program.
    fn load(&mut self, job: &PioJob<'_>) -> Result<(), crate::ErrorKind>;
    /// Runs the operations in order.
    fn transaction(&mut self, operations: &mut [PioOperation<'_>]) -> Result<(), crate::ErrorKind>;
}
