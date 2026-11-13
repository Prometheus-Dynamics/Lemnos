//! The VPU720 JPEG encoder's register file: 79 words, from `JpegeVpu720BaseReg` (words 0 to 42)
//! and `JpegeVpu720StatusReg` (words 48 to 78) of `hal_jpege_vpu720_reg.h`, with the five words
//! between them unnamed. Source: rockchip-linux/mpp at `9b56348cc61393c84662e6d6dd78feae67773a36`,
//! `mpp/hal/rkenc/jpege/hal_jpege_vpu720_reg.h` (Apache-2.0).
//!
//! Each word and bit field is transcribed from that header, and every one is pinned by the
//! compiler: `src/tests/pins.rs` holds the word, bit and width gcc and clang both give for each.
//! The header has no access modifiers, so writability follows its structure: the status struct,
//! the version word and the interrupt-state word are read-only, every other word may be written.
//! The alignments are the header's own comments on the address registers.

use crate::layout::{AddressReg, Field, Word};

// ---- base ----
pub mod reg000_version {
    use super::*;
    /// Word 0 (byte 0x0000), 1 word.
    pub const WORD: Word = Word::new(0, 1, false);
}
pub mod reg001_enc_strt {
    use super::*;
    /// Word 1 (byte 0x0004), 1 word.
    pub const WORD: Word = Word::new(1, 1, true);
    pub const LKT_NUM: Field = Field::new(1, 0, 8, true, &[]);
    pub const VEPU_CMD: Field =
        Field::new(1, 8, 4, true, &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7]);
}
pub mod reg002_003 {
    use super::*;
    /// Word 2 (byte 0x0008), 2 words.
    pub const WORD: Word = Word::new(2, 2, true);
}
pub mod reg004_int_en {
    use super::*;
    /// Word 4 (byte 0x0010), 1 word.
    pub const WORD: Word = Word::new(4, 1, true);
    pub const FENC_DONE_EN: Field = Field::new(4, 0, 1, true, &[]);
    pub const LKT_NODE_DONE_EN: Field = Field::new(4, 1, 1, true, &[]);
    pub const SCLR_DONE_EN: Field = Field::new(4, 2, 1, true, &[]);
    pub const VSLC_DONE_EN: Field = Field::new(4, 3, 1, true, &[]);
    pub const VBSB_OFLW_EN: Field = Field::new(4, 4, 1, true, &[]);
    pub const VBSB_SCT_EN: Field = Field::new(4, 5, 1, true, &[]);
    pub const FENC_ERR_EN: Field = Field::new(4, 6, 1, true, &[]);
    pub const WDG_EN: Field = Field::new(4, 7, 1, true, &[]);
    pub const LKT_OERR_EN: Field = Field::new(4, 8, 1, true, &[]);
    pub const LKT_ESTP_EN: Field = Field::new(4, 9, 1, true, &[]);
    pub const LKT_FSTP_EN: Field = Field::new(4, 10, 1, true, &[]);
    pub const LKT_NOTE_STP_EN: Field = Field::new(4, 11, 1, true, &[]);
    pub const LKT_DATA_ERROR_EN: Field = Field::new(4, 12, 1, true, &[]);
}
pub mod reg005_int_msk {
    use super::*;
    /// Word 5 (byte 0x0014), 1 word.
    pub const WORD: Word = Word::new(5, 1, true);
    pub const FENC_DONE_MSK: Field = Field::new(5, 0, 1, true, &[]);
    pub const LKT_NODE_DONE_MSK: Field = Field::new(5, 1, 1, true, &[]);
    pub const SCLR_DONE_MSK: Field = Field::new(5, 2, 1, true, &[]);
    pub const VSLC_DONE_MSK: Field = Field::new(5, 3, 1, true, &[]);
    pub const VBSB_OFLW_MSK: Field = Field::new(5, 4, 1, true, &[]);
    pub const VBSB_SCT_MSK: Field = Field::new(5, 5, 1, true, &[]);
    pub const FENC_ERR_MSK: Field = Field::new(5, 6, 1, true, &[]);
    pub const WDG_MSK: Field = Field::new(5, 7, 1, true, &[]);
    pub const LKT_OERR_MSK: Field = Field::new(5, 8, 1, true, &[]);
    pub const LKT_ESTP_MSK: Field = Field::new(5, 9, 1, true, &[]);
    pub const LKT_FSTP_MSK: Field = Field::new(5, 10, 1, true, &[]);
    pub const LKT_NOTE_STP_MSK: Field = Field::new(5, 11, 1, true, &[]);
    pub const LKT_DATA_ERROR_MSK: Field = Field::new(5, 12, 1, true, &[]);
}
pub mod reg006_int_clr {
    use super::*;
    /// Word 6 (byte 0x0018), 1 word.
    pub const WORD: Word = Word::new(6, 1, true);
    pub const FENC_DONE_CLR: Field = Field::new(6, 0, 1, true, &[]);
    pub const LKT_NODE_DONE_CLR: Field = Field::new(6, 1, 1, true, &[]);
    pub const SCLR_DONE_CLR: Field = Field::new(6, 2, 1, true, &[]);
    pub const VSLC_DONE_CLR: Field = Field::new(6, 3, 1, true, &[]);
    pub const VBSB_OFLW_CLR: Field = Field::new(6, 4, 1, true, &[]);
    pub const VBSB_SCT_CLR: Field = Field::new(6, 5, 1, true, &[]);
    pub const FENC_ERR_CLR: Field = Field::new(6, 6, 1, true, &[]);
    pub const WDG_CLR: Field = Field::new(6, 7, 1, true, &[]);
    pub const LKT_OERR_CLR: Field = Field::new(6, 8, 1, true, &[]);
    pub const LKT_ESTP_CLR: Field = Field::new(6, 9, 1, true, &[]);
    pub const LKT_FSTP_CLR: Field = Field::new(6, 10, 1, true, &[]);
    pub const LKT_NOTE_STP_CLR: Field = Field::new(6, 11, 1, true, &[]);
    pub const LKT_DATA_ERROR_CLR: Field = Field::new(6, 12, 1, true, &[]);
}
pub mod reg007_int_state {
    use super::*;
    /// Word 7 (byte 0x001c), 1 word.
    pub const WORD: Word = Word::new(7, 1, false);
    pub const FENC_DONE_STATE: Field = Field::new(7, 0, 1, false, &[]);
    pub const LKT_NODE_DONE_STATE: Field = Field::new(7, 1, 1, false, &[]);
    pub const SCLR_DONE_STATE: Field = Field::new(7, 2, 1, false, &[]);
    pub const VSLC_DONE_STATE: Field = Field::new(7, 3, 1, false, &[]);
    pub const VBSB_OFLW_STATE: Field = Field::new(7, 4, 1, false, &[]);
    pub const VBSB_SCT_STATE: Field = Field::new(7, 5, 1, false, &[]);
    pub const FENC_ERR_STATE: Field = Field::new(7, 6, 1, false, &[]);
    pub const WDG_STATE: Field = Field::new(7, 7, 1, false, &[]);
    pub const LKT_OERR_STATE: Field = Field::new(7, 8, 1, false, &[]);
    pub const LKT_ESTP_STATE: Field = Field::new(7, 9, 1, false, &[]);
    pub const LKT_FSTP_STATE: Field = Field::new(7, 10, 1, false, &[]);
    pub const LKT_NOTE_STP_STATE: Field = Field::new(7, 11, 1, false, &[]);
    pub const LKT_DATA_ERROR_STATE: Field = Field::new(7, 12, 1, false, &[]);
}
pub mod reg008_cru_ctrl {
    use super::*;
    /// Word 8 (byte 0x0020), 1 word.
    pub const WORD: Word = Word::new(8, 1, true);
    pub const RESETN_HW_EN: Field = Field::new(8, 0, 1, true, &[]);
    pub const SRAM_CKG_EN: Field = Field::new(8, 1, 1, true, &[]);
    pub const CKE: Field = Field::new(8, 2, 1, true, &[]);
}
pub mod reg009_lkt_base_addr {
    use super::*;
    /// Word 9 (byte 0x0024), 1 word.
    pub const WORD: Word = Word::new(9, 1, true);
    /// The address register: its bus address is the caller's buffer, 128-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(9, 128);
}
pub mod reg010_node_ocfg {
    use super::*;
    /// Word 10 (byte 0x0028), 1 word.
    pub const WORD: Word = Word::new(10, 1, true);
    pub const CORE_ID: Field = Field::new(10, 0, 4, true, &[]);
    pub const LKT_ERR_STOP_EN: Field = Field::new(10, 4, 1, true, &[]);
    pub const LKT_NODE_STOP_EN: Field = Field::new(10, 5, 1, true, &[]);
    pub const LKTR_BUS_EDIN: Field = Field::new(10, 16, 4, true, &[]);
    pub const LKTW_BUS_EDIN: Field = Field::new(10, 20, 4, true, &[]);
}
pub mod reg011_wdg_jpeg {
    use super::*;
    /// Word 11 (byte 0x002c), 1 word.
    pub const WORD: Word = Word::new(11, 1, true);
}
pub mod reg012 {
    use super::*;
    /// Word 12 (byte 0x0030), 1 word.
    pub const WORD: Word = Word::new(12, 1, true);
}
pub mod reg013_low_delay_ecs_ocfg {
    use super::*;
    /// Word 13 (byte 0x0034), 1 word.
    pub const WORD: Word = Word::new(13, 1, true);
}
pub mod reg014_low_delay_packet_ocfg {
    use super::*;
    /// Word 14 (byte 0x0038), 1 word.
    pub const WORD: Word = Word::new(14, 1, true);
}
pub mod reg015 {
    use super::*;
    /// Word 15 (byte 0x003c), 1 word.
    pub const WORD: Word = Word::new(15, 1, true);
}
pub mod reg016_adr_qtbl {
    use super::*;
    /// Word 16 (byte 0x0040), 1 word.
    pub const WORD: Word = Word::new(16, 1, true);
    /// The address register: its bus address is the caller's buffer, 128-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(16, 128);
}
pub mod reg017_adr_bsbt {
    use super::*;
    /// Word 17 (byte 0x0044), 1 word.
    pub const WORD: Word = Word::new(17, 1, true);
    /// The address register: its bus address is the caller's buffer, 16-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(17, 16);
}
pub mod reg018_adr_bsbb {
    use super::*;
    /// Word 18 (byte 0x0048), 1 word.
    pub const WORD: Word = Word::new(18, 1, true);
    /// The address register: its bus address is the caller's buffer, 16-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(18, 16);
}
pub mod reg019_adr_bsbr {
    use super::*;
    /// Word 19 (byte 0x004c), 1 word.
    pub const WORD: Word = Word::new(19, 1, true);
    /// The address register: its bus address is the caller's buffer, 1-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(19, 1);
}
pub mod reg020_adr_bsbs {
    use super::*;
    /// Word 20 (byte 0x0050), 1 word.
    pub const WORD: Word = Word::new(20, 1, true);
    /// The address register: its bus address is the caller's buffer, 1-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(20, 1);
}
pub mod reg021_adr_ecs_len {
    use super::*;
    /// Word 21 (byte 0x0054), 1 word.
    pub const WORD: Word = Word::new(21, 1, true);
    /// The address register: its bus address is the caller's buffer, 8-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(21, 8);
}
pub mod reg022_adr_src0 {
    use super::*;
    /// Word 22 (byte 0x0058), 1 word.
    pub const WORD: Word = Word::new(22, 1, true);
    /// The address register: its bus address is the caller's buffer, 1-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(22, 1);
}
pub mod reg023_adr_src1 {
    use super::*;
    /// Word 23 (byte 0x005c), 1 word.
    pub const WORD: Word = Word::new(23, 1, true);
    /// The address register: its bus address is the caller's buffer, 1-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(23, 1);
}
pub mod reg024_adr_src2 {
    use super::*;
    /// Word 24 (byte 0x0060), 1 word.
    pub const WORD: Word = Word::new(24, 1, true);
    /// The address register: its bus address is the caller's buffer, 1-byte aligned.
    pub const ADDRESS: AddressReg = AddressReg::new(24, 1);
}
pub mod reg025 {
    use super::*;
    /// Word 25 (byte 0x0064), 1 word.
    pub const WORD: Word = Word::new(25, 1, true);
}
pub mod reg026_axi_perf_ctrl0 {
    use super::*;
    /// Word 26 (byte 0x0068), 1 word.
    pub const WORD: Word = Word::new(26, 1, true);
    pub const PERF_WORK_E: Field = Field::new(26, 0, 1, true, &[]);
    pub const PERF_CLR_E: Field = Field::new(26, 1, 1, true, &[]);
    pub const PERF_FRM_TYPE: Field = Field::new(26, 2, 1, true, &[]);
    pub const CNT_TYPE: Field = Field::new(26, 3, 1, true, &[]);
    pub const RD_LATENCY_ID: Field = Field::new(26, 4, 4, true, &[]);
    pub const RD_LATENCY_THR: Field = Field::new(26, 8, 12, true, &[]);
}
pub mod reg027_axi_perf_ctrl1 {
    use super::*;
    /// Word 27 (byte 0x006c), 1 word.
    pub const WORD: Word = Word::new(27, 1, true);
    pub const ADDR_ALIGN_TYPE: Field = Field::new(27, 0, 2, true, &[]);
    pub const AR_CNT_ID_TYPE: Field = Field::new(27, 2, 1, true, &[]);
    pub const AW_CNT_ID_TYPE: Field = Field::new(27, 3, 1, true, &[]);
    pub const AR_COUNT_ID: Field = Field::new(27, 4, 4, true, &[]);
    pub const AW_COUNT_ID: Field = Field::new(27, 8, 4, true, &[]);
    pub const RD_TOTAL_BYTES_MODE: Field = Field::new(27, 12, 1, true, &[]);
}
pub mod reg028 {
    use super::*;
    /// Word 28 (byte 0x0070), 1 word.
    pub const WORD: Word = Word::new(28, 1, true);
}
pub mod reg029_sw_enc_rsl {
    use super::*;
    /// Word 29 (byte 0x0074), 1 word.
    pub const WORD: Word = Word::new(29, 1, true);
    pub const PIC_WD8_M1: Field = Field::new(29, 0, 13, true, &[]);
    pub const PIC_HD8_M1: Field = Field::new(29, 16, 13, true, &[]);
}
pub mod reg030_sw_src_fill {
    use super::*;
    /// Word 30 (byte 0x0078), 1 word.
    pub const WORD: Word = Word::new(30, 1, true);
    pub const PIC_WFILL_JPEG: Field = Field::new(30, 0, 6, true, &[]);
    pub const PIC_HFILL_JPEG: Field = Field::new(30, 16, 6, true, &[]);
}
pub mod reg031 {
    use super::*;
    /// Word 31 (byte 0x007c), 1 word.
    pub const WORD: Word = Word::new(31, 1, true);
}
pub mod reg032_sw_src_fmt {
    use super::*;
    /// Word 32 (byte 0x0080), 1 word.
    pub const WORD: Word = Word::new(32, 1, true);
    pub const RBUV_SWAP_JPEG: Field = Field::new(32, 1, 1, true, &[]);
    pub const SRC_FMT: Field = Field::new(
        32,
        2,
        4,
        true,
        &[
            0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xa, 0xc, 0xd,
        ],
    );
    pub const OUT_FMT: Field = Field::new(32, 6, 2, true, &[]);
    pub const SRC_RANGE_TRNS_EN: Field = Field::new(32, 9, 1, true, &[]);
    pub const SRC_RANGE_TRNS_SEL: Field = Field::new(32, 10, 1, true, &[]);
    pub const CHROMA_DS_MODE: Field = Field::new(32, 11, 1, true, &[]);
    pub const CHROMA_FORCE_EN: Field = Field::new(32, 12, 1, true, &[]);
    pub const SRC_MIRR_JPEG: Field = Field::new(32, 15, 1, true, &[]);
    pub const U_FORCE_VALUE: Field = Field::new(32, 16, 8, true, &[]);
    pub const V_FORCE_VALUE: Field = Field::new(32, 24, 8, true, &[]);
}
pub mod reg033_sw_pic_ofst {
    use super::*;
    /// Word 33 (byte 0x0084), 1 word.
    pub const WORD: Word = Word::new(33, 1, true);
    pub const PIC_OFST_X: Field = Field::new(33, 0, 16, true, &[]);
    pub const PIC_OFST_Y: Field = Field::new(33, 16, 16, true, &[]);
}
pub mod reg034_sw_src_strd_0 {
    use super::*;
    /// Word 34 (byte 0x0088), 1 word.
    pub const WORD: Word = Word::new(34, 1, true);
    pub const SRC_STRD_0: Field = Field::new(34, 0, 20, true, &[]);
}
pub mod reg035_sw_src_strd_1 {
    use super::*;
    /// Word 35 (byte 0x008c), 1 word.
    pub const WORD: Word = Word::new(35, 1, true);
    pub const SRC_STRD_1: Field = Field::new(35, 0, 19, true, &[]);
}
pub mod reg036_sw_jpeg_enc_cfg {
    use super::*;
    /// Word 36 (byte 0x0090), 1 word.
    pub const WORD: Word = Word::new(36, 1, true);
    pub const RST_INTV: Field = Field::new(36, 0, 16, true, &[]);
    pub const OUT_MODE: Field = Field::new(36, 25, 1, true, &[]);
    pub const RST_M: Field = Field::new(36, 26, 3, true, &[]);
    pub const PIC_LAST_ECS: Field = Field::new(36, 29, 1, true, &[]);
    pub const JPEG_QTBLE_NOLOAD: Field = Field::new(36, 30, 1, true, &[]);
}
pub mod reg037_bsp_size_jpeg {
    use super::*;
    /// Word 37 (byte 0x0094), 1 word.
    pub const WORD: Word = Word::new(37, 1, true);
}
pub mod reg038_sw_uvc_cfg {
    use super::*;
    /// Word 38 (byte 0x0098), 1 word.
    pub const WORD: Word = Word::new(38, 1, true);
    pub const UVC_PARTITION0_LEN: Field = Field::new(38, 0, 12, true, &[]);
    pub const UVC_PARTITION_LEN: Field = Field::new(38, 12, 12, true, &[]);
    pub const UVC_SKIP_LEN: Field = Field::new(38, 24, 6, true, &[]);
}
pub mod reg039_sw_jpeg_y_cfg {
    use super::*;
    /// Word 39 (byte 0x009c), 1 word.
    pub const WORD: Word = Word::new(39, 1, true);
    pub const BIAS_Y: Field = Field::new(39, 0, 15, true, &[]);
}
pub mod reg040_sw_jpeg_u_cfg {
    use super::*;
    /// Word 40 (byte 0x00a0), 1 word.
    pub const WORD: Word = Word::new(40, 1, true);
    pub const BIAS_U: Field = Field::new(40, 0, 15, true, &[]);
}
pub mod reg041_sw_jpeg_v_cfg {
    use super::*;
    /// Word 41 (byte 0x00a4), 1 word.
    pub const WORD: Word = Word::new(41, 1, true);
    pub const BIAS_V: Field = Field::new(41, 0, 15, true, &[]);
}
pub mod reg042_dbus_endn {
    use super::*;
    /// Word 42 (byte 0x00a8), 1 word.
    pub const WORD: Word = Word::new(42, 1, true);
    pub const JBSW_BUS_EDIN: Field = Field::new(42, 0, 4, true, &[]);
    pub const VSL_BUS_EDIN: Field = Field::new(42, 4, 4, true, &[]);
    pub const ECS_LEN_EDIN: Field = Field::new(42, 8, 4, true, &[]);
    pub const SW_QTBL_EDIN: Field = Field::new(42, 12, 4, true, &[]);
}

