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
//! **No Linux [`Device`].** Every request takes its argument by value or as a plain structure, but
//! issuing one needs `unsafe`, and `lemnos-linux-sys` has no public request for this device. The
//! window mapping exists ([`Mapping`](lemnos_linux_sys::mmap::Mapping) with `read_volatile`), but
//! reading registers with the engine's clocks off can stall the bus, so it is useless without the
//! claim. Both belong in a crate allowed `unsafe` (a `-sys` crate for this device, or a module of
//! `lemnos-linux-sys`): the requests in [`uapi`] with their argument types, and a mapping of
//! exactly [`uapi::WINDOW_BYTES`] at offset 0 (the kernel maps as many bytes as asked from the
//! window's start).
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
pub mod uapi;

pub use block::{
    Block, BufferId, Completion, Device, Error, Lock, MAX_BUFFERS, MAX_WAIT_S, Mode, State,
    backoff_ns,
};

#[cfg(all(test, target_os = "linux"))]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
