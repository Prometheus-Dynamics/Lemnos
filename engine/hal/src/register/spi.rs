use super::{
    AddressWidth, Endian, MAX_BURST, RegWrite, RegisterBus, RegisterError, RegisterResult, asynch,
    encode_address, pack_run,
};
use embedded_hal::spi::{Operation, SpiDevice};

/// A device's registers over SPI: one transaction per access (chip select held
/// throughout) carrying the register address, with configurable flag bits
/// or-ed into it for reads, writes and multi-byte transfers (for example
/// `0x80` for reads on most Bosch and ST sensors, `0x40` for auto-increment on
/// some ST parts).
///
/// Implements [`RegisterBus`] over a blocking [`SpiDevice`] and
/// [`asynch::RegisterBus`] over an async [`embedded_hal_async::spi::SpiDevice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiRegisters<SPI> {
    spi: SPI,
    width: AddressWidth,
    endian: Endian,
    read_flag: u16,
    write_flag: u16,
    multi_flag: u16,
    burst: usize,
}

impl<SPI> SpiRegisters<SPI> {
    /// Registers with `width` addresses, read flag `0x80` (8-bit addresses) or
    /// `0x8000` (16-bit), no write or multi-byte flag, big-endian values.
    pub fn new(spi: SPI, width: AddressWidth) -> Self {
        Self {
            spi,
            width,
            endian: Endian::Big,
            read_flag: match width {
                AddressWidth::Bits8 => 0x80,
                AddressWidth::Bits16 => 0x8000,
            },
            write_flag: 0,
            multi_flag: 0,
            burst: 1,
        }
    }

    /// Bits or-ed into the address for reads and writes.
    pub fn with_flags(mut self, read: u16, write: u16) -> Self {
        self.read_flag = read;
        self.write_flag = write;
        self
    }

    /// Bits or-ed into the address of transfers longer than one byte.
    pub fn with_multi_byte_flag(mut self, flag: u16) -> Self {
        self.multi_flag = flag;
        self
    }

    /// Byte order of multi-byte values.
    pub fn with_endian(mut self, endian: Endian) -> Self {
        self.endian = endian;
        self
    }

    /// Joins writes to consecutive registers into one transaction of up to
    /// `max_bytes` data bytes (clamped to [`MAX_BURST`]).
    pub fn with_bursts(mut self, max_bytes: usize) -> Self {
        self.burst = max_bytes.clamp(1, MAX_BURST);
        self
    }

    /// The device.
    pub fn spi(&self) -> &SPI {
        &self.spi
    }

    /// The device, mutably.
    pub fn spi_mut(&mut self) -> &mut SPI {
        &mut self.spi
    }

    /// Gives the device back.
    pub fn release(self) -> SPI {
        self.spi
    }

    fn header<E>(
        &self,
        address: u16,
        flag: u16,
        len: usize,
    ) -> RegisterResult<([u8; 2], usize), E> {
        let multi = if len > 1 { self.multi_flag } else { 0 };
        let flagged = address | flag | multi;
        if flagged & !(flag | multi) != address {
            return Err(RegisterError::AddressTooWide(address));
        }
        let mut buf = [0u8; 2];
        let n = encode_address(flagged, self.width, &mut buf)?;
        Ok((buf, n))
    }

    /// Address flags of a packed burst (always multi-byte capable).
    fn sequence_flag(&self) -> u16 {
        self.write_flag | self.multi_flag
    }
}

impl<SPI: SpiDevice> RegisterBus for SpiRegisters<SPI> {
    type BusError = SPI::Error;

    fn endian(&self) -> Endian {
        self.endian
    }

    fn read_burst(&mut self, address: u16, buf: &mut [u8]) -> RegisterResult<(), SPI::Error> {
        let (header, n) = self.header(address, self.read_flag, buf.len())?;
        self.spi
            .transaction(&mut [Operation::Write(&header[..n]), Operation::Read(buf)])
            .map_err(RegisterError::spi)
    }

    fn write_burst(&mut self, address: u16, data: &[u8]) -> RegisterResult<(), SPI::Error> {
        let (header, n) = self.header(address, self.write_flag, data.len())?;
        self.spi
            .transaction(&mut [Operation::Write(&header[..n]), Operation::Write(data)])
            .map_err(RegisterError::spi)
    }

    fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), SPI::Error> {
        if self.burst == 1 {
            return writes
                .iter()
                .try_for_each(|w| self.write(w.address, w.bytes, w.value));
        }
        let mut buf = [0u8; 2 + MAX_BURST];
        let flag = self.sequence_flag();
        let mut i = 0;
        while i < writes.len() {
            let (len, next) = pack_run(
                writes,
                i,
                self.width,
                self.endian,
                self.burst,
                flag,
                &mut buf,
            )?;
            self.spi.write(&buf[..len]).map_err(RegisterError::spi)?;
            i = next;
        }
        Ok(())
    }
}

impl<SPI: embedded_hal_async::spi::SpiDevice> asynch::RegisterBus for SpiRegisters<SPI> {
    type BusError = SPI::Error;

    fn endian(&self) -> Endian {
        self.endian
    }

    async fn read_burst(&mut self, address: u16, buf: &mut [u8]) -> RegisterResult<(), SPI::Error> {
        let (header, n) = self.header(address, self.read_flag, buf.len())?;
        self.spi
            .transaction(&mut [Operation::Write(&header[..n]), Operation::Read(buf)])
            .await
            .map_err(RegisterError::spi)
    }

    async fn write_burst(&mut self, address: u16, data: &[u8]) -> RegisterResult<(), SPI::Error> {
        let (header, n) = self.header(address, self.write_flag, data.len())?;
        self.spi
            .transaction(&mut [Operation::Write(&header[..n]), Operation::Write(data)])
            .await
            .map_err(RegisterError::spi)
    }

    async fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), SPI::Error> {
        if self.burst == 1 {
            for w in writes {
                self.write(w.address, w.bytes, w.value).await?;
            }
            return Ok(());
        }
        let mut buf = [0u8; 2 + MAX_BURST];
        let flag = self.sequence_flag();
        let mut i = 0;
        while i < writes.len() {
            let (len, next) = pack_run(
                writes,
                i,
                self.width,
                self.endian,
                self.burst,
                flag,
                &mut buf,
            )?;
            self.spi
                .write(&buf[..len])
                .await
                .map_err(RegisterError::spi)?;
            i = next;
        }
        Ok(())
    }
}
