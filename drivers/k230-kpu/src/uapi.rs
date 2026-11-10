//! The KPU's kernel interface, as the kernel the K230 Linux SDK builds defines it, and the memory
//! zone node that goes with it.
//!
//! Transcribed (interface facts only, no code copied) from:
//!
//! - `kendryte/k230_linux_sdk` at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`:
//!   `buildroot-overlay/configs/k230_canmv_defconfig` pins the kernel
//!   (`BR2_LINUX_KERNEL_CUSTOM_REPO_URL`, `..._REPO_VERSION`) and its defconfig (`k230`). The SDK
//!   holds no KPU code of its own; the `libnncase` package is the vendor's prebuilt runtime and is
//!   not read.
//! - That kernel, `ruyisdk/linux-xuantie-kernel` at commit
//!   `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529` (`GPL-2.0`):
//!   - `drivers/misc/canaan/gnne/k230_gnne.c`: the KPU's node. Its file operations are `open`,
//!     `release`, `poll` and `fasync`; there is no `ioctl`, `mmap`, `read` or `write`.
//!   - `drivers/misc/canaan/mmz/mmz.c`: the memory zone node, its two requests, `struct mmz_info`,
//!     and its `mmap`.
//!   - `arch/riscv/boot/dts/canaan/k230.dtsi`: the KPU's register window and interrupt.
//!   - `arch/riscv/configs/k230_defconfig`: `CONFIG_K230_GNNE_DRIVER=y`, `CONFIG_K230_MMZ=y`, and
//!     no `CONFIG_STRICT_DEVMEM`.
//!
//! The sizes and offsets are pinned in `layout.rs`, and were checked by `layout-check/check.c`
//! with `clang --target=riscv64-unknown-linux-gnu` and `gcc` on x86_64 (both 64-bit, the K230
//! Linux target).

/// The KPU's node (`device_create(..., "k230-gnne")`).
pub const DEVICE: &str = "/dev/k230-gnne";

/// The device-tree `compatible` the kernel driver binds.
pub const COMPATIBLE: &str = "k230-gnne";

/// The KPU's register window (`gnne@80400000`: `reg = <0x0 0x80400000 0x0 0x00000800>`).
pub const REGISTERS: u64 = 0x8040_0000;

/// The register window's length, in bytes.
pub const REGISTERS_LEN: u64 = 0x800;

/// The KPU's interrupt line (`interrupts = <189 IRQ_TYPE_LEVEL_HIGH>`).
pub const IRQ: u32 = 189;

/// The register the kernel's interrupt handler writes to clear the interrupt, as an offset in the
/// window, and the 64-bit value it writes (`gnne_interrupt_clear`).
pub const IRQ_CLEAR: (u64, u64) = (0x128, 0x4_0000_0004);

/// The system-control registers `open` and `release` write (physical addresses), with the value
/// `open` writes and the value `release` writes. The sources do not describe the registers; the
/// kernel pairs them with bringing the block up and down, and every opener's `release` writes the
/// second value, whoever else still holds the node.
pub const POWER: [(u64, u32, u32); 2] = [
    (0x9110_3028, 0x0003_0002, 0x0003_0001),
    (0x9110_0008, 0x8000_0405, 0x0000_0404),
];

/// The memory zone node (`misc_register`, name `mmz`).
pub const MMZ_DEVICE: &str = "/dev/mmz";

/// The memory zone's request type byte.
pub const MMZ_MAGIC: u8 = b'g';

/// `_IOWR` as the generic `ioctl.h` encodes it (RISC-V uses the generic encoding).
const fn iowr(ty: u8, nr: u8, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr as u32
}

/// `MMZ_ALLOC_MEM`, `_IOWR('g', 1, unsigned long)`: the size encoded is an `unsigned long` (8 on
/// the 64-bit target), although the request copies a whole [`MmzInfo`]. The kernel allocates
/// `length` bytes with `dma_alloc_coherent`, unchecked, and answers both addresses.
pub const MMZ_ALLOC_MEM: u32 = iowr(MMZ_MAGIC, 1, 8);

/// `MMZ_FREE_MEM`, `_IOWR('g', 2, unsigned long)`: the kernel frees `kernel_virt_addr`,
/// `mmz_phys` and `length` as given, checked against nothing it allocated.
pub const MMZ_FREE_MEM: u32 = iowr(MMZ_MAGIC, 2, 8);

/// `struct mmz_info`: two pointers and two `unsigned long`s, 8 bytes each on the 64-bit target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct MmzInfo {
    /// `void *user_virt_addr`: carried, never used by the kernel.
    pub user_virt_addr: u64,
    /// `void *kernel_virt_addr`: answered by an allocation, taken back by a free.
    pub kernel_virt_addr: u64,
    /// `unsigned long mmz_phys`: the allocation's physical address.
    pub mmz_phys: u64,
    /// `unsigned long length`: bytes.
    pub length: u64,
}
