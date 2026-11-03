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
//!
//! # When a client falls behind
//!
//! `lemnosd` queues each client's frames in a bounded write queue of 64 KiB
//! (the queue lives in `lemnosd`; this crate only defines the frames). When
//! the queue is full, frames are dropped in this order, and never otherwise:
//!
//! 1. [`Message::Reading`] frames go first, silently. The next reading
//!    supersedes them.
//! 2. [`Message::Event`] frames go next. Each dropped event is counted, and
//!    the count goes out as one [`Event::Dropped`] once the queue is under
//!    half full again.
//! 3. Every other message (replies, watched light frames, light info, the
//!    welcome) is never dropped.
//!
//! A client that needs every event must keep up; [`Event::Dropped`] says how
//! many it missed.

#![forbid(unsafe_code)]

mod client;
pub mod wire;

#[cfg(test)]
mod tests;

pub use client::FrameUpdate;
pub use client::{
    ClientError, ClientEvent, ClientOptions, DEFAULT_SOCKET, DEFAULT_TIMEOUT, DeviceClient,
    I2cDevice, LedClient, Line, Pwm, Reading, SpiDevice, Update,
};
/// The compact-model types the protocol carries, so clients need no
/// direct `lemnos-device` or `lemnos-hal` dependency.
pub use lemnos_device::{
    Axis, CalibrationCommand, CalibrationPart, CalibrationRoutine, CalibrationStatus, DeviceClass,
    DeviceStatus, NO_VALUE, Quantity, Unit,
};
pub use lemnos_hal::ErrorKind;
/// Raw line, PWM and SPI settings ([`DeviceClient::claim_line`] and friends).
pub use lemnos_hal::raw;
pub use lemnos_light::{
    Easing, EffectKind, LayerSpec, LookName, LookSpec, Phase, Spot, SpotList, SpotSource,
    SpotsSpec, Status as LedStatus, SystemState,
};
pub use wire::{
    ChannelDesc, ControlDesc, DeviceDesc, Event, I2cOp, LedRequest, LedShow, LightFrame, LightInfo,
    LineTarget, LooksOp, Message, PwmTarget, RawReading, RawRequest, Refusal, Request, SpiXfer,
    ReadStats,
    VERSION, WireError, decode_message, decode_request, encode_reading,
};
