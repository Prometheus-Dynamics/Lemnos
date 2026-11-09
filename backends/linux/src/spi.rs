//! An SPI device through spidev.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::path::Path;

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{RawSpi, SpiSegment};
use lemnos_hal::spi::{ErrorType, Operation, SpiDevice};
use lemnos_linux_sys::spi as sys;

use crate::{Claim, kind_of};

pub use sys::Transfer;

/// Segments of a message built on the stack.
const STACK_SEGMENTS: usize = 16;

/// Clock polarity and phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpiMode {
    Mode0,
    Mode1,
    Mode2,
    Mode3,
}

impl SpiMode {
    pub const fn bits(self) -> u32 {
        match self {
            Self::Mode0 => 0,
            Self::Mode1 => sys::MODE_CPHA,
            Self::Mode2 => sys::MODE_CPOL,
            Self::Mode3 => sys::MODE_CPOL | sys::MODE_CPHA,
        }
    }

    pub const fn from_bits(bits: u32) -> Self {
        match bits & 0x03 {
            0 => Self::Mode0,
            1 => Self::Mode1,
            2 => Self::Mode2,
            _ => Self::Mode3,
        }
    }
}

/// `/dev/spidevB.C`. A transaction is one `SPI_IOC_MESSAGE`, so chip select stays asserted from
/// the first operation to the last.
pub struct Spidev {
    file: File,
    /// The mode the device was last seen in.
    mode: Option<SpiMode>,
    _claim: Option<Claim>,
}

fn slots<'s, 'a>(
    stack: &'s mut [Transfer<'a>; STACK_SEGMENTS],
    heap: &'s mut Vec<Transfer<'a>>,
    needed: usize,
) -> &'s mut [Transfer<'a>] {
    if needed <= STACK_SEGMENTS {
        &mut stack[..needed]
    } else {
        heap.resize_with(needed, || Transfer::delay(0));
        heap
    }
}

/// How many segments a delay can take: each carries up to `u16::MAX` microseconds.
const fn delay_segments(ns: u32) -> usize {
    (ns as usize).div_ceil(1000).div_ceil(u16::MAX as usize)
}

/// A pair of padded buffers, for a full-duplex transfer whose halves differ in length. The longer
/// side wins and the shorter is padded with zeros.
fn padded(tx: &[u8], longest: usize) -> (Vec<u8>, Vec<u8>) {
    let mut padded = vec![0; longest];
    padded[..tx.len()].copy_from_slice(tx);
    (padded, vec![0; longest])
}

