//! i2c-dev (`/dev/i2c-N`): adapter functionality, target selection, combined
//! `I2C_RDWR` transfers and SMBus transfers. Ported from Styx
//! (`styx-kernel/src/bus/i2c.rs`).

use crate::ioctl::{ioctl_int, ioctl_ptr};
use std::io;
use std::os::fd::BorrowedFd;

const I2C_SLAVE: u32 = 0x0703;
const I2C_FUNCS: u32 = 0x0705;
const I2C_RDWR: u32 = 0x0707;
const I2C_SMBUS: u32 = 0x0720;

const I2C_M_RD: u16 = 0x0001;

/// Most messages the kernel accepts in one `I2C_RDWR` call (`I2C_RDWR_IOCTL_MAX_MSGS`).
pub const MAX_MESSAGES: usize = 42;
/// Longest message the kernel accepts (`8192` bytes).
pub const MAX_MESSAGE_LEN: usize = 8192;
/// Longest SMBus block (`I2C_SMBUS_BLOCK_MAX`).
pub const SMBUS_BLOCK_MAX: usize = 32;

/// `I2C_FUNC_*` bits of [`functionality`].
pub mod func {
    /// Plain I2C transfers (`I2C_RDWR`).
    pub const I2C: u64 = 0x0000_0001;
    /// 10-bit addresses.
    pub const TEN_BIT_ADDR: u64 = 0x0000_0002;
    /// Messages without a (repeated) start (`I2C_M_NOSTART`).
    pub const NOSTART: u64 = 0x0000_0010;
    /// SMBus quick command.
    pub const SMBUS_QUICK: u64 = 0x0001_0000;
    /// SMBus receive byte.
    pub const SMBUS_READ_BYTE: u64 = 0x0002_0000;
    /// SMBus send byte.
    pub const SMBUS_WRITE_BYTE: u64 = 0x0004_0000;
    /// SMBus read byte data.
    pub const SMBUS_READ_BYTE_DATA: u64 = 0x0008_0000;
    /// SMBus write byte data.
    pub const SMBUS_WRITE_BYTE_DATA: u64 = 0x0010_0000;
    /// SMBus read I2C block.
    pub const SMBUS_READ_I2C_BLOCK: u64 = 0x0400_0000;
    /// SMBus write I2C block.
    pub const SMBUS_WRITE_I2C_BLOCK: u64 = 0x0800_0000;
}

/// `struct i2c_msg`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    buf: *mut u8,
}

/// `struct i2c_rdwr_ioctl_data`.
#[repr(C)]
#[derive(Debug)]
struct I2cRdwrData {
    msgs: *mut I2cMsg,
    nmsgs: u32,
}

/// `union i2c_smbus_data` (the largest member is the block: length byte,
/// 32 data bytes, one spare).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct I2cSmbusData {
    block: [u8; SMBUS_BLOCK_MAX + 2],
}

/// `struct i2c_smbus_ioctl_data`.
#[repr(C)]
#[derive(Debug)]
struct I2cSmbusIoctlData {
    read_write: u8,
    command: u8,
    size: u32,
    data: *mut I2cSmbusData,
}

const SMBUS_READ: u8 = 1;
const SMBUS_WRITE: u8 = 0;
const SMBUS_BYTE: u32 = 1;
const SMBUS_BYTE_DATA: u32 = 2;
const SMBUS_I2C_BLOCK_DATA: u32 = 8;

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// One message of a combined transfer.
#[derive(Debug)]
pub enum Message<'a> {
    /// Write these bytes.
    Write(&'a [u8]),
    /// Read into this buffer.
    Read(&'a mut [u8]),
}

impl Message<'_> {
    /// Bytes this message moves.
    pub fn len(&self) -> usize {
        match self {
            Message::Write(data) => data.len(),
            Message::Read(buf) => buf.len(),
        }
    }

    /// Whether it moves no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn msg_len(len: usize) -> io::Result<u16> {
    if len == 0 || len > MAX_MESSAGE_LEN {
        return Err(invalid(format!(
            "I2C message of {len} bytes (1..={MAX_MESSAGE_LEN})"
        )));
    }
    Ok(len as u16)
}

