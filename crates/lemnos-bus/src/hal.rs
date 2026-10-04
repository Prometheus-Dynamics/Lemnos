//! embedded-hal views of Lemnos sessions.
//!
//! Lemnos device drivers are written against embedded-hal (see `lemnos-hal`);
//! these adapters let them run on any session the runtime opened, whatever the
//! backend (Linux, mock, a remote transport):
//!
//! - [`HalI2cBus`]: an [`I2cControllerSession`] as an embedded-hal [`I2c`] bus.
//! - [`HalI2cDevice`]: an [`I2cSession`] (one target) as an [`I2c`] bus that
//!   only talks to that target's address.
//! - [`HalSpiDevice`]: an [`SpiSession`] as a [`SpiDevice`]; a transaction is
//!   one full-duplex transfer, so chip select stays asserted throughout.
//! - [`HalPin`]: a [`GpioSession`] as an input/output pin.
//! - [`HalPwm`]: a [`PwmSession`] as a [`SetDutyCycle`] channel.
//!
//! [`BusError`] implements the embedded-hal error traits, so driver errors keep
//! their [`ErrorKind`](lemnos_core::ErrorKind) through
//! `lemnos_hal::HalError`.

use crate::{
    BusError, BusResult, GpioSession, I2cControllerSession, I2cSession, PwmSession, SpiSession,
};
use embedded_hal::digital::{ErrorType as PinErrorType, InputPin, OutputPin, StatefulOutputPin};
use embedded_hal::i2c::{ErrorType as I2cErrorType, I2c, Operation as I2cOp};
use embedded_hal::pwm::{ErrorType as PwmErrorType, SetDutyCycle};
use embedded_hal::spi::{ErrorType as SpiErrorType, Operation as SpiOp, SpiDevice};
use lemnos_core::{DeviceAddress, GpioLevel, I2cOperation};

/// Runs of an embedded-hal transaction with adjacent operations of one
/// direction merged (embedded-hal's contract: no restart between them).
fn merge_runs(operations: &[I2cOp<'_>]) -> Vec<I2cOperation> {
    let mut runs: Vec<I2cOperation> = Vec::new();
    for op in operations {
        match (op, runs.last_mut()) {
            (I2cOp::Write(data), Some(I2cOperation::Write { bytes })) => {
                bytes.extend_from_slice(data)
            }
            (I2cOp::Write(data), _) => runs.push(I2cOperation::Write {
                bytes: data.to_vec(),
            }),
            (I2cOp::Read(buf), Some(I2cOperation::Read { length })) => *length += buf.len() as u32,
            (I2cOp::Read(buf), _) => runs.push(I2cOperation::Read {
                length: buf.len() as u32,
            }),
        }
    }
    runs
}

/// Copies the read results of merged runs back into the read buffers.
fn scatter_reads(operations: &mut [I2cOp<'_>], results: &[Vec<u8>]) -> BusResult<()> {
    let mut reads = results.iter().filter(|r| !r.is_empty());
    let mut current: &[u8] = &[];
    for op in operations.iter_mut() {
        if let I2cOp::Read(buf) = op {
            if current.is_empty() {
                current = reads.next().map(Vec::as_slice).unwrap_or(&[]);
            }
            let n = buf.len().min(current.len());
            buf[..n].copy_from_slice(&current[..n]);
            current = &current[n..];
        }
    }
    Ok(())
}

/// An [`I2cControllerSession`] as an embedded-hal [`I2c`] bus.
pub struct HalI2cBus<'a, S: I2cControllerSession + ?Sized> {
    session: &'a mut S,
}

impl<'a, S: I2cControllerSession + ?Sized> HalI2cBus<'a, S> {
    /// Wraps a controller session.
    pub fn new(session: &'a mut S) -> Self {
        Self { session }
    }
}

impl<S: I2cControllerSession + ?Sized> I2cErrorType for HalI2cBus<'_, S> {
    type Error = BusError;
}

