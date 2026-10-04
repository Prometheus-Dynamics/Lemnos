use super::{IoError, invalid_input};
use embedded_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_linux_sys::i2c as sys;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

pub use sys::Message as I2cMessage;

/// An I2C adapter through i2c-dev (`/dev/i2c-N`, opened close-on-exec), as an embedded-hal
/// [`I2c`] bus. Ported from Styx (`styx-kernel`'s `I2cDevice`).
///
/// Every operation first claims its target address with `I2C_SLAVE` (only
/// when it changes) — never `I2C_SLAVE_FORCE` — so an address a kernel driver
/// owns fails with `EBUSY` ([`ErrorKind::Busy`](lemnos_hal::ErrorKind::Busy))
/// instead of racing the driver. Transfers are combined `I2C_RDWR` calls:
/// repeated starts between messages, one stop.
///
/// embedded-hal's transaction contract (adjacent operations of one direction
/// form one contiguous transfer, no restart) is honoured by merging them into
/// one message.
#[derive(Debug)]
pub struct I2cBus {
    file: File,
    path: PathBuf,
    functionality: u64,
    target: Option<u16>,
}

impl I2cBus {
    /// Opens `/dev/i2c-{bus}`.
    pub fn open(bus: u32) -> io::Result<Self> {
        Self::open_path(format!("/dev/i2c-{bus}"))
    }

