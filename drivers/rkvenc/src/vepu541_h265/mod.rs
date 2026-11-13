//! The VEPU541 H.265 encoder's register layouts, and the builder for its write side.
//!
//! Provenance: rockchip-linux/mpp at `9b56348cc61393c84662e6d6dd78feae67773a36` (Apache-2.0):
//! `mpp/hal/rkenc/h265e/hal_h265e_vepu541_reg.h` (`H265eV541RegSet`, the write side and the
//! read-back, and `H265eV541IoctlOutputElem`, whose `hw_status` bits name word 7) and
//! `mpp/hal/rkenc/h265e/hal_h265e_vepu54x_reg_l2.h` (`H265eV54xL2RegSet`, the second window). The
//! names, offsets, bit fields and the header's comments are transcribed; no code is copied.
//!
//! The layout is the C struct's, laid out with gcc's SysV bitfield rules: a bitfield never straddles
//! its 32-bit unit, and a plain member starts at the next offset its alignment allows. Every field's
//! bit position and width, and every register's offset, were checked against gcc and clang by
//! compiling the header and setting each member (a throwaway probe, not part of the crate).
//!
//! What the layouts leave out:
//!
//! - Reserved bits (fields named `reserve*` or `reserved*`) and reserved words (`reserved_*`,
//!   `reserve`, and `reg_thd` at 0x194, which the header marks reserved). They are never written.
//! - The VEPU540 arm of the write side's `dtrns_cfg` union (word 16): the VEPU541 arm
//!   (`dtrns_cfg_541`) is the one laid out here.
//! - The VEPU540 arm of the second window's `thd` union (`thd_540`): the `thd_541` arm is laid out.
//! - Reserved encodings: the header lists none, so every value of a field's width is accepted.
//! - Alignments: the header gives no alignment for any address register, so each is 1 byte.
//!
//! The second window: the header's comments put its first member at 0x4, while its C struct starts
//! at 0. The layouts follow the comments (one word after the struct's), so word 0 of the window is
//! not a member. The window's offsets are the kernel's, from `RKVENC_L2_OFFSET`.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/K1a3.md`.

pub mod l2;
pub mod ret;
pub mod set;

mod job;

pub use job::{
    CodecJob, L2_FIRST, L2_WORDS, READ_BACK, RegisterSet, SetError, StreamRef, WRITE_WORDS,
};
