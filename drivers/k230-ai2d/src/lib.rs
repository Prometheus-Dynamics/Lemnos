//! The K230's AI2D block (crop, resize, pad and affine transforms for a neural network's input):
//! what its kernel interface is, and checked job logic over a [`Device`] trait. No vendor
//! library: nothing here links, loads or follows one.
//!
//! **There is no hardware path, and this is not a driver.** Nothing implements [`Device`] on
//! Linux (see "Not here" below), so no job reaches the block. The crate has no `#[driver]` type,
//! serves no contract, is not in `lemnosd`, and no board file can name it. Everything below runs
//! only against the played-back block in its tests.
//!
//! - [`uapi`]: the node, the two requests and their structure, the register window and the
//!   interrupt, transcribed with their source and pinned by layout tests.
//! - [`job`]: [`Image`] and [`Job`], built only from addresses the kernel answered for imported
//!   dma-bufs, each image checked to lie inside its dma-buf by the dma-buf's real size, and the two
//!   images apart. Overflow-safe.
//! - [`Ai2d`]: checks a resize, imports both dma-bufs, runs the job on a [`Device`], and removes
//!   both imports whatever happened. After a failure it closes the block (the kernel then drops
//!   every import) and reopens with a backoff doubling from 250 ms, capped at 8 s. It never gives
//!   up and never turns itself off. A resize on an open block allocates nothing.
//! - [`Scaler`]: jobs in `image.scaler/1`'s job format taken from a stream whose buffers are
//!   dma-bufs, checked, and handed to [`Ai2d`]. It is the loop a driver would run, over the same
//!   trait; it does not implement the contract's methods. See [`scaler`].
//!
//! # Not here: a [`Device`] for Linux
//!
//! The reference does not give a way to run a job that can be kept to a caller's memory, so no
//! job reaches the block yet. What is missing, exactly:
//!
//! - **The register map.** The kernel driver (`drivers/misc/canaan/ai2d/k230_ai2d.c` in the
//!   kernel the SDK pins) only imports dma-bufs, answers their addresses, and wakes `poll` on the
//!   completion interrupt. It does not map the registers for anyone or take a job: the vendor's
//!   prebuilt runtime (`libnncase`) writes the job into the registers itself, from user space,
//!   addresses included. The SDK holds no register description, and the vendor runtime is not
//!   read. Without the map, nothing can be programmed, and a job whose address registers are
//!   written from user space can name any memory: the block has nothing in front of it.
//! - **A contiguous dma-buf, known to be.** `AI2D_IMPORT_DMABUF` answers the address of the
//!   *last* piece of the dma-buf's scatter list. That is the buffer's start only for a buffer in
//!   one piece (from a contiguous heap); for any other, the block would be sent past the
//!   buffer's end. Nothing user space can ask says how many pieces a dma-buf has, so a [`Device`]
//!   must be handed buffers from a contiguous heap and must not take one on trust.
//! - **One opener.** The kernel keeps one global list of imports and resets it on every `open`,
//!   so a second process opening the node corrupts the first one's imports (and their removal
//!   walks a list it no longer owns). Only the daemon may hold the node, by a claim.
//!
//! The [`Device`] trait says what a Linux implementation must do; the driver logic above it is
//! tested on a played-back block. A Linux implementation needs, besides the register map, the
//! block's registers through the module's register window (`RegistersHandle`) and the two
//! requests in a `drivers/k230-ai2d-sys` crate (`AI2D_IMPORT_DMABUF` takes a pointer the kernel
//! follows).
//!
//! Also not covered: crop, pad, shift and affine (the scaler contract has no words for them), and
//! the block's formats: [`Device::FORMATS`] is the implementation's, since the reference names
//! none.
//!
//! Hardware status: untested on board. The board check is in `testing/board-runs/E35.md`.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod engine;
pub mod job;
pub mod scaler;
pub mod uapi;

pub use engine::{Ai2d, Device, Request, RunError, State, backoff_ns, format};
pub use job::{Format, Image, Imported, Job, JobError, Limits, Shape};
pub use scaler::Scaler;

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
