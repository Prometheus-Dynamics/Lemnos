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
    LedClient, Reading, Update,
};
pub use lemnos_light::{Easing, EffectKind, Phase, Status as LedStatus, SystemState};
pub use wire::{
    ChannelDesc, ControlDesc, DeviceDesc, Event, LedRequest, LedShow, Message, RawReading, Refusal,
    Request, VERSION, WireError, decode_message, decode_request,
};
