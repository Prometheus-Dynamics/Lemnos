//! Raw access to the I2C bus, SPI device, GPIO line, PWM channel and UART a device is wired to,
//! served as `raw-bus/1`. Each board device uses the ones it names
//! (`uses = { i2c = "...", line = "...", uart = "..." }` or `bus`), and a call to one it doesn't have is `NOT_SUPPORTED`. Binding a resource is what opts a
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
//! - `uart_baud` (integer), `uart_parity` (`"none"`, `"even"` or `"odd"`, default `"none"`) and
//!   `uart_stop_bits` (1 or 2, default 1): the UART's frame, set at bind, always 8 data bits.
//!   Without `uart_baud` the port is left as the backend opened it, and the other two may not be
//!   given. Setting the frame sends nothing, so it needs no `writes`.
//!
//! A UART is a stream of bytes with no transactions: `uart_read` hands on what has arrived, which
//! may be part of a message or more than one, and the caller frames it. Bytes that arrive while no
//! one reads wait in the port's own buffer.
//!
//! A transfer's bytes arrive in its call, so the driver holds no transfer buffer and allocates
//! nothing. The driver makes no retries and no timers. A failed bus call is returned on that call and
//! marks health `Failed`. The next call tries again, and a success clears the mark.

#![no_std]

mod policy;

use core::time::Duration;
use lemnos_contract_raw_bus::RawBus as RawBusContract;

use lemnos_contract_raw_bus::UART_MAX_WAIT_MS;
use lemnos_core::{
    Config, ConfigValue, Health, I2c, Lifecycle, Line, Pwm, Result, Spi, Status, Uart, driver,
    status_of,
};
use lemnos_hal::i2c::{I2c as HalI2c, Operation as I2cOperation};
use lemnos_hal::raw::{Direction, LineConfig, Polarity, PwmConfig};
use lemnos_hal::spi::{Operation as SpiOperation, SpiDevice};
use lemnos_hal::{Parity, StopBits, UartFormat};

use policy::Policy;

const FAILED: &str = "the last bus call failed; the next call tries again";

/// A device with any of an I2C bus, an SPI device, a GPIO line, a PWM channel and a UART.
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
    #[needs(optional)]
    uart: Option<Uart>,
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
        if needs.i2c.is_none()
            && needs.spi.is_none()
            && needs.line.is_none()
            && needs.pwm.is_none()
            && needs.uart.is_none()
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
        let mut uart = needs.uart;
        if let Some(format) = uart_format(cfg)? {
            let uart = uart.as_mut().ok_or(Status::NOT_FOUND)?;
            uart.configure(format).map_err(status_of)?;
        }
        Ok(Self {
            i2c: needs.i2c,
            spi: needs.spi,
            line,
            pwm: needs.pwm,
            uart,
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

    fn uart_write(&mut self, data: &[u8]) -> Result<u32> {
        if data.is_empty() {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.policy.admit_uart_write()?;
        let uart = self.uart.as_mut().ok_or(Status::NOT_SUPPORTED)?;
        let result = uart.write(data);
        self.failed = result.is_err();
        let sent = result.map_err(status_of)?.min(data.len());
        Ok(u32::try_from(sent).unwrap_or(u32::MAX))
    }

    fn uart_read(&mut self, max_len: u32, timeout_ms: u32, into: &mut [u8]) -> Result<usize> {
        if timeout_ms > UART_MAX_WAIT_MS {
            return Err(Status::INVALID_ARGUMENT);
        }
        let read = read_len_of(max_len, into, true)?;
        let uart = self.uart.as_mut().ok_or(Status::NOT_SUPPORTED)?;
        let wait = Duration::from_millis(u64::from(timeout_ms));
        let result = uart.read(&mut into[..read], wait);
        self.failed = result.is_err();
        Ok(result.map_err(status_of)?.min(read))
    }
}

/// The UART's frame the settings ask for, or `None` when the board leaves the port as it opened.
fn uart_format(cfg: &Config<'_>) -> Result<Option<UartFormat>> {
    let baud = match cfg.get("uart_baud") {
        None => {
            if cfg.get("uart_parity").is_some() || cfg.get("uart_stop_bits").is_some() {
                return Err(Status::INVALID_ARGUMENT);
            }
            return Ok(None);
        }
        Some(ConfigValue::Int(baud)) => u32::try_from(baud)
            .ok()
            .filter(|baud| *baud > 0)
            .ok_or(Status::INVALID_ARGUMENT)?,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    let parity = match cfg.get("uart_parity") {
        None | Some(ConfigValue::Str("none")) => Parity::None,
        Some(ConfigValue::Str("even")) => Parity::Even,
        Some(ConfigValue::Str("odd")) => Parity::Odd,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    let stop_bits = match cfg.get("uart_stop_bits") {
        None | Some(ConfigValue::Int(1)) => StopBits::One,
        Some(ConfigValue::Int(2)) => StopBits::Two,
        Some(_) => return Err(Status::INVALID_ARGUMENT),
    };
    Ok(Some(UartFormat {
        baud,
        parity,
        stop_bits,
    }))
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
