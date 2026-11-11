//! The hardware traits through the Lemnos kernel module, for the buses it serves, and DMA
//! memory, register windows, interrupts, clocks and resets for the blocks bound to it.
//!
//! Claims here belong to the kernel: a holder that dies lets go, and a name someone holds is
//! refused. `LinuxBoard` chooses per bus, so anything the module doesn't serve stays on the stock
//! path. A block's resources have no stock path: only the module gives them (see [`block`]).

pub(crate) mod block;
pub(crate) mod dma;
mod events;
mod gpio;
mod i2c;
mod interrupt;
mod power;
mod pwm;
mod registers;
mod spi;

use std::fmt::{self, Write};
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;

use lemnos_linux_sys::module as sys;

pub use dma::ModuleDma;
pub(crate) use gpio::ModuleLine;
pub(crate) use i2c::ModuleI2c;
pub use interrupt::ModuleInterrupt;
pub use power::{ModuleClocks, ModuleReset};
pub(crate) use pwm::ModulePwm;
pub use registers::ModuleRegisters;
pub(crate) use spi::ModuleSpi;

/// The open control node. It has one opener at a time, so a process holds one and shares it,
/// with the interrupts read from it (see [`events`]).
#[derive(Clone)]
pub(crate) struct Node(Arc<Inner>);

struct Inner {
    fd: OwnedFd,
    events: events::Events,
}

/// A resource name, built without allocating.
struct Name {
    bytes: [u8; 64],
    len: usize,
}

impl Write for Name {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.len + text.len();
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl Node {
    /// Opens the node non-blocking: reading it is only the event stream, which [`events`] reads
    /// after `poll` says there is something, and nothing else on it looks at the flag.
    pub(crate) fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self(Arc::new(Inner {
            fd: file.into(),
            events: events::Events::default(),
        })))
    }

    fn fd(&self) -> BorrowedFd<'_> {
        self.0.fd.as_fd()
    }

    fn events(&self) -> &events::Events {
        &self.0.events
    }

    fn claim(&self, resource: fmt::Arguments<'_>) -> io::Result<OwnedFd> {
        let mut name = Name {
            bytes: [0; 64],
            len: 0,
        };
        name.write_fmt(resource)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let text = std::str::from_utf8(&name.bytes[..name.len])
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        sys::claim(self.fd(), text)
    }

    /// Whether a block of this device name is bound to the module.
    pub(crate) fn has_block(&self, name: &str) -> bool {
        matches!(sys::find_block(self.fd(), name), Ok(Some(_)))
    }

    /// Whether the module has this SPI device bound.
    pub(crate) fn serves_spi(&self, bus: u32, cs: u32) -> bool {
        sys::spi_mode(self.fd(), bus, cs).is_ok()
    }
}

/// Whether a failure to open through the module means only that the module doesn't serve this.
pub(crate) fn not_served(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENODEV | libc::ENOENT | libc::ENOTTY | libc::EINVAL)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fit_or_are_refused() {
        let mut name = Name {
            bytes: [0; 64],
            len: 0,
        };
        write!(name, "i2c-{}:0x{:x}", 1, 0x30).unwrap();
        assert_eq!(&name.bytes[..name.len], b"i2c-1:0x30");
        let long = "x".repeat(70);
        assert!(write!(name, "{long}").is_err());
    }

    #[test]
    fn only_some_failures_fall_back() {
        let served = |errno| !not_served(&io::Error::from_raw_os_error(errno));
        assert!(!served(libc::ENODEV));
        assert!(!served(libc::ENOTTY));
        // Someone holds it, or we may not have it: the stock path would be a way around that.
        assert!(served(libc::EBUSY));
        assert!(served(libc::EPERM));
    }

    #[test]
    fn a_missing_node_says_so() {
        let error = Node::open("/nonexistent/lemnos").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
