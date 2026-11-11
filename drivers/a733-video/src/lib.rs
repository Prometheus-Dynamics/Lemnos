//! The A733's second video engine (the encoder side) through the vendor kernel's
//! `/dev/cedar_dev_ve2`. No vendor library: the engine is reached only through the kernel's own
//! requests, and what it encodes or decodes is the caller's business.
//!
//! - [`uapi`]: the requests, lock bits, modes, structures and register-window facts, transcribed
//!   with their source and pinned by layout tests.
//! - [`Block`]: the engine as one caller holds it. [`Block::claim`] counts the caller as a user
//!   (clocks on) and takes the kernel's lock; [`Block::attach`] attaches a dma-buf the caller holds
//!   and records the bus address the kernel answers with the buffer's real size;
//!   [`Block::address`] hands out the bus address of a range only when the range lies inside an
//!   attached buffer; [`Block::read`] reads a register only inside the window; [`Block::wait`]
//!   waits for the engine's interrupt. Reopened after a failure with a backoff doubling from
//!   250 ms, capped at 8 s, never giving up and never turning itself off.
//! - [`Device`]: the kernel operations the block runs on, one per request.
//!
//! # What is not here, and why
//!
//! **No register writes.** The engine is programmed by writing its registers, and on this block a
//! register write can make it read or write memory: some registers take bus addresses, and others
//! (sizes, strides, lengths, the layout of reference and motion buffers) decide how far past an
//! address it goes. The kernel checks none of it, and the engine sits behind an IOMMU whose
//! address space other devices' buffers share. A driver could make an address register take only
//! an address [`Block::address`] derived, but it could not bound the extent behind it without
//! knowing what every size and stride register means for every codec, which is both the caller's
//! business under the block rule and not public. So no path that writes a register lands here: one
//! would let a caller reach memory it does not hold.
//!
//! # On a device, and for a caller
//!
//! [`LinuxDevice`] is the [`Device`] on the real node, over `lemnos-driver-a733-video-sys`,
//! which holds the `unsafe` and issues only the requests that take a value or one plain
//! structure, with a read-only mapping of exactly the window. [`A733Video`] gives the engine to
//! a caller as `codec.block/1` ([`codec`]). With no register write there is no coding job to
//! give it, and the contract says so honestly: the one job it runs claims the engine, reads
//! the registers the caller names and lets the engine go; everything else is refused.
//!
//! Kernel facts a caller must know: the lock blocks, uninterruptibly, while another opener holds
//! it; closing the file releases the lock but not the use counted by `IOCTL_ENGINE_REQ`, so a
//! process that dies while claimed leaves the clocks on; the bus address is the first scatterlist
//! entry's, so a buffer's range is contiguous only behind the IOMMU (the device tree gives this
//! engine one); `IOCTL_SET_REFCOUNT`, `IOCTL_FLUSH_CACHE_RANGE` and the address-answering
//! requests other than `IOCTL_MAP_DMA_BUF` are never issued.
//!
//! Hardware status: untested on board. The board check is in `testing/board-runs/E19.md`.

#![forbid(unsafe_code)]

mod block;
#[cfg(target_os = "linux")]
pub mod codec;
#[cfg(target_os = "linux")]
mod linux;
pub mod uapi;

#[cfg(target_os = "linux")]
pub use codec::{A733Video, EngineBlock};
#[cfg(target_os = "linux")]
pub use linux::LinuxDevice;

pub use block::{
    Block, BufferId, Completion, Device, Error, Lock, MAX_BUFFERS, MAX_WAIT_S, Mode, State,
    backoff_ns,
};

#[cfg(all(test, target_os = "linux"))]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
