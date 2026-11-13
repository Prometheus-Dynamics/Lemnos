//! The second video engine's character device on the vendor kernel (`/dev/cedar_dev_ve2`; the
//! path is always the caller's), as far as it can be used soundly from user space: the
//! requests that take a value or one small plain structure, and a read-only mapping of the
//! register window. This crate holds the `unsafe`; `lemnos-driver-a733-video` holds what the
//! requests are for.
//!
//! # What is issued, and why it is sound
//!
//! - [`Plain`] requests take their argument by value. The kernel reads nothing through it.
//!   Each value is checked against what the request takes before the call: no argument where
//!   the kernel takes none, one lock bit, a mode the kernel knows, a wait of at most
//!   [`MAX_WAIT_S`] seconds.
//! - [`Node::map_dma_buf`] and [`Node::unmap_dma_buf`] pass a pointer to an 8-byte
//!   [`DmaBufParam`] on this function's stack, alive for the call. The kernel reads the
//!   descriptor and writes the bus address into it. The descriptor is a borrowed one, so it
//!   is open for the call.
//! - The register window is mapped once, read-only, for exactly [`WINDOW_BYTES`] at offset
//!   0: the kernel maps as many bytes as asked from the window's start whatever the offset,
//!   so a longer mapping would reach whatever follows it. [`Node::read`] reads one aligned
//!   register inside it with a volatile load.
//!
//! # Register writes go through the kernel module
//!
//! Nothing here writes a register on the vendor node: the window is not mapped writable and no
//! function takes a value for one. A register write can make the engine read or write memory,
//! and how far it reaches cannot be bounded from user space, behind an IOMMU address space other
//! devices' buffers share. The kernel module's register jobs (`lemnos-driver-regjob-sys`) are
//! the route that writes them: `lemnos.ko` drives the engine itself, checks every write and every
//! address against the buffers a session holds, and writes the address registers itself. `IOCTL_SET_REFCOUNT`, `IOCTL_FLUSH_CACHE_RANGE`,
//! `IOCTL_GET_IOMMU_ADDR`, `IOCTL_GET_ENV_INFO` and the frequency requests are never issued.
//!
//! # Source
//!
//! Interface facts only, no code copied, from the Radxa `allwinner-bsp` tree at commit
//! `a842bb8dc74608ba13975c97c31b9518f1976c01` (branch `cubie-aiot-v1.5.0`):
//! `include/uapi/linux/cedar_ve_uapi.h` (`GPL-2.0-or-later WITH Linux-syscall-note`: the
//! request numbers of `enum IOCTL_CMD` and `struct dma_buf_param`),
//! `drivers/ve/cedar-ve/cedar_ve.h` (`VE_LOCK_*`, `enum VE_MODE`) and
//! `drivers/ve/cedar-ve/cedar_ve.c` (`_compat_cedardev_ioctl`: which requests take a value,
//! which a structure; `cedardev_mmap`). They are the facts `lemnos-driver-a733-video`'s `uapi`
//! module transcribes; that crate's tests check the two copies against each other. The
//! requests are plain numbers, not `_IOC` encodings.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E37.md`.

#![cfg(target_os = "linux")]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use lemnos_linux_sys::mmap::Mapping;

/// The register window's size in bytes, and the only length ever mapped.
pub const WINDOW_BYTES: usize = 0x1000;

/// The longest wait a wait request is given, in seconds. The kernel multiplies the value by
/// its tick rate without a bound of its own.
pub const MAX_WAIT_S: u32 = 3600;

/// `struct dma_buf_param`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DmaBufParam {
    /// The dma-buf's descriptor, in.
    pub fd: i32,
    /// Its bus address, out.
    pub phy_addr: u32,
}

/// `IOCTL_MAP_DMA_BUF`.
pub const MAP_DMA_BUF: u32 = 0x504;
/// `IOCTL_UNMAP_DMA_BUF`.
pub const UNMAP_DMA_BUF: u32 = 0x505;

