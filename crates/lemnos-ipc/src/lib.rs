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
//! - [`GpuClient`]: the advisory GPU lease. It orders the clients that ask
//!   (exclusive or shared, by priority) and reports who holds a GPU; it
//!   enforces nothing, and a client that never asks keeps working.
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
//!    welcome) is never dropped. A client with more than 4 MiB of them
//!    waiting has stopped reading, and the service closes it.
//!
//! A client that needs every event must keep up; [`Event::Dropped`] says how
//! many it missed.

#![forbid(unsafe_code)]

mod client;
pub mod wire;

#[cfg(test)]
mod spec_vectors;
#[cfg(test)]
mod tests;

#[cfg(feature = "tokio")]
pub use client::AsyncDeviceClient;
pub use client::FrameUpdate;
pub use client::{
    ClientError, ClientEvent, ClientOptions, DEFAULT_SOCKET, DEFAULT_TIMEOUT, DeviceClient,
    GpuClient, GpuNotice, I2cDevice, LedClient, Line, Polled, Pwm, Reading, SpiDevice, Update,
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
    AllocStats, ChannelDesc, ControlDesc, DeviceDesc, DeviceView, Event, FEATURE_ALLOC_STATS,
    FEATURE_GPU_LEASE, FEATURE_READ_ID, GpuAcquire, GpuHangReport, GpuHolder, GpuInfo,
    GpuLeaseState, GpuMode, I2cOp, LedRequest, LedShow, LightFrame, LightInfo, LineTarget, LooksOp,
    Message, PwmTarget, RawReading, RawRequest, ReadStats, Refusal, Request, SpiXfer, VERSION,
    WireError, decode_message, decode_request, encode_devices_into, encode_frame_into,
    encode_reading, encode_reading_into, encode_status_event_into,
};