// ---- status ----
pub mod st_bsl_l32_jpeg_head_bits {
    use super::*;
    /// Word 48 (byte 0x00c0), 1 word.
    pub const WORD: Word = Word::new(48, 1, false);
}
pub mod st_bsl_h32_jpeg_head_bits {
    use super::*;
    /// Word 49 (byte 0x00c4), 1 word.
    pub const WORD: Word = Word::new(49, 1, false);
}
pub mod st_vsp_value0 {
    use super::*;
    /// Word 50 (byte 0x00c8), 1 word.
    pub const WORD: Word = Word::new(50, 1, false);
    pub const Y_MAX_VALUE: Field = Field::new(50, 0, 8, false, &[]);
    pub const Y_MIN_VALUE: Field = Field::new(50, 8, 8, false, &[]);
    pub const U_MAX_VALUE: Field = Field::new(50, 16, 8, false, &[]);
    pub const U_MIN_VLAUE: Field = Field::new(50, 24, 8, false, &[]);
}
pub mod st_vsp_value1 {
    use super::*;
    /// Word 51 (byte 0x00cc), 1 word.
    pub const WORD: Word = Word::new(51, 1, false);
    pub const V_MAX_VALUE: Field = Field::new(51, 0, 8, false, &[]);
    pub const V_MIN_VLAUE: Field = Field::new(51, 8, 8, false, &[]);
    pub const TOTAL_ECS_NUM_MINUS1: Field = Field::new(51, 16, 8, false, &[]);
}
pub mod st_perf_rd_max_latency_num0 {
    use super::*;
    /// Word 52 (byte 0x00d0), 1 word.
    pub const WORD: Word = Word::new(52, 1, false);
}
pub mod st_perf_rd_latency_samp_num {
    use super::*;
    /// Word 53 (byte 0x00d4), 1 word.
    pub const WORD: Word = Word::new(53, 1, false);
}
pub mod st_perf_rd_latency_acc_sum {
    use super::*;
    /// Word 54 (byte 0x00d8), 1 word.
    pub const WORD: Word = Word::new(54, 1, false);
}
pub mod st_perf_rd_axi_total_byte {
    use super::*;
    /// Word 55 (byte 0x00dc), 1 word.
    pub const WORD: Word = Word::new(55, 1, false);
}
pub mod st_perf_wr_axi_total_byte {
    use super::*;
    /// Word 56 (byte 0x00e0), 1 word.
    pub const WORD: Word = Word::new(56, 1, false);
}
pub mod st_perf_working_cnt {
    use super::*;
    /// Word 57 (byte 0x00e4), 1 word.
    pub const WORD: Word = Word::new(57, 1, false);
}
pub mod sw_reserved_00e8_00ec {
    use super::*;
    /// Word 58 (byte 0x00e8), 2 words.
    pub const WORD: Word = Word::new(58, 2, false);
}
pub mod st_wdg {
    use super::*;
    /// Word 60 (byte 0x00f0), 1 word.
    pub const WORD: Word = Word::new(60, 1, false);
    pub const VSP_WORK_FLAG: Field = Field::new(60, 0, 1, false, &[]);
    pub const JPEG_CORE_WORK_FLAG: Field = Field::new(60, 1, 1, false, &[]);
    pub const DMA_WR_WORK_FLAG: Field = Field::new(60, 2, 1, false, &[]);
    pub const DMA_WORK_FLAG: Field = Field::new(60, 3, 1, false, &[]);
}
pub mod st_ppl_pos {
    use super::*;
    /// Word 61 (byte 0x00f4), 1 word.
    pub const WORD: Word = Word::new(61, 1, false);
}
pub mod st_core_pos {
    use super::*;
    /// Word 62 (byte 0x00f8), 1 word.
    pub const WORD: Word = Word::new(62, 1, false);
}
pub mod st_bus {
    use super::*;
    /// Word 63 (byte 0x00fc), 1 word.
    pub const WORD: Word = Word::new(63, 1, false);
    pub const EJPEG_ARREADY: Field = Field::new(63, 0, 1, false, &[]);
    pub const EJPEG_CFG_ARVALID: Field = Field::new(63, 1, 1, false, &[]);
    pub const EJPEG_CFG_ARVALID_TYPE: Field = Field::new(63, 2, 1, false, &[]);
    pub const EJPEG_CFG_ARREADY: Field = Field::new(63, 3, 1, false, &[]);
    pub const EJPEG_VSP_ARVALID: Field = Field::new(63, 4, 1, false, &[]);
    pub const EJPEG_VSP_ARREADY: Field = Field::new(63, 5, 1, false, &[]);
    pub const RKEJPEG_ARVALID: Field = Field::new(63, 6, 1, false, &[]);
    pub const EJPEG_CFG_AR_CNT: Field = Field::new(63, 7, 2, false, &[]);
    pub const RKEJPEG_ARREADY: Field = Field::new(63, 9, 1, false, &[]);
    pub const RKEJPEG_RAVLID: Field = Field::new(63, 10, 1, false, &[]);
    pub const RKEJPEG_RID: Field = Field::new(63, 11, 4, false, &[]);
    pub const RKEJPEG_RRESP: Field = Field::new(63, 15, 2, false, &[]);
    pub const RKEJPEG_RREADY: Field = Field::new(63, 17, 1, false, &[]);
    pub const AXI_WR_STATE_CS: Field = Field::new(63, 18, 1, false, &[]);
    pub const EJPEG_STRMD_AWVALID: Field = Field::new(63, 19, 1, false, &[]);
    pub const EJPEG_STRMD_AWTYPE: Field = Field::new(63, 20, 1, false, &[]);
    pub const EJPEG_STRMD_AWREADY: Field = Field::new(63, 21, 1, false, &[]);
    pub const EJPEG_STRMD_WVALID: Field = Field::new(63, 22, 1, false, &[]);
    pub const EJPEG_STRMD_WREADY: Field = Field::new(63, 23, 1, false, &[]);
    pub const EJPEG_CFG_AWVALID: Field = Field::new(63, 24, 1, false, &[]);
    pub const EJPEG_CFG_AWREADY: Field = Field::new(63, 25, 1, false, &[]);
    pub const RKEJPEG_AWVALID: Field = Field::new(63, 26, 1, false, &[]);
    pub const RKEJPEG_AWID: Field = Field::new(63, 27, 1, false, &[]);
    pub const RKEJPEG_WVALID: Field = Field::new(63, 28, 1, false, &[]);
    pub const RKEJPEG_WREADY: Field = Field::new(63, 29, 1, false, &[]);
    pub const EJPEG_FREEZE_FLAG: Field = Field::new(63, 30, 1, false, &[]);
}
pub mod dbg_ppl {
    use super::*;
    /// Word 64 (byte 0x0100), 1 word.
    pub const WORD: Word = Word::new(64, 1, false);
}
pub mod dbg_jpeg_core {
    use super::*;
    /// Word 65 (byte 0x0104), 1 word.
    pub const WORD: Word = Word::new(65, 1, false);
}
pub mod sw_reserved_0108 {
    use super::*;
    /// Word 66 (byte 0x0108), 1 word.
    pub const WORD: Word = Word::new(66, 1, false);
}
pub mod st_adr_jbsbw {
    use super::*;
    /// Word 67 (byte 0x010c), 1 word.
    pub const WORD: Word = Word::new(67, 1, false);
}
pub mod st_adr_ecs_len {
    use super::*;
    /// Word 68 (byte 0x0110), 1 word.
    pub const WORD: Word = Word::new(68, 1, false);
}
pub mod st_adr_src0_jpeg {
    use super::*;
    /// Word 69 (byte 0x0114), 1 word.
    pub const WORD: Word = Word::new(69, 1, false);
}
pub mod st_adr_src1_jpeg {
    use super::*;
    /// Word 70 (byte 0x0118), 1 word.
    pub const WORD: Word = Word::new(70, 1, false);
}
pub mod st_adr_src2_jpeg {
    use super::*;
    /// Word 71 (byte 0x011c), 1 word.
    pub const WORD: Word = Word::new(71, 1, false);
}
pub mod st_low_delay_packet_num {
    use super::*;
    /// Word 72 (byte 0x0120), 1 word.
    pub const WORD: Word = Word::new(72, 1, false);
    pub const BS_PACKET_NUM: Field = Field::new(72, 0, 16, false, &[]);
    pub const BS_PACKET_LST: Field = Field::new(72, 16, 1, false, &[]);
}
pub mod st_ecs_len_num {
    use super::*;
    /// Word 73 (byte 0x0124), 1 word.
    pub const WORD: Word = Word::new(73, 1, false);
    pub const ECS_LEN_NUM: Field = Field::new(73, 0, 16, false, &[]);
    pub const ECS_LEN_LST: Field = Field::new(73, 16, 1, false, &[]);
}
pub mod st_enc {
    use super::*;
    /// Word 74 (byte 0x0128), 1 word.
    pub const WORD: Word = Word::new(74, 1, false);
    pub const JPEG_ENC_STATE: Field = Field::new(74, 0, 4, false, &[]);
    pub const LKT_MODE_EN: Field = Field::new(74, 4, 1, false, &[]);
}
pub mod st_lkt_num {
    use super::*;
    /// Word 75 (byte 0x012c), 1 word.
    pub const WORD: Word = Word::new(75, 1, false);
    pub const LKT_CFG_NUM: Field = Field::new(75, 0, 8, false, &[]);
    pub const LKT_DONE_NUM: Field = Field::new(75, 8, 8, false, &[]);
    pub const LKT_INT_NUM: Field = Field::new(75, 16, 8, false, &[]);
    pub const LKT_CFG_LOAD_NUM: Field = Field::new(75, 24, 8, false, &[]);
}
pub mod st_lkt_info {
    use super::*;
    /// Word 76 (byte 0x0130), 1 word.
    pub const WORD: Word = Word::new(76, 1, false);
    pub const LKT_CORE_ID: Field = Field::new(76, 0, 4, false, &[]);
    pub const LKT_STOP_FLAG: Field = Field::new(76, 8, 1, false, &[]);
    pub const LKT_NODE_INT: Field = Field::new(76, 9, 1, false, &[]);
    pub const LKT_TASK_ID: Field = Field::new(76, 10, 12, false, &[]);
}
pub mod st_lkt_cfg_next_addr {
    use super::*;
    /// Word 77 (byte 0x0134), 1 word.
    pub const WORD: Word = Word::new(77, 1, false);
}
pub mod st_lkt_waddr {
    use super::*;
    /// Word 78 (byte 0x0138), 1 word.
    pub const WORD: Word = Word::new(78, 1, false);
}
