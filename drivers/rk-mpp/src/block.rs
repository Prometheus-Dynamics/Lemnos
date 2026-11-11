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
//!   (register classes), `MPP_DEVICE_VDPP` (two windows),
//!   `MPP_DEVICE_VEPU2_JPEG` (multi-core task path), `MPP_DEVICE_VDPU1_PP`, `MPP_DEVICE_VDPU2_PP`,
//!   `MPP_DEVICE_AVSPLUS_DEC` and `MPP_DEVICE_VEPU22`.

//!
//! # Parameter blocks
//!
//! One block takes no register file: the IEP2 deinterlacer (`MPP_DEVICE_IEP2`, `mpp_iep2.c`).
//! Its job is one parameter structure (`struct iep2_params`), copied whole into the task; the
//! kernel translates a fixed list of its words as descriptors (`iep2_addr_rnum`, no translation
//! table), and programs the block's registers from the rest itself. [`ParamFacts`] says what
//! the kernel does with each word, and [`crate::ParamJob`] is the job for it. Its
//! [`Facts::registers`] is 0, so no register-file job is taken on it.

use crate::uapi::client;

/// What the kernel does with one word of a parameter block's structure, other than translating
/// it: a range the word must be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// From `min` to `max`, both included.
    Range {
        /// The smallest value taken.
        min: u32,
        /// The largest value taken.
        max: u32,
    },
    /// One of these.
    OneOf(&'static [u32]),
}

impl Check {
    /// Whether `value` passes.
    #[must_use]
    pub fn allows(self, value: u32) -> bool {
        match self {
            Self::Range { min, max } => (min..=max).contains(&value),
            Self::OneOf(values) => values.contains(&value),
        }
    }
}

/// A word of the parameter structure the kernel translates as a descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// Its byte offset in the structure.
    pub at: u16,
    /// The register the kernel keys its offset by (`mem_region->reg_idx`, looked up in the
    /// task's `MPP_CMD_SET_REG_ADDR_OFFSET` list).
    pub register: u16,
}

/// A word of the parameter structure the kernel uses as an index, a size or a count, or that
/// selects how the block reaches memory, with the values it may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Word {
    /// Its byte offset in the structure.
    pub at: u16,
    /// What it must be.
    pub check: Check,
}

/// What the kernel does with a parameter block's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamFacts {
    /// Bytes of the parameter structure. The kernel copies a write of any size into it
    /// unchecked, so a job's is exactly this.
    pub bytes: usize,
    /// Bytes of the results structure. The kernel copies the size a read asks for out of it
    /// unchecked, so a job's read is exactly this.
    pub results: usize,
    /// Every word the kernel translates as a descriptor. A job places a buffer the claim holds
    /// in each one; the caller's value there is never sent.
    pub descriptors: &'static [Descriptor],
    /// The caller's words the kernel or the block takes as an index, a size, a count or a mode
    /// that changes what is reached. Every other word is carried as it is.
    pub words: &'static [Word],
}

