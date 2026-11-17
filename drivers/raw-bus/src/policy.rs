//! The write policy: which of the device's I2C addresses a caller may use, and whether it may write
//! at all. It is read once, from the device's `config` in the board file, and it is checked before
//! any call reaches the bus. It is closed by default: with no settings, every bus call is refused
//! except reading a GPIO line.
//!
//! What a read and a write are, exactly. A transfer on an I2C or SPI bus is a **read** when it has a
//! read part (a read length above zero), and a **write** when it has none. A **register read** is
//! one read transfer whose write part (the register pointer) comes before its read part, in the
//! same transaction; it is a read. The write part is sent to the device as given, so it is the
//! board owner's choice of address and device that decides what a read may send.
//!
//! The settings, both optional:
//!
//! - `writes` (boolean, default `false`). With `false`, every read is admitted (an allowed I2C
//!   address, and any SPI transfer that has a read part) and every write is refused: an I2C or SPI
//!   transfer with no read part, `line_set`, configuring the line as an output at bind (that
//!   drives its level), `pwm_set` and `uart_write`. Reading a line and the UART is always allowed.
//!   With `true`, writes are allowed too.
//! - `allow_i2c` (default none): the 7-bit addresses the device may use, read or write. It is an
//!   integer for one address, or a string of comma-separated addresses, each decimal or `0x` hex.
//!   Ranges are not supported. With none, every I2C transfer is refused, reads included.
//!
//! A bus, line, PWM channel or UART is the device's only because the board binds it to the device, so there
//! is no setting for those: the board opts in by binding one, and `writes` covers what is done on it.

use lemnos_core::{Config, ConfigError, ConfigValue, Result, Status};

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

    /// An I2C transaction to `address`. `read` is whether it has a read part: a transaction with
    /// none is a write, and needs `writes`. An address not on the list is refused either way.
    pub fn admit_i2c(&self, address: u8, read: bool) -> Result<()> {
        if !self.i2c_allowed(address) || (!read && !self.writes) {
            return Err(REFUSED);
        }
        Ok(())
    }

    /// An SPI transfer. `read` is whether it has a read part: one with none is a write, and needs
    /// `writes`.
    pub fn admit_spi(&self, read: bool) -> Result<()> {
        if !read && !self.writes {
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

    pub fn admit_uart_write(&self) -> Result<()> {
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

/// Refuses an `allow_i2c` that is not an address or a list of them, or that names one outside 7
/// bits. The same reading `from_config` makes, so a board file is refused before any device is made.
pub fn check_allow_i2c<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
    allowed_addresses(cfg.get("allow_i2c"))
        .map(|_| ())
        .map_err(|_| ConfigError::new("allow_i2c", "must be a 7-bit address, or a list of them"))
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
