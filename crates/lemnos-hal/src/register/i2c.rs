use super::{
    AddressWidth, Endian, MAX_BURST, RegWrite, RegisterBus, RegisterError, RegisterResult, asynch,
    encode_address, pack_run,
};
use embedded_hal::i2c::{I2c, Operation};

/// A device's registers over I2C: register address (8 or 16 bits, big-endian),
/// then data; reads are one combined transfer (write the address, repeated
/// start, read).
///
/// Register writes go out as one message per transfer: on the Raspberry Pi
/// CM5 (RP1 DesignWare I2C) with an OV9782, packing several write messages
/// into one combined transfer lost or misplaced some of them, so
/// [`write_sequence`](RegisterBus::write_sequence) never does that. With
/// [`with_bursts`](Self::with_bursts), writes to consecutive registers share
/// one message (the device auto-increments), which avoids repeated starts
/// altogether.
///
/// Implements [`RegisterBus`] over a blocking [`I2c`] and
/// [`asynch::RegisterBus`] over an async [`embedded_hal_async::i2c::I2c`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I2cRegisters<I2C> {
    i2c: I2C,
    address: u8,
    width: AddressWidth,
    endian: Endian,
    burst: usize,
}

impl<I2C> I2cRegisters<I2C> {
    /// Registers of the device at 7-bit `address`, with `width` register
    /// addresses, big-endian values and no bursts.
    pub fn new(i2c: I2C, address: u8, width: AddressWidth) -> Self {
        Self {
            i2c,
            address,
            width,
            endian: Endian::Big,
            burst: 1,
        }
    }

    /// Joins writes to consecutive registers in a sequence into one transfer
    /// of up to `max_bytes` data bytes (clamped to [`MAX_BURST`]); `0` or `1`
    /// turns bursts off. Only for devices that auto-increment on writes.
    pub fn with_bursts(mut self, max_bytes: usize) -> Self {
        self.burst = max_bytes.clamp(1, MAX_BURST);
        self
    }

    /// Byte order of multi-byte values.
    pub fn with_endian(mut self, endian: Endian) -> Self {
        self.endian = endian;
        self
    }

    /// The device's 7-bit address.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// The register address width.
    pub fn width(&self) -> AddressWidth {
        self.width
    }

    /// The bus.
    pub fn i2c(&self) -> &I2C {
        &self.i2c
    }

    /// The bus, mutably (for raw transfers).
    pub fn i2c_mut(&mut self) -> &mut I2C {
        &mut self.i2c
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }

    fn header<E>(&self, address: u16) -> RegisterResult<([u8; 2], usize), E> {
        let mut buf = [0u8; 2];
        let n = encode_address(address, self.width, &mut buf)?;
        Ok((buf, n))
    }

    /// Address and data in one stack buffer, when they fit.
    fn packed<E>(
        &self,
        address: u16,
        data: &[u8],
    ) -> RegisterResult<Option<([u8; 2 + MAX_BURST], usize)>, E> {
        let (header, a) = self.header(address)?;
        if a + data.len() > 2 + MAX_BURST {
            return Ok(None);
        }
        let mut buf = [0u8; 2 + MAX_BURST];
        buf[..a].copy_from_slice(&header[..a]);
        buf[a..a + data.len()].copy_from_slice(data);
        Ok(Some((buf, a + data.len())))
    }
}

impl<I2C: I2c> RegisterBus for I2cRegisters<I2C> {
    type BusError = I2C::Error;

    fn endian(&self) -> Endian {
        self.endian
    }

    fn read_burst(&mut self, address: u16, buf: &mut [u8]) -> RegisterResult<(), I2C::Error> {
        let (header, n) = self.header(address)?;
        self.i2c
            .write_read(self.address, &header[..n], buf)
            .map_err(RegisterError::i2c)
    }

    fn write_burst(&mut self, address: u16, data: &[u8]) -> RegisterResult<(), I2C::Error> {
        if let Some((buf, len)) = self.packed(address, data)? {
            return self
                .i2c
                .write(self.address, &buf[..len])
                .map_err(RegisterError::i2c);
        }
        // Too long for the stack buffer: adjacent writes of one transaction
        // are a single contiguous write by embedded-hal's contract.
        let (header, n) = self.header(address)?;
        self.i2c
            .transaction(
                self.address,
                &mut [Operation::Write(&header[..n]), Operation::Write(data)],
            )
            .map_err(RegisterError::i2c)
    }

    fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), I2C::Error> {
        let mut buf = [0u8; 2 + MAX_BURST];
        let mut i = 0;
        while i < writes.len() {
            let (len, next) =
                pack_run(writes, i, self.width, self.endian, self.burst, 0, &mut buf)?;
            self.i2c
                .write(self.address, &buf[..len])
                .map_err(RegisterError::i2c)?;
            i = next;
        }
        Ok(())
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> asynch::RegisterBus for I2cRegisters<I2C> {
    type BusError = I2C::Error;

    fn endian(&self) -> Endian {
        self.endian
    }

    async fn read_burst(&mut self, address: u16, buf: &mut [u8]) -> RegisterResult<(), I2C::Error> {
        let (header, n) = self.header(address)?;
        self.i2c
            .write_read(self.address, &header[..n], buf)
            .await
            .map_err(RegisterError::i2c)
    }

    async fn write_burst(&mut self, address: u16, data: &[u8]) -> RegisterResult<(), I2C::Error> {
        if let Some((buf, len)) = self.packed(address, data)? {
            return self
                .i2c
                .write(self.address, &buf[..len])
                .await
                .map_err(RegisterError::i2c);
        }
        let (header, n) = self.header(address)?;
        self.i2c
            .transaction(
                self.address,
                &mut [
                    embedded_hal_async::i2c::Operation::Write(&header[..n]),
                    embedded_hal_async::i2c::Operation::Write(data),
                ],
            )
            .await
            .map_err(RegisterError::i2c)
    }

    async fn write_sequence(&mut self, writes: &[RegWrite]) -> RegisterResult<(), I2C::Error> {
        let mut buf = [0u8; 2 + MAX_BURST];
        let mut i = 0;
        while i < writes.len() {
            let (len, next) =
                pack_run(writes, i, self.width, self.endian, self.burst, 0, &mut buf)?;
            self.i2c
                .write(self.address, &buf[..len])
                .await
                .map_err(RegisterError::i2c)?;
            i = next;
        }
        Ok(())
    }
}
