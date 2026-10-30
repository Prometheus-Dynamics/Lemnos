//! The simulated board's I2C buses: targets that answer to their addresses
//! with the register behaviour of their chips, and the `Buses` the service
//! builds its devices on.
//!
//! Two layouts cover the chips on the board:
//!
//! - [`Layout::Bytes`]: a register file of single bytes behind an 8-bit
//!   pointer that every transfer steps through (BMI088, BMM150). A read
//!   returns the bytes from the pointer on.
//! - [`Layout::Words`]: registers of their own width behind an 8-bit pointer
//!   that a read does not step (INA238, INA2xx). A read returns the register
//!   the pointer names, from its first byte.
//!
//! A target that is dead, or unattached, does not acknowledge: its transfers
//! fail with [`ErrorKind::Nack`], as on a bus.

use embedded_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_board::{BoardError, Buses, DynI2c, DynInputPin, DynOutputPin, GpioRef};
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::ErrorKind;
use lemnos_hal::raw::LineConfig;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// How a target's registers and pointer behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Bytes,
    Words,
}

#[derive(Debug, Default)]
struct Target {
    layout: Option<Layout>,
    dead: bool,
    pointer: u8,
    bytes: BTreeMap<u8, u8>,
    words: BTreeMap<u8, Vec<u8>>,
}

impl Target {
    fn write(&mut self, data: &[u8]) {
        match self.layout {
            Some(Layout::Words) => {
                if !data.is_empty() {
                    self.words.insert(self.pointer, data.to_vec());
                }
            }
            _ => {
                for &b in data {
                    self.bytes.insert(self.pointer, b);
                    self.pointer = self.pointer.wrapping_add(1);
                }
            }
        }
    }

    fn read(&mut self, buf: &mut [u8]) {
        match self.layout {
            Some(Layout::Words) => {
                let value = self.words.get(&self.pointer);
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = value.and_then(|v| v.get(i).copied()).unwrap_or(0);
                }
            }
            _ => {
                for b in buf.iter_mut() {
                    *b = self.bytes.get(&self.pointer).copied().unwrap_or(0);
                    self.pointer = self.pointer.wrapping_add(1);
                }
            }
        }
    }
}

/// One I2C bus of simulated targets. Clones share the bus: the plant updates
/// the registers the service reads.
#[derive(Debug, Clone, Default)]
pub(crate) struct SimBus(Arc<Mutex<BTreeMap<u8, Target>>>);

impl SimBus {
    fn with<R>(&self, f: impl FnOnce(&mut BTreeMap<u8, Target>) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Puts a target with `layout` at `address` (its registers, if any, stay).
    pub fn attach(&self, address: u8, layout: Layout) {
        self.with(|t| t.entry(address).or_default().layout = Some(layout));
    }

    /// Makes the target at `address` stop (or resume) acknowledging.
    pub fn set_dead(&self, address: u8, dead: bool) {
        self.with(|t| t.entry(address).or_default().dead = dead);
    }

    /// Whether the target at `address` is not acknowledging.
    #[cfg(test)]
    pub fn is_dead(&self, address: u8) -> bool {
        self.with(|t| t.get(&address).is_some_and(|t| t.dead))
    }

    /// Sets consecutive byte registers from `register` (a [`Layout::Bytes`] target).
    pub fn set_bytes(&self, address: u8, register: u8, bytes: &[u8]) {
        self.with(|t| {
            let target = t.entry(address).or_default();
            for (i, &b) in bytes.iter().enumerate() {
                target.bytes.insert(register.wrapping_add(i as u8), b);
            }
        });
    }

    /// Sets the value of one word register (a [`Layout::Words`] target).
    pub fn set_word(&self, address: u8, register: u8, bytes: &[u8]) {
        self.with(|t| {
            t.entry(address)
                .or_default()
                .words
                .insert(register, bytes.to_vec());
        });
    }

    /// One byte register (0 if unset).
    pub fn byte(&self, address: u8, register: u8) -> u8 {
        self.with(|t| {
            t.get(&address)
                .and_then(|t| t.bytes.get(&register).copied())
                .unwrap_or(0)
        })
    }

    /// One word register's bytes (empty if unset).
    pub fn word(&self, address: u8, register: u8) -> Vec<u8> {
        self.with(|t| {
            t.get(&address)
                .and_then(|t| t.words.get(&register).cloned())
                .unwrap_or_default()
        })
    }
}

impl ErrorType for SimBus {
    type Error = ErrorKind;
}

impl I2c for SimBus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        self.with(|targets| {
            let target = targets
                .get_mut(&address)
                .filter(|t| !t.dead && t.layout.is_some())
                .ok_or(ErrorKind::Nack)?;
            let mut previous_write = false;
            for op in operations.iter_mut() {
                match op {
                    Operation::Write(data) => {
                        let mut data: &[u8] = data;
                        if !previous_write && let Some((&first, rest)) = data.split_first() {
                            target.pointer = first;
                            data = rest;
                        }
                        target.write(data);
                        previous_write = true;
                    }
                    Operation::Read(buf) => {
                        target.read(buf);
                        previous_write = false;
                    }
                }
            }
            Ok(())
        })
    }
}

