//! Use Lemnos devices from another process.
//!
//! [`Client`] is the blocking form and can be shared between threads.
//! [`Session`] is the same thing owned by one thread, with
//! [`Session::send_call`] and [`Session::poll`] for code that must never wait.
//! A device's frame stream is taken with [`Session::open_stream`], which needs a
//! [`StreamPlatform`] for what only the operating system can do.
//! The `tokio` feature adds `AsyncClient`. All three share one connection
//! core, which reconnects by itself, restores subscriptions, and finds handles
//! again by name and tag after the service restarts.

#[cfg(feature = "tokio")]
mod asynchronous;
mod client;
mod connection;
mod error;
mod session;
mod stream;
mod values;

#[cfg(feature = "tokio")]
pub use asynchronous::AsyncClient;
pub use client::{Client, RemoteCaller};
pub use error::Error;
pub use lemnos_wire::DeviceDescription;
pub use session::{
    DeviceEntry, DeviceHealth, DeviceState, Event, Handle, Incoming, Session, Subscription,
    ValueInfo,
};
pub use stream::{FrameStream, HeldFrame, StreamPlatform};
