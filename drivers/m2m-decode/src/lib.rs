//! Stateless video decode through the kernel's own interfaces: V4L2 memory-to-memory queues and
//! media requests, with no vendor library and no code loaded at run time.
//!
//! - [`M2mSession`]: one memory-to-memory node with its output (source) and capture
//!   (destination) queues, their buffers, events and drains. Generic: it knows queues, formats
//!   and buffers, not what the device does with them.
//! - [`Request`]: a media request, which a stateless codec takes each frame's parameters and
//!   buffers in.
//! - [`hevc`]: the kernel's stateless HEVC controls, built from headers that
//!   `lemnos-driver-hevc` parsed.
//! - [`HevcDecoder`]: an access unit in, a frame out, on a node it opens itself and reopens
//!   after a failure (backoff capped at 8 s; it never gives up and never turns itself off).
//!
//! Every device call goes through a [`Node`]. On Linux that is a [`LinuxNode`] over
//! `lemnos-linux-sys`, which holds all the `unsafe`; this crate has none. Steady-state decoding
//! allocates nothing: buffers are mapped and records made when the session opens.
//!
//! Hardware status: untested on board. The tests run on a node that plays a device back; the
//! board check is in `testing/board-runs/E15.md`.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-linux/src/v4l2/{m2m,request,hevc}.rs`
//! and `docs/m2m.md`), onto `lemnos-linux-sys`'s V4L2 calls and its formats.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod decoder;
pub mod hevc;
mod node;
mod request;
mod session;

#[cfg(test)]
mod mock;

pub use decoder::{DecodeError, Frame, HevcDecoder, Settings, State, backoff_ns};
pub use node::{LinuxNode, Node, PlaneMap, Queued};
pub use request::{Request, media_device_for};
pub use session::{Drain, M2mSession, MAX_BUFFERS, QueueSetup, Ready, SourceChange, queue_type};

/// The extended control a request's controls are given as.
pub use lemnos_linux_sys::v4l2::device::ExtControl;
/// The kernel's V4L2 types a session takes and gives, re-exported.
pub use lemnos_linux_sys::v4l2::{
    BufType, Capability, Dequeued, Event, Format, MAX_PLANES, Memory, PixFormat, PixFormatMplane,
    PlaneFormat, PlaneMemory, QueuePlane, Rect,
};

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