impl<S: I2cControllerSession + ?Sized> I2c for HalI2cBus<'_, S> {
    fn read(&mut self, address: u8, read: &mut [u8]) -> BusResult<()> {
        self.session.read_into(address.into(), read)
    }

    fn write(&mut self, address: u8, write: &[u8]) -> BusResult<()> {
        self.session.write(address.into(), write)
    }

    fn write_read(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        self.session.write_read_into(address.into(), write, read)
    }

    fn transaction(&mut self, address: u8, operations: &mut [I2cOp<'_>]) -> BusResult<()> {
        let runs = merge_runs(operations);
        let results = self.session.transaction(address.into(), &runs)?;
        scatter_reads(operations, &results)
    }
}

/// An [`I2cSession`] (one target device) as an embedded-hal [`I2c`] bus.
/// Operations addressed to any other address fail with
/// [`BusError::InvalidRequest`].
pub struct HalI2cDevice<'a, S: I2cSession + ?Sized> {
    session: &'a mut S,
}

impl<'a, S: I2cSession + ?Sized> HalI2cDevice<'a, S> {
    /// Wraps a device session.
    pub fn new(session: &'a mut S) -> Self {
        Self { session }
    }

    fn check(&self, address: u8, operation: &'static str) -> BusResult<()> {
        match &self.session.device().address {
            Some(DeviceAddress::I2cDevice { address: own, .. }) if *own != u16::from(address) => {
                Err(BusError::InvalidRequest {
                    device_id: self.session.device().id.clone(),
                    operation,
                    reason: format!("session is bound to address {own:#04x}, not {address:#04x}"),
                })
            }
            _ => Ok(()),
        }
    }
}

impl<S: I2cSession + ?Sized> I2cErrorType for HalI2cDevice<'_, S> {
    type Error = BusError;
}

impl<S: I2cSession + ?Sized> I2c for HalI2cDevice<'_, S> {
    fn read(&mut self, address: u8, read: &mut [u8]) -> BusResult<()> {
        self.check(address, "i2c.read")?;
        self.session.read_into(read)
    }

    fn write(&mut self, address: u8, write: &[u8]) -> BusResult<()> {
        self.check(address, "i2c.write")?;
        self.session.write(write)
    }

    fn write_read(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        self.check(address, "i2c.write_read")?;
        self.session.write_read_into(write, read)
    }

    fn transaction(&mut self, address: u8, operations: &mut [I2cOp<'_>]) -> BusResult<()> {
        self.check(address, "i2c.transaction")?;
        let runs = merge_runs(operations);
        let results = self.session.transaction(&runs)?;
        scatter_reads(operations, &results)
    }
}

/// An [`SpiSession`] as an embedded-hal [`SpiDevice`]. Each transaction is
/// flattened into one full-duplex transfer (chip select held throughout);
/// delays inside a transaction are not supported.
pub struct HalSpiDevice<'a, S: SpiSession + ?Sized> {
    session: &'a mut S,
}

impl<'a, S: SpiSession + ?Sized> HalSpiDevice<'a, S> {
    /// Wraps an SPI session.
    pub fn new(session: &'a mut S) -> Self {
        Self { session }
    }
}

impl<S: SpiSession + ?Sized> SpiErrorType for HalSpiDevice<'_, S> {
    type Error = BusError;
}