    /// Opens an i2c-dev node by path.
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let functionality = sys::functionality(file.as_fd())?;
        Ok(Self {
            file,
            path,
            functionality,
            target: None,
        })
    }

    /// The device node.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The adapter's `I2C_FUNC_*` bits
    /// ([`lemnos_linux_sys::i2c::func`]).
    pub fn functionality(&self) -> u64 {
        self.functionality
    }

    /// Whether the adapter does plain I2C transfers (`I2C_RDWR`); SMBus-only
    /// adapters do not.
    pub fn supports_i2c(&self) -> bool {
        self.functionality & sys::func::I2C != 0
    }

    /// Claims 7-bit `address` for the following operations. Fails with `EBUSY`
    /// while a kernel driver owns it.
    pub fn claim(&mut self, address: u16) -> io::Result<()> {
        if self.target != Some(address) {
            self.target = None;
            sys::set_target(self.file.as_fd(), address)?;
            self.target = Some(address);
        }
        Ok(())
    }

    /// Runs `messages` to `address` as one combined transfer.
    pub fn transfer(&mut self, address: u16, messages: &mut [I2cMessage<'_>]) -> io::Result<()> {
        self.claim(address)?;
        sys::transfer(self.file.as_fd(), address, messages)
    }

    /// Reads `buf.len()` bytes from `address`.
    pub fn read_bytes(&mut self, address: u16, buf: &mut [u8]) -> io::Result<()> {
        self.transfer(address, &mut [I2cMessage::Read(buf)])
    }

    /// Writes `data` to `address`.
    pub fn write_bytes(&mut self, address: u16, data: &[u8]) -> io::Result<()> {
        self.transfer(address, &mut [I2cMessage::Write(data)])
    }

    /// Writes `write`, then reads `read` after a repeated start.
    pub fn write_read_bytes(
        &mut self,
        address: u16,
        write: &[u8],
        read: &mut [u8],
    ) -> io::Result<()> {
        self.transfer(
            address,
            &mut [I2cMessage::Write(write), I2cMessage::Read(read)],
        )
    }

    /// SMBus "send byte".
    pub fn smbus_write_byte(&mut self, address: u16, value: u8) -> io::Result<()> {
        self.claim(address)?;
        sys::smbus_write_byte(self.file.as_fd(), value)
    }

    /// SMBus "write byte data".
    pub fn smbus_write_byte_data(
        &mut self,
        address: u16,
        register: u8,
        value: u8,
    ) -> io::Result<()> {
        self.claim(address)?;
        sys::smbus_write_byte_data(self.file.as_fd(), register, value)
    }

    /// SMBus "read byte data".
    pub fn smbus_read_byte_data(&mut self, address: u16, register: u8) -> io::Result<u8> {
        self.claim(address)?;
        sys::smbus_read_byte_data(self.file.as_fd(), register)
    }

    /// SMBus "write I2C block" (up to 32 bytes).
    pub fn smbus_write_i2c_block_data(
        &mut self,
        address: u16,
        register: u8,
        values: &[u8],
    ) -> io::Result<()> {
        self.claim(address)?;
        sys::smbus_write_i2c_block_data(self.file.as_fd(), register, values)
    }

    /// SMBus "read I2C block" (up to 32 bytes); returns the bytes read.
    pub fn smbus_read_i2c_block_data(
        &mut self,
        address: u16,
        register: u8,
        out: &mut [u8],
    ) -> io::Result<usize> {
        self.claim(address)?;
        sys::smbus_read_i2c_block_data(self.file.as_fd(), register, out)
    }

    fn run(&mut self, address: u8, operations: &mut [Operation<'_>]) -> io::Result<()> {
        if operations.is_empty() {
            return Ok(());
        }
        let address = u16::from(address);
        let n = operations.len();
        if alternates(operations) && n <= STACK_MESSAGES {
            // The common case (register reads and writes, every camera frame):
            // one message per operation, built on the stack.
            let mut messages: [I2cMessage<'_>; STACK_MESSAGES] =
                std::array::from_fn(|_| I2cMessage::Write(&[]));
            for (m, op) in messages.iter_mut().zip(operations.iter_mut()) {
                *m = match op {
                    Operation::Write(data) => I2cMessage::Write(data),
                    Operation::Read(buf) => I2cMessage::Read(buf),
                };
            }
            return self.transfer(address, &mut messages[..n]);
        }
        let plan = Plan::new(operations);
        if plan.is_identity() {
            let mut messages: Vec<I2cMessage<'_>> = operations
                .iter_mut()
                .map(|op| match op {
                    Operation::Write(data) => I2cMessage::Write(data),
                    Operation::Read(buf) => I2cMessage::Read(buf),
                })
                .collect();
            return self.transfer(address, &mut messages);
        }
        // Adjacent operations of one direction: merge into one message each.
        let mut buffers = plan.buffers(operations);
        {
            let mut messages: Vec<I2cMessage<'_>> = buffers
                .iter_mut()
                .map(|(read, buf)| {
                    if *read {
                        I2cMessage::Read(buf.as_mut_slice())
                    } else {
                        I2cMessage::Write(buf.as_slice())
                    }
                })
                .collect();
            self.transfer(address, &mut messages)?;
        }
        plan.scatter(operations, &buffers);
        Ok(())
    }
}

/// Messages of a transaction built on the stack (longer ones allocate).
const STACK_MESSAGES: usize = 8;

/// Whether no two adjacent operations go the same direction (each is its own
/// message), without allocating.
fn alternates(operations: &[Operation<'_>]) -> bool {
    operations
        .windows(2)
        .all(|w| matches!(w[0], Operation::Read(_)) != matches!(w[1], Operation::Read(_)))
}

/// Which operations of a transaction share a message.
struct Plan {
    /// Index of the first operation of each run.
    runs: Vec<usize>,
}

impl Plan {
    fn new(operations: &[Operation<'_>]) -> Self {
        let mut runs = Vec::new();
        let mut last: Option<bool> = None;
        for (i, op) in operations.iter().enumerate() {
            let read = matches!(op, Operation::Read(_));
            if last != Some(read) {
                runs.push(i);
                last = Some(read);
            }
        }
        Self { runs }
    }

    fn is_identity(&self) -> bool {
        self.runs.len() == self.runs.last().map_or(0, |l| l + 1)
    }

    fn bounds(&self, len: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.runs
            .iter()
            .enumerate()
            .map(move |(k, &start)| (start, self.runs.get(k + 1).copied().unwrap_or(len)))
    }

    fn buffers(&self, operations: &[Operation<'_>]) -> Vec<(bool, Vec<u8>)> {
        self.bounds(operations.len())
            .map(|(start, end)| {
                let ops = &operations[start..end];
                match ops[0] {
                    Operation::Read(_) => {
                        let len = ops
                            .iter()
                            .map(|op| match op {
                                Operation::Read(buf) => buf.len(),
                                Operation::Write(_) => 0,
                            })
                            .sum();
                        (true, vec![0u8; len])
                    }
                    Operation::Write(_) => {
                        let mut data = Vec::new();
                        for op in ops {
                            if let Operation::Write(bytes) = op {
                                data.extend_from_slice(bytes);
                            }
                        }
                        (false, data)
                    }
                }
            })
            .collect()
    }

    fn scatter(&self, operations: &mut [Operation<'_>], buffers: &[(bool, Vec<u8>)]) {
        let bounds: Vec<(usize, usize)> = self.bounds(operations.len()).collect();
        for ((start, end), (read, data)) in bounds.into_iter().zip(buffers) {
            if !read {
                continue;
            }
            let mut at = 0;
            for op in &mut operations[start..end] {
                if let Operation::Read(buf) = op {
                    buf.copy_from_slice(&data[at..at + buf.len()]);
                    at += buf.len();
                }
            }
        }
    }
}

impl AsFd for I2cBus {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl ErrorType for I2cBus {
    type Error = IoError;
}

impl I2c for I2cBus {
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
        self.run(address, operations).map_err(IoError::from)
    }
}

/// Completes synchronously: kernel I2C transfers block the calling thread for
/// the transfer's duration (microseconds to a few milliseconds). Lets async
/// drivers run on Linux unchanged.
impl embedded_hal_async::i2c::I2c for I2cBus {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), IoError> {
        I2c::transaction(self, address, operations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_merge_adjacent_operations() {
        let a = [1u8, 2];
        let b = [3u8];
        let mut r1 = [0u8; 2];
        let mut r2 = [0u8; 1];
        let mut ops = [
            Operation::Write(&a),
            Operation::Write(&b),
            Operation::Read(&mut r1),
            Operation::Read(&mut r2),
        ];
        let plan = Plan::new(&ops);
        assert_eq!(plan.runs, [0, 2]);
        assert!(!plan.is_identity());
        assert!(!alternates(&ops));
        let mut buffers = plan.buffers(&ops);
        assert_eq!(buffers[0], (false, vec![1, 2, 3]));
        assert_eq!(buffers[1], (true, vec![0, 0, 0]));
        buffers[1].1.copy_from_slice(&[7, 8, 9]);
        plan.scatter(&mut ops, &buffers);
        assert_eq!((r1, r2), ([7, 8], [9]));
        let single = [Operation::Write(&a), Operation::Read(&mut [0u8; 1])];
        assert!(Plan::new(&single).is_identity());
        assert!(alternates(&single));
    }

    #[test]
    fn open_missing_bus_fails_cleanly() {
        let err = I2cBus::open_path("/dev/i2c-does-not-exist").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
