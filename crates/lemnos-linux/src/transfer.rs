//! Transfer engines over the Linux buses (`lemnos_bus::transfer`): SPI through
//! spidev and I2C through i2c-dev.
//!
//! These buses move bytes through the CPU, so an engine runs the transfer in
//! `submit`: the ticket is already complete, `poll` and `wait` report it at
//! once, and `cancel` has nothing to stop. dma-bufs are refused with
//! [`TransferError::Unsupported`]. USB bulk (feature `usb`) is asynchronous:
//! its ticket holds the URB until the kernel finishes it. A PCIe or RP1 DMA
//! engine implements the same trait when it lands.

#[cfg(any(feature = "i2c", feature = "spi"))]
use std::io;
#[cfg(any(feature = "i2c", feature = "spi"))]
use std::marker::PhantomData;
use std::time::Duration;

use lemnos_bus::{
    Completion, Sink, Source, Transfer, TransferEngine, TransferError, TransferResult,
};
use lemnos_hal::ErrorKind;

/// The ticket of a transfer that finished in `submit`.
#[cfg(any(feature = "i2c", feature = "spi"))]
pub struct Finished<'a> {
    bytes: usize,
    _buffers: PhantomData<&'a ()>,
}

#[cfg(any(feature = "i2c", feature = "spi"))]
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

#[cfg(any(feature = "i2c", feature = "spi"))]
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

#[cfg(feature = "usb")]
mod usb {
    use super::*;
    use lemnos_usb::{
        ErrorKind as UsbErrorKind, Transfer as UsbTransfer, TransferOutcome, UsbDevice,
    };

    /// USB bulk through usbdevfs: one bulk endpoint of a claimed interface, one
    /// direction (an endpoint is OUT or IN). A transfer is an URB. Its buffer
    /// is owned by the USB layer while the transfer is in flight, so the
    /// caller's slice is copied in at `submit` (OUT) or copied out when the
    /// transfer settles (IN). Both copies are on the caller's thread, in
    /// `poll`, `wait` or `submit`; a ticket that is dropped or cancelled
    /// leaves the caller's buffer alone.
    pub struct UsbBulkEngine {
        device: UsbDevice,
        endpoint: u8,
    }

    impl UsbBulkEngine {
        /// Runs transfers on bulk endpoint `endpoint` of `device`.
        pub fn new(device: UsbDevice, endpoint: u8) -> Self {
            Self { device, endpoint }
        }

        /// The endpoint address (`bEndpointAddress`).
        pub fn endpoint(&self) -> u8 {
            self.endpoint
        }

        /// The device.
        pub fn device(&self) -> &UsbDevice {
            &self.device
        }

        /// Gives back the device.
        pub fn into_inner(self) -> UsbDevice {
            self.device
        }
    }