impl<S: SpiSession + ?Sized> SpiDevice for HalSpiDevice<'_, S> {
    fn transaction(&mut self, operations: &mut [SpiOp<'_, u8>]) -> BusResult<()> {
        let mut tx = Vec::new();
        let mut reads = false;
        for op in operations.iter() {
            match op {
                SpiOp::Write(data) => tx.extend_from_slice(data),
                SpiOp::Read(buf) => {
                    reads = true;
                    tx.resize(tx.len() + buf.len(), 0);
                }
                SpiOp::Transfer(read, write) => {
                    reads = true;
                    let n = read.len().max(write.len());
                    let start = tx.len();
                    tx.resize(start + n, 0);
                    tx[start..start + write.len()].copy_from_slice(write);
                }
                SpiOp::TransferInPlace(buf) => {
                    reads = true;
                    tx.extend_from_slice(buf);
                }
                SpiOp::DelayNs(0) => {}
                SpiOp::DelayNs(_) => {
                    return Err(BusError::InvalidRequest {
                        device_id: self.session.device().id.clone(),
                        operation: "spi.transaction",
                        reason: "delays inside an SPI transaction are not supported by sessions"
                            .into(),
                    });
                }
            }
        }
        if tx.is_empty() {
            return Ok(());
        }
        if !reads {
            return self.session.write(&tx);
        }
        let rx = self.session.transfer(&tx)?;
        let mut at = 0;
        for op in operations.iter_mut() {
            let take = |at: &mut usize, n: usize| {
                let start = (*at).min(rx.len());
                let end = (*at + n).min(rx.len());
                *at += n;
                &rx[start..end]
            };
            match op {
                SpiOp::Write(data) => at += data.len(),
                SpiOp::Read(buf) => {
                    let got = take(&mut at, buf.len());
                    buf[..got.len()].copy_from_slice(got);
                }
                SpiOp::Transfer(read, write) => {
                    let n = read.len().max(write.len());
                    let got = take(&mut at, n);
                    let m = read.len().min(got.len());
                    read[..m].copy_from_slice(&got[..m]);
                }
                SpiOp::TransferInPlace(buf) => {
                    let got = take(&mut at, buf.len());
                    buf[..got.len()].copy_from_slice(got);
                }
                SpiOp::DelayNs(_) => {}
            }
        }
        Ok(())
    }
}

/// A [`GpioSession`] as an embedded-hal pin (levels are logical: the line's
/// active-low setting applies).
pub struct HalPin<'a, S: GpioSession + ?Sized> {
    session: &'a mut S,
}

impl<'a, S: GpioSession + ?Sized> HalPin<'a, S> {
    /// Wraps a GPIO session.
    pub fn new(session: &'a mut S) -> Self {
        Self { session }
    }
}

impl<S: GpioSession + ?Sized> PinErrorType for HalPin<'_, S> {
    type Error = BusError;
}

impl<S: GpioSession + ?Sized> OutputPin for HalPin<'_, S> {
    fn set_low(&mut self) -> BusResult<()> {
        self.session.write_level(GpioLevel::Low)
    }

    fn set_high(&mut self) -> BusResult<()> {
        self.session.write_level(GpioLevel::High)
    }
}

impl<S: GpioSession + ?Sized> StatefulOutputPin for HalPin<'_, S> {
    fn is_set_high(&mut self) -> BusResult<bool> {
        Ok(self.session.read_level()? == GpioLevel::High)
    }

    fn is_set_low(&mut self) -> BusResult<bool> {
        Ok(self.session.read_level()? == GpioLevel::Low)
    }
}

impl<S: GpioSession + ?Sized> InputPin for HalPin<'_, S> {
    fn is_high(&mut self) -> BusResult<bool> {
        Ok(self.session.read_level()? == GpioLevel::High)
    }

    fn is_low(&mut self) -> BusResult<bool> {
        Ok(self.session.read_level()? == GpioLevel::Low)
    }
}

/// A [`PwmSession`] as an embedded-hal [`SetDutyCycle`] channel over its
/// current period (duty `u16::MAX` is 100 %).
pub struct HalPwm<'a, S: PwmSession + ?Sized> {
    session: &'a mut S,
}

impl<'a, S: PwmSession + ?Sized> HalPwm<'a, S> {
    /// Wraps a PWM session.
    pub fn new(session: &'a mut S) -> Self {
        Self { session }
    }
}

impl<S: PwmSession + ?Sized> PwmErrorType for HalPwm<'_, S> {
    type Error = BusError;
}

impl<S: PwmSession + ?Sized> SetDutyCycle for HalPwm<'_, S> {
    fn max_duty_cycle(&self) -> u16 {
        u16::MAX
    }

    fn set_duty_cycle(&mut self, duty: u16) -> BusResult<()> {
        let period = self.session.configuration()?.period_ns;
        let duty_ns = (u128::from(period) * u128::from(duty) / u128::from(u16::MAX)) as u64;
        self.session.set_duty_cycle_ns(duty_ns)
    }
}
