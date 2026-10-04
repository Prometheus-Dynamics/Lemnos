use super::IoError;
use embedded_hal::spi::{ErrorType, Operation, SpiDevice};
use lemnos_linux_sys::spi as sys;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

pub use sys::Transfer as SpiTransfer;

/// Clock polarity and phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpiMode {
    /// CPOL 0, CPHA 0.
    Mode0,
    /// CPOL 0, CPHA 1.
    Mode1,
    /// CPOL 1, CPHA 0.
    Mode2,
    /// CPOL 1, CPHA 1.
    Mode3,
}

impl SpiMode {
    /// The CPOL/CPHA bits.
    pub const fn bits(self) -> u32 {
        match self {
            Self::Mode0 => 0,
            Self::Mode1 => sys::MODE_CPHA,
            Self::Mode2 => sys::MODE_CPOL,
            Self::Mode3 => sys::MODE_CPOL | sys::MODE_CPHA,
        }
    }

    /// The mode in the low two bits of `bits`.
    pub const fn from_bits(bits: u32) -> Self {
        match bits & 0x03 {
            0 => Self::Mode0,
            1 => Self::Mode1,
            2 => Self::Mode2,
            _ => Self::Mode3,
        }
    }
}

/// An SPI device through spidev (`/dev/spidevB.C`), as an embedded-hal
/// [`SpiDevice`]: each transaction is one `SPI_IOC_MESSAGE`, so the kernel
/// keeps chip select asserted from the first operation to the last.
#[derive(Debug)]
pub struct Spidev {
    file: File,
    path: PathBuf,
}

impl Spidev {
    /// Opens `/dev/spidev{bus}.{chip_select}`.
    pub fn open(bus: u32, chip_select: u16) -> io::Result<Self> {
        Self::open_path(format!("/dev/spidev{bus}.{chip_select}"))
    }

    /// Opens a spidev node by path.
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        Ok(Self { file, path })
    }

    /// The device node.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// All mode bits (`SPI_MODE_*`, `SPI_CS_HIGH`, `SPI_LSB_FIRST`, ...).
    pub fn mode_bits(&self) -> io::Result<u32> {
        sys::mode(self.file.as_fd())
    }

    /// Clock polarity and phase.
    pub fn mode(&self) -> io::Result<SpiMode> {
        self.mode_bits().map(SpiMode::from_bits)
    }

    /// Sets the clock polarity and phase, keeping the other mode bits.
    pub fn set_mode(&mut self, mode: SpiMode) -> io::Result<()> {
        let bits = (self.mode_bits()? & !0x03) | mode.bits();
        sys::set_mode(self.file.as_fd(), bits)
    }

    /// Maximum clock rate in hertz.
    pub fn max_speed_hz(&self) -> io::Result<u32> {
        sys::max_speed_hz(self.file.as_fd())
    }

    /// Sets the maximum clock rate.
    pub fn set_max_speed_hz(&mut self, hz: u32) -> io::Result<()> {
        sys::set_max_speed_hz(self.file.as_fd(), hz)
    }

    /// Bits per word (8 when the kernel reports 0).
    pub fn bits_per_word(&self) -> io::Result<u8> {
        sys::bits_per_word(self.file.as_fd()).map(|b| if b == 0 { 8 } else { b })
    }

    /// Sets bits per word.
    pub fn set_bits_per_word(&mut self, bits: u8) -> io::Result<()> {
        sys::set_bits_per_word(self.file.as_fd(), bits)
    }

    /// Whether bits go out least significant first.
    pub fn lsb_first(&self) -> io::Result<bool> {
        sys::lsb_first(self.file.as_fd())
    }

    /// Sets the bit order.
    pub fn set_lsb_first(&mut self, lsb_first: bool) -> io::Result<()> {
        sys::set_lsb_first(self.file.as_fd(), lsb_first)
    }

    /// Runs raw segments as one message.
    pub fn transfer(&mut self, transfers: &mut [SpiTransfer<'_>]) -> io::Result<()> {
        sys::transfer(self.file.as_fd(), transfers)
    }

    fn run(&mut self, operations: &mut [Operation<'_, u8>]) -> io::Result<()> {
        // Full-duplex transfers whose halves differ in length go through
        // padded scratch buffers (embedded-hal: the longer side wins).
        let mut scratch: Vec<(Vec<u8>, Vec<u8>)> = operations
            .iter()
            .filter_map(|op| match op {
                Operation::Transfer(read, write) if read.len() != write.len() => {
                    let n = read.len().max(write.len());
                    let mut tx = vec![0u8; n];
                    tx[..write.len()].copy_from_slice(write);
                    Some((tx, vec![0u8; n]))
                }
                _ => None,
            })
            .collect();
        {
            let mut spare = scratch.iter_mut();
            let mut segments: Vec<SpiTransfer<'_>> = Vec::with_capacity(operations.len());
            for op in operations.iter_mut() {
                match op {
                    Operation::Write(data) => segments.push(SpiTransfer::write(data)),
                    Operation::Read(buf) => segments.push(SpiTransfer::read(buf)),
                    Operation::Transfer(read, write) if read.len() == write.len() => {
                        segments.push(SpiTransfer::duplex(read, write)?)
                    }
                    Operation::Transfer(_, _) => {
                        let (tx, rx) = spare.next().ok_or_else(|| io::Error::other("scratch"))?;
                        segments.push(SpiTransfer::duplex(rx, tx)?);
                    }
                    Operation::TransferInPlace(buf) => segments.push(SpiTransfer::in_place(buf)),
                    Operation::DelayNs(ns) => {
                        let mut us = u64::from(*ns).div_ceil(1000);
                        while us > 0 {
                            let step = us.min(u64::from(u16::MAX)) as u16;
                            match segments.last_mut() {
                                Some(last) if last.delay_us == 0 => last.delay_us = step,
                                _ => segments.push(SpiTransfer::delay(step)),
                            }
                            us -= u64::from(step);
                        }
                    }
                }
            }
            sys::transfer(self.file.as_fd(), &mut segments)?;
        }
        let mut filled = scratch.iter();
        for op in operations.iter_mut() {
            if let Operation::Transfer(read, write) = op
                && read.len() != write.len()
                && let Some((_, rx)) = filled.next()
            {
                read.copy_from_slice(&rx[..read.len()]);
            }
        }
        Ok(())
    }
}

impl AsFd for Spidev {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl ErrorType for Spidev {
    type Error = IoError;
}

impl SpiDevice for Spidev {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), IoError> {
        self.run(operations).map_err(IoError::from)
    }
}

/// Completes synchronously (the kernel transfer blocks the calling thread).
impl embedded_hal_async::spi::SpiDevice for Spidev {
    async fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), IoError> {
        SpiDevice::transaction(self, operations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn open_missing_device_fails_cleanly() {
        let err = Spidev::open(250, 9).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
