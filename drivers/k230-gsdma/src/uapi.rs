//! The engine as the SDK kernel's driver programs it: its device-tree node, register map, the
//! 24-byte descriptor it walks, the codes in a descriptor, and the formats and rotations the
//! driver takes.
//!
//! Transcribed (interface facts only, no code copied) from the Kendryte K230 Linux SDK
//! (`github.com/kendryte/k230_linux_sdk`) at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`,
//! whose kernel patches add the driver to the kernel it builds
//! (`github.com/ruyisdk/linux-xuantie-kernel` at `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`):
//!
//! - `buildroot-overlay/linux/0023-add-gdma-vo-rotation.patch`: `drivers/soc/canaan/k230-gdma.c`
//!   (`GPL-2.0-only`: the registers as `struct k230_gdma_regs`, the descriptor as
//!   `struct k230_gdma_llt`, the interrupt and channel bits, the configuration values written at
//!   probe, the formats in `k230_gdma_build_format_desc`, the rotation checks and codes, the
//!   timeout) and `include/linux/soc/canaan/k230-gdma.h` (`struct canaan_gdma_frame`, at most 3
//!   planes).
//! - `buildroot-overlay/linux/0029-canaan-add-harden-DMA-address-validation.patch`: the address
//!   check the driver then makes (`k230_dma_addr_in_ddr`: the whole range below 2 GiB).
//! - `buildroot-overlay/linux/0019-dts-add-nonai2d.patch`: the node (`gdma@0x80800000`).
//! - The DRM rotation bits and four-character codes are the kernel's `include/uapi/drm/`
//!   `drm_mode.h` and `drm_fourcc.h`.
//!
//! The descriptor's bit fields were laid out with gcc on x86_64 and clang for riscv64 (both put
//! the first field in the low bits of each little-endian word); the tests pin every field.

/// The node's `compatible`.
pub const COMPATIBLE: &str = "canaan,gdma";

/// The register window's physical base.
pub const REGISTERS: u64 = 0x8080_0000;

/// The register window's size.
pub const REGISTERS_LEN: u64 = 0x1000;

/// The interrupt line.
pub const IRQ: u32 = 140;

/// Registers, as byte offsets into the window (`struct k230_gdma_regs`).
pub mod reg {
    /// `dma_ch_en`: [`super::CH_EN`] enables the channel.
    pub const CH_EN: usize = 0x00;
    /// `dma_int_mask`.
    pub const INT_MASK: usize = 0x04;
    /// `dma_int_stat`: written back to clear.
    pub const INT_STAT: usize = 0x08;
    /// `dma_cfg`.
    pub const CFG: usize = 0x0c;
    /// `gdma_ctrl`: 1 starts the chain.
    pub const CTRL: usize = 0x10;
    /// `dma_gdmalli_base`: the bus address of the first descriptor.
    pub const LLI_BASE: usize = 0x14;
    /// `gdma_ch_cnt[8]`.
    pub const CH_CNT: usize = 0x18;
    /// `gdma_current_llt`.
    pub const CURRENT_LLT: usize = 0x38;
    /// `gdma_current_size`.
    pub const CURRENT_SIZE: usize = 0x3c;
    /// `gdma_current_saddr`.
    pub const CURRENT_SADDR: usize = 0x40;
    /// `gdma_current_daddr`.
    pub const CURRENT_DADDR: usize = 0x44;
    /// `dma_weight`.
    pub const WEIGHT: usize = 0x48;
    /// The structure's size.
    pub const END: usize = 0x50;
}

/// `K230_GDMA_CH_EN`, in [`reg::CH_EN`].
pub const CH_EN: u32 = 1 << 4;
/// `K230_GDMA_DONE_INT`, in [`reg::INT_STAT`]: the chain finished.
pub const DONE_INT: u32 = 1 << 18;
/// `K230_GDMA_INT_MASK`: the interrupts the driver unmasks.
pub const INT_MASK: u32 = 0b111 << 16;
/// `K230_GDMA_DMA_CFG`, written to [`reg::CFG`] at probe.
pub const DMA_CFG: u32 = (7 << 8) | 0xff;
/// `K230_GDMA_DMA_WEIGHT`, written to [`reg::WEIGHT`] at probe.
pub const DMA_WEIGHT: u32 = 0x0044_4210;
/// `K230_GDMA_DEFAULT_TIMEOUT_MS`.
pub const DEFAULT_TIMEOUT_MS: u32 = 100;

/// `K230_GDMA_LLT_ENTRY_SIZE`.
pub const DESCRIPTOR_BYTES: usize = 24;
/// Descriptors in the driver's chain at most (`K230_GDMA_MAX_LLT_SIZE`): one per plane.
pub const MAX_DESCRIPTORS: usize = 3;
/// `CANAAN_GDMA_MAX_PLANES`.
pub const MAX_PLANES: usize = 3;

/// The end of the memory the driver lets the engine reach (`K230_DDR_PHYS_END`): DDR, from 0.
pub const DDR_END: u64 = 0x8000_0000;

/// `enum k230_gdma_pixel_bits`: a descriptor's element size.
pub mod pixel {
    /// `K230_GDMA_BIT_8`.
    pub const BITS_8: u32 = 0;
    /// `K230_GDMA_BIT_16`.
    pub const BITS_16: u32 = 1;
    /// `K230_GDMA_BIT_24`.
    pub const BITS_24: u32 = 2;
    /// `K230_GDMA_BIT_32`.
    pub const BITS_32: u32 = 3;
}

