//! The layouts against the vendor sources: sizes and offsets as `gcc` gives them on LP64 for the
//! structures transcribed from `mpp_iep2.c`, `mpp_common.c` and `rk-mpp.h`, and the numbers.

use std::mem::{offset_of, size_of};

use crate::mpp::{self, Msg, OffsetElem};
use crate::regs::{self, ADDRESS_REGISTERS};
use crate::{Addr, DilMode, FieldOrder, Params, PdComp, Results, Sampling, Swap};

#[test]
fn params_match_the_c_layout() {
    assert_eq!(size_of::<Addr>(), 12);
    assert_eq!(size_of::<Params>(), 656);
    assert_eq!(offset_of!(Params, tile_cols), 16);
    assert_eq!(offset_of!(Params, src_y_stride), 24);
    assert_eq!(offset_of!(Params, dst_y_stride), 32);
    assert_eq!(offset_of!(Params, src), 36);
    assert_eq!(offset_of!(Params, dst), 72);
    assert_eq!(offset_of!(Params, mv_addr), 96);
    assert_eq!(offset_of!(Params, md_addr), 100);
    assert_eq!(offset_of!(Params, dil_mode), 104);
    assert_eq!(offset_of!(Params, md_theta), 116);
    assert_eq!(offset_of!(Params, dect_resi_thr), 128);
    assert_eq!(offset_of!(Params, osd_area_num), 132);
    assert_eq!(offset_of!(Params, osd_limit_area), 152);
    assert_eq!(offset_of!(Params, osd_line_num), 160);
    assert_eq!(offset_of!(Params, osd_x_sta), 168);
    assert_eq!(offset_of!(Params, osd_y_end), 264);
    assert_eq!(offset_of!(Params, me_pena), 296);
    assert_eq!(offset_of!(Params, me_thr_offset), 312);
    assert_eq!(offset_of!(Params, mv_left_limit), 316);
    assert_eq!(offset_of!(Params, mv_tru_list), 324);
    assert_eq!(offset_of!(Params, mv_tru_vld), 332);
    assert_eq!(offset_of!(Params, eedi_thr0), 364);
    assert_eq!(offset_of!(Params, ble_backtoma_num), 368);
    assert_eq!(offset_of!(Params, comb_cnt_thr), 372);
    assert_eq!(offset_of!(Params, comb_osd_vld), 384);
    assert_eq!(offset_of!(Params, mtn_en), 416);
    assert_eq!(offset_of!(Params, mtn_tab), 420);
    assert_eq!(offset_of!(Params, pd_mode), 484);
    assert_eq!(offset_of!(Params, roi_en), 488);
    assert_eq!(offset_of!(Params, roi_mode), 496);
    assert_eq!(offset_of!(Params, yend), 624);
}

#[test]
fn results_match_the_c_layout() {
    assert_eq!(size_of::<Results>(), 412);
    assert_eq!(offset_of!(Results, dect_pd_tcnt), 224);
    assert_eq!(offset_of!(Results, dect_osd_cnt), 264);
    assert_eq!(offset_of!(Results, ff_gradt_bcnt), 280);
    assert_eq!(offset_of!(Results, x_sta), 284);
    assert_eq!(offset_of!(Results, y_end), 380);
}

#[test]
fn service_layouts_and_numbers() {
    assert_eq!(size_of::<Msg>(), 24);
    assert_eq!(mpp::MSG_BYTES, 24);
    assert_eq!(offset_of!(Msg, flags), 4);
    assert_eq!(offset_of!(Msg, size), 8);
    assert_eq!(offset_of!(Msg, offset), 12);
    assert_eq!(offset_of!(Msg, data_ptr), 16);
    assert_eq!(size_of::<OffsetElem>(), 8);
    assert_eq!(offset_of!(OffsetElem, offset), 4);
    // _IOW('v', 1, unsigned int).
    assert_eq!(mpp::IOC_CFG_V1, 0x4004_7601);
    assert_eq!(mpp::CMD_QUERY_HW_SUPPORT, 0);
    assert_eq!(mpp::CMD_INIT_CLIENT_TYPE, 0x100);
    assert_eq!(mpp::CMD_INIT_TRANS_TABLE, 0x102);
    assert_eq!(mpp::CMD_SET_REG_WRITE, 0x200);
    assert_eq!(mpp::CMD_SET_REG_READ, 0x201);
    assert_eq!(mpp::CMD_SET_REG_ADDR_OFFSET, 0x202);
    assert_eq!(mpp::CMD_POLL_HW_FINISH, 0x300);
    assert_eq!(mpp::CMD_RESET_SESSION, 0x400);
    assert_eq!(mpp::CMD_RELEASE_FD, 0x402);
    assert_eq!(
        [
            mpp::FLAG_MULTI_MSG,
            mpp::FLAG_LAST_MSG,
            mpp::FLAG_REG_FD_NO_TRANS,
            mpp::FLAG_SCL_FD_NO_TRANS,
            mpp::FLAG_REG_NO_OFFSET,
            mpp::FLAG_SECURE_MODE,
        ],
        [1, 2, 4, 8, 0x10, 0x1_0000]
    );
    assert_eq!(mpp::DEVICE_IEP2, 28);
    assert_eq!(mpp::MAX_MSGS, 16);
    assert_eq!(mpp::MAX_OFFSETS, 80);
}

