//! The vendor driver's `vvcam-isp` misc device (feature `device`, Linux): register reads and
//! writes through its ioctls, and the [`RegisterAccess`] a parameter builder writes through.
//!
//! ```text
//! /dev/vvcam-isp.0  (misc device, ioctl magic 'v')
//!   VVCAM_ISP_READ_REG   _IOWR('v', 0x02, vvcam_isp_reg_t)   { addr, value }
//!   VVCAM_ISP_WRITE_REG  _IOW ('v', 0x03, vvcam_isp_reg_t)
//!   VVCAM_ISP_RESET      _IOW ('v', 0x01, u32)
//! ```
//!
//! The request numbers and the structure are in `lemnos-linux-sys::vvcam_isp` (the `_IOC`
//! encodings of `vvcam_isp.h`, MIT, VeriSilicon). The driver maps the register window and reads
//! or writes at `base + addr`, with no range check of its own, so the addresses this crate
//! passes are the ones in [`crate::regs`]. `mmap` of the window is not used: a read or write per
//! register is enough for a parameter set of a few hundred writes per frame. Nothing here has
//! run on a board yet (written, untested on board).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::path::Path;

use lemnos_linux_sys::Ioctl;
use lemnos_linux_sys::vvcam_isp::{self, RegPair};

use crate::RegisterAccess;
use crate::stats::{self, Raw};

/// The misc device of the vendor driver's ISP.
pub const DEVICE_PATH: &str = "/dev/vvcam-isp.0";

/// An open `vvcam-isp` device.
#[derive(Debug)]
pub struct IspDevice {
    file: File,
}

impl IspDevice {
    /// Opens the device at `path` (normally [`DEVICE_PATH`]) for reading and writing.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    /// Resets the ISP with the driver's reset value.
    pub fn reset(&self, value: u32) -> io::Result<()> {
        let mut v = value;
        self.call(vvcam_isp::RESET, &mut v)
    }

    /// Reads a statistics snapshot ([`stats::read_raw`]).
    pub fn statistics(&mut self) -> io::Result<Raw> {
        stats::read_raw(self)
    }

    fn call<T: lemnos_linux_sys::Pod>(&self, request: Ioctl<T>, arg: &mut T) -> io::Result<()> {
        request.call(self.file.as_fd(), arg).map(|_| ())
    }
}

impl RegisterAccess for IspDevice {
    type Error = io::Error;

    fn read(&mut self, addr: u32) -> io::Result<u32> {
        let mut pair = RegPair { addr, value: 0 };
        self.call(vvcam_isp::READ_REG, &mut pair)?;
        Ok(pair.value)
    }

    fn write(&mut self, addr: u32, value: u32) -> io::Result<()> {
        let mut pair = RegPair { addr, value };
        self.call(vvcam_isp::WRITE_REG, &mut pair)
    }
}
