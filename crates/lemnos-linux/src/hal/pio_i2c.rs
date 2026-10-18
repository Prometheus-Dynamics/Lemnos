//! An I2C master on the RP1 PIO block (feature `rp1`): the `pio-i2c:` board
//! selectors, as an embedded-hal bus next to [`I2cBus`](super::I2cBus).

use super::{IoError, invalid_input};
use crate::transport::i2c::pio::{self, PioBus, Segment};
use embedded_hal::i2c::{I2c, Operation};
use lemnos_core::PioI2cPins;
use std::io;
use std::sync::Arc;

/// An I2C master on the RP1 PIO block (`pio-i2c:` board selectors), as an
/// embedded-hal bus. Every handle on the same SDA/SCL pair shares one state
/// machine; transactions on it are serialized.
#[derive(Debug, Clone)]
pub struct PioI2cBus {
    bus: Arc<PioBus>,
}

impl PioI2cBus {
    /// Opens (or reuses) the PIO bus on `pins`. Fails when the pins are owned by
    /// a kernel driver, the device node is not accessible, or no state machine
    /// or instruction space is free.
    pub fn open(pins: PioI2cPins) -> io::Result<Self> {
        pio::open_shared(pins)
            .map(|bus| Self { bus })
            .map_err(io::Error::other)
    }
}

impl embedded_hal::i2c::ErrorType for PioI2cBus {
    type Error = IoError;
}

impl I2c for PioI2cBus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), IoError> {
        if address > 0x7f {
            return Err(
                invalid_input(format!("7-bit I2C address {address:#x} out of range")).into(),
            );
        }
        let mut segments = Vec::with_capacity(operations.len());
        for operation in operations.iter() {
            match operation {
                Operation::Read(buffer) => segments.push(Segment::Read(buffer.len())),
                Operation::Write(bytes) => segments.push(Segment::Write(bytes)),
            }
        }
        let data = pio::run_segments(&self.bus, address, &segments)?;
        let mut cursor = 0usize;
        for operation in operations.iter_mut() {
            let Operation::Read(buffer) = operation else {
                continue;
            };
            let end = cursor + buffer.len();
            let Some(chunk) = data.get(cursor..end) else {
                return Err(invalid_input("PIO I2C returned fewer bytes than read").into());
            };
            buffer.copy_from_slice(chunk);
            cursor = end;
        }
        Ok(())
    }
}

/// Completes synchronously, as [`I2cBus`] does.
impl embedded_hal_async::i2c::I2c for PioI2cBus {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), IoError> {
        I2c::transaction(self, address, operations)
    }
}
