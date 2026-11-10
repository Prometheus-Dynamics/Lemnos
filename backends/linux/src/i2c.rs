//! An I2C adapter through i2c-dev.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::path::Path;

use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_linux_sys::i2c as sys;

use crate::kind_of;

pub use sys::Message;

/// Messages of a transaction built on the stack: as many as one `I2C_RDWR` takes.
const MAX_MESSAGES: usize = sys::MAX_MESSAGES;
/// Adjacent operations of one direction up to this size are joined on the stack.
const JOIN_ON_STACK: usize = 128;
/// The most an SMBus block carries.
const SMBUS_BLOCK: usize = 32;

/// `/dev/i2c-N`. Each target is claimed with `I2C_SLAVE`, never `I2C_SLAVE_FORCE`, so an address a
/// kernel driver owns fails with `Busy` rather than racing that driver.
pub struct I2cBus {
    file: File,
    functionality: u64,
    target: Option<u16>,
}

impl I2cBus {
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let functionality = sys::functionality(file.as_fd())?;
        Ok(Self {
            file,
            functionality,
            target: None,
        })
    }

    fn claim(&mut self, address: u16) -> io::Result<()> {
        if self.target != Some(address) {
            self.target = None;
            sys::set_target(self.file.as_fd(), address)?;
            self.target = Some(address);
        }
        Ok(())
    }

    fn transfer(&mut self, address: u16, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.claim(address)?;
        sys::transfer(self.file.as_fd(), address, messages)
    }

    fn run(&mut self, address: u16, operations: &mut [Operation<'_>]) -> io::Result<()> {
        if operations.is_empty() {
            return Ok(());
        }
        if self.functionality & sys::func::I2C == 0 {
            self.claim(address)?;
            return run_smbus(&mut Target(&self.file), operations);
        }
        run_plain(self, address, operations)
    }
}

impl Wire for I2cBus {
    fn send(&mut self, address: u16, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.transfer(address, messages)
    }
}

