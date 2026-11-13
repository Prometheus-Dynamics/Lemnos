//! The VEPU580 H.265 encoder's register layouts: every register of the write side and the
//! read-back side, one module per register, grouped by the kernel's register class.
//!
//! Provenance: rockchip-linux/mpp at `9b56348cc61393c84662e6d6dd78feae67773a36` (Apache-2.0):
//! `mpp/hal/rkenc/h265e/hal_h265e_vepu580_reg.h` (`H265eV580RegSet` and the structs it places:
//! `HevcVepu580ControlCfg`, `HevcVepu580Base`, `HevcVepu580Wgt`, `Vepu580RdoCfg`, and the OSD and
//! status and debug structs from the common header) and `mpp/hal/rkenc/common/vepu580_common.h` (the
//! shared `Vepu580CtlCommon`, `Vepu580BaseCommon`, `Vepu580RcKlut`, `Vepu580WgtCommon`,
//! `Vepu580OsdReg`, `Vepu580Status` and `Vepu580Dbg`). The names, word offsets, bit positions and
//! the alignment comments the headers give are transcribed; no code or comment is copied. The class
//! placement is the kernel's `rkvenc_v2_hw_info` (the same table as H.264; see
//! [`crate::vepu580_h264`]).
//!
//! Word numbering is absolute, as in [`crate::vepu580_h264`]: a register's module is `reg` followed
//! by its word, four digits. Bit positions are the C packing rule for `RK_U32` bitfields, checked
//! against gcc and clang by a throwaway probe that is not part of the crate.
//!
//! Shared with the H.264 layouts. A register that is the same register in both (same word, size,
//! type, access, alignment and fields) is a re-export of its [`crate::vepu580_h264`] module, not a
//! copy. Its `REGISTER` constant therefore carries the H.264 C path (`reg_osd.*` for `reg_osd_cfg.*`
//! in H.265, and the same for `reg_s3`/`reg_wgt`, which are different registers and are not shared).
//! The shared groups, by class, are: all of RC, OSD, ST and DEBUG (the same structs), 17 of the 18
//! BASE registers, 51 of 64 PIC registers (the `Vepu580BaseCommon` block both structs include; all
//! 32 address-register entries are shared), and 11 PAR and 11 SQI registers. Not shared: the H.265-only control word
//! `rdo_ckg_hevc` (word 22), the H.265 weight and parameter block (PAR), the H.265 RDO block, and the
//! scaling lists (SCL), which are RK_U32 arrays in H.265 and RK_U16 arrays in H.264.
//!
//! What is left out:
//! - Fields the header names `reserved` (any name starting with `reserve`), and reserved words. They
//!   are in no module.
//! - `enc_id` (`reg_ctl.enc_id`, word 23, byte 0x5c): the kernel's BASE class ends at byte 0x58, so
//!   the kernel never copies this word from the caller. It is not in any class and so in no module.
//! - `H265eV580StatusElem`: a host-side record of the status read-back (a union of `hw_status` and
//!   `Vepu580Status`), not a member of the register set. The ST class is `Vepu580Status` at byte
//!   0x4000, as in H.264, and is the layout this module gives. The union itself is not modelled.
//! - The `vepu580_osd_cfg` and `vepu580Status` typedefs, which name `Vepu580OsdReg` and
//!   `Vepu580Status` again.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/K1a4.md`.

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

pub use crate::vepu580_h264::VEPU580_CLASSES;
pub use job::{
    BLOCK_WORDS, CodecJob, READ_BACK, RegisterSet, SetError, StreamRef, TRANSLATED_CLASS,
    WRITE_CLASSES, WRITE_RUNS,
};
