//! Pins: every word and bit field of the VPU720 encoder's file, against the values gcc and clang
//! give for `hal_jpege_vpu720_reg.h` at rockchip-linux/mpp `9b56348`. The probe ran with
//! `-std=c11 -Wall -Werror -Wno-overflow` (the overflow warning is the probe's own `~0u` written
//! into a 1-bit field, which the compilers truncate by design); both compilers print the same lines.

use crate::vpu720::*;

#[test]
fn every_word_sits_where_the_header_puts_it() {
    assert_eq!(
        (reg000_version::WORD.index, reg000_version::WORD.words),
        (0, 1),
        "reg000_version"
    );
    assert_eq!(
        (reg001_enc_strt::WORD.index, reg001_enc_strt::WORD.words),
        (1, 1),
        "reg001_enc_strt"
    );
    assert_eq!(
        (reg002_003::WORD.index, reg002_003::WORD.words),
        (2, 2),
        "reg002_003"
    );
    assert_eq!(
        (reg004_int_en::WORD.index, reg004_int_en::WORD.words),
        (4, 1),
        "reg004_int_en"
    );
    assert_eq!(
        (reg005_int_msk::WORD.index, reg005_int_msk::WORD.words),
        (5, 1),
        "reg005_int_msk"
    );
    assert_eq!(
        (reg006_int_clr::WORD.index, reg006_int_clr::WORD.words),
        (6, 1),
        "reg006_int_clr"
    );
    assert_eq!(
        (reg007_int_state::WORD.index, reg007_int_state::WORD.words),
        (7, 1),
        "reg007_int_state"
    );
    assert_eq!(
        (reg008_cru_ctrl::WORD.index, reg008_cru_ctrl::WORD.words),
        (8, 1),
        "reg008_cru_ctrl"
    );
    assert_eq!(
        (
            reg009_lkt_base_addr::WORD.index,
            reg009_lkt_base_addr::WORD.words
        ),
        (9, 1),
        "reg009_lkt_base_addr"
    );
    assert_eq!(
        (reg010_node_ocfg::WORD.index, reg010_node_ocfg::WORD.words),
        (10, 1),
        "reg010_node_ocfg"
    );
    assert_eq!(
        (reg011_wdg_jpeg::WORD.index, reg011_wdg_jpeg::WORD.words),
        (11, 1),
        "reg011_wdg_jpeg"
    );
    assert_eq!((reg012::WORD.index, reg012::WORD.words), (12, 1), "reg012");
    assert_eq!(
        (
            reg013_low_delay_ecs_ocfg::WORD.index,
            reg013_low_delay_ecs_ocfg::WORD.words
        ),
        (13, 1),
        "reg013_low_delay_ecs_ocfg"
    );
    assert_eq!(
        (
            reg014_low_delay_packet_ocfg::WORD.index,
            reg014_low_delay_packet_ocfg::WORD.words
        ),
        (14, 1),
        "reg014_low_delay_packet_ocfg"
    );
    assert_eq!((reg015::WORD.index, reg015::WORD.words), (15, 1), "reg015");
    assert_eq!(
        (reg016_adr_qtbl::WORD.index, reg016_adr_qtbl::WORD.words),
        (16, 1),
        "reg016_adr_qtbl"
    );
    assert_eq!(
        (reg017_adr_bsbt::WORD.index, reg017_adr_bsbt::WORD.words),
        (17, 1),
        "reg017_adr_bsbt"
    );
    assert_eq!(
        (reg018_adr_bsbb::WORD.index, reg018_adr_bsbb::WORD.words),
        (18, 1),
        "reg018_adr_bsbb"
    );
    assert_eq!(
        (reg019_adr_bsbr::WORD.index, reg019_adr_bsbr::WORD.words),
        (19, 1),
        "reg019_adr_bsbr"
    );
    assert_eq!(
        (reg020_adr_bsbs::WORD.index, reg020_adr_bsbs::WORD.words),
        (20, 1),
        "reg020_adr_bsbs"
    );
    assert_eq!(
        (
            reg021_adr_ecs_len::WORD.index,
            reg021_adr_ecs_len::WORD.words
        ),
        (21, 1),
        "reg021_adr_ecs_len"
    );
    assert_eq!(
        (reg022_adr_src0::WORD.index, reg022_adr_src0::WORD.words),
        (22, 1),
        "reg022_adr_src0"
    );
    assert_eq!(
        (reg023_adr_src1::WORD.index, reg023_adr_src1::WORD.words),
        (23, 1),
        "reg023_adr_src1"
    );
    assert_eq!(
        (reg024_adr_src2::WORD.index, reg024_adr_src2::WORD.words),
        (24, 1),
        "reg024_adr_src2"
    );
    assert_eq!((reg025::WORD.index, reg025::WORD.words), (25, 1), "reg025");
    assert_eq!(
        (
            reg026_axi_perf_ctrl0::WORD.index,
            reg026_axi_perf_ctrl0::WORD.words
        ),
        (26, 1),
        "reg026_axi_perf_ctrl0"
    );
    assert_eq!(
        (
            reg027_axi_perf_ctrl1::WORD.index,
            reg027_axi_perf_ctrl1::WORD.words
        ),
        (27, 1),
        "reg027_axi_perf_ctrl1"
    );
    assert_eq!((reg028::WORD.index, reg028::WORD.words), (28, 1), "reg028");
    assert_eq!(
        (reg029_sw_enc_rsl::WORD.index, reg029_sw_enc_rsl::WORD.words),
        (29, 1),
        "reg029_sw_enc_rsl"
    );
    assert_eq!(
        (
            reg030_sw_src_fill::WORD.index,
            reg030_sw_src_fill::WORD.words
        ),
        (30, 1),
        "reg030_sw_src_fill"
    );
    assert_eq!((reg031::WORD.index, reg031::WORD.words), (31, 1), "reg031");
    assert_eq!(
        (reg032_sw_src_fmt::WORD.index, reg032_sw_src_fmt::WORD.words),
        (32, 1),
        "reg032_sw_src_fmt"
    );
    assert_eq!(
        (
            reg033_sw_pic_ofst::WORD.index,
            reg033_sw_pic_ofst::WORD.words
        ),
        (33, 1),
        "reg033_sw_pic_ofst"
    );
    assert_eq!(
        (
            reg034_sw_src_strd_0::WORD.index,
            reg034_sw_src_strd_0::WORD.words
        ),
        (34, 1),
        "reg034_sw_src_strd_0"
    );
    assert_eq!(
        (
            reg035_sw_src_strd_1::WORD.index,
            reg035_sw_src_strd_1::WORD.words
        ),
        (35, 1),
        "reg035_sw_src_strd_1"
    );
    assert_eq!(
        (
            reg036_sw_jpeg_enc_cfg::WORD.index,
            reg036_sw_jpeg_enc_cfg::WORD.words
        ),
        (36, 1),
        "reg036_sw_jpeg_enc_cfg"
    );
    assert_eq!(
        (
            reg037_bsp_size_jpeg::WORD.index,
            reg037_bsp_size_jpeg::WORD.words
        ),
        (37, 1),
        "reg037_bsp_size_jpeg"
    );
    assert_eq!(
        (reg038_sw_uvc_cfg::WORD.index, reg038_sw_uvc_cfg::WORD.words),
        (38, 1),
        "reg038_sw_uvc_cfg"
    );
    assert_eq!(
        (
            reg039_sw_jpeg_y_cfg::WORD.index,
            reg039_sw_jpeg_y_cfg::WORD.words
        ),
        (39, 1),
        "reg039_sw_jpeg_y_cfg"
    );
    assert_eq!(
        (
            reg040_sw_jpeg_u_cfg::WORD.index,
            reg040_sw_jpeg_u_cfg::WORD.words
        ),
        (40, 1),
        "reg040_sw_jpeg_u_cfg"
    );
    assert_eq!(
        (
            reg041_sw_jpeg_v_cfg::WORD.index,
            reg041_sw_jpeg_v_cfg::WORD.words
        ),
        (41, 1),
        "reg041_sw_jpeg_v_cfg"
    );
    assert_eq!(
        (reg042_dbus_endn::WORD.index, reg042_dbus_endn::WORD.words),
        (42, 1),
        "reg042_dbus_endn"
    );
    assert_eq!(
        (
            st_bsl_l32_jpeg_head_bits::WORD.index,
            st_bsl_l32_jpeg_head_bits::WORD.words
        ),
        (48, 1),
        "st_bsl_l32_jpeg_head_bits"
    );
    assert_eq!(
        (
            st_bsl_h32_jpeg_head_bits::WORD.index,
            st_bsl_h32_jpeg_head_bits::WORD.words
        ),
        (49, 1),
        "st_bsl_h32_jpeg_head_bits"
    );
    assert_eq!(
        (st_vsp_value0::WORD.index, st_vsp_value0::WORD.words),
        (50, 1),
        "st_vsp_value0"
    );
    assert_eq!(
        (st_vsp_value1::WORD.index, st_vsp_value1::WORD.words),
        (51, 1),
        "st_vsp_value1"
    );
    assert_eq!(
        (
            st_perf_rd_max_latency_num0::WORD.index,
            st_perf_rd_max_latency_num0::WORD.words
        ),
        (52, 1),
        "st_perf_rd_max_latency_num0"
    );
    assert_eq!(
        (
            st_perf_rd_latency_samp_num::WORD.index,
            st_perf_rd_latency_samp_num::WORD.words
        ),
        (53, 1),
        "st_perf_rd_latency_samp_num"
    );
    assert_eq!(
        (
            st_perf_rd_latency_acc_sum::WORD.index,
            st_perf_rd_latency_acc_sum::WORD.words
        ),
        (54, 1),
        "st_perf_rd_latency_acc_sum"
    );
    assert_eq!(
        (
            st_perf_rd_axi_total_byte::WORD.index,
            st_perf_rd_axi_total_byte::WORD.words
        ),
        (55, 1),
        "st_perf_rd_axi_total_byte"
    );
    assert_eq!(
        (
            st_perf_wr_axi_total_byte::WORD.index,
            st_perf_wr_axi_total_byte::WORD.words
        ),
        (56, 1),
        "st_perf_wr_axi_total_byte"
    );
    assert_eq!(
        (
            st_perf_working_cnt::WORD.index,
            st_perf_working_cnt::WORD.words
        ),
        (57, 1),
        "st_perf_working_cnt"
    );
    assert_eq!(
        (
            sw_reserved_00e8_00ec::WORD.index,
            sw_reserved_00e8_00ec::WORD.words
        ),
        (58, 2),
        "sw_reserved_00e8_00ec"
    );
    assert_eq!((st_wdg::WORD.index, st_wdg::WORD.words), (60, 1), "st_wdg");
    assert_eq!(
        (st_ppl_pos::WORD.index, st_ppl_pos::WORD.words),
        (61, 1),
        "st_ppl_pos"
    );
    assert_eq!(
        (st_core_pos::WORD.index, st_core_pos::WORD.words),
        (62, 1),
        "st_core_pos"
    );
    assert_eq!((st_bus::WORD.index, st_bus::WORD.words), (63, 1), "st_bus");
    assert_eq!(
        (dbg_ppl::WORD.index, dbg_ppl::WORD.words),
        (64, 1),
        "dbg_ppl"
    );
    assert_eq!(
        (dbg_jpeg_core::WORD.index, dbg_jpeg_core::WORD.words),
        (65, 1),
        "dbg_jpeg_core"
    );
    assert_eq!(
        (sw_reserved_0108::WORD.index, sw_reserved_0108::WORD.words),
        (66, 1),
        "sw_reserved_0108"
    );
    assert_eq!(
        (st_adr_jbsbw::WORD.index, st_adr_jbsbw::WORD.words),
        (67, 1),
        "st_adr_jbsbw"
    );
    assert_eq!(
        (st_adr_ecs_len::WORD.index, st_adr_ecs_len::WORD.words),
        (68, 1),
        "st_adr_ecs_len"
    );
    assert_eq!(
        (st_adr_src0_jpeg::WORD.index, st_adr_src0_jpeg::WORD.words),
        (69, 1),
        "st_adr_src0_jpeg"
    );
    assert_eq!(
        (st_adr_src1_jpeg::WORD.index, st_adr_src1_jpeg::WORD.words),
        (70, 1),
        "st_adr_src1_jpeg"
    );
    assert_eq!(
        (st_adr_src2_jpeg::WORD.index, st_adr_src2_jpeg::WORD.words),
        (71, 1),
        "st_adr_src2_jpeg"
    );
    assert_eq!(
        (
            st_low_delay_packet_num::WORD.index,
            st_low_delay_packet_num::WORD.words
        ),
        (72, 1),
        "st_low_delay_packet_num"
    );
    assert_eq!(
        (st_ecs_len_num::WORD.index, st_ecs_len_num::WORD.words),
        (73, 1),
        "st_ecs_len_num"
    );
    assert_eq!((st_enc::WORD.index, st_enc::WORD.words), (74, 1), "st_enc");
    assert_eq!(
        (st_lkt_num::WORD.index, st_lkt_num::WORD.words),
        (75, 1),
        "st_lkt_num"
    );
    assert_eq!(
        (st_lkt_info::WORD.index, st_lkt_info::WORD.words),
        (76, 1),
        "st_lkt_info"
    );
    assert_eq!(
        (
            st_lkt_cfg_next_addr::WORD.index,
            st_lkt_cfg_next_addr::WORD.words
        ),
        (77, 1),
        "st_lkt_cfg_next_addr"
    );
    assert_eq!(
        (st_lkt_waddr::WORD.index, st_lkt_waddr::WORD.words),
        (78, 1),
        "st_lkt_waddr"
    );
}