/// A descriptor's `rotation_mode` (`k230_gdma_rotation_to_hw`): clockwise quarter turns.
pub mod turn {
    /// None.
    pub const R0: u32 = 0;
    /// 90 degrees.
    pub const R90: u32 = 1;
    /// 180 degrees.
    pub const R180: u32 = 2;
    /// 270 degrees.
    pub const R270: u32 = 3;
}

/// The DRM rotation bits the driver takes (`DRM_MODE_ROTATE_*`, `DRM_MODE_REFLECT_*`).
pub mod drm {
    /// `DRM_MODE_ROTATE_0`.
    pub const ROTATE_0: u32 = 1 << 0;
    /// `DRM_MODE_ROTATE_90`.
    pub const ROTATE_90: u32 = 1 << 1;
    /// `DRM_MODE_ROTATE_180`.
    pub const ROTATE_180: u32 = 1 << 2;
    /// `DRM_MODE_ROTATE_270`.
    pub const ROTATE_270: u32 = 1 << 3;
    /// `DRM_MODE_ROTATE_MASK`.
    pub const ROTATE_MASK: u32 = 0x0f;
    /// `DRM_MODE_REFLECT_X`.
    pub const REFLECT_X: u32 = 1 << 4;
    /// `DRM_MODE_REFLECT_Y`.
    pub const REFLECT_Y: u32 = 1 << 5;
    /// `DRM_MODE_REFLECT_MASK`.
    pub const REFLECT_MASK: u32 = REFLECT_X | REFLECT_Y;
}

/// The DRM four-character codes of the formats the driver takes.
pub mod fourcc {
    const fn code(c: &[u8; 4]) -> u32 {
        u32::from_le_bytes(*c)
    }
    /// `DRM_FORMAT_ARGB1555`.
    pub const ARGB1555: u32 = code(b"AR15");
    /// `DRM_FORMAT_ARGB4444`.
    pub const ARGB4444: u32 = code(b"AR12");
    /// `DRM_FORMAT_RGB565`.
    pub const RGB565: u32 = code(b"RG16");
    /// `DRM_FORMAT_RGB888`.
    pub const RGB888: u32 = code(b"RG24");
    /// `DRM_FORMAT_BGR888`.
    pub const BGR888: u32 = code(b"BG24");
    /// `DRM_FORMAT_ARGB8888`.
    pub const ARGB8888: u32 = code(b"AR24");
    /// `DRM_FORMAT_NV12`.
    pub const NV12: u32 = code(b"NV12");
    /// `DRM_FORMAT_NV21`.
    pub const NV21: u32 = code(b"NV21");
    /// `DRM_FORMAT_NV16`.
    pub const NV16: u32 = code(b"NV16");
    /// `DRM_FORMAT_NV61`.
    pub const NV61: u32 = code(b"NV61");
}

/// One descriptor (`struct k230_gdma_llt`, packed, 24 bytes): one plane of one job.
///
/// Word 0 holds, from the low bit: `rotation_mode` (2 bits), `x_mirror`, `y_mirror`, 4 reserved,
/// `pixel_width` (2), 6 reserved, `channel` (3), 10 reserved, `pause`, `node_intr`, 1 reserved.
/// Word 2 is `width` (low 16) and `height`; word 3 `src_stride` (low 16) and `dst_stride`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Descriptor {
    /// `rotation_mode`, a [`turn`] value.
    pub rotation_mode: u32,
    /// `x_mirror`.
    pub x_mirror: bool,
    /// `y_mirror`.
    pub y_mirror: bool,
    /// `pixel_width`, a [`pixel`] value.
    pub pixel_width: u32,
    /// `channel` (the driver uses 0).
    pub channel: u32,
    /// `pause`.
    pub pause: bool,
    /// `node_intr`: raise the done interrupt after this descriptor (the driver sets it on the
    /// last).
    pub node_intr: bool,
    /// `src_addr`: the source plane's bus address.
    pub src_addr: u32,
    /// `width`: the source plane's width, in elements.
    pub width: u16,
    /// `height`: the source plane's height, in rows.
    pub height: u16,
    /// `src_stride`, in bytes.
    pub src_stride: u16,
    /// `dst_stride`, in bytes.
    pub dst_stride: u16,
    /// `dst_addr`: the destination plane's bus address.
    pub dst_addr: u32,
    /// `next_llt_addr`: the next descriptor's bus address (0 on the last).
    pub next: u32,
}

impl Descriptor {
    /// The six little-endian words the engine reads.
    #[must_use]
    pub fn words(&self) -> [u32; 6] {
        let w0 = (self.rotation_mode & 0b11)
            | u32::from(self.x_mirror) << 2
            | u32::from(self.y_mirror) << 3
            | (self.pixel_width & 0b11) << 8
            | (self.channel & 0b111) << 16
            | u32::from(self.pause) << 29
            | u32::from(self.node_intr) << 30;
        [
            w0,
            self.src_addr,
            u32::from(self.width) | u32::from(self.height) << 16,
            u32::from(self.src_stride) | u32::from(self.dst_stride) << 16,
            self.dst_addr,
            self.next,
        ]
    }

    /// The descriptor as the engine reads it from memory.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; DESCRIPTOR_BYTES] {
        let mut out = [0; DESCRIPTOR_BYTES];
        for (to, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.words()) {
            *to = word.to_le_bytes();
        }
        out
    }
}

#[cfg(test)]
mod tests;
