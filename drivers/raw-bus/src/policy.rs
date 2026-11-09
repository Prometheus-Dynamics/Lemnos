//! The write policy: which of the device's I2C addresses a caller may use, and whether it may write
//! at all. It is read once, from the device's `config` in the board file, and it is checked before
//! any call reaches the bus. It is closed by default: with no settings, every bus call is refused
//! except reading a GPIO line.
//!
//! The settings, both optional:
//!
//! - `writes` (boolean, default `false`). With `false`, these are refused: an I2C transfer with a
//!   write part, every SPI transfer (SPI clocks bytes out on every transfer), `line_set`, configuring
//!   the line as an output at bind (that drives its level), and `pwm_set`. With `true`, they are
//!   allowed.
//! - `allow_i2c` (default none): the 7-bit addresses the device may use, read or write. It is an
//!   integer for one address, or a string of comma-separated addresses, each decimal or `0x` hex.
//!   Ranges are not supported. With none, every I2C transfer is refused, reads included.
//!
//! A bus, line or PWM channel is the device's only because the board binds it to the device, so there
//! is no setting for those: the board opts in by binding one, and `writes` covers what is done on it.

use lemnos_core::{Config, ConfigValue, Result, Status};

/// The refusal for a call the policy does not allow.
const REFUSED: Status = Status::NOT_PERMITTED;

/// The write policy of one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Policy {
    writes: bool,
    /// One bit per 7-bit I2C address the device may use.
    allowed_i2c: [u64; 2],
}

impl Policy {
    /// Reads the settings from a device's `config`. A setting of the wrong type, or an address
    /// outside 7 bits, is `INVALID_ARGUMENT`.
    pub fn from_config(cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            writes: flag(cfg, "writes")?,
            allowed_i2c: allowed_addresses(cfg.get("allow_i2c"))?,
        })
    }

    fn i2c_allowed(&self, address: u8) -> bool {
        let address = usize::from(address);
        (self.allowed_i2c[address / 64] >> (address % 64)) & 1 == 1
    }

    /// An I2C transaction to `address`. `write` is whether it has a write part.
    pub fn admit_i2c(&self, address: u8, write: bool) -> Result<()> {
        if !self.i2c_allowed(address) || (write && !self.writes) {
            return Err(REFUSED);
        }
        Ok(())
    }

    /// An SPI transfer. Every one clocks bytes out, so it is a write.
    pub fn admit_spi(&self) -> Result<()> {
        if !self.writes {
            return Err(REFUSED);
        }
        Ok(())
    }

    /// Configuring the GPIO line at bind. An output drives the line to its initial level, so it is
    /// a write; an input is not.
    pub fn admit_line_configure(&self, output: bool) -> Result<()> {
        if output && !self.writes {
            return Err(REFUSED);
        }
        Ok(())
    }

    pub fn admit_line_set(&self) -> Result<()> {
        if !self.writes {
            return Err(REFUSED);
        }
        Ok(())
    }

    pub fn admit_pwm(&self) -> Result<()> {
        if !self.writes {
            return Err(REFUSED);
        }
        Ok(())
    }
}

/// A boolean setting: absent means `false`, and a non-boolean is refused.
fn flag(cfg: &Config<'_>, key: &str) -> Result<bool> {
    match cfg.get(key) {
        None => Ok(false),
        Some(ConfigValue::Bool(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The allowed addresses as a bitmap over 0..=0x7f. Absent allows none.
fn allowed_addresses(value: Option<ConfigValue<'_>>) -> Result<[u64; 2]> {
    let mut bits = [0u64; 2];
    let mut allow = |address: i64| -> Result<()> {
        let address = u8::try_from(address)
            .ok()
            .filter(|address| *address <= 0x7f)
            .ok_or(Status::INVALID_ARGUMENT)?;
        bits[usize::from(address) / 64] |= 1 << (address % 64);
        Ok(())
    };
    match value {
        None => {}
        Some(ConfigValue::Int(address)) => allow(address)?,
        Some(ConfigValue::Str(list)) => {
            for item in list
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
            {
                allow(parse_address(item).ok_or(Status::INVALID_ARGUMENT)?)?;
            }
        }
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    }
    Ok(bits)
}

fn parse_address(text: &str) -> Option<i64> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}
