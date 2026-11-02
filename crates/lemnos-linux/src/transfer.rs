//! Transfer engines over the Linux buses (`lemnos_bus::transfer`): SPI through
//! spidev and I2C through i2c-dev.
//!
//! These buses move bytes through the CPU, so an engine runs the transfer in
//! `submit`: the ticket is already complete, `poll` and `wait` report it at
//! once, and `cancel` has nothing to stop. dma-bufs are refused with
//! [`TransferError::Unsupported`]. A DMA engine (PCIe, RP1, a USB endpoint)
//! implements the same trait when it lands.

use std::io;
use std::marker::PhantomData;
use std::time::Duration;

use lemnos_bus::{
    Completion, Sink, Source, Transfer, TransferEngine, TransferError, TransferResult,
};
use lemnos_hal::ErrorKind;

/// The ticket of a transfer that finished in `submit`.
pub struct Finished<'a> {
    bytes: usize,
    _buffers: PhantomData<&'a ()>,
}

impl Finished<'_> {
    fn completion(&self) -> Completion {
        Completion { bytes: self.bytes }
    }
}

/// A transfer's memory sides: the bytes to write, and the buffer to read into.
type Sides<'a> = (Option<&'a [u8]>, Option<&'a mut [u8]>);

/// The memory sides of a transfer. A dma-buf side is refused: these buses have
/// no way to read or write one.
fn memory_sides(transfer: Transfer<'_>) -> TransferResult<Sides<'_>> {
    let source = match transfer.source {
        None => None,
        Some(Source::Memory(bytes)) => Some(bytes),
        Some(Source::DmaBuf { .. }) => return Err(dma_buf_refused()),
    };
    let sink = match transfer.sink {
        None => None,
        Some(Sink::Memory(bytes)) => Some(bytes),
        Some(Sink::DmaBuf { .. }) => return Err(dma_buf_refused()),
    };
    Ok((source, sink))
}

fn dma_buf_refused() -> TransferError {
    TransferError::Unsupported("this bus moves bytes through the CPU; dma-bufs need a DMA engine")
}

fn failed(error: io::Error) -> TransferError {
    let kind = match error.raw_os_error() {
        Some(errno) => ErrorKind::from_errno(errno),
        None => ErrorKind::from_io(error.kind()),
    };
    TransferError::Failed {
        kind,
        reason: error.to_string(),
    }
}

#[cfg(feature = "spi")]
mod spi {
    use super::*;
    use crate::hal::{SpiTransfer, Spidev};

    /// SPI through spidev: a transfer is one `SPI_IOC_MESSAGE`, so chip select
    /// stays asserted from the first side to the last.
    #[derive(Debug)]
    pub struct SpidevEngine {
        device: Spidev,
    }

    impl SpidevEngine {
        /// Runs transfers on `device`.
        pub fn new(device: Spidev) -> Self {
            Self { device }
        }

        /// The device.
        pub fn device(&self) -> &Spidev {
            &self.device
        }

        /// Gives back the device.
        pub fn into_inner(self) -> Spidev {
            self.device
        }
    }

    impl TransferEngine for SpidevEngine {
        type Ticket<'a> = Finished<'a>;

        fn submit<'a>(&mut self, transfer: Transfer<'a>) -> TransferResult<Finished<'a>> {
            let (source, sink) = memory_sides(transfer)?;
            let bytes = match (source, sink) {
                (None, None) => 0,
                (Some(tx), None) => {
                    self.device
                        .transfer(&mut [SpiTransfer::write(tx)])
                        .map_err(failed)?;
                    tx.len()
                }
                (None, Some(rx)) => {
                    let n = rx.len();
                    self.device
                        .transfer(&mut [SpiTransfer::read(rx)])
                        .map_err(failed)?;
                    n
                }
                (Some(tx), Some(rx)) => {
                    if tx.len() != rx.len() {
                        return Err(TransferError::Invalid(format!(
                            "spi duplex of {} bytes out, {} in",
                            tx.len(),
                            rx.len()
                        )));
                    }
                    let n = rx.len();
                    let segment = SpiTransfer::duplex(rx, tx).map_err(failed)?;
                    self.device.transfer(&mut [segment]).map_err(failed)?;
                    n
                }
            };
            Ok(Finished {
                bytes,
                _buffers: PhantomData,
            })
        }

        fn poll<'a>(&mut self, ticket: &mut Finished<'a>) -> TransferResult<Option<Completion>> {
            Ok(Some(ticket.completion()))
        }

        fn wait<'a>(
            &mut self,
            ticket: &mut Finished<'a>,
            _timeout: Option<Duration>,
        ) -> TransferResult<Option<Completion>> {
            Ok(Some(ticket.completion()))
        }

        fn cancel<'a>(&mut self, _ticket: Finished<'a>) -> TransferResult<()> {
            Ok(())
        }
    }
}

