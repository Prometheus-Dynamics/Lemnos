//! Transfers: one way to start, poll, wait for and cancel a transfer on a bus
//! or a DMA engine, over memory or dma-bufs.
//!
//! A transfer moves bytes from a [`Source`] to a [`Sink`]. Each side is plain
//! memory the CPU can reach, or a dma-buf the device reads or writes in place.
//! While a transfer is in flight its [`TransferEngine::Ticket`] borrows the
//! buffers, so they cannot be freed or reused before the transfer is finished
//! or cancelled.
//!
//! Who implements it: `lemnos_linux::transfer` has the synchronous buses (SPI
//! through spidev, I2C through i2c-dev). They move bytes through the CPU, so
//! they complete a transfer in `submit` and refuse dma-bufs. Planned, not
//! implemented: USB bulk endpoints (a hook for them when the USB transport
//! lands), PCIe DMA and RP1 DMA channels. The engines never decide what data
//! moves where: pools and buffer choices stay with the consumer, and `lemnosd`
//! arbitrates ownership of an engine but is not in the data path.

use std::os::fd::BorrowedFd;
use std::time::Duration;

use lemnos_core::ErrorKind;
use thiserror::Error;

/// Where a transfer's bytes come from.
#[derive(Clone, Copy, Debug)]
pub enum Source<'a> {
    /// Bytes in plain memory.
    Memory(&'a [u8]),
    /// A dma-buf the device reads, `len` bytes from its start.
    DmaBuf {
        /// The dma-buf's descriptor.
        fd: BorrowedFd<'a>,
        /// Bytes to read.
        len: usize,
    },
}

impl Source<'_> {
    /// Bytes this side moves.
    pub fn len(&self) -> usize {
        match self {
            Self::Memory(bytes) => bytes.len(),
            Self::DmaBuf { len, .. } => *len,
        }
    }

    /// Whether it moves no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Where a transfer's bytes go.
#[derive(Debug)]
pub enum Sink<'a> {
    /// Bytes into plain memory.
    Memory(&'a mut [u8]),
    /// A dma-buf the device writes, `len` bytes from its start.
    DmaBuf {
        /// The dma-buf's descriptor.
        fd: BorrowedFd<'a>,
        /// Bytes to write.
        len: usize,
    },
}

impl Sink<'_> {
    /// Bytes this side moves.
    pub fn len(&self) -> usize {
        match self {
            Self::Memory(bytes) => bytes.len(),
            Self::DmaBuf { len, .. } => *len,
        }
    }

    /// Whether it moves no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One transfer: an optional source and an optional sink. Both set is a
/// duplex transfer on a bus (SPI clocks both ways; I2C writes then reads after
/// a repeated start); a source alone is a write; a sink alone is a read; a
/// copy engine takes both.
#[derive(Debug)]
pub struct Transfer<'a> {
    /// Where the bytes come from, if anywhere.
    pub source: Option<Source<'a>>,
    /// Where the bytes go, if anywhere.
    pub sink: Option<Sink<'a>>,
}

impl<'a> Transfer<'a> {
    /// Writes `bytes`.
    pub fn write(bytes: &'a [u8]) -> Self {
        Self {
            source: Some(Source::Memory(bytes)),
            sink: None,
        }
    }

    /// Reads into `buf`.
    pub fn read(buf: &'a mut [u8]) -> Self {
        Self {
            source: None,
            sink: Some(Sink::Memory(buf)),
        }
    }

    /// Writes `write`, then reads into `read`.
    pub fn write_read(write: &'a [u8], read: &'a mut [u8]) -> Self {
        Self {
            source: Some(Source::Memory(write)),
            sink: Some(Sink::Memory(read)),
        }
    }

    /// Moves `source` to `sink`, whatever each side is.
    pub fn copy(source: Source<'a>, sink: Sink<'a>) -> Self {
        Self {
            source: Some(source),
            sink: Some(sink),
        }
    }
}

/// What a finished transfer moved: the sink's length, or the source's when
/// there is no sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Completion {
    /// Bytes moved.
    pub bytes: usize,
}

/// Why a transfer was not run or did not finish.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TransferError {
    /// The engine cannot do this kind of transfer (a dma-buf on a bus that
    /// moves bytes through the CPU, say).
    #[error("transfer not supported: {0}")]
    Unsupported(&'static str),
    /// The transfer is malformed (sides of different lengths, for one).
    #[error("invalid transfer: {0}")]
    Invalid(String),
    /// The engine or its device failed.
    #[error("transfer failed ({kind:?}): {reason}")]
    Failed {
        /// The portable classification.
        kind: ErrorKind,
        /// The underlying reason.
        reason: String,
    },
}

impl TransferError {
    /// The portable classification of the error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Unsupported(_) => ErrorKind::Unsupported,
            Self::Invalid(_) => ErrorKind::InvalidInput,
            Self::Failed { kind, .. } => *kind,
        }
    }
}

/// The result type of transfer calls.
pub type TransferResult<T> = Result<T, TransferError>;

