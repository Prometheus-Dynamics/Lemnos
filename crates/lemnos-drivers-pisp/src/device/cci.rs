//! A camera sensor's registers over i2c-dev (feature `csi2-module`): 16-bit register addresses,
//! big-endian values (the MIPI CCI convention most sensors follow), written from a register file
//! read at run time. Lemnos ships no register tables: the file is the caller's.
//!
//! A register file is text, one item per line, `#` to the end of a line a comment:
//!
//! ```text
//! [init]               # a section; items before the first belong to ""
//! 0x0103 0x01          # write one byte
//! delay 10             # sleep 10 ms
//! 0x380c:2 0x02d8      # write 2 bytes, big-endian (1 to 4)
//! [stream_on]
//! 0x0100 0x01
//! ```
//!
//! [`I2cCci`] claims the address with `I2C_SLAVE`, never forcing it: a kernel driver bound to the
//! sensor must be unbound first. [`MockCci`] keeps the registers in memory, for tests and for a
//! simulator without a sensor.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use lemnos_linux_sys::i2c::{self, Message};

/// One item of a register file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterOp {
    /// Write `len` bytes (1 to 4) of `value`, big-endian, from register `addr` on.
    Write { addr: u16, len: u8, value: u32 },
    /// Sleep this many milliseconds.
    Delay(u32),
}

/// A register file: named sections of [`RegisterOp`]s, in file order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegisterFile {
    sections: Vec<(String, Vec<RegisterOp>)>,
}

fn number(s: &str) -> Option<u32> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

impl RegisterFile {
    /// Parses `text`; the error names the first bad line.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut file = Self::default();
        let mut current = (String::new(), Vec::new());
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let bad = || format!("line {}: {raw:?}", n + 1);
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let done = std::mem::replace(&mut current, (name.trim().to_string(), Vec::new()));
                if !done.0.is_empty() || !done.1.is_empty() {
                    file.sections.push(done);
                }
                continue;
            }
            let mut words = line.split_whitespace();
            let (first, second) = (words.next().ok_or_else(bad)?, words.next().ok_or_else(bad)?);
            if words.next().is_some() {
                return Err(bad());
            }
            let op = if first == "delay" {
                RegisterOp::Delay(number(second).ok_or_else(bad)?)
            } else {
                let (addr, len) = match first.split_once(':') {
                    Some((a, l)) => (a, number(l).ok_or_else(bad)?),
                    None => (first, 1),
                };
                let addr = number(addr).filter(|a| *a <= 0xffff).ok_or_else(bad)?;
                let value = number(second).ok_or_else(bad)?;
                if !(1..=4).contains(&len) || (len < 4 && value >> (8 * len) != 0) {
                    return Err(bad());
                }
                RegisterOp::Write {
                    addr: addr as u16,
                    len: len as u8,
                    value,
                }
            };
            current.1.push(op);
        }
        if !current.0.is_empty() || !current.1.is_empty() {
            file.sections.push(current);
        }
        Ok(file)
    }

    /// Reads and parses the file at `path`.
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// The section `name`'s items, or `None` when the file has no such section. A name given
    /// twice is one section, in file order.
    pub fn section(&self, name: &str) -> Option<Vec<RegisterOp>> {
        let mut found = None;
        for (n, ops) in &self.sections {
            if n == name {
                found.get_or_insert_with(Vec::new).extend_from_slice(ops);
            }
        }
        found
    }

    /// The section names, in file order, each once.
    pub fn names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for (n, _) in &self.sections {
            if !out.contains(&n.as_str()) {
                out.push(n);
            }
        }
        out
    }
}

/// A sensor's register space.
pub trait Cci {
    /// Writes `bytes` from register `addr` on.
    fn write(&mut self, addr: u16, bytes: &[u8]) -> io::Result<()>;
    /// Reads `buf.len()` bytes from register `addr` on.
    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()>;

    /// Reads `len` bytes (1 to 4) from `addr` as one big-endian value.
    fn read_be(&mut self, addr: u16, len: usize) -> io::Result<u32> {
        let mut buf = [0; 4];
        let buf = buf
            .get_mut(..len)
            .filter(|b| !b.is_empty())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.read(addr, buf)?;
        Ok(buf.iter().fold(0, |v, b| (v << 8) | u32::from(*b)))
    }

    /// Writes `value` as `len` bytes (1 to 4), big-endian, from `addr` on.
    fn write_be(&mut self, addr: u16, len: usize, value: u32) -> io::Result<()> {
        let bytes = value.to_be_bytes();
        let bytes = bytes
            .get(4 - len.min(4)..)
            .filter(|b| !b.is_empty() && len <= 4)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.write(addr, bytes)
    }

