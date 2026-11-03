//! The DesignWare AXI DMAC inside RP1 (`dma2` in `/sys/kernel/debug/dmaengine`,
//! 8 channels, at RP1 `0x1f00188000`): register offsets and bit fields as the
//! kernel's `dw-axi-dmac` driver uses them, and a mapping of the registers.
//!
//! The controller is the kernel's: its driver owns the channels that the RP1
//! PIO uses. The mapping is root only (it is `/dev/mem`), and it is the only
//! way in from user space. Untested on board for transfers (see
//! `docs/dma.md`); the reads of the version and the channel state are the parts
//! verified on the BCM2712 boards.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

/// `DMAC_ID`.
pub const ID: usize = 0x000;
/// `DMAC_COMPVER`: the component version, ASCII (`"*301"`-style) on this IP.
pub const COMPVER: usize = 0x008;
/// `DMAC_CFG`: bit 0 `DMAC_EN` (the controller runs), bit 1 `INT_EN`.
pub const CFG: usize = 0x010;
/// `DMAC_CHEN`, low word: channel `n` is enabled at bit `n`, and its write
/// enable at bit `8 + n` (a write sets only the channels whose write enable is
/// also set). Channels 0 to 7 fit in this word.
pub const CHEN: usize = 0x018;
/// `DMAC_COMMON_INTSTATUS`.
pub const COMMON_INTSTATUS: usize = 0x050;

/// `DMAC_CFG.DMAC_EN`.
pub const CFG_DMAC_EN: u32 = 1 << 0;

/// The first channel register block (`COMMON_REG_LEN`).
pub const CHAN_BASE: usize = 0x100;
/// The size of one channel's register block (`CHAN_REG_LEN`).
pub const CHAN_LEN: usize = 0x100;

/// A channel's `CH_SAR` (low word; the high word is `+ 4`).
pub const CH_SAR: usize = 0x000;
/// A channel's `CH_DAR` (low word; the high word is `+ 4`).
pub const CH_DAR: usize = 0x008;
/// A channel's `CH_BLOCK_TS`: the block's transfers, minus one.
pub const CH_BLOCK_TS: usize = 0x010;
/// A channel's `CH_CTL_L`.
pub const CH_CTL_L: usize = 0x018;
/// A channel's `CH_CTL_H`.
pub const CH_CTL_H: usize = 0x01c;
/// A channel's `CH_CFG_L`.
pub const CH_CFG_L: usize = 0x020;
/// A channel's `CH_CFG_H`.
pub const CH_CFG_H: usize = 0x024;
/// A channel's `CH_LLP` (zero: no linked list).
pub const CH_LLP: usize = 0x028;
/// A channel's `CH_INTSTATUS`: the interrupt bits the channel raised.
pub const CH_INTSTATUS: usize = 0x088;
/// A channel's `CH_INTCLEAR`: write 1s to clear the matching `CH_INTSTATUS` bits.
pub const CH_INTCLEAR: usize = 0x098;

/// `CH_INTSTATUS` bit 0: a block finished.
pub const IRQ_BLOCK_TRF: u32 = 1 << 0;
/// `CH_INTSTATUS` bit 1: the whole transfer finished.
pub const IRQ_DMA_TRF: u32 = 1 << 1;
/// `CH_INTSTATUS` bits that report an error: source and destination decode,
/// slave, invalid, multi-block type, and the write-to-read-only, read-to-write-only
/// and channel-state errors (`DWAXIDMAC_IRQ_ALL_ERR`).
pub const IRQ_ALL_ERR: u32 = (0x3f << 16) | (0x3ff << 5);

/// The offset of channel `n`'s register block.
pub const fn channel_offset(n: u8) -> usize {
    CHAN_BASE + (n as usize) * CHAN_LEN
}

/// Register access to the DMAC, for the engine. A window reads and writes
/// 32-bit words at byte offsets from its base.
pub trait Registers {
    /// Reads the word at `offset`.
    fn read32(&self, offset: usize) -> u32;
    /// Writes `value` to the word at `offset`.
    fn write32(&mut self, offset: usize, value: u32) -> io::Result<()>;
}

/// A mapping of `len` bytes of physical memory from `/dev/mem`, at `base`.
/// Read-only unless mapped for writing; the writes on a read-only mapping fail.
pub struct Window {
    ptr: *mut u8,
    len: usize,
    writable: bool,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("len", &self.len)
            .field("writable", &self.writable)
            .finish()
    }
}

impl Window {
    /// Maps `len` bytes of `/dev/mem` (`devmem`) at physical address `base`.
    /// `len` is rounded to whole pages by the kernel; `base` must be page
    /// aligned. The mapping is shared, and writable only when `writable` is set.
    pub fn map(devmem: BorrowedFd<'_>, base: u64, len: usize, writable: bool) -> io::Result<Self> {
        let prot = libc::PROT_READ | if writable { libc::PROT_WRITE } else { 0 };
        // SAFETY: a null hint asks the kernel to choose the address; the file is
        // `/dev/mem`, and the mapping's length and offset are the caller's.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                devmem.as_raw_fd(),
                base as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            ptr: ptr.cast(),
            len,
            writable,
        })
    }

    /// Whether writes are allowed (the mapping is writable).
    pub fn is_writable(&self) -> bool {
        self.writable
    }
}

impl Window {
    /// The address of the word at `offset`, checked to be inside the mapping and
    /// word aligned.
    fn word(&self, offset: usize) -> *mut u32 {
        assert!(
            offset + 4 <= self.len,
            "register offset {offset:#x} outside the window"
        );
        assert_eq!(offset % 4, 0, "register offsets are word aligned");
        // SAFETY: the offset is inside the mapping (checked above), so the
        // result stays inside it.
        let base = unsafe { self.ptr.add(offset) };
        base.cast::<u32>()
    }
}

impl Registers for Window {
    fn read32(&self, offset: usize) -> u32 {
        let at = self.word(offset);
        // SAFETY: `word` returns an aligned address inside the mapping. Registers
        // are read with volatile loads, so the access is neither elided nor
        // reordered by the compiler.
        unsafe { std::ptr::read_volatile(at) }
    }

    fn write32(&mut self, offset: usize, value: u32) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the DMAC window was mapped read-only",
            ));
        }
        let at = self.word(offset);
        // SAFETY: as for `read32`, and the mapping is writable (checked above).
        unsafe { std::ptr::write_volatile(at, value) };
        Ok(())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` are exactly what `mmap` returned for this window.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_blocks_follow_the_kernel_layout() {
        assert_eq!(channel_offset(0), 0x100);
        assert_eq!(channel_offset(3), 0x400);
        assert_eq!(channel_offset(7), 0x800);
    }

    #[test]
    fn the_error_mask_covers_the_driver_error_bits() {
        // DWAXIDMAC_IRQ_ALL_ERR = GENMASK(21, 16) | GENMASK(14, 5).
        assert_eq!(IRQ_ALL_ERR & IRQ_DMA_TRF, 0);
        assert_ne!(IRQ_ALL_ERR & (1 << 5), 0);
        assert_ne!(IRQ_ALL_ERR & (1 << 21), 0);
        assert_eq!(IRQ_ALL_ERR & (1 << 22), 0);
        assert_eq!(IRQ_ALL_ERR & (1 << 15), 0, "GENMASK(14, 5) stops at bit 14");
        assert_eq!(IRQ_ALL_ERR & (1 << 4), 0);
    }
}