/// A request whose argument the kernel takes by value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Plain {
    /// `IOCTL_WAIT_VE_DE`: waits for the decoder interrupt, a timeout in seconds.
    WaitDecoder = 0x102,
    /// `IOCTL_WAIT_VE_EN`: waits for the encoder interrupt, a timeout in seconds.
    WaitEncoder = 0x103,
    /// `IOCTL_RESET_VE`: pulses the engine's reset line. No argument.
    Reset = 0x104,
    /// `IOCTL_ENGINE_REQ`: counts a user of the engine. No argument.
    EngineRequest = 0x206,
    /// `IOCTL_ENGINE_REL`: the reverse. No argument.
    EngineRelease = 0x207,
    /// `IOCTL_GET_LOCK`: takes the lock named by one lock bit.
    GetLock = 0x310,
    /// `IOCTL_RELEASE_LOCK`: releases it.
    ReleaseLock = 0x311,
    /// `IOCTL_VE_MODE`: which interrupt the engine's handler looks at.
    Mode = 0x800,
    /// `IOCTL_WAIT_VCU_ENC`: a wait, in seconds.
    WaitVcuEncoder = 0x801,
    /// `IOCTL_CLEAR_EN_INT_FLAG`: forgets an encoder interrupt. No argument.
    ClearEncoderInterrupt = 0x803,
    /// `IOCTL_GET_VE_TOP_REG_OFFSET`: answers the offset as the call's result. No argument.
    TopRegisterOffset = 0x804,
    /// `IOCTL_WAIT_VCU_DEC`: a wait, in seconds.
    WaitVcuDecoder = 0x805,
}

impl Plain {
    /// Whether the request takes `value`.
    #[must_use]
    pub const fn takes(self, value: u32) -> bool {
        match self {
            Self::Reset
            | Self::EngineRequest
            | Self::EngineRelease
            | Self::ClearEncoderInterrupt
            | Self::TopRegisterOffset => value == 0,
            // `VE_LOCK_VDEC`, `VE_LOCK_VENC`, `VE_LOCK_JDEC`: one of them.
            Self::GetLock | Self::ReleaseLock => matches!(value, 1 | 2 | 4),
            // `enum VE_MODE`: 0 to 4.
            Self::Mode => value <= 4,
            Self::WaitDecoder | Self::WaitEncoder | Self::WaitVcuEncoder | Self::WaitVcuDecoder => {
                value <= MAX_WAIT_S
            }
        }
    }
}

/// One open of the engine's node, with the register window mapped read-only.
#[derive(Debug)]
pub struct Node {
    /// Declared first: the mapping is dropped before the file is closed.
    window: Mapping,
    file: File,
}

impl Node {
    /// Opens the node at `path` read-write and close-on-exec, and maps its register window.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        Self::on(file)
    }

    /// The node on an open file: maps exactly [`WINDOW_BYTES`] of it at offset 0, read-only.
    pub fn on(file: File) -> io::Result<Self> {
        let window = Mapping::new(file.as_fd(), WINDOW_BYTES, 0, false)?;
        Ok(Self { window, file })
    }

    /// Issues `request` with `value`, and returns the call's result. `InvalidInput`, and no
    /// call, when the request doesn't take the value.
    pub fn plain(&self, request: Plain, value: u32) -> io::Result<i32> {
        if !request.takes(value) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let argument = libc::c_ulong::from(value);
        // SAFETY: the request takes its argument by value (`_compat_cedardev_ioctl` reads
        // `arg` as a number for every `Plain` request), so the kernel follows no pointer;
        // the descriptor is this node's open file.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, argument) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ret)
    }

    /// Attaches `buffer` to the engine (`IOCTL_MAP_DMA_BUF`) and returns the bus address the
    /// kernel answers.
    pub fn map_dma_buf(&self, buffer: BorrowedFd<'_>) -> io::Result<u32> {
        let mut param = DmaBufParam {
            fd: buffer.as_raw_fd(),
            phy_addr: 0,
        };
        self.dma_buf(MAP_DMA_BUF, &mut param)?;
        Ok(param.phy_addr)
    }

    /// Detaches the mapping made from `buffer` (`IOCTL_UNMAP_DMA_BUF`): the same descriptor
    /// that was mapped.
    pub fn unmap_dma_buf(&self, buffer: BorrowedFd<'_>) -> io::Result<()> {
        let mut param = DmaBufParam {
            fd: buffer.as_raw_fd(),
            phy_addr: 0,
        };
        self.dma_buf(UNMAP_DMA_BUF, &mut param)
    }

    fn dma_buf(&self, request: u32, param: &mut DmaBufParam) -> io::Result<()> {
        let pointer: *mut DmaBufParam = param;
        // SAFETY: both requests copy one `struct dma_buf_param` in from the pointer and the
        // map request copies it back out; `pointer` is to a live, exclusively borrowed
        // `DmaBufParam` of that layout, which outlives the call.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, pointer) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The register at byte `offset` of the window. `None` unless `offset` is a multiple of 4
    /// with the whole register inside the window. Read it only while the engine's clocks are
    /// on (after [`Plain::EngineRequest`]): a read with them off can stall the bus.
    #[must_use]
    pub fn read(&self, offset: usize) -> Option<u32> {
        if !offset.is_multiple_of(4) {
            return None;
        }
        self.window
            .read_volatile::<4>(offset)
            .map(u32::from_ne_bytes)
    }
}

#[cfg(test)]
mod tests;