/// Builds the kernel message array for `messages` to `addr`. The returned
/// structs borrow the buffers in `messages` through raw pointers; the caller
/// keeps `messages` alive and unmoved while they are used.
fn build_messages(addr: u16, messages: &mut [Message<'_>]) -> io::Result<Vec<I2cMsg>> {
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err(invalid(format!(
            "{} messages in one transfer (1..={MAX_MESSAGES})",
            messages.len()
        )));
    }
    messages
        .iter_mut()
        .map(|m| match m {
            Message::Write(data) => Ok(I2cMsg {
                addr,
                flags: 0,
                len: msg_len(data.len())?,
                // The kernel only reads write buffers.
                buf: data.as_ptr().cast_mut(),
            }),
            Message::Read(buf) => Ok(I2cMsg {
                addr,
                flags: I2C_M_RD,
                len: msg_len(buf.len())?,
                buf: buf.as_mut_ptr(),
            }),
        })
        .collect()
}

/// The adapter's `I2C_FUNC_*` bits (see [`func`]).
pub fn functionality(fd: BorrowedFd<'_>) -> io::Result<u64> {
    let mut funcs: libc::c_ulong = 0;
    // SAFETY: I2C_FUNCS writes one `unsigned long` through the pointer, which
    // points at a live local.
    unsafe { ioctl_ptr(fd, I2C_FUNCS, &mut funcs) }?;
    Ok(funcs as u64)
}

/// Selects (claims) the 7-bit target `addr` for plain `read`/`write` and SMBus
/// calls with `I2C_SLAVE`. Fails with `EBUSY` while a kernel driver is bound
/// to the address. `I2C_SLAVE_FORCE` is deliberately not offered.
pub fn set_target(fd: BorrowedFd<'_>, addr: u16) -> io::Result<()> {
    if addr > 0x7f {
        return Err(invalid(format!("7-bit I2C address {addr:#x} out of range")));
    }
    ioctl_int(fd, I2C_SLAVE, libc::c_ulong::from(addr)).map(|_| ())
}

/// Runs `messages` to `addr` as one combined transfer (`I2C_RDWR`: repeated
/// starts between messages, one stop).
pub fn transfer(fd: BorrowedFd<'_>, addr: u16, messages: &mut [Message<'_>]) -> io::Result<()> {
    let mut msgs = build_messages(addr, messages)?;
    let mut data = I2cRdwrData {
        msgs: msgs.as_mut_ptr(),
        nmsgs: msgs.len() as u32,
    };
    // SAFETY: `data` points at `msgs`, whose buffers point into `messages`; all
    // stay alive and unmoved for the duration of the call, and read buffers
    // are exclusively borrowed.
    let done = unsafe { ioctl_ptr(fd, I2C_RDWR, &mut data) }?;
    if done as usize != msgs.len() {
        return Err(io::Error::other(format!(
            "I2C transfer completed {done} of {} messages",
            msgs.len()
        )));
    }
    Ok(())
}

fn smbus(
    fd: BorrowedFd<'_>,
    read_write: u8,
    command: u8,
    size: u32,
    data: &mut I2cSmbusData,
) -> io::Result<()> {
    let mut args = I2cSmbusIoctlData {
        read_write,
        command,
        size,
        data,
    };
    // SAFETY: `args` points at `data`, a live, exclusively borrowed
    // `i2c_smbus_data` the kernel reads and writes for the call's duration.
    unsafe { ioctl_ptr(fd, I2C_SMBUS, &mut args) }?;
    Ok(())
}

fn empty_data() -> I2cSmbusData {
    I2cSmbusData {
        block: [0; SMBUS_BLOCK_MAX + 2],
    }
}

/// SMBus "send byte" to the selected target.
pub fn smbus_write_byte(fd: BorrowedFd<'_>, value: u8) -> io::Result<()> {
    let mut data = empty_data();
    // The value travels in the command field.
    smbus(fd, SMBUS_WRITE, value, SMBUS_BYTE, &mut data)
}

/// SMBus "receive byte" from the selected target.
pub fn smbus_read_byte(fd: BorrowedFd<'_>) -> io::Result<u8> {
    let mut data = empty_data();
    smbus(fd, SMBUS_READ, 0, SMBUS_BYTE, &mut data)?;
    Ok(data.block[0])
}

