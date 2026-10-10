//! The `lemnosd` socket protocol and its clients.
//!
//! - [`DeviceClient`]: lists devices (compact-model descriptions: class,
//!   channels with units, controls with ranges, status), reads them,
//!   subscribes to readings, sets and gets controls (answered with the
//!   applied value or a [`Refusal`] from the device's write policy), and
//!   receives events (status, control and LED-owner changes).
//! - [`LedClient`]: LED intents: status with effects, colours, frames,
//!   single LEDs, gauges, spinners, system animations and locate, with
//!   per-request fade, easing, rate, depth and brightness.
//! - Raw access through the service ([`DeviceClient::claim_line`],
//!   [`DeviceClient::claim_pwm`], [`DeviceClient::i2c`], [`DeviceClient::spi`]):
//!   claims belong to the connection and end with it.
//! - Both report connection changes in order with data ([`ClientEvent`]),
//!   reconnect in the background when asked, and restore subscriptions and
//!   held intents after `lemnosd` restarts.
//!
//! [`wire`] is the protocol: length-prefixed little-endian frames over a
//! Unix stream socket.

#![forbid(unsafe_code)]

mod client;
pub mod wire;

#[cfg(test)]
mod tests;

pub use client::{
    ClientError, ClientEvent, ClientOptions, DEFAULT_SOCKET, DEFAULT_TIMEOUT, DeviceClient,
    I2cDevice, LedClient, Line, Pwm, Reading, SpiDevice, Update,
};
/// The compact-model types the protocol carries, so clients need no
/// direct `lemnos-device` or `lemnos-hal` dependency.
pub use lemnos_device::{Axis, DeviceClass, DeviceStatus, NO_VALUE, Quantity, Unit};
pub use lemnos_hal::ErrorKind;
/// Raw line, PWM and SPI settings ([`DeviceClient::claim_line`] and friends).
pub use lemnos_hal::raw;
pub use lemnos_light::{
    Easing, EffectKind, LayerSpec, LookName, LookSpec, Phase, Status as LedStatus, SystemState,
};
pub use wire::{
    ChannelDesc, ControlDesc, DeviceDesc, Event, I2cOp, LedRequest, LedShow, LineTarget, LooksOp,
    Message, PwmTarget, RawReading, RawRequest, Refusal, Request, SpiXfer, VERSION, WireError,
    decode_message, decode_request,
};