    /// The ticket of a USB bulk transfer: the URB while it is in flight, the
    /// caller's buffer to copy an IN transfer into, and the completion once
    /// it has settled.
    pub struct UsbTicket<'a> {
        transfer: Option<UsbTransfer>,
        sink: Option<&'a mut [u8]>,
        done: Option<Completion>,
    }

    impl UsbTicket<'_> {
        fn settle(&mut self, outcome: TransferOutcome) -> TransferResult<Completion> {
            let completion = settle_outcome(outcome, self.sink.take())?;
            self.done = Some(completion);
            Ok(completion)
        }
    }

    impl TransferEngine for UsbBulkEngine {
        type Ticket<'a> = UsbTicket<'a>;

        fn submit<'a>(&mut self, transfer: Transfer<'a>) -> TransferResult<UsbTicket<'a>> {
            let (source, sink) = memory_sides(transfer)?;
            check_one_direction(source.is_some(), sink.is_some())?;
            match (source, sink) {
                (None, None) => Ok(UsbTicket {
                    transfer: None,
                    sink: None,
                    done: Some(Completion { bytes: 0 }),
                }),
                (Some(tx), None) => {
                    let urb = self
                        .device
                        .submit_bulk_out(self.endpoint, tx.to_vec())
                        .map_err(usb_failed)?;
                    Ok(UsbTicket {
                        transfer: Some(urb),
                        sink: None,
                        done: None,
                    })
                }
                (None, Some(rx)) => {
                    let urb = self
                        .device
                        .submit_bulk_in(self.endpoint, vec![0; rx.len()])
                        .map_err(usb_failed)?;
                    Ok(UsbTicket {
                        transfer: Some(urb),
                        sink: Some(rx),
                        done: None,
                    })
                }
                (Some(_), Some(_)) => Err(one_direction_refused()),
            }
        }

        fn poll<'a>(&mut self, ticket: &mut UsbTicket<'a>) -> TransferResult<Option<Completion>> {
            if let Some(done) = ticket.done {
                return Ok(Some(done));
            }
            let urb = ticket.transfer.take().ok_or_else(already_settled)?;
            if !urb.is_complete() {
                ticket.transfer = Some(urb);
                return Ok(None);
            }
            let outcome = urb.wait().map_err(usb_failed)?;
            ticket.settle(outcome).map(Some)
        }

        fn wait<'a>(
            &mut self,
            ticket: &mut UsbTicket<'a>,
            timeout: Option<Duration>,
        ) -> TransferResult<Option<Completion>> {
            if let Some(done) = ticket.done {
                return Ok(Some(done));
            }
            let urb = ticket.transfer.take().ok_or_else(already_settled)?;
            match timeout {
                None => {
                    let outcome = urb.wait().map_err(usb_failed)?;
                    ticket.settle(outcome).map(Some)
                }
                Some(limit) => match urb.wait_timeout(limit) {
                    Ok(outcome) => {
                        let outcome = outcome.map_err(usb_failed)?;
                        ticket.settle(outcome).map(Some)
                    }
                    Err(still_in_flight) => {
                        ticket.transfer = Some(still_in_flight);
                        Ok(None)
                    }
                },
            }
        }

        fn cancel<'a>(&mut self, ticket: UsbTicket<'a>) -> TransferResult<()> {
            if let Some(urb) = ticket.transfer {
                urb.cancel();
            }
            Ok(())
        }
    }

    /// The bytes an IN transfer copies into the caller's buffer, and the count
    /// an OUT transfer reports. An IN transfer fills at most the buffer's length.
    fn settle_outcome(
        outcome: TransferOutcome,
        sink: Option<&mut [u8]>,
    ) -> TransferResult<Completion> {
        // A transfer that did not complete moves nothing: its status is the error.
        if let Some(error) = outcome.status.to_error("USB bulk transfer") {
            return Err(usb_failed(error));
        }
        let moved = outcome.actual_length.min(outcome.buffer.len());
        match sink {
            Some(dst) => {
                let n = moved.min(dst.len());
                dst[..n].copy_from_slice(&outcome.buffer.as_slice()[..n]);
                Ok(Completion { bytes: n })
            }
            None => Ok(Completion { bytes: moved }),
        }
    }

    /// A bulk endpoint is one direction: a transfer with both sides is refused.
    fn check_one_direction(has_source: bool, has_sink: bool) -> TransferResult<()> {
        if has_source && has_sink {
            Err(one_direction_refused())
        } else {
            Ok(())
        }
    }

    fn one_direction_refused() -> TransferError {
        TransferError::Invalid("a USB bulk endpoint is OUT or IN, not both in one transfer".into())
    }

    fn already_settled() -> TransferError {
        TransferError::Invalid("the transfer already settled".into())
    }

    /// Maps a USB error onto the bus's kinds: the same meaning where one
    /// exists; a stall or a cancellation is a failure of the endpoint.
    fn usb_failed(error: lemnos_usb::Error) -> TransferError {
        let kind = match error.kind() {
            UsbErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            UsbErrorKind::Busy => ErrorKind::Busy,
            UsbErrorKind::Disconnected => ErrorKind::Unavailable,
            UsbErrorKind::Unsupported => ErrorKind::Unsupported,
            UsbErrorKind::NotFound => ErrorKind::NotFound,
            UsbErrorKind::Timeout => ErrorKind::Timeout,
            UsbErrorKind::InvalidInput => ErrorKind::InvalidInput,
            _ => ErrorKind::Failed,
        };
        TransferError::Failed {
            kind,
            reason: error.to_string(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use lemnos_usb::{IsoPacket, TransferBuffer};

        fn outcome(buffer: Vec<u8>, actual_length: usize) -> TransferOutcome {
            TransferOutcome {
                buffer: TransferBuffer::from_vec(buffer),
                actual_length,
                packets: Vec::<IsoPacket>::new(),
                status: lemnos_usb::TransferStatus::Ok,
                reaped: std::time::Instant::now(),
            }
        }

        #[test]
        fn an_in_transfer_copies_what_moved_into_the_callers_buffer() {
            let mut dst = [0u8; 6];
            let done = settle_outcome(outcome(vec![1, 2, 3, 4], 3), Some(&mut dst)).unwrap();
            assert_eq!(done.bytes, 3);
            assert_eq!(dst, [1, 2, 3, 0, 0, 0]);
        }

        #[test]
        fn an_in_transfer_never_writes_past_a_shorter_caller_buffer() {
            let mut dst = [0u8; 2];
            let done = settle_outcome(outcome(vec![9, 8, 7, 6], 4), Some(&mut dst)).unwrap();
            assert_eq!(done.bytes, 2);
            assert_eq!(dst, [9, 8]);
        }

        #[test]
        fn an_out_transfer_reports_the_bytes_the_device_took() {
            let done = settle_outcome(outcome(vec![1, 2, 3], 3), None).unwrap();
            assert_eq!(done.bytes, 3);
            let short = settle_outcome(outcome(vec![1, 2, 3], 1), None).unwrap();
            assert_eq!(short.bytes, 1);
        }

        #[test]
        fn a_bulk_transfer_with_both_sides_is_refused() {
            assert!(check_one_direction(true, false).is_ok());
            assert!(check_one_direction(false, true).is_ok());
            assert!(check_one_direction(false, false).is_ok());
            assert!(matches!(
                check_one_direction(true, true),
                Err(TransferError::Invalid(_))
            ));
        }

        #[test]
        fn usb_errors_keep_their_meaning_on_the_bus() {
            let kind_of = |kind: UsbErrorKind| match usb_failed(lemnos_usb::Error::new(kind, "x")) {
                TransferError::Failed { kind, .. } => kind,
                other => panic!("not a failure: {other:?}"),
            };
            assert_eq!(kind_of(UsbErrorKind::Timeout), ErrorKind::Timeout);
            assert_eq!(kind_of(UsbErrorKind::Busy), ErrorKind::Busy);
            assert_eq!(kind_of(UsbErrorKind::Disconnected), ErrorKind::Unavailable);
            assert_eq!(kind_of(UsbErrorKind::Stall), ErrorKind::Failed);
            assert_eq!(kind_of(UsbErrorKind::Cancelled), ErrorKind::Failed);
        }
    }
}

#[cfg(feature = "usb")]
pub use usb::{UsbBulkEngine, UsbTicket};