#[test]
fn every_field_sits_where_the_header_puts_it() {
    assert_eq!(
        (
            reg001_enc_strt::LKT_NUM.word,
            reg001_enc_strt::LKT_NUM.lsb,
            reg001_enc_strt::LKT_NUM.width
        ),
        (1, 0, 8),
        "reg001_enc_strt.lkt_num"
    );
    assert_eq!(
        (
            reg001_enc_strt::VEPU_CMD.word,
            reg001_enc_strt::VEPU_CMD.lsb,
            reg001_enc_strt::VEPU_CMD.width
        ),
        (1, 8, 4),
        "reg001_enc_strt.vepu_cmd"
    );
    assert_eq!(
        (
            reg004_int_en::FENC_DONE_EN.word,
            reg004_int_en::FENC_DONE_EN.lsb,
            reg004_int_en::FENC_DONE_EN.width
        ),
        (4, 0, 1),
        "reg004_int_en.fenc_done_en"
    );
    assert_eq!(
        (
            reg004_int_en::LKT_NODE_DONE_EN.word,
            reg004_int_en::LKT_NODE_DONE_EN.lsb,
            reg004_int_en::LKT_NODE_DONE_EN.width
        ),
        (4, 1, 1),
        "reg004_int_en.lkt_node_done_en"
    );
    assert_eq!(
        (
            reg004_int_en::SCLR_DONE_EN.word,
            reg004_int_en::SCLR_DONE_EN.lsb,
            reg004_int_en::SCLR_DONE_EN.width
        ),
        (4, 2, 1),
        "reg004_int_en.sclr_done_en"
    );
    assert_eq!(
        (
            reg004_int_en::VSLC_DONE_EN.word,
            reg004_int_en::VSLC_DONE_EN.lsb,
            reg004_int_en::VSLC_DONE_EN.width
        ),
        (4, 3, 1),
        "reg004_int_en.vslc_done_en"
    );
    assert_eq!(
        (
            reg004_int_en::VBSB_OFLW_EN.word,
            reg004_int_en::VBSB_OFLW_EN.lsb,
            reg004_int_en::VBSB_OFLW_EN.width
        ),
        (4, 4, 1),
        "reg004_int_en.vbsb_oflw_en"
    );
    assert_eq!(
        (
            reg004_int_en::VBSB_SCT_EN.word,
            reg004_int_en::VBSB_SCT_EN.lsb,
            reg004_int_en::VBSB_SCT_EN.width
        ),
        (4, 5, 1),
        "reg004_int_en.vbsb_sct_en"
    );
    assert_eq!(
        (
            reg004_int_en::FENC_ERR_EN.word,
            reg004_int_en::FENC_ERR_EN.lsb,
            reg004_int_en::FENC_ERR_EN.width
        ),
        (4, 6, 1),
        "reg004_int_en.fenc_err_en"
    );
    assert_eq!(
        (
            reg004_int_en::WDG_EN.word,
            reg004_int_en::WDG_EN.lsb,
            reg004_int_en::WDG_EN.width
        ),
        (4, 7, 1),
        "reg004_int_en.wdg_en"
    );
    assert_eq!(
        (
            reg004_int_en::LKT_OERR_EN.word,
            reg004_int_en::LKT_OERR_EN.lsb,
            reg004_int_en::LKT_OERR_EN.width
        ),
        (4, 8, 1),
        "reg004_int_en.lkt_oerr_en"
    );
    assert_eq!(
        (
            reg004_int_en::LKT_ESTP_EN.word,
            reg004_int_en::LKT_ESTP_EN.lsb,
            reg004_int_en::LKT_ESTP_EN.width
        ),
        (4, 9, 1),
        "reg004_int_en.lkt_estp_en"
    );
    assert_eq!(
        (
            reg004_int_en::LKT_FSTP_EN.word,
            reg004_int_en::LKT_FSTP_EN.lsb,
            reg004_int_en::LKT_FSTP_EN.width
        ),
        (4, 10, 1),
        "reg004_int_en.lkt_fstp_en"
    );
    assert_eq!(
        (
            reg004_int_en::LKT_NOTE_STP_EN.word,
            reg004_int_en::LKT_NOTE_STP_EN.lsb,
            reg004_int_en::LKT_NOTE_STP_EN.width
        ),
        (4, 11, 1),
        "reg004_int_en.lkt_note_stp_en"
    );
    assert_eq!(
        (
            reg004_int_en::LKT_DATA_ERROR_EN.word,
            reg004_int_en::LKT_DATA_ERROR_EN.lsb,
            reg004_int_en::LKT_DATA_ERROR_EN.width
        ),
        (4, 12, 1),
        "reg004_int_en.lkt_data_error_en"
    );
    assert_eq!(
        (
            reg005_int_msk::FENC_DONE_MSK.word,
            reg005_int_msk::FENC_DONE_MSK.lsb,
            reg005_int_msk::FENC_DONE_MSK.width
        ),
        (5, 0, 1),
        "reg005_int_msk.fenc_done_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::LKT_NODE_DONE_MSK.word,
            reg005_int_msk::LKT_NODE_DONE_MSK.lsb,
            reg005_int_msk::LKT_NODE_DONE_MSK.width
        ),
        (5, 1, 1),
        "reg005_int_msk.lkt_node_done_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::SCLR_DONE_MSK.word,
            reg005_int_msk::SCLR_DONE_MSK.lsb,
            reg005_int_msk::SCLR_DONE_MSK.width
        ),
        (5, 2, 1),
        "reg005_int_msk.sclr_done_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::VSLC_DONE_MSK.word,
            reg005_int_msk::VSLC_DONE_MSK.lsb,
            reg005_int_msk::VSLC_DONE_MSK.width
        ),
        (5, 3, 1),
        "reg005_int_msk.vslc_done_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::VBSB_OFLW_MSK.word,
            reg005_int_msk::VBSB_OFLW_MSK.lsb,
            reg005_int_msk::VBSB_OFLW_MSK.width
        ),
        (5, 4, 1),
        "reg005_int_msk.vbsb_oflw_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::VBSB_SCT_MSK.word,
            reg005_int_msk::VBSB_SCT_MSK.lsb,
            reg005_int_msk::VBSB_SCT_MSK.width
        ),
        (5, 5, 1),
        "reg005_int_msk.vbsb_sct_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::FENC_ERR_MSK.word,
            reg005_int_msk::FENC_ERR_MSK.lsb,
            reg005_int_msk::FENC_ERR_MSK.width
        ),
        (5, 6, 1),
        "reg005_int_msk.fenc_err_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::WDG_MSK.word,
            reg005_int_msk::WDG_MSK.lsb,
            reg005_int_msk::WDG_MSK.width
        ),
        (5, 7, 1),
        "reg005_int_msk.wdg_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::LKT_OERR_MSK.word,
            reg005_int_msk::LKT_OERR_MSK.lsb,
            reg005_int_msk::LKT_OERR_MSK.width
        ),
        (5, 8, 1),
        "reg005_int_msk.lkt_oerr_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::LKT_ESTP_MSK.word,
            reg005_int_msk::LKT_ESTP_MSK.lsb,
            reg005_int_msk::LKT_ESTP_MSK.width
        ),
        (5, 9, 1),
        "reg005_int_msk.lkt_estp_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::LKT_FSTP_MSK.word,
            reg005_int_msk::LKT_FSTP_MSK.lsb,
            reg005_int_msk::LKT_FSTP_MSK.width
        ),
        (5, 10, 1),
        "reg005_int_msk.lkt_fstp_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::LKT_NOTE_STP_MSK.word,
            reg005_int_msk::LKT_NOTE_STP_MSK.lsb,
            reg005_int_msk::LKT_NOTE_STP_MSK.width
        ),
        (5, 11, 1),
        "reg005_int_msk.lkt_note_stp_msk"
    );
    assert_eq!(
        (
            reg005_int_msk::LKT_DATA_ERROR_MSK.word,
            reg005_int_msk::LKT_DATA_ERROR_MSK.lsb,
            reg005_int_msk::LKT_DATA_ERROR_MSK.width
        ),
        (5, 12, 1),
        "reg005_int_msk.lkt_data_error_msk"
    );
    assert_eq!(
        (
            reg006_int_clr::FENC_DONE_CLR.word,
            reg006_int_clr::FENC_DONE_CLR.lsb,
            reg006_int_clr::FENC_DONE_CLR.width
        ),
        (6, 0, 1),
        "reg006_int_clr.fenc_done_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::LKT_NODE_DONE_CLR.word,
            reg006_int_clr::LKT_NODE_DONE_CLR.lsb,
            reg006_int_clr::LKT_NODE_DONE_CLR.width
        ),
        (6, 1, 1),
        "reg006_int_clr.lkt_node_done_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::SCLR_DONE_CLR.word,
            reg006_int_clr::SCLR_DONE_CLR.lsb,
            reg006_int_clr::SCLR_DONE_CLR.width
        ),
        (6, 2, 1),
        "reg006_int_clr.sclr_done_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::VSLC_DONE_CLR.word,
            reg006_int_clr::VSLC_DONE_CLR.lsb,
            reg006_int_clr::VSLC_DONE_CLR.width
        ),
        (6, 3, 1),
        "reg006_int_clr.vslc_done_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::VBSB_OFLW_CLR.word,
            reg006_int_clr::VBSB_OFLW_CLR.lsb,
            reg006_int_clr::VBSB_OFLW_CLR.width
        ),
        (6, 4, 1),
        "reg006_int_clr.vbsb_oflw_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::VBSB_SCT_CLR.word,
            reg006_int_clr::VBSB_SCT_CLR.lsb,
            reg006_int_clr::VBSB_SCT_CLR.width
        ),
        (6, 5, 1),
        "reg006_int_clr.vbsb_sct_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::FENC_ERR_CLR.word,
            reg006_int_clr::FENC_ERR_CLR.lsb,
            reg006_int_clr::FENC_ERR_CLR.width
        ),
        (6, 6, 1),
        "reg006_int_clr.fenc_err_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::WDG_CLR.word,
            reg006_int_clr::WDG_CLR.lsb,
            reg006_int_clr::WDG_CLR.width
        ),
        (6, 7, 1),
        "reg006_int_clr.wdg_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::LKT_OERR_CLR.word,
            reg006_int_clr::LKT_OERR_CLR.lsb,
            reg006_int_clr::LKT_OERR_CLR.width
        ),
        (6, 8, 1),
        "reg006_int_clr.lkt_oerr_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::LKT_ESTP_CLR.word,
            reg006_int_clr::LKT_ESTP_CLR.lsb,
            reg006_int_clr::LKT_ESTP_CLR.width
        ),
        (6, 9, 1),
        "reg006_int_clr.lkt_estp_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::LKT_FSTP_CLR.word,
            reg006_int_clr::LKT_FSTP_CLR.lsb,
            reg006_int_clr::LKT_FSTP_CLR.width
        ),
        (6, 10, 1),
        "reg006_int_clr.lkt_fstp_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::LKT_NOTE_STP_CLR.word,
            reg006_int_clr::LKT_NOTE_STP_CLR.lsb,
            reg006_int_clr::LKT_NOTE_STP_CLR.width
        ),
        (6, 11, 1),
        "reg006_int_clr.lkt_note_stp_clr"
    );
    assert_eq!(
        (
            reg006_int_clr::LKT_DATA_ERROR_CLR.word,
            reg006_int_clr::LKT_DATA_ERROR_CLR.lsb,
            reg006_int_clr::LKT_DATA_ERROR_CLR.width
        ),
        (6, 12, 1),
        "reg006_int_clr.lkt_data_error_clr"
    );
    assert_eq!(
        (
            reg007_int_state::FENC_DONE_STATE.word,
            reg007_int_state::FENC_DONE_STATE.lsb,
            reg007_int_state::FENC_DONE_STATE.width
        ),
        (7, 0, 1),
        "reg007_int_state.fenc_done_state"
    );
    assert_eq!(
        (
            reg007_int_state::LKT_NODE_DONE_STATE.word,
            reg007_int_state::LKT_NODE_DONE_STATE.lsb,
            reg007_int_state::LKT_NODE_DONE_STATE.width
        ),
        (7, 1, 1),
        "reg007_int_state.lkt_node_done_state"
    );
    assert_eq!(
        (
            reg007_int_state::SCLR_DONE_STATE.word,
            reg007_int_state::SCLR_DONE_STATE.lsb,
            reg007_int_state::SCLR_DONE_STATE.width
        ),
        (7, 2, 1),
        "reg007_int_state.sclr_done_state"
    );
    assert_eq!(
        (
            reg007_int_state::VSLC_DONE_STATE.word,
            reg007_int_state::VSLC_DONE_STATE.lsb,
            reg007_int_state::VSLC_DONE_STATE.width
        ),
        (7, 3, 1),
        "reg007_int_state.vslc_done_state"
    );
    assert_eq!(
        (
            reg007_int_state::VBSB_OFLW_STATE.word,
            reg007_int_state::VBSB_OFLW_STATE.lsb,
            reg007_int_state::VBSB_OFLW_STATE.width
        ),
        (7, 4, 1),
        "reg007_int_state.vbsb_oflw_state"
    );
    assert_eq!(
        (
            reg007_int_state::VBSB_SCT_STATE.word,
            reg007_int_state::VBSB_SCT_STATE.lsb,
            reg007_int_state::VBSB_SCT_STATE.width
        ),
        (7, 5, 1),
        "reg007_int_state.vbsb_sct_state"
    );
    assert_eq!(
        (
            reg007_int_state::FENC_ERR_STATE.word,
            reg007_int_state::FENC_ERR_STATE.lsb,
            reg007_int_state::FENC_ERR_STATE.width
        ),
        (7, 6, 1),
        "reg007_int_state.fenc_err_state"
    );
    assert_eq!(
        (
            reg007_int_state::WDG_STATE.word,
            reg007_int_state::WDG_STATE.lsb,
            reg007_int_state::WDG_STATE.width
        ),
        (7, 7, 1),
        "reg007_int_state.wdg_state"
    );
    assert_eq!(
        (
            reg007_int_state::LKT_OERR_STATE.word,
            reg007_int_state::LKT_OERR_STATE.lsb,
            reg007_int_state::LKT_OERR_STATE.width
        ),
        (7, 8, 1),
        "reg007_int_state.lkt_oerr_state"
    );
    assert_eq!(
        (
            reg007_int_state::LKT_ESTP_STATE.word,
            reg007_int_state::LKT_ESTP_STATE.lsb,
            reg007_int_state::LKT_ESTP_STATE.width
        ),
        (7, 9, 1),
        "reg007_int_state.lkt_estp_state"
    );
    assert_eq!(
        (
            reg007_int_state::LKT_FSTP_STATE.word,
            reg007_int_state::LKT_FSTP_STATE.lsb,
            reg007_int_state::LKT_FSTP_STATE.width
        ),
        (7, 10, 1),
        "reg007_int_state.lkt_fstp_state"
    );
    assert_eq!(
        (
            reg007_int_state::LKT_NOTE_STP_STATE.word,
            reg007_int_state::LKT_NOTE_STP_STATE.lsb,
            reg007_int_state::LKT_NOTE_STP_STATE.width
        ),
        (7, 11, 1),
        "reg007_int_state.lkt_note_stp_state"
    );
    assert_eq!(
        (
            reg007_int_state::LKT_DATA_ERROR_STATE.word,
            reg007_int_state::LKT_DATA_ERROR_STATE.lsb,
            reg007_int_state::LKT_DATA_ERROR_STATE.width
        ),
        (7, 12, 1),
        "reg007_int_state.lkt_data_error_state"
    );
    assert_eq!(
        (
            reg008_cru_ctrl::RESETN_HW_EN.word,
            reg008_cru_ctrl::RESETN_HW_EN.lsb,
            reg008_cru_ctrl::RESETN_HW_EN.width
        ),
        (8, 0, 1),
        "reg008_cru_ctrl.resetn_hw_en"
    );
    assert_eq!(
        (
            reg008_cru_ctrl::SRAM_CKG_EN.word,
            reg008_cru_ctrl::SRAM_CKG_EN.lsb,
            reg008_cru_ctrl::SRAM_CKG_EN.width
        ),
        (8, 1, 1),
        "reg008_cru_ctrl.sram_ckg_en"
    );
    assert_eq!(
        (
            reg008_cru_ctrl::CKE.word,
            reg008_cru_ctrl::CKE.lsb,
            reg008_cru_ctrl::CKE.width
        ),
        (8, 2, 1),
        "reg008_cru_ctrl.cke"
    );
    assert_eq!(
        (
            reg010_node_ocfg::CORE_ID.word,
            reg010_node_ocfg::CORE_ID.lsb,
            reg010_node_ocfg::CORE_ID.width
        ),
        (10, 0, 4),
        "reg010_node_ocfg.core_id"
    );
    assert_eq!(
        (
            reg010_node_ocfg::LKT_ERR_STOP_EN.word,
            reg010_node_ocfg::LKT_ERR_STOP_EN.lsb,
            reg010_node_ocfg::LKT_ERR_STOP_EN.width
        ),
        (10, 4, 1),
        "reg010_node_ocfg.lkt_err_stop_en"
    );
    assert_eq!(
        (
            reg010_node_ocfg::LKT_NODE_STOP_EN.word,
            reg010_node_ocfg::LKT_NODE_STOP_EN.lsb,
            reg010_node_ocfg::LKT_NODE_STOP_EN.width
        ),
        (10, 5, 1),
        "reg010_node_ocfg.lkt_node_stop_en"
    );
    assert_eq!(
        (
            reg010_node_ocfg::LKTR_BUS_EDIN.word,
            reg010_node_ocfg::LKTR_BUS_EDIN.lsb,
            reg010_node_ocfg::LKTR_BUS_EDIN.width
        ),
        (10, 16, 4),
        "reg010_node_ocfg.lktr_bus_edin"
    );
    assert_eq!(
        (
            reg010_node_ocfg::LKTW_BUS_EDIN.word,
            reg010_node_ocfg::LKTW_BUS_EDIN.lsb,
            reg010_node_ocfg::LKTW_BUS_EDIN.width
        ),
        (10, 20, 4),
        "reg010_node_ocfg.lktw_bus_edin"
    );
    assert_eq!(
        (
            reg026_axi_perf_ctrl0::PERF_WORK_E.word,
            reg026_axi_perf_ctrl0::PERF_WORK_E.lsb,
            reg026_axi_perf_ctrl0::PERF_WORK_E.width
        ),
        (26, 0, 1),
        "reg026_axi_perf_ctrl0.perf_work_e"
    );
    assert_eq!(
        (
            reg026_axi_perf_ctrl0::PERF_CLR_E.word,
            reg026_axi_perf_ctrl0::PERF_CLR_E.lsb,
            reg026_axi_perf_ctrl0::PERF_CLR_E.width
        ),
        (26, 1, 1),
        "reg026_axi_perf_ctrl0.perf_clr_e"
    );
    assert_eq!(
        (
            reg026_axi_perf_ctrl0::PERF_FRM_TYPE.word,
            reg026_axi_perf_ctrl0::PERF_FRM_TYPE.lsb,
            reg026_axi_perf_ctrl0::PERF_FRM_TYPE.width
        ),
        (26, 2, 1),
        "reg026_axi_perf_ctrl0.perf_frm_type"
    );
    assert_eq!(
        (
            reg026_axi_perf_ctrl0::CNT_TYPE.word,
            reg026_axi_perf_ctrl0::CNT_TYPE.lsb,
            reg026_axi_perf_ctrl0::CNT_TYPE.width
        ),
        (26, 3, 1),
        "reg026_axi_perf_ctrl0.cnt_type"
    );
    assert_eq!(
        (
            reg026_axi_perf_ctrl0::RD_LATENCY_ID.word,
            reg026_axi_perf_ctrl0::RD_LATENCY_ID.lsb,
            reg026_axi_perf_ctrl0::RD_LATENCY_ID.width
        ),
        (26, 4, 4),
        "reg026_axi_perf_ctrl0.rd_latency_id"
    );
    assert_eq!(
        (
            reg026_axi_perf_ctrl0::RD_LATENCY_THR.word,
            reg026_axi_perf_ctrl0::RD_LATENCY_THR.lsb,
            reg026_axi_perf_ctrl0::RD_LATENCY_THR.width
        ),
        (26, 8, 12),
        "reg026_axi_perf_ctrl0.rd_latency_thr"
    );
    assert_eq!(
        (
            reg027_axi_perf_ctrl1::ADDR_ALIGN_TYPE.word,
            reg027_axi_perf_ctrl1::ADDR_ALIGN_TYPE.lsb,
            reg027_axi_perf_ctrl1::ADDR_ALIGN_TYPE.width
        ),
        (27, 0, 2),
        "reg027_axi_perf_ctrl1.addr_align_type"
    );
    assert_eq!(
        (
            reg027_axi_perf_ctrl1::AR_CNT_ID_TYPE.word,
            reg027_axi_perf_ctrl1::AR_CNT_ID_TYPE.lsb,
            reg027_axi_perf_ctrl1::AR_CNT_ID_TYPE.width
        ),
        (27, 2, 1),
        "reg027_axi_perf_ctrl1.ar_cnt_id_type"
    );
    assert_eq!(
        (
            reg027_axi_perf_ctrl1::AW_CNT_ID_TYPE.word,
            reg027_axi_perf_ctrl1::AW_CNT_ID_TYPE.lsb,
            reg027_axi_perf_ctrl1::AW_CNT_ID_TYPE.width
        ),
        (27, 3, 1),
        "reg027_axi_perf_ctrl1.aw_cnt_id_type"
    );
    assert_eq!(
        (
            reg027_axi_perf_ctrl1::AR_COUNT_ID.word,
            reg027_axi_perf_ctrl1::AR_COUNT_ID.lsb,
            reg027_axi_perf_ctrl1::AR_COUNT_ID.width
        ),
        (27, 4, 4),
        "reg027_axi_perf_ctrl1.ar_count_id"
    );
    assert_eq!(
        (
            reg027_axi_perf_ctrl1::AW_COUNT_ID.word,
            reg027_axi_perf_ctrl1::AW_COUNT_ID.lsb,
            reg027_axi_perf_ctrl1::AW_COUNT_ID.width
        ),
        (27, 8, 4),
        "reg027_axi_perf_ctrl1.aw_count_id"
    );
    assert_eq!(
        (
            reg027_axi_perf_ctrl1::RD_TOTAL_BYTES_MODE.word,
            reg027_axi_perf_ctrl1::RD_TOTAL_BYTES_MODE.lsb,
            reg027_axi_perf_ctrl1::RD_TOTAL_BYTES_MODE.width
        ),
        (27, 12, 1),
        "reg027_axi_perf_ctrl1.rd_total_bytes_mode"
    );
    assert_eq!(
        (
            reg029_sw_enc_rsl::PIC_WD8_M1.word,
            reg029_sw_enc_rsl::PIC_WD8_M1.lsb,
            reg029_sw_enc_rsl::PIC_WD8_M1.width
        ),
        (29, 0, 13),
        "reg029_sw_enc_rsl.pic_wd8_m1"
    );
    assert_eq!(
        (
            reg029_sw_enc_rsl::PIC_HD8_M1.word,
            reg029_sw_enc_rsl::PIC_HD8_M1.lsb,
            reg029_sw_enc_rsl::PIC_HD8_M1.width
        ),
        (29, 16, 13),
        "reg029_sw_enc_rsl.pic_hd8_m1"
    );
    assert_eq!(
        (
            reg030_sw_src_fill::PIC_WFILL_JPEG.word,
            reg030_sw_src_fill::PIC_WFILL_JPEG.lsb,
            reg030_sw_src_fill::PIC_WFILL_JPEG.width
        ),
        (30, 0, 6),
        "reg030_sw_src_fill.pic_wfill_jpeg"
    );
    assert_eq!(
        (
            reg030_sw_src_fill::PIC_HFILL_JPEG.word,
            reg030_sw_src_fill::PIC_HFILL_JPEG.lsb,
            reg030_sw_src_fill::PIC_HFILL_JPEG.width
        ),
        (30, 16, 6),
        "reg030_sw_src_fill.pic_hfill_jpeg"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::RBUV_SWAP_JPEG.word,
            reg032_sw_src_fmt::RBUV_SWAP_JPEG.lsb,
            reg032_sw_src_fmt::RBUV_SWAP_JPEG.width
        ),
        (32, 1, 1),
        "reg032_sw_src_fmt.rbuv_swap_jpeg"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::SRC_FMT.word,
            reg032_sw_src_fmt::SRC_FMT.lsb,
            reg032_sw_src_fmt::SRC_FMT.width
        ),
        (32, 2, 4),
        "reg032_sw_src_fmt.src_fmt"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::OUT_FMT.word,
            reg032_sw_src_fmt::OUT_FMT.lsb,
            reg032_sw_src_fmt::OUT_FMT.width
        ),
        (32, 6, 2),
        "reg032_sw_src_fmt.out_fmt"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::SRC_RANGE_TRNS_EN.word,
            reg032_sw_src_fmt::SRC_RANGE_TRNS_EN.lsb,
            reg032_sw_src_fmt::SRC_RANGE_TRNS_EN.width
        ),
        (32, 9, 1),
        "reg032_sw_src_fmt.src_range_trns_en"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::SRC_RANGE_TRNS_SEL.word,
            reg032_sw_src_fmt::SRC_RANGE_TRNS_SEL.lsb,
            reg032_sw_src_fmt::SRC_RANGE_TRNS_SEL.width
        ),
        (32, 10, 1),
        "reg032_sw_src_fmt.src_range_trns_sel"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::CHROMA_DS_MODE.word,
            reg032_sw_src_fmt::CHROMA_DS_MODE.lsb,
            reg032_sw_src_fmt::CHROMA_DS_MODE.width
        ),
        (32, 11, 1),
        "reg032_sw_src_fmt.chroma_ds_mode"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::CHROMA_FORCE_EN.word,
            reg032_sw_src_fmt::CHROMA_FORCE_EN.lsb,
            reg032_sw_src_fmt::CHROMA_FORCE_EN.width
        ),
        (32, 12, 1),
        "reg032_sw_src_fmt.chroma_force_en"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::SRC_MIRR_JPEG.word,
            reg032_sw_src_fmt::SRC_MIRR_JPEG.lsb,
            reg032_sw_src_fmt::SRC_MIRR_JPEG.width
        ),
        (32, 15, 1),
        "reg032_sw_src_fmt.src_mirr_jpeg"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::U_FORCE_VALUE.word,
            reg032_sw_src_fmt::U_FORCE_VALUE.lsb,
            reg032_sw_src_fmt::U_FORCE_VALUE.width
        ),
        (32, 16, 8),
        "reg032_sw_src_fmt.u_force_value"
    );
    assert_eq!(
        (
            reg032_sw_src_fmt::V_FORCE_VALUE.word,
            reg032_sw_src_fmt::V_FORCE_VALUE.lsb,
            reg032_sw_src_fmt::V_FORCE_VALUE.width
        ),
        (32, 24, 8),
        "reg032_sw_src_fmt.v_force_value"
    );
    assert_eq!(
        (
            reg033_sw_pic_ofst::PIC_OFST_X.word,
            reg033_sw_pic_ofst::PIC_OFST_X.lsb,
            reg033_sw_pic_ofst::PIC_OFST_X.width
        ),
        (33, 0, 16),
        "reg033_sw_pic_ofst.pic_ofst_x"
    );
    assert_eq!(
        (
            reg033_sw_pic_ofst::PIC_OFST_Y.word,
            reg033_sw_pic_ofst::PIC_OFST_Y.lsb,
            reg033_sw_pic_ofst::PIC_OFST_Y.width
        ),
        (33, 16, 16),
        "reg033_sw_pic_ofst.pic_ofst_y"
    );
    assert_eq!(
        (
            reg034_sw_src_strd_0::SRC_STRD_0.word,
            reg034_sw_src_strd_0::SRC_STRD_0.lsb,
            reg034_sw_src_strd_0::SRC_STRD_0.width
        ),
        (34, 0, 20),
        "reg034_sw_src_strd_0.src_strd_0"
    );
    assert_eq!(
        (
            reg035_sw_src_strd_1::SRC_STRD_1.word,
            reg035_sw_src_strd_1::SRC_STRD_1.lsb,
            reg035_sw_src_strd_1::SRC_STRD_1.width
        ),
        (35, 0, 19),
        "reg035_sw_src_strd_1.src_strd_1"
    );
    assert_eq!(
        (
            reg036_sw_jpeg_enc_cfg::RST_INTV.word,
            reg036_sw_jpeg_enc_cfg::RST_INTV.lsb,
            reg036_sw_jpeg_enc_cfg::RST_INTV.width
        ),
        (36, 0, 16),
        "reg036_sw_jpeg_enc_cfg.rst_intv"
    );
    assert_eq!(
        (
            reg036_sw_jpeg_enc_cfg::OUT_MODE.word,
            reg036_sw_jpeg_enc_cfg::OUT_MODE.lsb,
            reg036_sw_jpeg_enc_cfg::OUT_MODE.width
        ),
        (36, 25, 1),
        "reg036_sw_jpeg_enc_cfg.out_mode"
    );
    assert_eq!(
        (
            reg036_sw_jpeg_enc_cfg::RST_M.word,
            reg036_sw_jpeg_enc_cfg::RST_M.lsb,
            reg036_sw_jpeg_enc_cfg::RST_M.width
        ),
        (36, 26, 3),
        "reg036_sw_jpeg_enc_cfg.rst_m"
    );
    assert_eq!(
        (
            reg036_sw_jpeg_enc_cfg::PIC_LAST_ECS.word,
            reg036_sw_jpeg_enc_cfg::PIC_LAST_ECS.lsb,
            reg036_sw_jpeg_enc_cfg::PIC_LAST_ECS.width
        ),
        (36, 29, 1),
        "reg036_sw_jpeg_enc_cfg.pic_last_ecs"
    );
    assert_eq!(
        (
            reg036_sw_jpeg_enc_cfg::JPEG_QTBLE_NOLOAD.word,
            reg036_sw_jpeg_enc_cfg::JPEG_QTBLE_NOLOAD.lsb,
            reg036_sw_jpeg_enc_cfg::JPEG_QTBLE_NOLOAD.width
        ),
        (36, 30, 1),
        "reg036_sw_jpeg_enc_cfg.jpeg_qtble_noload"
    );
    assert_eq!(
        (
            reg038_sw_uvc_cfg::UVC_PARTITION0_LEN.word,
            reg038_sw_uvc_cfg::UVC_PARTITION0_LEN.lsb,
            reg038_sw_uvc_cfg::UVC_PARTITION0_LEN.width
        ),
        (38, 0, 12),
        "reg038_sw_uvc_cfg.uvc_partition0_len"
    );
    assert_eq!(
        (
            reg038_sw_uvc_cfg::UVC_PARTITION_LEN.word,
            reg038_sw_uvc_cfg::UVC_PARTITION_LEN.lsb,
            reg038_sw_uvc_cfg::UVC_PARTITION_LEN.width
        ),
        (38, 12, 12),
        "reg038_sw_uvc_cfg.uvc_partition_len"
    );
    assert_eq!(
        (
            reg038_sw_uvc_cfg::UVC_SKIP_LEN.word,
            reg038_sw_uvc_cfg::UVC_SKIP_LEN.lsb,
            reg038_sw_uvc_cfg::UVC_SKIP_LEN.width
        ),
        (38, 24, 6),
        "reg038_sw_uvc_cfg.uvc_skip_len"
    );
    assert_eq!(
        (
            reg039_sw_jpeg_y_cfg::BIAS_Y.word,
            reg039_sw_jpeg_y_cfg::BIAS_Y.lsb,
            reg039_sw_jpeg_y_cfg::BIAS_Y.width
        ),
        (39, 0, 15),
        "reg039_sw_jpeg_y_cfg.bias_y"
    );
    assert_eq!(
        (
            reg040_sw_jpeg_u_cfg::BIAS_U.word,
            reg040_sw_jpeg_u_cfg::BIAS_U.lsb,
            reg040_sw_jpeg_u_cfg::BIAS_U.width
        ),
        (40, 0, 15),
        "reg040_sw_jpeg_u_cfg.bias_u"
    );
    assert_eq!(
        (
            reg041_sw_jpeg_v_cfg::BIAS_V.word,
            reg041_sw_jpeg_v_cfg::BIAS_V.lsb,
            reg041_sw_jpeg_v_cfg::BIAS_V.width
        ),
        (41, 0, 15),
        "reg041_sw_jpeg_v_cfg.bias_v"
    );
    assert_eq!(
        (
            reg042_dbus_endn::JBSW_BUS_EDIN.word,
            reg042_dbus_endn::JBSW_BUS_EDIN.lsb,
            reg042_dbus_endn::JBSW_BUS_EDIN.width
        ),
        (42, 0, 4),
        "reg042_dbus_endn.jbsw_bus_edin"
    );
    assert_eq!(
        (
            reg042_dbus_endn::VSL_BUS_EDIN.word,
            reg042_dbus_endn::VSL_BUS_EDIN.lsb,
            reg042_dbus_endn::VSL_BUS_EDIN.width
        ),
        (42, 4, 4),
        "reg042_dbus_endn.vsl_bus_edin"
    );
    assert_eq!(
        (
            reg042_dbus_endn::ECS_LEN_EDIN.word,
            reg042_dbus_endn::ECS_LEN_EDIN.lsb,
            reg042_dbus_endn::ECS_LEN_EDIN.width
        ),
        (42, 8, 4),
        "reg042_dbus_endn.ecs_len_edin"
    );
    assert_eq!(
        (
            reg042_dbus_endn::SW_QTBL_EDIN.word,
            reg042_dbus_endn::SW_QTBL_EDIN.lsb,
            reg042_dbus_endn::SW_QTBL_EDIN.width
        ),
        (42, 12, 4),
        "reg042_dbus_endn.sw_qtbl_edin"
    );
    assert_eq!(
        (
            st_vsp_value0::Y_MAX_VALUE.word,
            st_vsp_value0::Y_MAX_VALUE.lsb,
            st_vsp_value0::Y_MAX_VALUE.width
        ),
        (50, 0, 8),
        "st_vsp_value0.y_max_value"
    );
    assert_eq!(
        (
            st_vsp_value0::Y_MIN_VALUE.word,
            st_vsp_value0::Y_MIN_VALUE.lsb,
            st_vsp_value0::Y_MIN_VALUE.width
        ),
        (50, 8, 8),
        "st_vsp_value0.y_min_value"
    );
    assert_eq!(
        (
            st_vsp_value0::U_MAX_VALUE.word,
            st_vsp_value0::U_MAX_VALUE.lsb,
            st_vsp_value0::U_MAX_VALUE.width
        ),
        (50, 16, 8),
        "st_vsp_value0.u_max_value"
    );
    assert_eq!(
        (
            st_vsp_value0::U_MIN_VLAUE.word,
            st_vsp_value0::U_MIN_VLAUE.lsb,
            st_vsp_value0::U_MIN_VLAUE.width
        ),
        (50, 24, 8),
        "st_vsp_value0.u_min_vlaue"
    );
    assert_eq!(
        (
            st_vsp_value1::V_MAX_VALUE.word,
            st_vsp_value1::V_MAX_VALUE.lsb,
            st_vsp_value1::V_MAX_VALUE.width
        ),
        (51, 0, 8),
        "st_vsp_value1.v_max_value"
    );
    assert_eq!(
        (
            st_vsp_value1::V_MIN_VLAUE.word,
            st_vsp_value1::V_MIN_VLAUE.lsb,
            st_vsp_value1::V_MIN_VLAUE.width
        ),
        (51, 8, 8),
        "st_vsp_value1.v_min_vlaue"
    );
    assert_eq!(
        (
            st_vsp_value1::TOTAL_ECS_NUM_MINUS1.word,
            st_vsp_value1::TOTAL_ECS_NUM_MINUS1.lsb,
            st_vsp_value1::TOTAL_ECS_NUM_MINUS1.width
        ),
        (51, 16, 8),
        "st_vsp_value1.total_ecs_num_minus1"
    );
    assert_eq!(
        (
            st_wdg::VSP_WORK_FLAG.word,
            st_wdg::VSP_WORK_FLAG.lsb,
            st_wdg::VSP_WORK_FLAG.width
        ),
        (60, 0, 1),
        "st_wdg.vsp_work_flag"
    );
    assert_eq!(
        (
            st_wdg::JPEG_CORE_WORK_FLAG.word,
            st_wdg::JPEG_CORE_WORK_FLAG.lsb,
            st_wdg::JPEG_CORE_WORK_FLAG.width
        ),
        (60, 1, 1),
        "st_wdg.jpeg_core_work_flag"
    );
    assert_eq!(
        (
            st_wdg::DMA_WR_WORK_FLAG.word,
            st_wdg::DMA_WR_WORK_FLAG.lsb,
            st_wdg::DMA_WR_WORK_FLAG.width
        ),
        (60, 2, 1),
        "st_wdg.dma_wr_work_flag"
    );
    assert_eq!(
        (
            st_wdg::DMA_WORK_FLAG.word,
            st_wdg::DMA_WORK_FLAG.lsb,
            st_wdg::DMA_WORK_FLAG.width
        ),
        (60, 3, 1),
        "st_wdg.dma_work_flag"
    );
    assert_eq!(
        (
            st_bus::EJPEG_ARREADY.word,
            st_bus::EJPEG_ARREADY.lsb,
            st_bus::EJPEG_ARREADY.width
        ),
        (63, 0, 1),
        "st_bus.ejpeg_arready"
    );
    assert_eq!(
        (
            st_bus::EJPEG_CFG_ARVALID.word,
            st_bus::EJPEG_CFG_ARVALID.lsb,
            st_bus::EJPEG_CFG_ARVALID.width
        ),
        (63, 1, 1),
        "st_bus.ejpeg_cfg_arvalid"
    );
    assert_eq!(
        (
            st_bus::EJPEG_CFG_ARVALID_TYPE.word,
            st_bus::EJPEG_CFG_ARVALID_TYPE.lsb,
            st_bus::EJPEG_CFG_ARVALID_TYPE.width
        ),
        (63, 2, 1),
        "st_bus.ejpeg_cfg_arvalid_type"
    );
    assert_eq!(
        (
            st_bus::EJPEG_CFG_ARREADY.word,
            st_bus::EJPEG_CFG_ARREADY.lsb,
            st_bus::EJPEG_CFG_ARREADY.width
        ),
        (63, 3, 1),
        "st_bus.ejpeg_cfg_arready"
    );
    assert_eq!(
        (
            st_bus::EJPEG_VSP_ARVALID.word,
            st_bus::EJPEG_VSP_ARVALID.lsb,
            st_bus::EJPEG_VSP_ARVALID.width
        ),
        (63, 4, 1),
        "st_bus.ejpeg_vsp_arvalid"
    );
    assert_eq!(
        (
            st_bus::EJPEG_VSP_ARREADY.word,
            st_bus::EJPEG_VSP_ARREADY.lsb,
            st_bus::EJPEG_VSP_ARREADY.width
        ),
        (63, 5, 1),
        "st_bus.ejpeg_vsp_arready"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_ARVALID.word,
            st_bus::RKEJPEG_ARVALID.lsb,
            st_bus::RKEJPEG_ARVALID.width
        ),
        (63, 6, 1),
        "st_bus.rkejpeg_arvalid"
    );
    assert_eq!(
        (
            st_bus::EJPEG_CFG_AR_CNT.word,
            st_bus::EJPEG_CFG_AR_CNT.lsb,
            st_bus::EJPEG_CFG_AR_CNT.width
        ),
        (63, 7, 2),
        "st_bus.ejpeg_cfg_ar_cnt"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_ARREADY.word,
            st_bus::RKEJPEG_ARREADY.lsb,
            st_bus::RKEJPEG_ARREADY.width
        ),
        (63, 9, 1),
        "st_bus.rkejpeg_arready"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_RAVLID.word,
            st_bus::RKEJPEG_RAVLID.lsb,
            st_bus::RKEJPEG_RAVLID.width
        ),
        (63, 10, 1),
        "st_bus.rkejpeg_ravlid"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_RID.word,
            st_bus::RKEJPEG_RID.lsb,
            st_bus::RKEJPEG_RID.width
        ),
        (63, 11, 4),
        "st_bus.rkejpeg_rid"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_RRESP.word,
            st_bus::RKEJPEG_RRESP.lsb,
            st_bus::RKEJPEG_RRESP.width
        ),
        (63, 15, 2),
        "st_bus.rkejpeg_rresp"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_RREADY.word,
            st_bus::RKEJPEG_RREADY.lsb,
            st_bus::RKEJPEG_RREADY.width
        ),
        (63, 17, 1),
        "st_bus.rkejpeg_rready"
    );
    assert_eq!(
        (
            st_bus::AXI_WR_STATE_CS.word,
            st_bus::AXI_WR_STATE_CS.lsb,
            st_bus::AXI_WR_STATE_CS.width
        ),
        (63, 18, 1),
        "st_bus.axi_wr_state_cs"
    );
    assert_eq!(
        (
            st_bus::EJPEG_STRMD_AWVALID.word,
            st_bus::EJPEG_STRMD_AWVALID.lsb,
            st_bus::EJPEG_STRMD_AWVALID.width
        ),
        (63, 19, 1),
        "st_bus.ejpeg_strmd_awvalid"
    );
    assert_eq!(
        (
            st_bus::EJPEG_STRMD_AWTYPE.word,
            st_bus::EJPEG_STRMD_AWTYPE.lsb,
            st_bus::EJPEG_STRMD_AWTYPE.width
        ),
        (63, 20, 1),
        "st_bus.ejpeg_strmd_awtype"
    );
    assert_eq!(
        (
            st_bus::EJPEG_STRMD_AWREADY.word,
            st_bus::EJPEG_STRMD_AWREADY.lsb,
            st_bus::EJPEG_STRMD_AWREADY.width
        ),
        (63, 21, 1),
        "st_bus.ejpeg_strmd_awready"
    );
    assert_eq!(
        (
            st_bus::EJPEG_STRMD_WVALID.word,
            st_bus::EJPEG_STRMD_WVALID.lsb,
            st_bus::EJPEG_STRMD_WVALID.width
        ),
        (63, 22, 1),
        "st_bus.ejpeg_strmd_wvalid"
    );
    assert_eq!(
        (
            st_bus::EJPEG_STRMD_WREADY.word,
            st_bus::EJPEG_STRMD_WREADY.lsb,
            st_bus::EJPEG_STRMD_WREADY.width
        ),
        (63, 23, 1),
        "st_bus.ejpeg_strmd_wready"
    );
    assert_eq!(
        (
            st_bus::EJPEG_CFG_AWVALID.word,
            st_bus::EJPEG_CFG_AWVALID.lsb,
            st_bus::EJPEG_CFG_AWVALID.width
        ),
        (63, 24, 1),
        "st_bus.ejpeg_cfg_awvalid"
    );
    assert_eq!(
        (
            st_bus::EJPEG_CFG_AWREADY.word,
            st_bus::EJPEG_CFG_AWREADY.lsb,
            st_bus::EJPEG_CFG_AWREADY.width
        ),
        (63, 25, 1),
        "st_bus.ejpeg_cfg_awready"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_AWVALID.word,
            st_bus::RKEJPEG_AWVALID.lsb,
            st_bus::RKEJPEG_AWVALID.width
        ),
        (63, 26, 1),
        "st_bus.rkejpeg_awvalid"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_AWID.word,
            st_bus::RKEJPEG_AWID.lsb,
            st_bus::RKEJPEG_AWID.width
        ),
        (63, 27, 1),
        "st_bus.rkejpeg_awid"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_WVALID.word,
            st_bus::RKEJPEG_WVALID.lsb,
            st_bus::RKEJPEG_WVALID.width
        ),
        (63, 28, 1),
        "st_bus.rkejpeg_wvalid"
    );
    assert_eq!(
        (
            st_bus::RKEJPEG_WREADY.word,
            st_bus::RKEJPEG_WREADY.lsb,
            st_bus::RKEJPEG_WREADY.width
        ),
        (63, 29, 1),
        "st_bus.rkejpeg_wready"
    );
    assert_eq!(
        (
            st_bus::EJPEG_FREEZE_FLAG.word,
            st_bus::EJPEG_FREEZE_FLAG.lsb,
            st_bus::EJPEG_FREEZE_FLAG.width
        ),
        (63, 30, 1),
        "st_bus.ejpeg_freeze_flag"
    );
    assert_eq!(
        (
            st_low_delay_packet_num::BS_PACKET_NUM.word,
            st_low_delay_packet_num::BS_PACKET_NUM.lsb,
            st_low_delay_packet_num::BS_PACKET_NUM.width
        ),
        (72, 0, 16),
        "st_low_delay_packet_num.bs_packet_num"
    );
    assert_eq!(
        (
            st_low_delay_packet_num::BS_PACKET_LST.word,
            st_low_delay_packet_num::BS_PACKET_LST.lsb,
            st_low_delay_packet_num::BS_PACKET_LST.width
        ),
        (72, 16, 1),
        "st_low_delay_packet_num.bs_packet_lst"
    );
    assert_eq!(
        (
            st_ecs_len_num::ECS_LEN_NUM.word,
            st_ecs_len_num::ECS_LEN_NUM.lsb,
            st_ecs_len_num::ECS_LEN_NUM.width
        ),
        (73, 0, 16),
        "st_ecs_len_num.ecs_len_num"
    );
    assert_eq!(
        (
            st_ecs_len_num::ECS_LEN_LST.word,
            st_ecs_len_num::ECS_LEN_LST.lsb,
            st_ecs_len_num::ECS_LEN_LST.width
        ),
        (73, 16, 1),
        "st_ecs_len_num.ecs_len_lst"
    );
    assert_eq!(
        (
            st_enc::JPEG_ENC_STATE.word,
            st_enc::JPEG_ENC_STATE.lsb,
            st_enc::JPEG_ENC_STATE.width
        ),
        (74, 0, 4),
        "st_enc.jpeg_enc_state"
    );
    assert_eq!(
        (
            st_enc::LKT_MODE_EN.word,
            st_enc::LKT_MODE_EN.lsb,
            st_enc::LKT_MODE_EN.width
        ),
        (74, 4, 1),
        "st_enc.lkt_mode_en"
    );
    assert_eq!(
        (
            st_lkt_num::LKT_CFG_NUM.word,
            st_lkt_num::LKT_CFG_NUM.lsb,
            st_lkt_num::LKT_CFG_NUM.width
        ),
        (75, 0, 8),
        "st_lkt_num.lkt_cfg_num"
    );
    assert_eq!(
        (
            st_lkt_num::LKT_DONE_NUM.word,
            st_lkt_num::LKT_DONE_NUM.lsb,
            st_lkt_num::LKT_DONE_NUM.width
        ),
        (75, 8, 8),
        "st_lkt_num.lkt_done_num"
    );
    assert_eq!(
        (
            st_lkt_num::LKT_INT_NUM.word,
            st_lkt_num::LKT_INT_NUM.lsb,
            st_lkt_num::LKT_INT_NUM.width
        ),
        (75, 16, 8),
        "st_lkt_num.lkt_int_num"
    );
    assert_eq!(
        (
            st_lkt_num::LKT_CFG_LOAD_NUM.word,
            st_lkt_num::LKT_CFG_LOAD_NUM.lsb,
            st_lkt_num::LKT_CFG_LOAD_NUM.width
        ),
        (75, 24, 8),
        "st_lkt_num.lkt_cfg_load_num"
    );
    assert_eq!(
        (
            st_lkt_info::LKT_CORE_ID.word,
            st_lkt_info::LKT_CORE_ID.lsb,
            st_lkt_info::LKT_CORE_ID.width
        ),
        (76, 0, 4),
        "st_lkt_info.lkt_core_id"
    );
    assert_eq!(
        (
            st_lkt_info::LKT_STOP_FLAG.word,
            st_lkt_info::LKT_STOP_FLAG.lsb,
            st_lkt_info::LKT_STOP_FLAG.width
        ),
        (76, 8, 1),
        "st_lkt_info.lkt_stop_flag"
    );
    assert_eq!(
        (
            st_lkt_info::LKT_NODE_INT.word,
            st_lkt_info::LKT_NODE_INT.lsb,
            st_lkt_info::LKT_NODE_INT.width
        ),
        (76, 9, 1),
        "st_lkt_info.lkt_node_int"
    );
    assert_eq!(
        (
            st_lkt_info::LKT_TASK_ID.word,
            st_lkt_info::LKT_TASK_ID.lsb,
            st_lkt_info::LKT_TASK_ID.width
        ),
        (76, 10, 12),
        "st_lkt_info.lkt_task_id"
    );
}

#[test]
fn the_file_is_the_header_size() {
    // 43 base words, the five unnamed words, and 31 status words: 79, the block's JPGENC_REG_NUM.
    assert_eq!(172, 43 * 4);
    assert_eq!(124, 31 * 4);
    assert_eq!(192, 0xc0);
}
