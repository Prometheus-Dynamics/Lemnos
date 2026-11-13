//! What the receivers that run a stock V4L2 capture node share: the node as a driver sees it
//! ([`Hardware`], and [`LinuxHardware`] over `lemnos-v4l2`), and the [`Pump`] that moves its
//! frames into a frame stream. No driver of its own, and nothing here knows which receiver
//! the node belongs to.
//!
//! # Two ways for a frame to reach the stream
//!
//! - **The node fills the stream's own buffers.** When the stream's buffers are dma-bufs
//!   ([`BufferKind::Handles`]) and the node takes them, the pump hands each free stream buffer
//!   to the node (`V4L2_MEMORY_DMABUF`), and a filled one is published as it is. Nothing is
//!   copied and the driver never calls `write`. Stream buffer `n` is the node's slot `n`. A
//!   node that writes a frame in several memory planes takes one dma-buf per plane, so this
//!   needs a stream of as many planes ([`StreamEndpoint::planes`](lemnos_hal::blocks::StreamEndpoint::planes)), each lent on its own.
//! - **One copy.** On a stream of plain memory, a node that takes no dma-bufs, or a frame of
//!   several planes on a stream of fewer, the node fills its own mapped buffers and each
//!   frame is copied into a stream buffer with `write`, plane by plane at the stream's plane
//!   offsets when the stream has as many planes, end to end otherwise.
//!
//! [`Pump::begin`] picks: it asks for the first and falls back to the second.
//!
//! # A consumer that isn't there, or changes
//!
//! By copy, a frame with nobody to take it is dropped and the node's buffer goes straight
//! back, so capture never waits on a consumer. With the node filling the stream's buffers, a
//! stream on standby has memory waiting while no consumer is attached: the node keeps
//! filling it, and a consumer that attaches adopts that memory and is handed the newest
//! frame at once, with nothing copied. A stream not on standby has nothing to fill while
//! nobody is attached, so the node idles with no buffer queued and takes up again when one
//! attaches.
//!
//! When the consumer goes, the kernel still holds the dma-bufs it was given, under holds of
//! its own, so a frame in flight lands in memory that is the old consumer's alone and its
//! publish reaches no one. Buffers taken afterwards are the new consumer's, and queueing a
//! slot on a new dma-buf makes the kernel let go of the old one.
//!
//! # What is checked
//!
//! - A dma-buf is queued only if its real size, asked of the kernel, holds a whole frame as
//!   the node was asked for (the pump checks, [`LinuxHardware`] checks against the node's own
//!   format, and the kernel checks again). A stream buffer too small is counted in `errors`,
//!   never queued and never published: the stream stays empty, which points at the board
//!   file.
//! - A stream buffer is used as slot `n` only when `n` is below the slots asked for, which is
//!   the stream's own buffer count, at most [`MAX_SLOTS`].
//! - A filled slot the node names must be one the pump queued, and its byte count is what the
//!   stream checks against its buffers when it is published.
//! - Directly, every plane's dma-buf is checked against that plane's size as the node set it,
//!   and a frame of more than [`MAX_PLANES`] planes is refused.
//! - By copy, each plane goes through the stream's `write`, which refuses a range outside the
//!   buffer.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod format;
mod hw;
mod pump;

pub use format::{fourcc, parse_order};
pub use hw::{
    Frame, Granted, Graph, Hardware, LinuxHardware, MAX_PLANES, Prepare, Settings, Wanted,
};
pub use pump::{MAX_SLOTS, Mode, Pump};

#[cfg(doc)]
use lemnos_hal::blocks::BufferKind;
