//! The unsafe edge of `lemnos-linux`: Linux uAPI structures and safe wrappers
//! over the system calls Lemnos needs (i2c-dev, spidev, GPIO uAPI v2, netlink
//! uevents, inotify, poll, signalfd, dma-heap and dma-buf, V4L2, subdevices and
//! the media controller, mappings).
//!
//! One of two Lemnos crates allowed `unsafe` (the other, `lemnos-capi`, is the
//! C boundary); every block states
//! why it is sound in a `// SAFETY:` comment. Everything public here is safe to
//! call. Structures carry layout tests against the kernel headers (64-bit
//! targets: x86_64, aarch64, riscv64).

#![cfg(target_os = "linux")]

pub mod dma_buf;
pub mod dma_heap;
pub mod drm;
pub mod gpio;
pub mod i2c;
pub mod inotify;
mod ioctl;
pub mod lemnos_pisp_be;
pub mod lemnos_rp1_csi2;
pub mod media;
pub mod mmap;
pub mod netlink;
pub mod poll;
pub mod pool;
pub mod process;
pub mod rga;

#[cfg(feature = "rp1")]
pub mod rp1_pio;
pub mod signal;
pub mod spi;
pub mod time;
pub mod usbfs;
pub mod v3d;
pub mod v4l2;
pub mod vg_lite;
pub mod vvcam_isp;

pub use ioctl::{Ioctl, Pod, from_bytes, ioc, ior, iow, iowr, zeroed};

#[cfg(any(test, feature = "alloc-count"))]
pub mod test_alloc;
