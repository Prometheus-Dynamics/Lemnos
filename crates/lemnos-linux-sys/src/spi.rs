//! spidev (`/dev/spidevB.C`): mode, word size, speed, bit order and
//! `SPI_IOC_MESSAGE` transfers.

use crate::ioctl::{ioc, ioctl_ptr, ior, iow};
use std::io;
use std::os::fd::BorrowedFd;

const SPI_IOC_MAGIC: u8 = b'k';

/// `SPI_CPHA`.
pub const MODE_CPHA: u32 = 0x01;
/// `SPI_CPOL`.
pub const MODE_CPOL: u32 = 0x02;
/// `SPI_CS_HIGH`.
pub const MODE_CS_HIGH: u32 = 0x04;
/// `SPI_LSB_FIRST`.
pub const MODE_LSB_FIRST: u32 = 0x08;
/// `SPI_3WIRE`.
pub const MODE_3WIRE: u32 = 0x10;
/// `SPI_NO_CS`.
pub const MODE_NO_CS: u32 = 0x40;

/// Most transfers in one `SPI_IOC_MESSAGE` (the size field holds 14 bits).
pub const MAX_TRANSFERS: usize = 511;

/// `struct spi_ioc_transfer`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct SpiIocTransfer {
    tx_buf: u64,
    rx_buf: u64,
    len: u32,
    speed_hz: u32,
    delay_usecs: u16,
    bits_per_word: u8,
    cs_change: u8,
    tx_nbits: u8,
    rx_nbits: u8,
    word_delay_usecs: u8,
    pad: u8,
}

/// One segment of an SPI message: bytes out, bytes in, or both, and settings
/// for this segment only.
#[derive(Debug)]
pub struct Transfer<'a> {
    tx: Option<&'a [u8]>,
    rx: Option<&'a mut [u8]>,
    in_place: bool,
    len: usize,
    /// Clock rate for this segment (0: the device's).
    pub speed_hz: u32,
    /// Microseconds to wait after this segment before the next (or before
    /// releasing chip select).
    pub delay_us: u16,
    /// Word size for this segment (0: the device's).
    pub bits_per_word: u8,
    /// Release chip select after this segment.
    pub cs_change: bool,
}

impl<'a> Transfer<'a> {
    fn new(tx: Option<&'a [u8]>, rx: Option<&'a mut [u8]>, in_place: bool, len: usize) -> Self {
        Self {
            tx,
            rx,
            in_place,
            len,
            speed_hz: 0,
            delay_us: 0,
            bits_per_word: 0,
            cs_change: false,
        }
    }

    /// Clocks `data` out (input ignored).
    pub fn write(data: &'a [u8]) -> Self {
        Self::new(Some(data), None, false, data.len())
    }

    /// Clocks `buf.len()` bytes in (zeros out).
    pub fn read(buf: &'a mut [u8]) -> Self {
        let len = buf.len();
        Self::new(None, Some(buf), false, len)
    }

    /// Clocks `tx` out and `rx` in; both must have the same length.
    pub fn duplex(rx: &'a mut [u8], tx: &'a [u8]) -> io::Result<Self> {
        if rx.len() != tx.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("SPI transfer of {} bytes in, {} out", rx.len(), tx.len()),
            ));
        }
        let len = tx.len();
        Ok(Self::new(Some(tx), Some(rx), false, len))
    }

    /// Clocks `buf` out and replaces it with the bytes clocked in.
    pub fn in_place(buf: &'a mut [u8]) -> Self {
        let len = buf.len();
        Self::new(None, Some(buf), true, len)
    }

    /// Waits `delay_us` with no data (chip select held).
    pub fn delay(delay_us: u16) -> Self {
        let mut t = Self::new(None, None, false, 0);
        t.delay_us = delay_us;
        t
    }

    /// Bytes this segment moves.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether it moves no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn raw(&mut self) -> io::Result<SpiIocTransfer> {
        let len = u32::try_from(self.len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "SPI transfer too long"))?;
        let rx_ptr = self.rx.as_mut().map_or(0, |b| b.as_mut_ptr() as u64);
        let tx_ptr = if self.in_place {
            rx_ptr
        } else {
            self.tx.map_or(0, |b| b.as_ptr() as u64)
        };
        Ok(SpiIocTransfer {
            tx_buf: tx_ptr,
            rx_buf: rx_ptr,
            len,
            speed_hz: self.speed_hz,
            delay_usecs: self.delay_us,
            bits_per_word: self.bits_per_word,
            cs_change: u8::from(self.cs_change),
            ..SpiIocTransfer::default()
        })
    }
}