#[cfg(feature = "spi")]
pub use spi::SpidevEngine;

#[cfg(feature = "i2c")]
mod i2c {
    use super::*;
    use crate::hal::I2cBus;

    /// I2C through i2c-dev, to one target address: a transfer is one combined
    /// `I2C_RDWR`, with a repeated start between a write and a read.
    #[derive(Debug)]
    pub struct I2cEngine {
        bus: I2cBus,
        address: u16,
    }

    impl I2cEngine {
        /// Runs transfers to `address` on `bus`.
        pub fn new(bus: I2cBus, address: u16) -> Self {
            Self { bus, address }
        }

        /// The target address.
        pub fn address(&self) -> u16 {
            self.address
        }

        /// Gives back the bus.
        pub fn into_inner(self) -> I2cBus {
            self.bus
        }
    }

    impl TransferEngine for I2cEngine {
        type Ticket<'a> = Finished<'a>;

        fn submit<'a>(&mut self, transfer: Transfer<'a>) -> TransferResult<Finished<'a>> {
            let (source, sink) = memory_sides(transfer)?;
            let address = self.address;
            let bytes = match (source, sink) {
                (None, None) => 0,
                (Some(tx), None) => {
                    self.bus.write_bytes(address, tx).map_err(failed)?;
                    tx.len()
                }
                (None, Some(rx)) => {
                    let n = rx.len();
                    self.bus.read_bytes(address, rx).map_err(failed)?;
                    n
                }
                (Some(tx), Some(rx)) => {
                    let n = rx.len();
                    self.bus.write_read_bytes(address, tx, rx).map_err(failed)?;
                    n
                }
            };
            Ok(Finished {
                bytes,
                _buffers: PhantomData,
            })
        }

        fn poll<'a>(&mut self, ticket: &mut Finished<'a>) -> TransferResult<Option<Completion>> {
            Ok(Some(ticket.completion()))
        }

        fn wait<'a>(
            &mut self,
            ticket: &mut Finished<'a>,
            _timeout: Option<Duration>,
        ) -> TransferResult<Option<Completion>> {
            Ok(Some(ticket.completion()))
        }

        fn cancel<'a>(&mut self, _ticket: Finished<'a>) -> TransferResult<()> {
            Ok(())
        }
    }
}

#[cfg(feature = "i2c")]
pub use i2c::I2cEngine;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dma_buf_side_is_refused_before_the_bus_is_touched() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsFd::as_fd(&file);
        let t = Transfer::copy(
            Source::DmaBuf { fd, len: 4 },
            Sink::Memory(Box::leak(Box::new([0u8; 4]))),
        );
        let err = memory_sides(t).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn memory_sides_pass_through() {
        let src = [1u8, 2];
        let mut dst = [0u8; 2];
        let (tx, rx) =
            memory_sides(Transfer::copy(Source::Memory(&src), Sink::Memory(&mut dst))).unwrap();
        assert_eq!(tx, Some(&src[..]));
        assert!(rx.is_some());
    }

    #[test]
    fn io_errors_classify_by_errno() {
        // EBUSY on Linux.
        let busy = io::Error::from_raw_os_error(16);
        match failed(busy) {
            TransferError::Failed { kind, .. } => assert_eq!(kind, ErrorKind::Busy),
            other => panic!("{other:?}"),
        }
    }
}
