//! The microcontroller backend: Lemnos's hardware traits over the peripherals a firmware already
//! has, with no allocator and no `unsafe`.
//!
//! - [`I2cBus`] and [`SpiBus`]: any embedded-hal 1.0 bus, with its errors as [`ErrorKind`].
//! - [`OutputLine`] and [`InputLine`]: an embedded-hal pin as a [`RawLine`].
//! - [`PwmChannel`]: an embedded-hal duty-cycle channel as a [`RawPwm`].
//! - [`usb`]: [`UsbFunction`] and [`UsbHost`] over a small port trait the firmware implements on
//!   its USB peripheral.
//! - [`Stall`]: where a bus lives when no driver holds it, and where it comes back to when its
//!   driver is dropped. [`Buses`]: the firmware's stalls by name, as the [`Provider`] a host binds
//!   drivers from.
//!
//! Every type is generic over the peripheral, so the same code is tested here against mocks and
//! built for the target with the firmware's HAL.
//!
//! [`ErrorKind`]: lemnos_hal::ErrorKind
//! [`RawLine`]: lemnos_hal::raw::RawLine
//! [`RawPwm`]: lemnos_hal::raw::RawPwm
//! [`UsbFunction`]: lemnos_hal::UsbFunction
//! [`UsbHost`]: lemnos_hal::UsbHost
//! [`Provider`]: lemnos_core::Provider

#![no_std]

#[cfg(test)]
extern crate std;

mod bus;
mod buses;
mod line;
mod pwm;
pub mod usb;

pub use bus::{I2cBus, SpiBus};
pub use buses::{
    Buses, DynI2c, DynLine, DynPwm, DynRawSpi, DynSpi, DynUart, DynUsbDevice, DynUsbFunction,
    Stall, Stalled,
};
pub use line::{InputLine, OutputLine};
pub use pwm::PwmChannel;

#[cfg(test)]
mod tests;
