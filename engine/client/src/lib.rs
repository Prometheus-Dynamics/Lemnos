//! Use Lemnos devices from another process.
//!
//! [`Client`] is the blocking form and can be shared between threads.
//! [`Session`] is the same thing owned by one thread, with
//! [`Session::send_call`] and [`Session::poll`] for code that must never wait.
//! A device's frame stream is taken with [`Session::open_stream`], which needs a
//! [`StreamPlatform`] for what only the operating system can do. A stream whose buffers the
//! consumer fills (a job's input image, a configuration) is taken with
//! [`Session::open_stream_writable`] and filled with [`FrameStream::write`].
//! A long job (a method its contract marks as one) is started with [`Session::start_job`],
//! and its progress and end come from [`Session::next_job_event`] or [`Session::wait_job`].
//! The `tokio` feature adds `AsyncClient`. All three share one connection
//! core, which reconnects by itself, restores subscriptions, and finds handles
//! again by name and tag after the service restarts.

#[cfg(feature = "tokio")]
mod asynchronous;
mod bind;
mod client;
mod connection;
mod error;
mod holds;
mod session;
mod stream;
mod values;

#[cfg(feature = "tokio")]
pub use asynchronous::AsyncClient;
pub use client::{Client, RemoteCaller};
pub use error::Error;
pub use lemnos_wire::DeviceDescription;
pub use session::{
    DeviceEntry, DeviceHealth, DeviceState, Event, Handle, Incoming, JobEvent, PackageEntry,
    PackageList, PackageMode, Session, Subscription, UsbEndpoints, ValueInfo, follow,
};
pub use stream::{Access, FrameStream, HeldFrame, MemoryKind, Region, StreamPlatform};
