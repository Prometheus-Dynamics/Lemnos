//! Raw access to the I2C bus, SPI device, GPIO line and PWM channel a device is wired to, served as
//! `raw-bus/1`. Each board device uses the ones it names (`uses = { i2c = "...", line = "..." }` or
//! `bus`), and a call to one it doesn't have is `NOT_SUPPORTED`. Binding a resource is what opts a
//! device in to it.
//!
//! The driver knows nothing about what is on the bus. What a caller may do is the write policy in
//! `policy.rs`, read from the device's `config`, and it is checked before any call reaches the bus.
//! It is closed by default; see that file for `writes` and `allow_i2c`.
//!
//! Other settings from the device's `config`:
//!
//! - `line_direction` (`"input"` or `"output"`): configures the line at bind. Without it the line is
//!   left as it is. `"output"` drives the line to `line_initial` (default `false`) in that one
//!   configuration, so it needs `writes = true`. `line_active_low` (default `false`) sets the line's
//!   polarity.
//! - `pwm_inverted` (default `false`): the PWM polarity every `pwm_set` uses.
//!
//! A transfer's bytes arrive in its call, so the driver holds no transfer buffer and allocates
//! nothing. The driver makes no retries and no timers. A failed bus call is returned on that call and
//! marks health `Failed`. The next call tries again, and a success clears the mark.

#![no_std]

mod policy;

use lemnos_contract_raw_bus::RawBus as RawBusContract;
use lemnos_core::{
    Config, ConfigValue, Health, I2c, Lifecycle, Line, Pwm, Result, Spi, Status, driver, status_of,
};
use lemnos_hal::i2c::{I2c as HalI2c, Operation as I2cOperation};
use lemnos_hal::raw::{Direction, LineConfig, Polarity, PwmConfig};
use lemnos_hal::spi::{Operation as SpiOperation, SpiDevice};

use policy::Policy;

const FAILED: &str = "the last bus call failed; the next call tries again";

/// A device with any of an I2C bus, an SPI device, a GPIO line and a PWM channel.
#[driver("raw-bus", provides = [RawBusContract])]
pub struct RawBus {
    #[needs(optional)]
    i2c: Option<I2c>,
    #[needs(optional)]
    spi: Option<Spi>,
    #[needs(optional)]
    line: Option<Line>,
    #[needs(optional)]
    pwm: Option<Pwm>,
    policy: Policy,
    pwm_polarity: Polarity,
    /// Whether the last bus call failed.
    failed: bool,
}

impl Lifecycle for RawBus {
    type Needs = RawBusNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn start(needs: RawBusNeeds, cfg: &Config<'_>) -> Result<Self> {
        if needs.i2c.is_none() && needs.spi.is_none() && needs.line.is_none() && needs.pwm.is_none()
        {
            return Err(Status::NOT_FOUND);
        }
        let policy = Policy::from_config(cfg)?;
        let pwm_polarity = if flag(cfg, "pwm_inverted")? {
            Polarity::Inversed
        } else {
            Polarity::Normal
        };
        let mut line = needs.line;
        if let Some(config) = line_config(cfg)? {
            policy.admit_line_configure(config.direction == Direction::Output)?;
            let line = line.as_mut().ok_or(Status::NOT_FOUND)?;
            line.configure(&config).map_err(status_of)?;
        }
        Ok(Self {
            i2c: needs.i2c,
            spi: needs.spi,
            line,
            pwm: needs.pwm,
            policy,
            pwm_polarity,
            failed: false,
        })
    }

    fn health(&self) -> Health {
        if self.failed {
            return Health::Failed(FAILED);
        }
        Health::Ok
    }
}

