//! The VEPU541 H.264 encoder's register layouts, and the builder for its write side.
//!
//! Provenance: rockchip-linux/mpp at `9b56348cc61393c84662e6d6dd78feae67773a36` (Apache-2.0):
//! `mpp/hal/rkenc/h264e/hal_h264e_vepu541_reg.h` (`Vepu541H264eRegSet_t`, the write side, and
//! `Vepu541H264eRegRet_t`, the read-back), `mpp/hal/rkenc/h264e/hal_h264e_vepu541_reg_l2.h`
//! (`Vepu541H264eRegL2Set_t`), and `mpp/hal/rkenc/common/vepu541_common.h` for the two small
//! structs they use (`Vepu541OsdPos_t`, `Vepu541B8NumQp_t`). The names, offsets, bit fields, access
//! types and the alignments the header's comments state are transcribed; no code is copied.
//!
//! Each register's byte offset comes from its header comment, and each field's position from the
//! bitfields in C packing. Every field and every offset was checked against gcc, clang and zig cc
//! by compiling the header and setting each bitfield (a throwaway probe, not part of the crate).
//! Two places are where the header's C struct and its own register map disagree, and the map is
//! used:
//!
//! - `reg143` (`ST_SNUM`): `sli_num` (6 bits) and `reserved` (30 bits) do not fit one word, so the
//!   struct gives the register two words and puts `reg144` at 0x244; the register map gives one
//!   word and `reg144` at 0x240. The register is one word here, its reserved bits left out.
//! - The read-back and L2 structs are views placed at the register map's own addresses, not at 0.
//!   Their fields are checked inside each register and their spacing against the struct.
//!
//! Alignment: an address register's alignment is the one its header comment gives, either as
//! `N B aligned` or as `High N bits` (the low `32 - N` bits are not used, so the address is
//! `2^(32-N)` aligned). Where neither is given it is 1, and the builder accepts any offset.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/K1a.md`.

pub mod l2;
pub mod ret;
pub mod set;

mod job;

pub use job::{CodecJob, READ_BACK, RegisterSet, SetError, StreamRef, WRITE_WORDS};
