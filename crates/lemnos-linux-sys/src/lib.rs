//! The unsafe edge of `lemnos-linux`: Linux uAPI structures and safe wrappers
//! over the system calls Lemnos needs (i2c-dev, spidev, GPIO uAPI v2, netlink
//! uevents, inotify, poll, signalfd).
//!
//! This is the only Lemnos crate allowed to use `unsafe`; every block states
//! why it is sound in a `// SAFETY:` comment. Everything public here is safe to
//! call. Structures carry layout tests against the kernel headers (64-bit
//! targets: x86_64, aarch64, riscv64).

#![cfg(target_os = "linux")]

pub mod gpio;
pub mod i2c;
pub mod inotify;
mod ioctl;
pub mod netlink;
pub mod poll;
pub mod signal;
pub mod spi;

pub use ioctl::{ioc, ior, iow, iowr};