/// Runs `transfers` as one message: chip select stays asserted from the first
/// segment to the last unless a segment sets `cs_change`.
pub fn transfer(fd: BorrowedFd<'_>, transfers: &mut [Transfer<'_>]) -> io::Result<()> {
    if transfers.is_empty() {
        return Ok(());
    }
    if transfers.len() > MAX_TRANSFERS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} SPI segments in one message", transfers.len()),
        ));
    }
    let mut raw = transfers
        .iter_mut()
        .map(Transfer::raw)
        .collect::<io::Result<Vec<_>>>()?;
    let request = ioc(1, SPI_IOC_MAGIC, 0, raw.len() * size_of::<SpiIocTransfer>());
    // SAFETY: `raw` is an array of `spi_ioc_transfer` whose length the request
    // encodes; its buffer pointers come from slices borrowed by `transfers`
    // (read buffers exclusively) that stay alive and unmoved during the call,
    // each at least `len` bytes long.
    unsafe { ioctl_ptr(fd, request, raw.as_mut_ptr()) }?;
    Ok(())
}

fn get<T: Default>(fd: BorrowedFd<'_>, nr: u8) -> io::Result<T> {
    let mut value = T::default();
    // SAFETY: the SPI_IOC_RD_* request `nr` writes one `T` (the type encoded
    // in the request) through the pointer to a live local.
    unsafe { ioctl_ptr(fd, ior::<T>(SPI_IOC_MAGIC, nr), &mut value) }?;
    Ok(value)
}

fn set<T>(fd: BorrowedFd<'_>, nr: u8, mut value: T) -> io::Result<()> {
    // SAFETY: the SPI_IOC_WR_* request `nr` reads one `T` (the type encoded in
    // the request) through the pointer to a live local.
    unsafe { ioctl_ptr(fd, iow::<T>(SPI_IOC_MAGIC, nr), &mut value) }?;
    Ok(())
}

/// The mode bits (`SPI_IOC_RD_MODE32`).
pub fn mode(fd: BorrowedFd<'_>) -> io::Result<u32> {
    get::<u32>(fd, 5)
}

/// Sets the mode bits (`SPI_IOC_WR_MODE32`).
pub fn set_mode(fd: BorrowedFd<'_>, mode: u32) -> io::Result<()> {
    set::<u32>(fd, 5, mode)
}

/// Whether bits go out least significant first (`SPI_IOC_RD_LSB_FIRST`).
pub fn lsb_first(fd: BorrowedFd<'_>) -> io::Result<bool> {
    get::<u8>(fd, 2).map(|v| v != 0)
}

/// Sets the bit order (`SPI_IOC_WR_LSB_FIRST`).
pub fn set_lsb_first(fd: BorrowedFd<'_>, lsb_first: bool) -> io::Result<()> {
    set::<u8>(fd, 2, u8::from(lsb_first))
}

/// Bits per word (`SPI_IOC_RD_BITS_PER_WORD`; 0 means 8).
pub fn bits_per_word(fd: BorrowedFd<'_>) -> io::Result<u8> {
    get::<u8>(fd, 3)
}

/// Sets bits per word (`SPI_IOC_WR_BITS_PER_WORD`).
pub fn set_bits_per_word(fd: BorrowedFd<'_>, bits: u8) -> io::Result<()> {
    set::<u8>(fd, 3, bits)
}

/// Maximum clock rate (`SPI_IOC_RD_MAX_SPEED_HZ`).
pub fn max_speed_hz(fd: BorrowedFd<'_>) -> io::Result<u32> {
    get::<u32>(fd, 4)
}

/// Sets the maximum clock rate (`SPI_IOC_WR_MAX_SPEED_HZ`).
pub fn set_max_speed_hz(fd: BorrowedFd<'_>, hz: u32) -> io::Result<()> {
    set::<u32>(fd, 4, hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_and_requests_match_the_kernel() {
        assert_eq!(size_of::<SpiIocTransfer>(), 32);
        assert_eq!(std::mem::offset_of!(SpiIocTransfer, len), 16);
        assert_eq!(std::mem::offset_of!(SpiIocTransfer, delay_usecs), 24);
        // SPI_IOC_MESSAGE(1), SPI_IOC_RD_MODE32, SPI_IOC_WR_MAX_SPEED_HZ.
        assert_eq!(ioc(1, SPI_IOC_MAGIC, 0, 32), 0x4020_6b00);
        assert_eq!(ior::<u32>(SPI_IOC_MAGIC, 5), 0x8004_6b05);
        assert_eq!(iow::<u32>(SPI_IOC_MAGIC, 4), 0x4004_6b04);
    }

    #[test]
    fn builds_segments() {
        let tx = [1u8, 2, 3];
        let mut rx = [0u8; 3];
        let mut t = Transfer::duplex(&mut rx, &tx).unwrap();
        t.cs_change = true;
        let raw = t.raw().unwrap();
        assert_eq!((raw.len, raw.cs_change), (3, 1));
        assert!(raw.tx_buf != 0 && raw.rx_buf != 0);
        let mut buf = [9u8; 2];
        let raw = Transfer::in_place(&mut buf).raw().unwrap();
        assert_eq!(raw.tx_buf, raw.rx_buf);
        let raw = Transfer::delay(10).raw().unwrap();
        assert_eq!((raw.len, raw.delay_usecs, raw.tx_buf), (0, 10, 0));
        let mut short = [0u8; 2];
        assert!(Transfer::duplex(&mut short, &tx).is_err());
    }
}
