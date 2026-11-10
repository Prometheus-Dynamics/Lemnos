//! An I2C adapter through the module.

use std::io;
use std::os::fd::{AsFd, OwnedFd};

use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_linux_sys::i2c::Message;
use lemnos_linux_sys::module::I2cTarget;

use super::Node;
use crate::i2c::{Smbus, Wire, run_plain, run_smbus};
use crate::kind_of;

const ADDRESSES: usize = 128;

/// One adapter. Each target is claimed on first use and held until this is dropped, so two owners
/// of one address can't both run. An address that doesn't answer is let go, so probing a bus
/// keeps nothing from the driver that later owns one of those addresses.
pub(crate) struct ModuleI2c {
    node: Node,
    bus: u32,
    claims: [Option<OwnedFd>; ADDRESSES],
    /// The adapter has refused plain transfers, so only SMBus is left.
    smbus_only: bool,
}

impl ModuleI2c {
    pub(crate) fn new(node: Node, bus: u32) -> Self {
        Self {
            node,
            bus,
            claims: [const { None }; ADDRESSES],
            smbus_only: false,
        }
    }

    fn run(&mut self, address: u8, operations: &mut [Operation<'_>]) -> io::Result<()> {
        if operations.is_empty() {
            return Ok(());
        }
        let result = self.attempt(address, operations);
        if let Err(error) = &result
            && matches!(error.raw_os_error(), Some(libc::ENXIO | libc::EREMOTEIO))
        {
            self.claims[usize::from(address)] = None;
        }
        result
    }

    fn attempt(&mut self, address: u8, operations: &mut [Operation<'_>]) -> io::Result<()> {
        let slot = &mut self.claims[usize::from(address)];
        if slot.is_none() {
            let (bus, node) = (self.bus, &self.node);
            *slot = Some(node.claim(format_args!("i2c-{bus}:0x{address:x}"))?);
        }
        let Some(claim) = slot.as_ref() else {
            return Err(io::ErrorKind::NotFound.into());
        };
        let mut target = Target(I2cTarget {
            node: self.node.fd(),
            claim: claim.as_fd(),
            bus: self.bus,
            address: u16::from(address),
        });
        if !self.smbus_only {
            match run_plain(&mut target, u16::from(address), operations) {
                Err(error) if error.raw_os_error() == Some(libc::EOPNOTSUPP) => {
                    self.smbus_only = true;
                }
                done => return done,
            }
        }
        run_smbus(&mut target, operations)
    }
}

struct Target<'a>(I2cTarget<'a>);

impl Wire for Target<'_> {
    fn send(&mut self, _address: u16, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.0.transfer(messages)
    }
}

impl Smbus for Target<'_> {
    fn write_byte(&mut self, value: u8) -> io::Result<()> {
        self.0.smbus_write_byte(value)
    }

    fn write_byte_data(&mut self, register: u8, value: u8) -> io::Result<()> {
        self.0.smbus_write_byte_data(register, value)
    }

    fn write_block(&mut self, register: u8, values: &[u8]) -> io::Result<()> {
        self.0.smbus_write_block(register, values)
    }

    fn read_byte_data(&mut self, register: u8) -> io::Result<u8> {
        self.0.smbus_read_byte_data(register)
    }

    fn read_block(&mut self, register: u8, out: &mut [u8]) -> io::Result<usize> {
        self.0.smbus_read_block(register, out)
    }
}

impl ErrorType for ModuleI2c {
    type Error = ErrorKind;
}

impl I2c for ModuleI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        if usize::from(address) >= ADDRESSES {
            return Err(ErrorKind::InvalidInput);
        }
        self.run(address, operations).map_err(|e| kind_of(&e))
    }
}
