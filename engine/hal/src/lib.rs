//! The hardware vocabulary of Lemnos, without `std` or allocation.
//!
//! - Bus traits come from [embedded-hal] 1.0 and [embedded-hal-async] 1.0,
//!   re-exported here ([`i2c`], [`spi`], [`digital`], [`delay`], [`pwm`],
//!   [`asynch`]). Lemnos device drivers are written against them, so they run
//!   on Linux and on microcontrollers alike.
//! - [`register`] adds register maps over I2C and SPI (blocking and async);
//!   [`asynch::Blocking`] runs async driver code over blocking buses and maps.
//! - [`power`] adds regulators and clock outputs.
//! - [`blocks`] adds what a driver asks of a hardware block (registers,
//!   interrupts, DMA, clocks, resets), a frame stream, and USB in both roles.
//! - [`raw`] defines claimed GPIO lines and PWM channels ([`raw::RawLine`],
//!   [`raw::RawPwm`]) and per-transfer SPI settings, for every layer.
//! - [`ErrorKind`] and [`HalError`] classify failures portably; [`erased`]
//!   reduces a bus's errors to `ErrorKind` so buses of different types fit
//!   one `dyn` bus.
//! - `mock` (feature `mock`) provides in-memory doubles for tests, one for each
//!   trait here.
//!
//! [embedded-hal]: https://docs.rs/embedded-hal
//! [embedded-hal-async]: https://docs.rs/embedded-hal-async

#![no_std]
#![forbid(unsafe_code)]

mod blocking;
pub mod blocks;
pub mod erased;
mod error;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod power;
pub mod raw;
pub mod register;

pub use blocks::{
    Clocks, DmaBuffers, Interrupt, Registers, Reset, StreamEndpoint, Uart, UsbDevice, UsbFunction,
    UsbHost,
};
pub use embedded_hal;
pub use embedded_hal::{delay, digital, i2c, pwm, spi};
pub use embedded_hal_async;
pub use error::{ErrorKind, HalError};
pub use power::{ClockOutput, FixedClock, GpioRegulator, Regulator};
pub use register::{
    AddressWidth, Endian, I2cRegisters, RegWrite, RegisterBus, RegisterError, SpiRegisters,
};

/// The async bus traits ([`embedded_hal_async`]), and [`Blocking`](asynch::Blocking)
/// to use a blocking implementation through them.
pub mod asynch {
    pub use crate::blocking::Blocking;
    pub use crate::register::asynch::RegisterBus;
    pub use embedded_hal_async::{delay, digital, i2c, spi};
}