#[test]
fn enum_values() {
    assert_eq!([Sampling::Yuv422 as u32, Sampling::Yuv420 as u32], [2, 3]);
    assert_eq!(
        [
            Swap::SemiUv as u32,
            Swap::SemiVu as u32,
            Swap::Planar as u32
        ],
        [0, 1, 3]
    );
    assert_eq!(
        [
            DilMode::Disable,
            DilMode::I5O2,
            DilMode::I5O1T,
            DilMode::I5O1B,
            DilMode::I2O2,
            DilMode::I1O1B,
            DilMode::Pd,
            DilMode::Bypass,
            DilMode::Dect,
        ]
        .map(|m| m as u32),
        [0, 1, 2, 3, 4, 6, 7, 8, 9]
    );
    assert_eq!(
        [FieldOrder::TopFirst as u32, FieldOrder::BottomFirst as u32],
        [0, 1]
    );
    assert_eq!(
        [PdComp::CurCur, PdComp::CurNext, PdComp::NextCur].map(|p| p as u32),
        [0, 1, 2]
    );
}

#[test]
fn address_words_land_in_the_kernel_table() {
    // iep2_addr_rnum: 24, 27, 28, 25, 29, 30, 26, 31, 32, 44, 46, -1, 45, 47, -1, 34, 33.
    assert_eq!(
        ADDRESS_REGISTERS,
        [
            Some(24),
            Some(27),
            Some(28),
            Some(25),
            Some(29),
            Some(30),
            Some(26),
            Some(31),
            Some(32),
            Some(44),
            Some(46),
            None,
            Some(45),
            Some(47),
            None,
            Some(34),
            Some(33),
        ]
    );
    // The address words run from src[0].y to md_addr with nothing between them.
    assert_eq!(
        offset_of!(Params, md_addr) - offset_of!(Params, src),
        4 * (regs::ADDRESS_WORDS - 1)
    );
}

#[test]
fn register_offsets() {
    assert_eq!(regs::IEP_CONFIG0, 0x04);
    assert_eq!(regs::VIR_SRC_IMG_WIDTH, 0x30);
    assert_eq!(regs::VIR_DST_IMG_WIDTH, 0x34);
    assert_eq!(regs::SRC_IMG_SIZE, 0x38);
    assert_eq!(regs::DIL_CONFIG0, 0x40);
    assert_eq!(regs::SRC_ADDR_MD, 0x84);
    assert_eq!(regs::ROI_ADDR, 0x8c);
    assert_eq!(regs::DST_ADDR_MV, 0xc4);
    assert_eq!(regs::OSD_AREA_CONF, 0x100);
    assert_eq!(regs::DIL_MTN_TAB, 0x140);
    assert_eq!(regs::RO_PD_TCNT, 0x400);
    assert_eq!(regs::RO_MV_HIST_BIN, 0x480);
    assert_eq!(
        [
            regs::INT_FRM_DONE,
            regs::INT_OSD_MAX,
            regs::INT_BUS_ERROR,
            regs::INT_TIMEOUT
        ],
        [1, 2, 16, 32]
    );
    // The largest image fits the 11-bit size fields, the largest stride the 16-bit ones.
    const { assert!(crate::MAX_WIDTH - 1 < 1 << regs::SIZE_FIELD_BITS) };
    const { assert!(crate::MAX_HEIGHT - 1 < 1 << regs::SIZE_FIELD_BITS) };
    assert_eq!(crate::MAX_STRIDE / crate::STRIDE_ALIGN, 0xffff);
    assert_eq!(crate::OSD_AREAS, 1 << (regs::OSD_AREA_NUM_FIELD_BITS - 1));
}
