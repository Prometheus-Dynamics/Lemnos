//! The AI2D block's kernel interface, as the kernel the K230 Linux SDK builds defines it.
//!
//! Transcribed (interface facts only, no code copied) from:
//!
//! - `kendryte/k230_linux_sdk` at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`:
//!   `buildroot-overlay/configs/k230_canmv_defconfig` pins the kernel
//!   (`BR2_LINUX_KERNEL_CUSTOM_REPO_URL`, `..._REPO_VERSION`), and builds no AI2D code of its own
//!   (the `libnncase` package is the vendor's prebuilt runtime, not read).
//! - That kernel, `ruyisdk/linux-xuantie-kernel` at commit
//!   `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`:
//!   - `drivers/misc/canaan/ai2d/k230_ai2d.c` (`GPL-2.0`): the node, the two requests and their
//!     structure, the completion through `poll`, and the interrupt registers the handler clears.
//!   - `arch/riscv/boot/dts/canaan/k230.dtsi`: the register window and the interrupt.
//!
//! What the kernel offers is all here, and it is not a way to run a job. Importing a dma-buf
//! answers its DMA address; the job itself is written into the block's registers by whoever
//! drives it, and the kernel neither maps them for anyone nor checks what is written. See the
//! crate docs for why no job is run on Linux.
//!
//! The sizes and offsets are pinned by the tests, and were checked against `gcc` on x86_64 and
//! `clang --target=riscv64-unknown-linux-gnu` (16 bytes, `addr` at 8) and
//! `--target=riscv32-unknown-linux-gnu` (8 bytes, `addr` at 4) on the same declaration.

use core::ffi::c_int;

use lemnos_linux_sys::{iow, iowr};

/// The device node (`device_create(..., "k230-ai2d")`).
pub const DEVICE: &str = "/dev/k230-ai2d";

/// The device-tree `compatible` the kernel driver binds.
pub const COMPATIBLE: &str = "k230-ai2d";

/// The request type byte.
pub const MAGIC: u8 = b'A';

/// `struct ai2d_import_dmabuf`: a dma-buf descriptor in, its DMA address out.
///
/// The kernel answers the address of the *last* entry of the dma-buf's scatter list, so the
/// address is the buffer's start only when the buffer is one contiguous piece.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportDmabuf {
    /// The dma-buf's descriptor in the caller's process.
    pub fd: c_int,
    /// `uintptr_t`: the DMA address the kernel answers.
    pub addr: usize,
}

/// `AI2D_IMPORT_DMABUF`, `_IOWR('A', 0, struct ai2d_import_dmabuf)`: attaches and maps the
/// dma-buf for the block, keeps it until removed or until the node is closed, and answers its
/// address. Every failure answers `-1`, which user space sees as `EPERM`.
pub const IMPORT_DMABUF: u32 = iowr::<ImportDmabuf>(MAGIC, 0);

/// `AI2D_REMOVE_DMABUF`, `_IOW('A', 1, int)`: the argument is the descriptor *number*, passed by
/// value although the request encodes an `int` to copy. The kernel finds the import by that
/// number, so a descriptor must be removed before its number is closed or reused.
pub const REMOVE_DMABUF: u32 = iow::<c_int>(MAGIC, 1);

/// The block's register window (`reg = <0x0 0x80400c00 0x0 0x00000400>`).
pub const REGISTERS_BASE: u64 = 0x8040_0c00;
/// The register window's size in bytes.
pub const REGISTERS_SIZE: usize = 0x400;
/// The block's interrupt (`interrupts = <191 IRQ_TYPE_LEVEL_HIGH>`).
pub const IRQ: u32 = 191;

/// The interrupt registers the kernel's handler writes on every interrupt: `1` to the first,
/// `0` to the other three. Their fields are not described anywhere in the source.
pub const INTERRUPT_CLEAR: [(usize, u32); 4] = [(0xa0, 1), (0xa4, 0), (0xa8, 0), (0xac, 0)];

#[cfg(test)]
mod tests {
    use core::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn the_import_structure_is_the_c_compilers() {
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<ImportDmabuf>(), 16);
            assert_eq!(offset_of!(ImportDmabuf, fd), 0);
            assert_eq!(offset_of!(ImportDmabuf, addr), 8);
            assert_eq!(align_of::<ImportDmabuf>(), 8);
        }
        #[cfg(target_pointer_width = "32")]
        {
            assert_eq!(size_of::<ImportDmabuf>(), 8);
            assert_eq!(offset_of!(ImportDmabuf, addr), 4);
        }
    }

    #[test]
    fn the_request_numbers_are_the_c_compilers() {
        // From gcc on x86_64 (and the same encoding on riscv64): _IOWR('A', 0, struct
        // ai2d_import_dmabuf) and _IOW('A', 1, int).
        #[cfg(target_pointer_width = "64")]
        assert_eq!(IMPORT_DMABUF, 0xc010_4100);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(IMPORT_DMABUF, 0xc008_4100);
        assert_eq!(REMOVE_DMABUF, 0x4004_4101);
        assert_eq!(MAGIC, b'A');
    }

    #[test]
    fn the_register_window_holds_the_interrupt_registers() {
        assert_eq!(
            (REGISTERS_BASE, REGISTERS_SIZE, IRQ),
            (0x8040_0c00, 0x400, 191)
        );
        for (offset, _) in INTERRUPT_CLEAR {
            assert!(offset % 4 == 0 && offset + 4 <= REGISTERS_SIZE);
        }
        assert_eq!(DEVICE, "/dev/k230-ai2d");
    }
}