impl RawBusContract for RawBus {
    fn i2c_transfer(
        &mut self,
        address: u16,
        write: &[u8],
        read_len: u32,
        into: &mut [u8],
    ) -> Result<usize> {
        let address = u8::try_from(address)
            .ok()
            .filter(|address| *address <= 0x7f)
            .ok_or(Status::INVALID_ARGUMENT)?;
        let read = read_len_of(read_len, into, write.is_empty())?;
        self.policy.admit_i2c(address, !write.is_empty())?;
        let bus = self.i2c.as_mut().ok_or(Status::NOT_SUPPORTED)?;
        let incoming = &mut into[..read];
        let result = match (write.is_empty(), read) {
            (_, 0) => bus.transaction(address, &mut [I2cOperation::Write(write)]),
            (true, _) => bus.transaction(address, &mut [I2cOperation::Read(incoming)]),
            _ => bus.transaction(
                address,
                &mut [I2cOperation::Write(write), I2cOperation::Read(incoming)],
            ),
        };
        self.failed = result.is_err();
        result.map_err(status_of)?;
        Ok(read)
    }

    fn spi_transfer(&mut self, write: &[u8], read_len: u32, into: &mut [u8]) -> Result<usize> {
        let read = read_len_of(read_len, into, write.is_empty())?;
        self.policy.admit_spi()?;
        let spi = self.spi.as_mut().ok_or(Status::NOT_SUPPORTED)?;
        let common = write.len().min(read);
        let result = match (write.len(), read) {
            (_, 0) => spi.transaction(&mut [SpiOperation::Write(write)]),
            (0, _) => spi.transaction(&mut [SpiOperation::Read(&mut into[..read])]),
            (w, r) if w == r => {
                spi.transaction(&mut [SpiOperation::Transfer(&mut into[..r], write)])
            }
            (w, _) if w > read => spi.transaction(&mut [
                SpiOperation::Transfer(&mut into[..common], &write[..common]),
                SpiOperation::Write(&write[common..]),
            ]),
            _ => {
                let (duplex, tail) = into[..read].split_at_mut(common);
                spi.transaction(&mut [
                    SpiOperation::Transfer(duplex, &write[..common]),
                    SpiOperation::Read(tail),
                ])
            }
        };
        self.failed = result.is_err();
        result.map_err(status_of)?;
        Ok(read)
    }

    fn line_get(&mut self) -> Result<bool> {
        let line = self.line.as_mut().ok_or(Status::NOT_SUPPORTED)?;
        let result = line.get();
        self.failed = result.is_err();
        result.map_err(status_of)
    }

    fn line_set(&mut self, value: bool) -> Result<()> {
        self.policy.admit_line_set()?;
        let line = self.line.as_mut().ok_or(Status::NOT_SUPPORTED)?;
        let result = line.set(value);
        self.failed = result.is_err();
        result.map_err(status_of)
    }

    fn pwm_set(&mut self, period_ns: u64, duty_ns: u64, enabled: bool) -> Result<()> {
        self.policy.admit_pwm()?;
        let config = PwmConfig {
            period_ns,
            duty_ns,
            polarity: self.pwm_polarity,
            enabled,
        };
        let pwm = self.pwm.as_mut().ok_or(Status::NOT_SUPPORTED)?;
        let result = pwm.configure(&config);
        self.failed = result.is_err();
        result.map_err(status_of)
    }
}

/// The number of bytes a read asks for, checked: a transfer must do something, and the reply buffer
/// must hold what it reads.
fn read_len_of(read_len: u32, into: &[u8], no_write: bool) -> Result<usize> {
    let read = usize::try_from(read_len).map_err(|_| Status::INVALID_ARGUMENT)?;
    if no_write && read == 0 {
        return Err(Status::INVALID_ARGUMENT);
    }
    if read > into.len() {
        return Err(Status::BUFFER_TOO_SMALL);
    }
    Ok(read)
}

/// The line configuration the settings ask for, or `None` when the board leaves the line alone.
fn line_config(cfg: &Config<'_>) -> Result<Option<LineConfig>> {
    let direction = match cfg.get("line_direction") {
        None => return Ok(None),
        Some(ConfigValue::Str("input")) => Direction::Input,
        Some(ConfigValue::Str("output")) => Direction::Output,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    let mut config = LineConfig::input();
    config.direction = direction;
    config.initial = flag(cfg, "line_initial")?;
    config.active_low = flag(cfg, "line_active_low")?;
    Ok(Some(config))
}

/// A boolean setting: absent means `false`, and a non-boolean is refused.
fn flag(cfg: &Config<'_>, key: &str) -> Result<bool> {
    match cfg.get(key) {
        None => Ok(false),
        Some(ConfigValue::Bool(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}
