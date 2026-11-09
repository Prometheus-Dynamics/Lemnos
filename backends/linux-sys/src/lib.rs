//! Linux uAPI structures and safe wrappers over the system calls Lemnos needs: i2c-dev, spidev,
//! GPIO uAPI v2, netlink uevents, inotify, poll, signalfd, dma-heap, dma-buf, mappings, V4L2 and
//! the media controller.
//!
//! One of the few crates allowed `unsafe`. Every block states why it is sound in a `SAFETY`
//! comment, and everything public here is safe to call. Chip-specific layouts arrive with the
//! crates that need them.

#![cfg(target_os = "linux")]

pub mod dma_buf;
pub mod dma_heap;
pub mod gpio;
pub mod i2c;
pub mod inotify;
pub mod media;
// Its table macros and checked requests serve uAPI layouts that arrive with later crates.
#[allow(dead_code, unused_imports, unused_macros)]
mod ioctl;
pub mod mmap;
pub mod netlink;
pub mod poll;
pub mod pool;
pub mod process;
pub mod rp1_pio;
pub mod serial;
pub mod signal;
pub mod spi;
pub mod time;
pub mod udmabuf;
pub mod usbfs;
pub mod v4l2;

pub use ioctl::{Ioctl, Pod, from_bytes, ioc, ior, iow, iowr, zeroed};

#[cfg(any(test, feature = "alloc-count"))]
pub mod test_alloc;
#[cfg(any(test, feature = "test-pty"))]
pub mod test_pty;
