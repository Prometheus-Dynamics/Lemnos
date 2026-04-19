#![forbid(unsafe_code)]

pub mod contract;

mod backend;
mod error;
mod gpio;
mod i2c;
mod pwm;
mod session;
mod spi;
mod uart;
mod usb;

pub use backend::BusBackend;
pub use error::{BusError, BusResult};
pub use gpio::{GpioBusBackend, GpioEdgeEvent, GpioEdgeStreamSession, GpioSession};
pub use i2c::{I2cBusBackend, I2cControllerSession, I2cSession};
pub use pwm::{PwmBusBackend, PwmSession};
pub use session::{BusSession, SessionAccess, SessionMetadata, SessionState, StreamSession};
pub use spi::{SpiBusBackend, SpiSession};
pub use uart::{UartBusBackend, UartReadChunk, UartSession, UartStreamSession};
pub use usb::{UsbBusBackend, UsbInterruptPacket, UsbInterruptStreamSession, UsbSession};

#[cfg(test)]
mod tests;