    /// Runs `ops` in order.
    fn apply(&mut self, ops: &[RegisterOp]) -> io::Result<()> {
        for op in ops {
            match *op {
                RegisterOp::Write { addr, len, value } => {
                    self.write_be(addr, usize::from(len), value)?;
                }
                RegisterOp::Delay(ms) => std::thread::sleep(Duration::from_millis(u64::from(ms))),
            }
        }
        Ok(())
    }
}

/// A sensor on an i2c-dev bus (`/dev/i2c-N`) at a 7-bit address.
pub struct I2cCci {
    bus: File,
    addr: u16,
}

impl I2cCci {
    /// Opens `bus` and claims `addr` (`EBUSY` while a kernel driver is bound to it).
    pub fn open(bus: impl AsRef<Path>, addr: u16) -> io::Result<Self> {
        let bus = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(bus)?;
        i2c::set_target(bus.as_fd(), addr)?;
        Ok(Self { bus, addr })
    }
}

impl Cci for I2cCci {
    fn write(&mut self, addr: u16, bytes: &[u8]) -> io::Result<()> {
        let mut msg = [0u8; 2 + 32];
        let out = msg
            .get_mut(..2 + bytes.len())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        out[..2].copy_from_slice(&addr.to_be_bytes());
        out[2..].copy_from_slice(bytes);
        i2c::transfer(self.bus.as_fd(), self.addr, &mut [Message::Write(out)])
    }

    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()> {
        let reg = addr.to_be_bytes();
        i2c::transfer(
            self.bus.as_fd(),
            self.addr,
            &mut [Message::Write(&reg), Message::Read(buf)],
        )
    }
}

/// A register space in memory: every write recorded, reads of unwritten registers 0.
#[derive(Clone, Debug, Default)]
pub struct MockCci {
    /// The registers' values.
    pub registers: BTreeMap<u16, u8>,
    /// Every write, in order: the first register and the bytes.
    pub writes: Vec<(u16, Vec<u8>)>,
}

impl MockCci {
    /// A register space with `registers` set (an identification register, say).
    pub fn with(registers: &[(u16, u8)]) -> Self {
        Self {
            registers: registers.iter().copied().collect(),
            writes: Vec::new(),
        }
    }
}

impl Cci for MockCci {
    fn write(&mut self, addr: u16, bytes: &[u8]) -> io::Result<()> {
        for (i, b) in bytes.iter().enumerate() {
            self.registers.insert(addr.wrapping_add(i as u16), *b);
        }
        self.writes.push((addr, bytes.to_vec()));
        Ok(())
    }

    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()> {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self
                .registers
                .get(&addr.wrapping_add(i as u16))
                .copied()
                .unwrap_or(0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# a sensor
0x0103 0x01
delay 1
[init]
0x380c:2 0x02d8   # HTS
0x3500:3 0x2820
[stream_on]
0x0100 1
[init]
0x5e00 0x80
";

    #[test]
    fn a_register_file_parses_into_sections() {
        let f = RegisterFile::parse(FILE).unwrap();
        assert_eq!(f.names(), ["", "init", "stream_on"]);
        assert_eq!(
            f.section("").unwrap(),
            [
                RegisterOp::Write {
                    addr: 0x0103,
                    len: 1,
                    value: 1
                },
                RegisterOp::Delay(1)
            ]
        );
        let init = f.section("init").unwrap();
        assert_eq!(init.len(), 3);
        assert_eq!(
            init[2],
            RegisterOp::Write {
                addr: 0x5e00,
                len: 1,
                value: 0x80
            }
        );
        assert_eq!(f.section("missing"), None);
    }

    #[test]
    fn bad_lines_are_named() {
        for bad in [
            "0x10000 1",
            "0x0100 0x100",
            "0x0100:5 0",
            "0x0100",
            "0x0100 1 2",
            "x 1",
        ] {
            let err = RegisterFile::parse(&format!("[a]\n{bad}\n")).unwrap_err();
            assert!(err.starts_with("line 2:"), "{bad}: {err}");
        }
    }

    #[test]
    fn writes_are_big_endian_and_recorded() {
        let f = RegisterFile::parse(FILE).unwrap();
        let mut cci = MockCci::with(&[(0x300a, 0x97), (0x300b, 0x82)]);
        assert_eq!(cci.read_be(0x300a, 2).unwrap(), 0x9782);
        cci.apply(&f.section("init").unwrap()).unwrap();
        assert_eq!(cci.writes[0], (0x380c, vec![0x02, 0xd8]));
        assert_eq!(cci.writes[1], (0x3500, vec![0x00, 0x28, 0x20]));
        assert_eq!(cci.read_be(0x3500, 3).unwrap(), 0x2820);
        assert!(cci.write_be(0, 5, 0).is_err());
        assert!(cci.read_be(0, 0).is_err());
    }
}