/// The service's buses on the simulated board: the simulated I2C buses, and
/// the mock hardware for GPIO, PWM and SPI (and the sysfs root).
pub(crate) struct SimBuses {
    pub(crate) i2c: BTreeMap<u32, SimBus>,
    pub(crate) rest: crate::mock::MockBuses,
}

impl Buses for SimBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        let bus = self.i2c.entry(bus).or_default().clone();
        Ok(DynI2c::new(bus))
    }

    fn sys(&self) -> SysRoot {
        self.rest.sys()
    }

    fn gpio_output(&mut self, line: &GpioRef, initial: bool) -> Result<DynOutputPin, BoardError> {
        self.rest.gpio_output(line, initial)
    }

    fn gpio_input(&mut self, line: &GpioRef) -> Result<DynInputPin, BoardError> {
        self.rest.gpio_input(line)
    }

    fn line(
        &mut self,
        chip: &str,
        offset: u32,
        config: &LineConfig,
        consumer: &str,
    ) -> Result<lemnos_board::raw::DynLine, BoardError> {
        self.rest.line(chip, offset, config, consumer)
    }

    fn line_chip_id(&self, chip: &str) -> String {
        self.rest.line_chip_id(chip)
    }

    fn find_line(&self, name: &str) -> Option<(String, u32)> {
        self.rest.find_line(name)
    }

    fn pwm(&mut self, chip: u32, channel: u32) -> Result<lemnos_board::raw::DynPwm, BoardError> {
        self.rest.pwm(chip, channel)
    }

    fn spi(&mut self, bus: u32, chip_select: u16) -> Result<lemnos_board::raw::DynSpi, BoardError> {
        self.rest.spi(bus, chip_select)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal::i2c::I2c;

    fn read(bus: &mut SimBus, address: u8, pointer: u8, len: usize) -> Result<Vec<u8>, ErrorKind> {
        let mut buf = vec![0u8; len];
        bus.transaction(
            address,
            &mut [Operation::Write(&[pointer]), Operation::Read(&mut buf)],
        )?;
        Ok(buf)
    }

    #[test]
    fn byte_targets_step_the_pointer_and_words_do_not() {
        let bus = SimBus::default();
        bus.attach(0x18, Layout::Bytes);
        bus.set_bytes(0x18, 0x12, &[1, 2, 3, 4]);
        let mut b = bus.clone();
        assert_eq!(read(&mut b, 0x18, 0x12, 4).unwrap(), vec![1, 2, 3, 4]);
        // A word register at 0x06 does not overlap 0x07: each keeps its own value.
        bus.attach(0x40, Layout::Words);
        bus.set_word(0x40, 0x06, &[0x12, 0x34]);
        bus.set_word(0x40, 0x07, &[0xab, 0xcd]);
        let mut b = bus.clone();
        assert_eq!(read(&mut b, 0x40, 0x06, 2).unwrap(), vec![0x12, 0x34]);
        assert_eq!(read(&mut b, 0x40, 0x07, 2).unwrap(), vec![0xab, 0xcd]);
    }

    #[test]
    fn writes_land_in_the_register_the_pointer_names() {
        let bus = SimBus::default();
        bus.attach(0x40, Layout::Words);
        let mut b = bus.clone();
        b.transaction(0x40, &mut [Operation::Write(&[0x02, 0x10, 0x00])])
            .unwrap();
        assert_eq!(bus.word(0x40, 0x02), vec![0x10, 0x00]);
    }

    #[test]
    fn dead_and_unattached_targets_nack() {
        let mut bus = SimBus::default();
        bus.attach(0x10, Layout::Bytes);
        assert_eq!(read(&mut bus, 0x11, 0, 1), Err(ErrorKind::Nack));
        bus.set_dead(0x10, true);
        assert!(bus.is_dead(0x10));
        assert_eq!(read(&mut bus, 0x10, 0, 1), Err(ErrorKind::Nack));
        bus.set_dead(0x10, false);
        assert!(read(&mut bus, 0x10, 0, 1).is_ok());
    }
}
