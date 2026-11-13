//! The VEPU580 H.264 encoder's register layouts: every register of the write side and the
//! read-back side, one module per register, grouped by the kernel's register class.
//!
//! Provenance: rockchip-linux/mpp at `9b56348cc61393c84662e6d6dd78feae67773a36` (Apache-2.0):
//! `mpp/hal/rkenc/h264e/hal_h264e_vepu580_reg.h` (`HalVepu580RegSet` and the structs it places:
//! `Vepu580ControlCfg`, `Vepu580BaseCfg`, `Vepu580RdoCfg`, `Vepu580SclCfg`, `Vepu580Section3`) and
//! `mpp/hal/rkenc/common/vepu580_common.h` (the shared `Vepu580CtlCommon`, `Vepu580BaseCommon`,
//! `Vepu580RcKlut`, `Vepu580WgtCommon`, `Vepu580OsdReg`, `Vepu580Status` and `Vepu580Dbg`). The
//! names, word offsets, bit positions and the one alignment comment the headers give are
//! transcribed; no code or comment is copied. The class placement is from the kernel's
//! `rkvenc_v2_hw_info` (the VEPU580 table in `kern_mpp_rkvenc2.c`, Linux kernel tree,
//! `GPL-2.0+ OR MIT`): each class's first byte (`base_s`) and last byte (`base_e`). Only its numbers
//! are used.
//!
//! Word numbering is absolute: a register's `word` is its byte offset in the whole block divided by
//! 4. A register's module is `reg` followed by that word, four digits (`reg0022` is at byte 0x58).
//! Bit positions are the C packing rule for `RK_U32` bitfields (LSB first, a field never straddles
//! a 32-bit word), checked against gcc and clang by compiling the header and setting each bitfield
//! (a throwaway probe, not part of the crate).
//!
//! The C struct `HalVepu580RegSet` packs its members back to back, but the register map does not:
//! each member sits at its class base (`reg_base` at byte 0x280, `reg_osd` at 0x3000, and so on).
//! Each member's own offsets are checked against the header's `0x........ regNNNN` comments (348
//! comments; one is left out, see below).
//!
//! What is left out:
//! - Fields the header names `reserved` (any name starting with `reserve`), and reserved words
//!   (`reserved*` arrays and scalars). They are in no module.
//! - `enc_id` (`reg_ctl.enc_id`, word 23, byte 0x5c): the kernel's BASE class ends at byte 0x58, so
//!   the kernel never copies this word from the caller. It is not in any class and so in no module.
//! - `Vepu580OsdInvEn`: a typedef the register set does not use.
//! - `node_addr` (word 4106) is a status address: it is in the read-only ST class and is not an
//!   `AddressReg`, since the driver never writes it.
//!
//! Decisions:
//! - Address registers (`AddressReg`): the writable words named `adr_*` or `*_addr` in the base
//!   block and `osd_addr[8]`. Their alignment is the header's comment where it gives one (only
//!   `osd_addr`: "16B aligned"), and 1 where it gives none. The 28-bit `bsbw_addr` in the ST class
//!   is a bitfield, not a whole word, and is read-only; it is not an address register.
//! - Arrays of `RK_U16` and `RK_U8` (the scaling lists, `aq_tthd`, `aq_step`) are one module each,
//!   with the packed entries in the doc comment. Arrays of `RK_U32` are one module each, with no
//!   fields. `osd_addr` is one module of 8 words, and its `OSD_ADDR` names each entry.
//! - Struct arrays with bitfields (`osd_pos[8]`, `plt_data[256]`) are one module per 32-bit word, so
//!   that the fields stay named; a word is named by its element and index.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/K1a2b.md`.

pub mod base;
pub mod debug;
pub mod job;
pub mod osd;
pub mod par;
pub mod pic;
pub mod rc;
pub mod scl;
pub mod sqi;
pub mod st;

pub use job::{CodecJob, READ_BACK, RegisterSet, SetError, StreamRef, WRITE_CLASSES, WRITE_RUNS};

use crate::layout::Class;
use crate::layout::Region;

/// The nine register classes of the VEPU580 block, in the kernel's `RKVENC_CLASS_TYPE` order. Each
/// class's `first` word and `words` are the kernel's `base_s / 4` and `(base_e - base_s) / 4 + 1`.
pub const VEPU580_CLASSES: [Class; 9] = [
    Class {
        region: Region::Class(0),
        first: 0,
        words: 23,
        writable: true,
    },
    Class {
        region: Region::Class(1),
        first: 160,
        words: 94,
        writable: true,
    },
    Class {
        region: Region::Class(2),
        first: 1024,
        words: 57,
        writable: true,
    },
    Class {
        region: Region::Class(3),
        first: 1472,
        words: 374,
        writable: true,
    },
    Class {
        region: Region::Class(4),
        first: 2048,
        words: 122,
        writable: true,
    },
    Class {
        region: Region::Class(5),
        first: 2176,
        words: 679,
        writable: true,
    },
    Class {
        region: Region::Class(6),
        first: 3072,
        words: 288,
        writable: true,
    },
    Class {
        region: Region::Class(7),
        first: 4096,
        words: 180,
        writable: false,
    },
    Class {
        region: Region::Class(8),
        first: 5120,
        words: 214,
        writable: false,
    },
];