impl Spidev {
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            file,
            mode: None,
            _claim: None,
        })
    }

    pub(crate) fn holding(mut self, claim: Claim) -> Self {
        self._claim = Some(claim);
        self
    }

    pub fn mode(&self) -> io::Result<SpiMode> {
        sys::mode(self.file.as_fd()).map(SpiMode::from_bits)
    }

    /// Sets the clock polarity and phase, keeping the other mode bits.
    pub fn set_mode(&mut self, mode: SpiMode) -> io::Result<()> {
        let bits = (sys::mode(self.file.as_fd())? & !0x03) | mode.bits();
        sys::set_mode(self.file.as_fd(), bits)?;
        self.mode = Some(mode);
        Ok(())
    }

    fn run(&mut self, operations: &mut [Operation<'_, u8>]) -> io::Result<()> {
        // Only unequal halves need these, and an empty Vec allocates nothing.
        let mut scratch: Vec<(Vec<u8>, Vec<u8>)> = operations
            .iter()
            .filter_map(|operation| match operation {
                Operation::Transfer(read, write) if read.len() != write.len() => {
                    Some(padded(write, read.len().max(write.len())))
                }
                _ => None,
            })
            .collect();
        let needed = operations
            .iter()
            .map(|operation| match operation {
                Operation::DelayNs(ns) => delay_segments(*ns),
                _ => 1,
            })
            .sum();
        {
            let mut stack: [Transfer<'_>; STACK_SEGMENTS] =
                std::array::from_fn(|_| Transfer::delay(0));
            let mut heap = Vec::new();
            let segments = slots(&mut stack, &mut heap, needed);
            let mut spare = scratch.iter_mut();
            let mut count = 0usize;
            for operation in operations.iter_mut() {
                let segment = match operation {
                    Operation::Write(data) => Transfer::write(data),
                    Operation::Read(buffer) => Transfer::read(buffer),
                    Operation::Transfer(read, write) if read.len() == write.len() => {
                        Transfer::duplex(read, write)?
                    }
                    Operation::Transfer(_, _) => {
                        let (tx, rx) = spare.next().ok_or(io::ErrorKind::InvalidInput)?;
                        Transfer::duplex(rx, tx)?
                    }
                    Operation::TransferInPlace(buffer) => Transfer::in_place(buffer),
                    Operation::DelayNs(ns) => {
                        let mut us = (*ns as usize).div_ceil(1000);
                        while us > 0 {
                            let step = us.min(usize::from(u16::MAX));
                            match count.checked_sub(1).map(|last| &mut segments[last]) {
                                Some(last) if last.delay_us == 0 => last.delay_us = step as u16,
                                _ => {
                                    segments[count] = Transfer::delay(step as u16);
                                    count += 1;
                                }
                            }
                            us -= step;
                        }
                        continue;
                    }
                };
                segments[count] = segment;
                count += 1;
            }
            sys::transfer(self.file.as_fd(), &mut segments[..count])?;
        }
        let mut filled = scratch.iter();
        for operation in operations.iter_mut() {
            if let Operation::Transfer(read, write) = operation
                && read.len() != write.len()
                && let Some((_, rx)) = filled.next()
            {
                read.copy_from_slice(&rx[..read.len()]);
            }
        }
        Ok(())
    }

    fn select_mode(&mut self, mode: SpiMode) -> io::Result<()> {
        if self.mode != Some(mode) {
            if self.mode()? != mode {
                self.set_mode(mode)?;
            }
            self.mode = Some(mode);
        }
        Ok(())
    }

    fn run_raw(&mut self, segments: &mut [SpiSegment<'_>]) -> io::Result<()> {
        let Some(first) = segments.first() else {
            return Ok(());
        };
        let mode = first.config.mode;
        if segments.iter().any(|s| s.config.mode != mode) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.select_mode(SpiMode::from_bits(u32::from(mode.bits())))?;
        let unequal =
            |s: &SpiSegment<'_>| !s.tx.is_empty() && !s.rx.is_empty() && s.tx.len() != s.rx.len();
        let mut scratch: Vec<(Vec<u8>, Vec<u8>)> = segments
            .iter()
            .filter(|s| unequal(s))
            .map(|s| padded(s.tx, s.tx.len().max(s.rx.len())))
            .collect();
        {
            let mut stack: [Transfer<'_>; STACK_SEGMENTS] =
                std::array::from_fn(|_| Transfer::delay(0));
            let mut heap = Vec::new();
            let transfers = slots(&mut stack, &mut heap, segments.len());
            let mut spare = scratch.iter_mut();
            for (segment, transfer) in segments.iter_mut().zip(transfers.iter_mut()) {
                let mut built = if !segment.tx.is_empty() && !segment.rx.is_empty() {
                    if segment.tx.len() == segment.rx.len() {
                        Transfer::duplex(segment.rx, segment.tx)?
                    } else {
                        let (tx, rx) = spare.next().ok_or(io::ErrorKind::InvalidInput)?;
                        Transfer::duplex(rx, tx)?
                    }
                } else if !segment.tx.is_empty() {
                    Transfer::write(segment.tx)
                } else if !segment.rx.is_empty() {
                    Transfer::read(segment.rx)
                } else {
                    Transfer::delay(0)
                };
                built.speed_hz = segment.config.speed_hz;
                built.bits_per_word = segment.config.bits_per_word;
                built.cs_change = segment.cs_change;
                built.delay_us = segment.delay_us;
                *transfer = built;
            }
            sys::transfer(self.file.as_fd(), transfers)?;
        }
        let mut filled = scratch.iter();
        for segment in segments.iter_mut() {
            if unequal(segment)
                && let Some((_, rx)) = filled.next()
            {
                let n = segment.rx.len();
                segment.rx.copy_from_slice(&rx[..n]);
            }
        }
        Ok(())
    }
}

impl ErrorType for Spidev {
    type Error = ErrorKind;
}

impl SpiDevice for Spidev {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), ErrorKind> {
        self.run(operations).map_err(|e| kind_of(&e))
    }
}

impl RawSpi for Spidev {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Result<(), ErrorKind> {
        self.run_raw(segments).map_err(|e| kind_of(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_measure::test_alloc::allocations;

    #[test]
    fn modes_round_trip() {
        for mode in [
            SpiMode::Mode0,
            SpiMode::Mode1,
            SpiMode::Mode2,
            SpiMode::Mode3,
        ] {
            assert_eq!(SpiMode::from_bits(mode.bits() | sys::MODE_CS_HIGH), mode);
        }
    }

    #[test]
    fn opening_a_missing_device_fails_cleanly() {
        let error = Spidev::open_path("/dev/spidev250.9").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_delay_counts_the_segments_it_can_take() {
        assert_eq!(delay_segments(0), 0);
        assert_eq!(delay_segments(1), 1);
        assert_eq!(delay_segments(65_535_000), 1);
        assert_eq!(delay_segments(65_535_001), 2);
        assert_eq!(delay_segments(u32::MAX), 66);
    }

    fn null_device() -> Spidev {
        Spidev {
            file: File::open("/dev/null").unwrap(),
            mode: None,
            _claim: None,
        }
    }

    #[test]
    fn a_long_delay_after_many_operations_fits_its_segments() {
        // /dev/null refuses the message once it is built. Overrunning the slots would panic.
        let mut device = null_device();
        let mut ops = [
            Operation::DelayNs(u32::MAX),
            Operation::Write(&[1]),
            Operation::DelayNs(u32::MAX),
        ];
        assert!(device.transaction(&mut ops).is_err());
    }

    #[test]
    fn building_a_message_allocates_nothing() {
        let mut device = null_device();
        let tx = [1, 2, 3];
        let mut rx = [0; 3];
        let (result, n) = allocations(|| {
            device.transaction(&mut [
                Operation::Write(&[1]),
                Operation::Transfer(&mut rx, &tx),
                Operation::DelayNs(1000),
            ])
        });
        assert!(result.is_err());
        assert_eq!(n, 0);
    }
}