/// Starts transfers on a bus or DMA engine and finishes them.
///
/// A ticket is one transfer in flight. [`Self::poll`] and [`Self::wait`] return
/// its [`Completion`] once it is done (`None` while it is not, or when the wait
/// times out); after that the ticket can be dropped, which returns the buffers.
/// [`Self::cancel`] stops a pending transfer where the engine can, and consumes
/// the ticket: an engine that cannot stop a running transfer waits for it
/// instead. A ticket dropped before it completes is a bug for hardware that
/// is still writing its buffers; use `cancel`.
pub trait TransferEngine {
    /// One transfer in flight. It borrows the buffers for `'a`.
    type Ticket<'a>;

    /// Starts `transfer`. Synchronous engines finish it here.
    fn submit<'a>(&mut self, transfer: Transfer<'a>) -> TransferResult<Self::Ticket<'a>>;

    /// The transfer's completion if it is done, without waiting.
    fn poll<'a>(&mut self, ticket: &mut Self::Ticket<'a>) -> TransferResult<Option<Completion>>;

    /// Waits until the transfer is done or `timeout` passes (`None`: forever).
    fn wait<'a>(
        &mut self,
        ticket: &mut Self::Ticket<'a>,
        timeout: Option<Duration>,
    ) -> TransferResult<Option<Completion>>;

    /// Stops the transfer where possible, and releases its buffers.
    fn cancel<'a>(&mut self, ticket: Self::Ticket<'a>) -> TransferResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference engine that copies in `submit`: it exercises the trait's
    /// contract without any hardware, and is not a backend.
    #[derive(Default)]
    struct CopyEngine;

    struct Done<'a> {
        bytes: usize,
        _buffers: std::marker::PhantomData<Transfer<'a>>,
    }

    impl TransferEngine for CopyEngine {
        type Ticket<'a> = Done<'a>;

        fn submit<'a>(&mut self, transfer: Transfer<'a>) -> TransferResult<Done<'a>> {
            let bytes = match (transfer.source, transfer.sink) {
                (Some(Source::Memory(src)), Some(Sink::Memory(dst))) => {
                    if src.len() != dst.len() {
                        return Err(TransferError::Invalid("unequal sides".into()));
                    }
                    dst.copy_from_slice(src);
                    dst.len()
                }
                (Some(Source::DmaBuf { .. }), _) | (_, Some(Sink::DmaBuf { .. })) => {
                    return Err(TransferError::Unsupported("dma-buf"));
                }
                _ => 0,
            };
            Ok(Done {
                bytes,
                _buffers: std::marker::PhantomData,
            })
        }

        fn poll<'a>(&mut self, ticket: &mut Done<'a>) -> TransferResult<Option<Completion>> {
            Ok(Some(Completion {
                bytes: ticket.bytes,
            }))
        }

        fn wait<'a>(
            &mut self,
            ticket: &mut Done<'a>,
            _timeout: Option<Duration>,
        ) -> TransferResult<Option<Completion>> {
            Ok(Some(Completion {
                bytes: ticket.bytes,
            }))
        }

        fn cancel<'a>(&mut self, _ticket: Done<'a>) -> TransferResult<()> {
            Ok(())
        }
    }

    #[test]
    fn a_memory_copy_completes_through_the_ticket() {
        let src = [1u8, 2, 3, 4];
        let mut dst = [0u8; 4];
        let mut engine = CopyEngine;
        let mut ticket = engine
            .submit(Transfer::copy(Source::Memory(&src), Sink::Memory(&mut dst)))
            .unwrap();
        assert_eq!(
            engine.poll(&mut ticket).unwrap(),
            Some(Completion { bytes: 4 })
        );
        assert_eq!(
            engine.wait(&mut ticket, None).unwrap(),
            Some(Completion { bytes: 4 })
        );
        engine.cancel(ticket).unwrap();
        assert_eq!(dst, src);
    }

    #[test]
    fn write_and_read_constructors_leave_the_other_side_empty() {
        let w = [9u8; 2];
        let t = Transfer::write(&w);
        assert!(t.sink.is_none());
        assert_eq!(t.source.map(|s| s.len()), Some(2));
        let mut r = [0u8; 3];
        let t = Transfer::read(&mut r);
        assert!(t.source.is_none());
        assert_eq!(t.sink.map(|s| s.len()), Some(3));
    }

    #[test]
    fn a_dma_buf_side_is_refused_by_an_engine_without_one() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let mut engine = CopyEngine;
        let err = engine
            .submit(Transfer::copy(
                Source::DmaBuf {
                    fd: std::os::fd::AsFd::as_fd(&file),
                    len: 16,
                },
                Sink::Memory(&mut [0u8; 16]),
            ))
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn errors_classify_for_the_portable_error_kind() {
        assert_eq!(
            TransferError::Invalid("x".into()).kind(),
            ErrorKind::InvalidInput
        );
        let failed = TransferError::Failed {
            kind: ErrorKind::Busy,
            reason: "claimed".into(),
        };
        assert_eq!(failed.kind(), ErrorKind::Busy);
    }
}
