//! The A733's second video engine (the encoder side), on two routes: encode jobs through
//! `lemnos.ko`, which checks them and writes every address itself ([`module`]), and register reads
//! through the vendor kernel's `/dev/cedar_dev_ve2`. No vendor library: what the engine encodes is
//! the caller's business.
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
//! - [`encoder`]: the encoder's QP map and CTU info records and the values the vendor header names.
//!   The header gives no register layout, so no job is built from them (see that module).
//!
//! # Register writes go through the kernel module
//!
//! The engine is programmed by writing its registers, and on this block a register write can make
//! it read or write memory: some registers take bus addresses, and others (sizes, strides, lengths)
//! decide how far past an address it goes. The vendor kernel checks none of it, and the engine
//! sits behind an IOMMU whose address space other devices' buffers share. So the vendor route
//! writes no register. Jobs that encode go through `lemnos.ko` instead ([`module`]): the module
//! takes the job's writes and the caller's dma-bufs, writes every address register itself from a
//! buffer it holds, refuses every other write to one, and refuses a range shorter than what the
//! job's own frame size, stride and format make the engine reach. The module's profile allows
//! the H.264 encoder's registers only, so a decode job is refused there.
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
//! Hardware status: untested on board. The board checks are in `testing/board-runs/E19.md` (the
//! vendor route) and `testing/board-runs/H8a.md` (the module route).

#![forbid(unsafe_code)]

mod block;
#[cfg(target_os = "linux")]
pub mod codec;
pub mod encoder;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub mod module;
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
