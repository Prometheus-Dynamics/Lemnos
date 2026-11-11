//! The layouts against the vendor sources: sizes and offsets as `gcc` gives them on LP64 for the
//! structures transcribed from `mpp_iep2.c`, and the numbers; and `lemnos-driver-rk-mpp`'s
//! facts about the parameter block against them.

use std::mem::{offset_of, size_of};

use lemnos_driver_rk_mpp::{Block, Check};

use crate::regs::{self, ADDRESS_REGISTERS};
use crate::{
    Addr, DilMode, FieldOrder, PARAMS_BYTES, Params, PdComp, RESULTS_BYTES, Results, SLOTS,
    Sampling, Slot, Swap, TUNING_BYTES, TUNING_START,
};

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
fn the_service_layers_facts_are_this_structure() {
    let facts = Block::Iep2.params().expect("a parameter block");
    assert_eq!((facts.bytes, facts.results), (PARAMS_BYTES, RESULTS_BYTES));
    assert_eq!((PARAMS_BYTES, RESULTS_BYTES), (656, 412));
    assert_eq!((TUNING_START, TUNING_BYTES), (116, 368));

    // Every descriptor word is a slot's, in slot order, landing in the kernel's register for
    // that word; the outputs' third words are no slot's.
    assert_eq!(facts.descriptors.len(), SLOTS);
    for (descriptor, slot) in facts.descriptors.iter().zip(Slot::ALL) {
        let word = slot.word();
        assert_eq!(usize::from(descriptor.at), word, "{slot:?}");
        let index = (word - offset_of!(Params, src)) / 4;
        assert_eq!(
            ADDRESS_REGISTERS[index],
            Some(u32::from(descriptor.register)),
            "{slot:?}"
        );
    }

    // The words checked are the ones this crate writes from its own checked numbers, and the
    // two it holds at zero.
    let at = |at: usize| {
        facts
            .words
            .iter()
            .find(|w| usize::from(w.at) == at)
            .map(|w| w.check)
    };
    let range = |min, max| Some(Check::Range { min, max });
    assert_eq!(at(offset_of!(Params, tile_cols)), range(1, 120));
    assert_eq!(at(offset_of!(Params, tile_rows)), range(1, 272));
    for stride in [
        offset_of!(Params, src_y_stride),
        offset_of!(Params, src_uv_stride),
        offset_of!(Params, dst_y_stride),
    ] {
        assert_eq!(at(stride), range(0, 0xffff));
    }
    assert_eq!(at(offset_of!(Params, dst) + 8), range(0, 0));
    assert_eq!(at(offset_of!(Params, dst) + 20), range(0, 0));
    assert_eq!(at(offset_of!(Params, dil_out_mode)), range(0, 0));
    assert_eq!(at(offset_of!(Params, osd_area_num)), range(0, 8));
    assert_eq!(at(offset_of!(Params, roi_en)), range(0, 0));
    let Some(Check::OneOf(modes)) = at(offset_of!(Params, dil_mode)) else {
        panic!("dil_mode is checked against the modes");
    };
    assert_eq!(
        modes,
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
        .map(|m| m as u32)
    );
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
