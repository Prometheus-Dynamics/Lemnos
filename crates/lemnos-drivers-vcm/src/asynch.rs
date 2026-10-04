//! The async VCM driver, over embedded-hal-async I2C.

use crate::{DEFAULT_ADDRESS, FormatError, State, VcmChip, VcmError, VcmFormat};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;

/// A VCM over an async I2C bus; see [`crate::Vcm`].
#[derive(Debug)]
pub struct Vcm<'a, I2C> {
    i2c: I2C,
    state: State<'a>,
}

impl<I2C: I2c> Vcm<'static, I2C> {
    /// A built-in chip at [`DEFAULT_ADDRESS`].
    pub fn chip(i2c: I2C, chip: VcmChip) -> Self {
        Self {
            i2c,
            state: State::new(DEFAULT_ADDRESS, chip.format()).expect("built-in formats are valid"),
        }
    }
}

impl<'a, I2C: I2c> Vcm<'a, I2C> {
    /// A VCM at 7-bit `address` driven with `format`.
    pub fn new(i2c: I2C, address: u8, format: VcmFormat<'a>) -> Result<Self, FormatError> {
        Ok(Self {
            i2c,
            state: State::new(address, format)?,
        })
    }

    /// The last position written (clamped), if any.
    pub fn position(&self) -> Option<i32> {
        self.state.position
    }

    /// Whether [`power_up`](Self::power_up) ran last.
    pub fn is_powered(&self) -> bool {
        self.state.powered
    }

    /// Sends the power-up writes and waits the chip's settle time.
    pub async fn power_up(&mut self, delay: &mut impl DelayNs) -> Result<(), VcmError<I2C::Error>> {
        for message in self.state.format.power_up.iter().filter(|m| !m.is_empty()) {
            self.i2c
                .write(self.state.address, message)
                .await
                .map_err(VcmError::I2c)?;
        }
        if self.state.format.power_up_us > 0 {
            delay.delay_us(self.state.format.power_up_us).await;
        }
        self.state.powered = true;
        Ok(())
    }

    /// Sends the standby writes.
    pub async fn power_down(&mut self) -> Result<(), VcmError<I2C::Error>> {
        for message in self
            .state
            .format
            .power_down
            .iter()
            .filter(|m| !m.is_empty())
        {
            self.i2c
                .write(self.state.address, message)
                .await
                .map_err(VcmError::I2c)?;
        }
        self.state.powered = false;
        Ok(())
    }

    /// Moves to `position` (clamped); returns the position written.
    pub async fn move_to(&mut self, position: i32) -> Result<i32, VcmError<I2C::Error>> {
        let (message, len, clamped) = self.state.format.encode(position);
        self.i2c
            .write(self.state.address, &message[..len])
            .await
            .map_err(VcmError::I2c)?;
        self.state.position = Some(clamped);
        Ok(clamped)
    }

    /// Gives the bus back.
    pub fn release(self) -> I2C {
        self.i2c
    }
}
