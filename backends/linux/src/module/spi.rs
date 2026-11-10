//! A SPI device bound to the module.

use std::io;
use std::os::fd::{AsFd, OwnedFd};

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{RawSpi, SpiSegment};
use lemnos_hal::spi::{ErrorType, Operation, SpiDevice};
use lemnos_linux_sys::module as sys;

use super::Node;
use crate::kind_of;
use crate::spi::{SpiMode, Transfer, Wire, run_operations, run_segments};

const CLOCK_BITS: u32 = 0x03;

/// One device (bus and chip select), held by a claim.
pub(crate) struct ModuleSpi {
    node: Node,
    claim: OwnedFd,
    bus: u32,
    cs: u32,
    /// What the next message asks for. It starts as the device's own.
    mode: u32,
}

impl ModuleSpi {
    pub(crate) fn open(node: Node, bus: u32, cs: u32) -> io::Result<Self> {
        let claim = node.claim(format_args!("spi-{bus}.{cs}"))?;
        let mode = sys::spi_mode(node.fd(), bus, cs)?;
        Ok(Self {
            node,
            claim,
            bus,
            cs,
            mode,
        })
    }
}

impl Wire for ModuleSpi {
    fn send(&mut self, transfers: &mut [Transfer<'_>]) -> io::Result<()> {
        sys::spi_transfer(
            self.node.fd(),
            self.claim.as_fd(),
            (self.bus, self.cs),
            self.mode,
            transfers,
        )
    }

    fn select_mode(&mut self, mode: SpiMode) -> io::Result<()> {
        self.mode = (self.mode & !CLOCK_BITS) | mode.bits();
        Ok(())
    }
}

impl ErrorType for ModuleSpi {
    type Error = ErrorKind;
}

impl SpiDevice for ModuleSpi {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), ErrorKind> {
        run_operations(self, operations).map_err(|e| kind_of(&e))
    }
}

impl RawSpi for ModuleSpi {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Result<(), ErrorKind> {
        run_segments(self, segments).map_err(|e| kind_of(&e))
    }
}
