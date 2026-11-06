//! Safe Rust interfaces to the Linux kernel's camera devices, with no C libraries: V4L2 video
//! nodes, the media controller, V4L2 subdevices and V4L2 events. Every system call goes
//! through `lemnos-linux-sys`; this crate has no `unsafe` of its own.
//!
//! Every device type owns its file descriptor and implements [`std::os::fd::AsFd`], so an async
//! layer can register it with any reactor. Devices are opened non-blocking: dequeue calls return
//! `Ok(None)` when nothing is ready instead of blocking.
//!
//! The kernel uAPI structures live in `lemnos-linux-sys` (`v4l2::device`, `media`), with layout
//! checks against the kernel headers on 64-bit targets. Memory-to-memory sessions are in
//! `lemnos-linux` (`v4l2::m2m`), not here.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod error;
mod flags;
mod fourcc;
mod node;

pub mod event;
pub mod media;
pub mod subdev;
pub mod v4l2;

pub use error::{Error, Result};
pub use event::Events;
pub use fourcc::FourCc;
pub use lemnos_linux_sys::mmap::Mapping;
pub use lemnos_linux_sys::poll::{Ready, Wait};
pub use lemnos_linux_sys::time::monotonic_now;
pub use lemnos_linux_sys::v4l2::device::{Fraction, Rect};
pub use node::{poll, poll_into};
