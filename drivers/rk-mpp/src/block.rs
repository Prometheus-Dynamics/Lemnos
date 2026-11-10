//! The blocks a claim may bind to, and what the kernel does with each one's registers.
//!
//! A block is taken only when every path by which the kernel turns a register value into a
//! descriptor or an address has been read and is covered by the checks in [`crate::job`]. The
//! facts below come from the per-block drivers in `drivers/video/rockchip/mpp/` of
//! rockchip-linux/kernel `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`:
//! `mpp_vdpu1.c`, `mpp_vdpu2.c`, `mpp_vepu1.c`, `mpp_vepu2.c`, `mpp_jpgdec.c` and `mpp_jpgenc.c`.
//! Each has one flat register array, checks register reads and writes with `mpp_check_req`
//! against it, translates registers with `mpp_translate_reg_address` and then adds
//! `mpp_translate_reg_offset_info`, and allocates its task zeroed.
//!
//! Left out, because a caller's bytes reach a descriptor or an index there by a path this crate
//! does not check yet:
//!
//! - `MPP_DEVICE_RKVDEC` and `MPP_DEVICE_HEVC_DEC`: the type is served by `mpp_rkvdec.c` on some
//!   chips, which reads a descriptor out of the caller's PPS buffer (`fill_scaling_list_pps`), and
//!   by `mpp_rkvdec2.c` on others; the type alone does not say which.
//! - `MPP_DEVICE_RKVENC` (`mpp_rkvenc.c` or `mpp_rkvenc2.c`, register classes), `MPP_DEVICE_AV1DEC`
//!   (register classes), `MPP_DEVICE_IEP2` (parameter block), `MPP_DEVICE_VDPP` (two windows),
//!   `MPP_DEVICE_VEPU2_JPEG` (multi-core task path), `MPP_DEVICE_VDPU1_PP`, `MPP_DEVICE_VDPU2_PP`,
//!   `MPP_DEVICE_AVSPLUS_DEC` and `MPP_DEVICE_VEPU22`.

use crate::uapi::client;

/// A block a claim binds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Block {
    /// The first-generation video decoder (`MPP_DEVICE_VDPU1`, `mpp_vdpu1.c`).
    Vdpu1,
    /// The second-generation video decoder (`MPP_DEVICE_VDPU2`, `mpp_vdpu2.c`).
    Vdpu2,
    /// The first-generation video encoder (`MPP_DEVICE_VEPU1`, `mpp_vepu1.c`).
    Vepu1,
    /// The second-generation video encoder (`MPP_DEVICE_VEPU2`, `mpp_vepu2.c`).
    Vepu2,
    /// The JPEG decoder (`MPP_DEVICE_RKJPEGD`, `mpp_jpgdec.c`).
    JpegDecoder,
    /// The JPEG encoder (`MPP_DEVICE_RKJPEGE`, `mpp_jpgenc.c`).
    JpegEncoder,
}

/// A register the kernel translates outside the translation table when another register holds a
/// given value: the decoders' direct motion vector base in H.264 mode. Its descriptor is taken
/// from its low bits whatever the table says, so a job must name its buffer there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Direct {
    /// The register translated.
    pub register: u16,
    /// The register whose field selects the mode.
    pub mode_register: u16,
    /// The field's shift.
    pub shift: u32,
    /// The field's mask, after the shift.
    pub mask: u32,
    /// The field's value that turns the translation on.
    pub value: u32,
}

impl Direct {
    /// Whether the kernel translates [`Self::register`] for a task whose mode register holds
    /// `mode`.
    #[must_use]
    pub const fn applies(&self, mode: u32) -> bool {
        (mode >> self.shift) & self.mask == self.value
    }
}

/// What the kernel does with one block's registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The client type sent with `MPP_CMD_INIT_CLIENT_TYPE`.
    pub client: u32,
    /// Registers a job may name: the smallest `reg_num` among the block's variants, which is never
    /// more than its task's register array. Reads and writes, translated registers and offsets
    /// stay below it.
    pub registers: u16,
    /// `reg_start`: the kernel skips a read or write that ends before this register.
    pub first: u16,
    /// A register translated outside the table, if any.
    pub direct: Option<Direct>,
}

/// The most registers of any block here: [`Block::Vepu2`]'s.
pub const MAX_REGISTERS: usize = 184;

impl Block {
    /// Every block a claim may bind to.
    pub const ALL: [Self; 6] = [
        Self::Vdpu1,
        Self::Vdpu2,
        Self::Vepu1,
        Self::Vepu2,
        Self::JpegDecoder,
        Self::JpegEncoder,
    ];

    /// The block's facts.
    #[must_use]
    pub const fn facts(self) -> Facts {
        match self {
            // VDPU1_REG_NUM 60 (the task array holds VDPU1_REG_PP_NUM 101), reg_start 0. The
            // direct motion vector base is VDPU1_REG_DIR_MV_BASE_INDEX 41, translated when
            // VDPU1_GET_FORMAT(reg[VDPU1_REG_SYS_CTRL_INDEX 3]) = (x >> 28) & 0xf is
            // VDPU1_FMT_H264D 0.
            Self::Vdpu1 => Facts {
                client: client::VDPU1,
                registers: 60,
                first: 0,
                direct: Some(Direct {
                    register: 41,
                    mode_register: 3,
                    shift: 28,
                    mask: 0xf,
                    value: 0,
                }),
            },
            // VDPU2_REG_NUM 159, VDPU2_REG_START_INDEX 50. VDPU2_REG_DIR_MV_BASE_INDEX 62,
            // translated when VDPU2_GET_FORMAT(reg[VDPU2_REG_SYS_CTRL_INDEX 53]) = x & 0xf is
            // VDPU2_FMT_H264D 0.
            Self::Vdpu2 => Facts {
                client: client::VDPU2,
                registers: 159,
                first: 50,
                direct: Some(Direct {
                    register: 62,
                    mode_register: 53,
                    shift: 0,
                    mask: 0xf,
                    value: 0,
                }),
            },
            // VEPU1_REG_NUM 164, reg_start 0.
            Self::Vepu1 => Facts {
                client: client::VEPU1,
                registers: 164,
                first: 0,
                direct: None,
            },
            // VEPU2_REG_NUM 184, reg_start 0. In JPEG mode the kernel also looks the output
            // register's descriptor up among the session's own mapped buffers, for a cache sync up
            // to that register's offset; it maps nothing there.
            Self::Vepu2 => Facts {
                client: client::VEPU2,
                registers: 184,
                first: 0,
                direct: None,
            },
            // JPGDEC_REG_NUM 42 (JPGDEC_VPU730_REG_NUM 70 on the newer variant), reg_start 0.
            Self::JpegDecoder => Facts {
                client: client::RKJPEGD,
                registers: 42,
                first: 0,
                direct: None,
            },
            // JPGENC_REG_NUM 79 (JPGENC_VPU730_REG_NUM 241 on the newer variant), reg_start 0.
            Self::JpegEncoder => Facts {
                client: client::RKJPEGE,
                registers: 79,
                first: 0,
                direct: None,
            },
        }
    }
}