/// `struct iep2_params`, from `mpp_iep2.c`: 656 bytes (`gcc` on LP64; no padding, all `u32`
/// but eight `s8`s).
const IEP2: ParamFacts = ParamFacts {
    bytes: 656,
    // `struct iep2_output`.
    results: 412,
    // `src[3]` at 36 and `dst[2]` at 72, each `{y, cbcr, cr}`, then `mv_addr` at 96 and
    // `md_addr` at 100, in `iep2_addr_rnum` order: cur 24 27 28, nxt 25 29 30, prv 26 31 32,
    // top 44 46 -1, bot 45 47 -1, mv 34, md 33. The kernel skips a word whose register is -1
    // (the outputs' `cr`) and a word of 0; it translates the rest.
    descriptors: &[
        Descriptor {
            at: 36,
            register: 24,
        },
        Descriptor {
            at: 40,
            register: 27,
        },
        Descriptor {
            at: 44,
            register: 28,
        },
        Descriptor {
            at: 48,
            register: 25,
        },
        Descriptor {
            at: 52,
            register: 29,
        },
        Descriptor {
            at: 56,
            register: 30,
        },
        Descriptor {
            at: 60,
            register: 26,
        },
        Descriptor {
            at: 64,
            register: 31,
        },
        Descriptor {
            at: 68,
            register: 32,
        },
        Descriptor {
            at: 72,
            register: 44,
        },
        Descriptor {
            at: 76,
            register: 46,
        },
        Descriptor {
            at: 84,
            register: 45,
        },
        Descriptor {
            at: 88,
            register: 47,
        },
        Descriptor {
            at: 96,
            register: 34,
        },
        Descriptor {
            at: 100,
            register: 33,
        },
    ],
    words: &[
        // `tile_cols` and `tile_rows`: the size, written as `tiles * 16 - 1` and `tiles * 4 - 1`
        // into 11-bit fields; at most `IEP2_TILE_W_MAX` 120 and `IEP2_TILE_H_MAX` 272, the size
        // of the kernel's own ROI buffer; 0 would wrap to the largest size.
        Word {
            at: 16,
            check: Check::Range { min: 1, max: 120 },
        },
        Word {
            at: 20,
            check: Check::Range { min: 1, max: 272 },
        },
        // The three strides, in 4-byte words: 16-bit fields of the block's pitch registers.
        Word {
            at: 24,
            check: Check::Range {
                min: 0,
                max: 0xffff,
            },
        },
        Word {
            at: 28,
            check: Check::Range {
                min: 0,
                max: 0xffff,
            },
        },
        Word {
            at: 32,
            check: Check::Range {
                min: 0,
                max: 0xffff,
            },
        },
        // The outputs' `cr`: neither translated nor written by the kernel. Held at 0, so that
        // no caller's number sits in a descriptor-shaped word.
        Word {
            at: 80,
            check: Check::Range { min: 0, max: 0 },
        },
        Word {
            at: 92,
            check: Check::Range { min: 0, max: 0 },
        },
        // `dil_mode`: the modes of `enum rockchip_iep2_dil_mode` but `I1O1T` (5), in which the
        // block reads past the current frame's buffer and the kernel maps a spare page where it
        // faults. An unknown mode does what the source doesn't say.
        Word {
            at: 104,
            check: Check::OneOf(&[0, 1, 2, 3, 4, 6, 7, 8, 9]),
        },
        // `dil_out_mode`: its effect on what the block writes is not in the source. Held at 0.
        Word {
            at: 108,
            check: Check::Range { min: 0, max: 0 },
        },
        // `osd_area_num`: the kernel indexes the eight-entry OSD arrays, and writes one OSD
        // register per area, by it (`iep2_osd_cfg`).
        Word {
            at: 132,
            check: Check::Range { min: 0, max: 8 },
        },
        // `roi_en`: points the block at the kernel's own ROI buffer, which nothing fills.
        Word {
            at: 488,
            check: Check::Range { min: 0, max: 0 },
        },
    ],
};

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
    /// The IEP2 deinterlacer (`MPP_DEVICE_IEP2`, `mpp_iep2.c`): a parameter block, whose jobs
    /// are [`crate::ParamJob`]s.
    Iep2,
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
    pub const ALL: [Self; 7] = [
        Self::Vdpu1,
        Self::Vdpu2,
        Self::Vepu1,
        Self::Vepu2,
        Self::JpegDecoder,
        Self::JpegEncoder,
        Self::Iep2,
    ];

    /// What the kernel does with the block's parameter structure, for a parameter block;
    /// `None` for a register-file block.
    #[must_use]
    pub const fn params(self) -> Option<&'static ParamFacts> {
        match self {
            Self::Iep2 => Some(&IEP2),
            _ => None,
        }
    }

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
            // No register file: a register-file job is refused on it, as running past 0
            // registers. Its jobs are parameter blocks ([`Block::params`]).
            Self::Iep2 => Facts {
                client: client::IEP2,
                registers: 0,
                first: 0,
                direct: None,
            },
        }
    }
}