/// How a transfer list reaches the adapter.
pub(crate) trait Wire {
    fn send(&mut self, address: u16, messages: &mut [Message<'_>]) -> io::Result<()>;
}

/// Runs `operations` as one plain transfer.
pub(crate) fn run_plain(
    wire: &mut impl Wire,
    address: u16,
    operations: &mut [Operation<'_>],
) -> io::Result<()> {
    if alternates(operations) && operations.len() <= MAX_MESSAGES {
        let n = operations.len();
        let mut messages: [Message<'_>; MAX_MESSAGES] =
            std::array::from_fn(|_| Message::Write(&[]));
        for (message, operation) in messages.iter_mut().zip(operations.iter_mut()) {
            *message = match operation {
                Operation::Write(data) => Message::Write(data),
                Operation::Read(buffer) => Message::Read(buffer),
            };
        }
        return wire.send(address, &mut messages[..n]);
    }
    run_joined(wire, address, operations)
}

/// embedded-hal's adjacent operations of one direction are one contiguous transfer, with no
/// restart between them, so each such run becomes one message.
fn run_joined(
    wire: &mut impl Wire,
    address: u16,
    operations: &mut [Operation<'_>],
) -> io::Result<()> {
    let total = operations.iter().map(length).sum();
    let mut stack = [0u8; JOIN_ON_STACK];
    let mut heap = Vec::new();
    let joined: &mut [u8] = if total <= JOIN_ON_STACK {
        &mut stack[..total]
    } else {
        heap.resize(total, 0);
        &mut heap
    };
    let mut at = 0;
    for operation in operations.iter() {
        if let Operation::Write(data) = operation {
            joined[at..at + data.len()].copy_from_slice(data);
        }
        at += length(operation);
    }
    {
        let mut messages: [Message<'_>; MAX_MESSAGES] =
            std::array::from_fn(|_| Message::Write(&[]));
        let mut count = 0;
        let mut rest = &mut *joined;
        let mut index = 0;
        while index < operations.len() {
            let read = is_read(&operations[index]);
            let run = operations[index..]
                .iter()
                .take_while(|operation| is_read(operation) == read)
                .count();
            let length = operations[index..index + run].iter().map(length).sum();
            if count == MAX_MESSAGES {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let (part, tail) = core::mem::take(&mut rest).split_at_mut(length);
            rest = tail;
            messages[count] = if read {
                Message::Read(part)
            } else {
                Message::Write(part)
            };
            count += 1;
            index += run;
        }
        wire.send(address, &mut messages[..count])?;
    }
    let mut at = 0;
    for operation in operations.iter_mut() {
        let n = length(operation);
        if let Operation::Read(buffer) = operation {
            buffer.copy_from_slice(&joined[at..at + n]);
        }
        at += n;
    }
    Ok(())
}

fn is_read(operation: &Operation<'_>) -> bool {
    matches!(operation, Operation::Read(_))
}

fn length(operation: &Operation<'_>) -> usize {
    match operation {
        Operation::Read(buffer) => buffer.len(),
        Operation::Write(data) => data.len(),
    }
}

fn alternates(operations: &[Operation<'_>]) -> bool {
    operations
        .windows(2)
        .all(|w| is_read(&w[0]) != is_read(&w[1]))
}

impl ErrorType for I2cBus {
    type Error = ErrorKind;
}

impl I2c for I2cBus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        if address > 0x7f {
            return Err(ErrorKind::InvalidInput);
        }
        self.run(u16::from(address), operations)
            .map_err(|e| kind_of(&e))
    }
}

/// The SMBus calls an adapter without plain transfers is left with, on the claimed target.
pub(crate) trait Smbus {
    fn write_byte(&mut self, value: u8) -> io::Result<()>;
    fn write_byte_data(&mut self, register: u8, value: u8) -> io::Result<()>;
    fn write_block(&mut self, register: u8, values: &[u8]) -> io::Result<()>;
    fn read_byte_data(&mut self, register: u8) -> io::Result<u8>;
    fn read_block(&mut self, register: u8, out: &mut [u8]) -> io::Result<usize>;
}

struct Target<'a>(&'a File);

impl Smbus for Target<'_> {
    fn write_byte(&mut self, value: u8) -> io::Result<()> {
        sys::smbus_write_byte(self.0.as_fd(), value)
    }

    fn write_byte_data(&mut self, register: u8, value: u8) -> io::Result<()> {
        sys::smbus_write_byte_data(self.0.as_fd(), register, value)
    }

    fn write_block(&mut self, register: u8, values: &[u8]) -> io::Result<()> {
        sys::smbus_write_i2c_block_data(self.0.as_fd(), register, values)
    }

    fn read_byte_data(&mut self, register: u8) -> io::Result<u8> {
        sys::smbus_read_byte_data(self.0.as_fd(), register)
    }

    fn read_block(&mut self, register: u8, out: &mut [u8]) -> io::Result<usize> {
        sys::smbus_read_i2c_block_data(self.0.as_fd(), register, out)
    }
}

/// What SMBus can express: a write, or one register byte then a read.
pub(crate) fn run_smbus(
    device: &mut impl Smbus,
    operations: &mut [Operation<'_>],
) -> io::Result<()> {
    match operations {
        [Operation::Write(bytes)] => smbus_write(device, bytes),
        [Operation::Write([register]), Operation::Read(out)] => smbus_read(device, *register, out),
        _ => Err(io::ErrorKind::Unsupported.into()),
    }
}

fn is_unsupported(error: &io::Error) -> bool {
    // EINVAL, ENOTTY, ENOSYS, EOPNOTSUPP: all of these are how adapters decline a block call.
    error.kind() == io::ErrorKind::Unsupported
        || matches!(
            error.raw_os_error(),
            Some(libc::EINVAL | libc::ENOTTY | libc::ENOSYS | libc::EOPNOTSUPP)
        )
}

fn smbus_write(device: &mut impl Smbus, bytes: &[u8]) -> io::Result<()> {
    match bytes {
        [] => Ok(()),
        [value] => device.write_byte(*value),
        [register, value] => device.write_byte_data(*register, *value),
        [register, values @ ..] => {
            if values.len() <= SMBUS_BLOCK {
                match device.write_block(*register, values) {
                    Err(error) if is_unsupported(&error) => {}
                    done => return done,
                }
            }
            for (offset, value) in values.iter().enumerate() {
                device.write_byte_data(register.wrapping_add(offset as u8), *value)?;
            }
            Ok(())
        }
    }
}

fn smbus_read(device: &mut impl Smbus, register: u8, out: &mut [u8]) -> io::Result<()> {
    if out.is_empty() {
        return Ok(());
    }
    if out.len() <= SMBUS_BLOCK
        && matches!(device.read_block(register, out), Ok(n) if n == out.len())
    {
        return Ok(());
    }
    for (offset, byte) in out.iter_mut().enumerate() {
        *byte = device.read_byte_data(register.wrapping_add(offset as u8))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_measure::test_alloc::allocations;

    /// An 8-bit register map that can be told to refuse block calls.
    struct Registers {
        registers: [u8; 256],
        block_read: bool,
        block_write: bool,
        block_reads: usize,
        block_writes: usize,
    }

    impl Registers {
        fn new() -> Self {
            Self {
                registers: [0; 256],
                block_read: false,
                block_write: false,
                block_reads: 0,
                block_writes: 0,
            }
        }
    }

    impl Smbus for Registers {
        fn write_byte(&mut self, value: u8) -> io::Result<()> {
            self.registers[0] = value;
            Ok(())
        }

        fn write_byte_data(&mut self, register: u8, value: u8) -> io::Result<()> {
            self.registers[usize::from(register)] = value;
            Ok(())
        }

        fn write_block(&mut self, register: u8, values: &[u8]) -> io::Result<()> {
            if !self.block_write {
                return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
            }
            self.block_writes += 1;
            self.registers[usize::from(register)..][..values.len()].copy_from_slice(values);
            Ok(())
        }

        fn read_byte_data(&mut self, register: u8) -> io::Result<u8> {
            Ok(self.registers[usize::from(register)])
        }

        fn read_block(&mut self, register: u8, out: &mut [u8]) -> io::Result<usize> {
            if !self.block_read {
                return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
            }
            self.block_reads += 1;
            out.copy_from_slice(&self.registers[usize::from(register)..][..out.len()]);
            Ok(out.len())
        }
    }

    #[test]
    fn a_read_falls_back_to_one_register_at_a_time() {
        let mut device = Registers::new();
        device.registers[0x10..0x13].copy_from_slice(&[0xaa, 0xbb, 0xcc]);
        let mut out = [0; 3];
        run_smbus(
            &mut device,
            &mut [Operation::Write(&[0x10]), Operation::Read(&mut out)],
        )
        .unwrap();
        assert_eq!(out, [0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn a_write_falls_back_to_one_register_at_a_time() {
        let mut device = Registers::new();
        run_smbus(
            &mut device,
            &mut [Operation::Write(&[0x20, 0x11, 0x22, 0x33])],
        )
        .unwrap();
        assert_eq!(device.registers[0x20..0x23], [0x11, 0x22, 0x33]);
    }

    #[test]
    fn a_failing_register_write_is_not_retried_as_bytes() {
        struct Failing(Registers);
        impl Smbus for Failing {
            fn write_byte(&mut self, value: u8) -> io::Result<()> {
                self.0.write_byte(value)
            }
            fn write_byte_data(&mut self, _: u8, _: u8) -> io::Result<()> {
                panic!("a real failure must not turn into byte writes")
            }
            fn write_block(&mut self, _: u8, _: &[u8]) -> io::Result<()> {
                Err(io::Error::from_raw_os_error(libc::EREMOTEIO))
            }
            fn read_byte_data(&mut self, register: u8) -> io::Result<u8> {
                self.0.read_byte_data(register)
            }
            fn read_block(&mut self, register: u8, out: &mut [u8]) -> io::Result<usize> {
                self.0.read_block(register, out)
            }
        }
        let error = smbus_write(&mut Failing(Registers::new()), &[1, 2, 3]).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EREMOTEIO));
    }

    #[test]
    fn a_read_needs_a_single_register_byte() {
        let mut device = Registers::new();
        let mut out = [0; 2];
        let error = run_smbus(
            &mut device,
            &mut [Operation::Write(&[0, 0x10]), Operation::Read(&mut out)],
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn block_calls_are_used_when_the_adapter_has_them() {
        let mut device = Registers::new();
        device.block_read = true;
        device.block_write = true;
        device.registers[0x30..0x33].copy_from_slice(&[0x10, 0x20, 0x30]);
        let mut out = [0; 3];
        run_smbus(
            &mut device,
            &mut [Operation::Write(&[0x30]), Operation::Read(&mut out)],
        )
        .unwrap();
        assert_eq!(out, [0x10, 0x20, 0x30]);
        assert_eq!(device.block_reads, 1);
        run_smbus(&mut device, &mut [Operation::Write(&[0x40, 0xaa, 0xbb])]).unwrap();
        assert_eq!(device.registers[0x40..0x42], [0xaa, 0xbb]);
        assert_eq!(device.block_writes, 1);
    }

    #[test]
    fn a_register_run_wraps_at_the_end_of_the_map() {
        let mut device = Registers::new();
        run_smbus(&mut device, &mut [Operation::Write(&[0xff, 1, 2, 3])]).unwrap();
        assert_eq!(device.registers[0xff], 1);
        assert_eq!(device.registers[0..2], [2, 3]);
    }

    #[test]
    fn opening_a_missing_bus_fails_cleanly() {
        let error = I2cBus::open_path("/dev/i2c-does-not-exist").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    fn null_bus(functionality: u64) -> I2cBus {
        I2cBus {
            file: File::open("/dev/null").unwrap(),
            functionality,
            target: None,
        }
    }

    #[test]
    fn an_address_past_seven_bits_is_refused() {
        let mut bus = null_bus(sys::func::I2C);
        let error = bus.transaction(0x80, &mut [Operation::Write(&[1])]);
        assert_eq!(error, Err(ErrorKind::InvalidInput));
    }

    #[test]
    fn building_a_transaction_allocates_nothing() {
        // /dev/null refuses at the claim, after the messages are built.
        let mut bus = null_bus(sys::func::I2C);
        let mut read = [0; 4];
        let (result, n) = allocations(|| {
            bus.transaction(
                0x50,
                &mut [
                    Operation::Write(&[1]),
                    Operation::Write(&[2, 3]),
                    Operation::Read(&mut read),
                ],
            )
        });
        assert!(result.is_err());
        assert_eq!(n, 0);
    }
}
