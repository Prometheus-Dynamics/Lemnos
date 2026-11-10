//! A simulated board. It implements every hardware trait a driver can ask
//! for, so drivers and the host can be tested with no hardware.
//!
//! - [`SimClock`]: time that moves only when a test moves it.
//! - [`SimI2c`]: register-file targets and FIFOs that fill on simulated time.
//! - [`FaultScript`]: makes chosen operations fail, in order or for a period.
//! - [`SimFrameSource`]: a producer of frames with known contents, for testing frame streams.
//! - [`SimHints`]: what the board says as resources come and go, as a real board's hotplug
//!   events do.
//! - [`SimBoard`]: named resources, handed out through
//!   [`Provider`](lemnos_core::Provider). An exclusive resource can be held by
//!   one driver at a time, and is released when the driver lets go of it.

mod board;
mod clock;
mod faults;
mod hints;
mod i2c;
mod spi;
mod stream;

pub use board::SimBoard;
pub use clock::SimClock;
pub use faults::{Fault, FaultScript, Faulty};
pub use hints::SimHints;
pub use i2c::{FifoLevel, Layout, SimI2c};
pub use lemnos_hal::mock;
pub use spi::SimSpi;
pub use stream::SimFrameSource;

#[cfg(test)]
mod tests;