/// SMBus "write byte data": `register`, then `value`.
pub fn smbus_write_byte_data(fd: BorrowedFd<'_>, register: u8, value: u8) -> io::Result<()> {
    let mut data = empty_data();
    data.block[0] = value;
    smbus(fd, SMBUS_WRITE, register, SMBUS_BYTE_DATA, &mut data)
}

/// SMBus "read byte data" from `register`.
pub fn smbus_read_byte_data(fd: BorrowedFd<'_>, register: u8) -> io::Result<u8> {
    let mut data = empty_data();
    smbus(fd, SMBUS_READ, register, SMBUS_BYTE_DATA, &mut data)?;
    Ok(data.block[0])
}

/// SMBus "write I2C block": `register`, then up to 32 bytes.
pub fn smbus_write_i2c_block_data(
    fd: BorrowedFd<'_>,
    register: u8,
    values: &[u8],
) -> io::Result<()> {
    if values.is_empty() || values.len() > SMBUS_BLOCK_MAX {
        return Err(invalid(format!(
            "SMBus block of {} bytes (1..={SMBUS_BLOCK_MAX})",
            values.len()
        )));
    }
    let mut data = empty_data();
    data.block[0] = values.len() as u8;
    data.block[1..=values.len()].copy_from_slice(values);
    smbus(fd, SMBUS_WRITE, register, SMBUS_I2C_BLOCK_DATA, &mut data)
}

/// SMBus "read I2C block": up to 32 bytes from `register` into `out`; returns
/// the bytes read.
pub fn smbus_read_i2c_block_data(
    fd: BorrowedFd<'_>,
    register: u8,
    out: &mut [u8],
) -> io::Result<usize> {
    if out.is_empty() || out.len() > SMBUS_BLOCK_MAX {
        return Err(invalid(format!(
            "SMBus block of {} bytes (1..={SMBUS_BLOCK_MAX})",
            out.len()
        )));
    }
    let mut data = empty_data();
    data.block[0] = out.len() as u8;
    smbus(fd, SMBUS_READ, register, SMBUS_I2C_BLOCK_DATA, &mut data)?;
    let n = usize::from(data.block[0]).min(out.len());
    out[..n].copy_from_slice(&data.block[1..=n]);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::fd::AsFd;

    #[test]
    fn struct_layouts_match_the_kernel() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<I2cMsg>(), 16);
            assert_eq!(size_of::<I2cRdwrData>(), 16);
            assert_eq!(size_of::<I2cSmbusIoctlData>(), 16);
        }
        assert_eq!(std::mem::offset_of!(I2cMsg, len), 4);
        assert_eq!(size_of::<I2cSmbusData>(), 34);
    }

    #[test]
    fn builds_combined_read() {
        let addr = [0x30, 0x0a];
        let mut out = [0u8; 2];
        let mut messages = [Message::Write(&addr), Message::Read(&mut out)];
        let msgs = build_messages(0x60, &mut messages).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!((msgs[0].addr, msgs[0].flags, msgs[0].len), (0x60, 0, 2));
        assert_eq!(
            (msgs[1].addr, msgs[1].flags, msgs[1].len),
            (0x60, I2C_M_RD, 2)
        );
    }

    #[test]
    fn rejects_bad_transfers() {
        assert!(build_messages(0x60, &mut []).is_err());
        let empty: [u8; 0] = [];
        assert!(build_messages(0x60, &mut [Message::Write(&empty)]).is_err());
        let data = [0u8; 1];
        let mut many: Vec<Message<'_>> =
            (0..=MAX_MESSAGES).map(|_| Message::Write(&data)).collect();
        assert!(build_messages(0x60, &mut many).is_err());
        let long = vec![0u8; MAX_MESSAGE_LEN + 1];
        assert!(build_messages(0x60, &mut [Message::Write(&long)]).is_err());
    }

    /// Queries every i2c-dev node present and claims an address nothing uses
    /// on a typical system, without transferring. Skips when there are none.
    #[test]
    fn claims_addresses_on_present_buses() {
        let Ok(dir) = std::fs::read_dir("/dev") else {
            return;
        };
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("i2c-") {
                continue;
            }
            let Ok(file) = OpenOptions::new().read(true).write(true).open(entry.path()) else {
                continue;
            };
            assert!(functionality(file.as_fd()).is_ok());
            let _ = set_target(file.as_fd(), 0x7e);
        }
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(set_target(file.as_fd(), 0x80).is_err());
    }
}
