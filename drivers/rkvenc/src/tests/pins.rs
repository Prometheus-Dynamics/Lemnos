//! Layout pins: every register's word and size, and every named field's word, bit, width,
//! sign and writability, against the header values the layout modules were generated from.
//! Generated from the header by a throwaway extractor; a change to a layout must change its
//! pin, on purpose.

use super::super::layout::{AddressReg, Field, Register};
use super::super::vepu541_h264::{l2, ret, set};

fn check_reg(r: Register, word: u16, words: u16) {
    assert_eq!((r.word, r.words), (word, words), "{}", r.name);
}

fn check_field(
    f: Field,
    word: u16,
    lsb: u8,
    width: u8,
    signed: bool,
    writable: bool,
    values: &[u32],
) {
    assert_eq!(
        (f.word, f.lsb, f.width, f.signed, f.writable, f.values),
        (word, lsb, width, signed, writable, values)
    );
}

fn check_addr(a: AddressReg, word: u16, align: u32) {
    assert_eq!((a.word, a.align), (word, align));
}

#[test]
fn set_pins() {
    check_reg(set::reg000::REGISTER, 0, 1);
    check_field(set::reg000::SUB_VER, 0, 0, 8, false, false, &[]);
    check_field(set::reg000::H264_ENC, 0, 8, 1, false, false, &[]);
    check_field(set::reg000::H265_ENC, 0, 9, 1, false, false, &[]);
    check_field(set::reg000::PIC_SIZE, 0, 12, 4, false, false, &[0, 1]);
    check_field(set::reg000::OSD_CAP, 0, 16, 2, false, false, &[0, 3]);
    check_field(set::reg000::FILTR_CAP, 0, 18, 2, false, false, &[0, 3]);
    check_field(set::reg000::BFRM_CAP, 0, 20, 1, false, false, &[]);
    check_field(set::reg000::FBC_CAP, 0, 21, 1, false, false, &[]);
    check_field(set::reg000::RKVENC_VER, 0, 24, 8, false, false, &[]);
    check_reg(set::reg001::REGISTER, 1, 1);
    check_field(set::reg001::LKT_NUM, 1, 0, 8, false, true, &[]);
    check_field(set::reg001::RKVENC_CMD, 1, 8, 2, false, true, &[]);
    check_field(set::reg001::CLK_GATE_EN, 1, 16, 1, false, true, &[]);
    check_field(set::reg001::RESETN_HW_EN, 1, 17, 1, false, true, &[]);
    check_field(set::reg001::ENC_DONE_TMVP_EN, 1, 18, 1, false, true, &[]);
    check_reg(set::reg002::REGISTER, 2, 1);
    check_field(set::reg002::SAFE_CLR, 2, 0, 1, false, true, &[]);
    check_field(set::reg002::FORCE_CLR, 2, 1, 1, false, true, &[]);
    check_reg(set::reg003::REGISTER, 3, 1);
    check_addr(set::reg003::LKT_ADDR, 3, 16);
    check_reg(set::reg004::REGISTER, 4, 1);
    check_field(set::reg004::ENC_DONE_EN, 4, 0, 1, false, true, &[]);
    check_field(set::reg004::LKT_DONE_EN, 4, 1, 1, false, true, &[]);
    check_field(set::reg004::SCLR_DONE_EN, 4, 2, 1, false, true, &[]);
    check_field(set::reg004::ENC_SLICE_DONE_EN, 4, 3, 1, false, true, &[]);
    check_field(set::reg004::OFLW_DONE_EN, 4, 4, 1, false, true, &[]);
    check_field(set::reg004::BRSP_DONE_EN, 4, 5, 1, false, true, &[]);
    check_field(set::reg004::BERR_DONE_EN, 4, 6, 1, false, true, &[]);
    check_field(set::reg004::RERR_DONE_EN, 4, 7, 1, false, true, &[]);
    check_field(set::reg004::WDG_DONE_EN, 4, 8, 1, false, true, &[]);
    check_reg(set::reg005::REGISTER, 5, 1);
    check_field(set::reg005::ENC_DONE_MSK, 5, 0, 1, false, true, &[]);
    check_field(set::reg005::LKT_DONE_MSK, 5, 1, 1, false, true, &[]);
    check_field(set::reg005::SCLR_DONE_MSK, 5, 2, 1, false, true, &[]);
    check_field(set::reg005::ENC_SLICE_DONE_MSK, 5, 3, 1, false, true, &[]);
    check_field(set::reg005::OFLW_DONE_MSK, 5, 4, 1, false, true, &[]);
    check_field(set::reg005::BRSP_DONE_MSK, 5, 5, 1, false, true, &[]);
    check_field(set::reg005::BERR_DONE_MSK, 5, 6, 1, false, true, &[]);
    check_field(set::reg005::RERR_DONE_MSK, 5, 7, 1, false, true, &[]);
    check_field(set::reg005::WDG_DONE_MSK, 5, 8, 1, false, true, &[]);
    check_reg(set::reg006::REGISTER, 6, 1);
    check_field(set::reg006::ENC_DONE_CLR, 6, 0, 1, false, true, &[]);
    check_field(set::reg006::LKT_DONE_CLR, 6, 1, 1, false, true, &[]);
    check_field(set::reg006::SCLR_DONE_CLR, 6, 2, 1, false, true, &[]);
    check_field(set::reg006::ENC_SLICE_DONE_CLR, 6, 3, 1, false, true, &[]);
    check_field(set::reg006::OFLW_DONE_CLR, 6, 4, 1, false, true, &[]);
    check_field(set::reg006::BRSP_DONE_CLR, 6, 5, 1, false, true, &[]);
    check_field(set::reg006::BERR_DONE_CLR, 6, 6, 1, false, true, &[]);
    check_field(set::reg006::RERR_DONE_CLR, 6, 7, 1, false, true, &[]);
    check_field(set::reg006::WDG_DONE_CLR, 6, 8, 1, false, true, &[]);
    check_reg(set::reg007::REGISTER, 7, 1);
    check_field(set::reg007::ENC_DONE_STA, 7, 0, 1, false, true, &[]);
    check_field(set::reg007::LKT_DONE_STA, 7, 1, 1, false, true, &[]);
    check_field(set::reg007::SCLR_DONE_STA, 7, 2, 1, false, true, &[]);
    check_field(set::reg007::ENC_SLICE_DONE_STA, 7, 3, 1, false, true, &[]);
    check_field(set::reg007::OFLW_DONE_STA, 7, 4, 1, false, true, &[]);
    check_field(set::reg007::BRSP_DONE_STA, 7, 5, 1, false, true, &[]);
    check_field(set::reg007::BERR_DONE_STA, 7, 6, 1, false, true, &[]);
    check_field(set::reg007::RERR_DONE_STA, 7, 7, 1, false, true, &[]);
    check_field(set::reg007::WDG_DONE_STA, 7, 8, 1, false, true, &[]);
    check_reg(set::reg_008_011::REGISTER, 8, 4);
    check_field(
        set::reg_008_011::REG_008_011[0],
        8,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_008_011::REG_008_011[1],
        9,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_008_011::REG_008_011[2],
        10,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_008_011::REG_008_011[3],
        11,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(set::reg012::REGISTER, 12, 1);
    check_field(set::reg012::PIC_WD8_M1, 12, 0, 9, false, true, &[]);
    check_field(set::reg012::PIC_WFILL, 12, 10, 6, false, true, &[]);
    check_field(set::reg012::PIC_HD8_M1, 12, 16, 9, false, true, &[]);
    check_field(set::reg012::PIC_HFILL, 12, 26, 6, false, true, &[]);
    check_reg(set::reg013::REGISTER, 13, 1);
    check_field(set::reg013::ENC_STND, 13, 0, 1, false, true, &[]);
    check_field(set::reg013::ROI_ENC, 13, 1, 1, false, true, &[]);
    check_field(set::reg013::CUR_FRM_REF, 13, 2, 1, false, true, &[]);
    check_field(set::reg013::MEI_STOR, 13, 3, 1, false, true, &[]);
    check_field(set::reg013::BS_SCP, 13, 4, 1, false, true, &[]);
    check_field(set::reg013::LAMB_MOD_SEL, 13, 5, 1, false, true, &[]);
    check_field(set::reg013::PIC_QP, 13, 8, 6, false, true, &[]);
    check_field(set::reg013::TOT_POC_NUM, 13, 14, 5, false, true, &[]);
    check_field(set::reg013::LOG2_CTU_NUM, 13, 19, 4, false, true, &[]);
    check_field(set::reg013::ATR_THD_SEL, 13, 23, 1, false, true, &[]);
    check_field(set::reg013::DCHS_RXID, 13, 24, 2, false, true, &[]);
    check_field(set::reg013::DCHS_TXID, 13, 26, 2, false, true, &[]);
    check_field(set::reg013::DCHS_RXE, 13, 28, 1, false, true, &[]);
    check_field(set::reg013::SATD_BYPS_EN, 13, 29, 1, false, true, &[]);
    check_field(set::reg013::SLEN_FIFO, 13, 30, 1, false, true, &[]);
    check_field(set::reg013::NODE_INT, 13, 31, 1, false, true, &[]);
    check_reg(set::reg014::REGISTER, 14, 1);
    check_field(set::reg014::VS_LOAD_THD, 14, 0, 24, false, true, &[]);
    check_field(set::reg014::RFP_LOAD_THRD, 14, 24, 8, false, true, &[]);
    check_reg(set::reg015::REGISTER, 15, 1);
    check_field(set::reg015::LPFW_BUS_ORDR, 15, 0, 1, false, true, &[]);
    check_field(set::reg015::CMVW_BUS_ORDR, 15, 1, 1, false, true, &[]);
    check_field(set::reg015::DSPW_BUS_ORDR, 15, 2, 1, false, true, &[]);
    check_field(set::reg015::RFPW_BUS_ORDR, 15, 3, 1, false, true, &[]);
    check_field(set::reg015::SRC_BUS_EDIN, 15, 4, 4, false, true, &[]);
    check_field(set::reg015::MEIW_BUS_EDIN, 15, 8, 4, false, true, &[]);
    check_field(set::reg015::BSW_BUS_EDIN, 15, 12, 3, false, true, &[]);
    check_field(set::reg015::LKTR_BUS_EDIN, 15, 15, 4, false, true, &[]);
    check_field(set::reg015::ROIR_BUS_EDIN, 15, 19, 4, false, true, &[]);
    check_field(set::reg015::LKTW_BUS_EDIN, 15, 23, 4, false, true, &[]);
    check_field(set::reg015::AFBC_BSIZE, 15, 27, 1, false, true, &[]);
    check_reg(set::reg016::REGISTER, 16, 1);
    check_field(
        set::reg016::VEPU541_AXI_BRSP_CKE,
        16,
        0,
        7,
        false,
        true,
        &[],
    );
    check_field(set::reg016::VEPU541_DSPR_OTSD, 16, 7, 1, false, true, &[]);
    check_field(set::reg016::VEPU540_DSPR_OTSD, 16, 7, 1, false, true, &[]);
    check_field(
        set::reg016::VEPU540_AXI_BRSP_CKE,
        16,
        16,
        8,
        false,
        true,
        &[],
    );
    check_reg(set::reg017::REGISTER, 17, 1);
    check_field(set::reg017::ALPHA_SWAP, 17, 0, 1, false, true, &[]);
    check_field(set::reg017::RBUV_SWAP, 17, 1, 1, false, true, &[]);
    check_field(
        set::reg017::SRC_CFMT,
        17,
        2,
        4,
        false,
        true,
        &[0, 1, 2, 4, 5, 6, 7, 8, 9],
    );
    check_field(set::reg017::SRC_RANGE, 17, 6, 1, false, true, &[]);
    check_field(set::reg017::OUT_FMT_CFG, 17, 7, 1, false, true, &[]);
    check_reg(set::reg018::REGISTER, 18, 1);
    check_field(set::reg018::CSC_WGT_B2Y, 18, 0, 9, false, true, &[]);
    check_field(set::reg018::CSC_WGT_G2Y, 18, 9, 9, false, true, &[]);
    check_field(set::reg018::CSC_WGT_R2Y, 18, 18, 9, false, true, &[]);
    check_reg(set::reg019::REGISTER, 19, 1);
    check_field(set::reg019::CSC_WGT_B2U, 19, 0, 9, false, true, &[]);
    check_field(set::reg019::CSC_WGT_G2U, 19, 9, 9, false, true, &[]);
    check_field(set::reg019::CSC_WGT_R2U, 19, 18, 9, false, true, &[]);
    check_reg(set::reg020::REGISTER, 20, 1);
    check_field(set::reg020::CSC_WGT_B2V, 20, 0, 9, false, true, &[]);
    check_field(set::reg020::CSC_WGT_G2V, 20, 9, 9, false, true, &[]);
    check_field(set::reg020::CSC_WGT_R2V, 20, 18, 9, false, true, &[]);
    check_reg(set::reg021::REGISTER, 21, 1);
    check_field(set::reg021::CSC_OFST_V, 21, 0, 8, false, true, &[]);
    check_field(set::reg021::CSC_OFST_U, 21, 8, 8, false, true, &[]);
    check_field(set::reg021::CSC_OFST_Y, 21, 16, 5, false, true, &[]);
    check_reg(set::reg022::REGISTER, 22, 1);
    check_field(set::reg022::SRC_MIRR, 22, 26, 1, false, true, &[]);
    check_field(set::reg022::SRC_ROT, 22, 27, 2, false, true, &[]);
    check_field(set::reg022::TXA_EN, 22, 29, 1, false, true, &[]);
    check_field(set::reg022::AFBCD_EN, 22, 30, 1, false, true, &[]);
    check_reg(set::reg023::REGISTER, 23, 1);
    check_field(set::reg023::SLI_CRS_EN, 23, 31, 1, false, true, &[]);
    check_reg(set::reg_024::REGISTER, 24, 1);
    check_field(set::reg_024::REG_024, 24, 0, 32, false, false, &[]);
    check_reg(set::reg025::REGISTER, 25, 1);
    check_field(set::reg025::CHRM_KLUT_OFST, 25, 0, 3, false, true, &[]);
    check_reg(set::reg026::REGISTER, 26, 1);
    check_field(set::reg026::CHRM_KLUT_WGT0, 26, 0, 18, false, true, &[]);
    check_field(set::reg026::CHRM_KLUT_WGT1_L9, 26, 23, 9, false, true, &[]);
    check_reg(set::reg027::REGISTER, 27, 1);
    check_field(set::reg027::CHRM_KLUT_WGT1_H9, 27, 0, 9, false, true, &[]);
    check_field(set::reg027::CHRM_KLUT_WGT2, 27, 14, 18, false, true, &[]);
    check_reg(set::reg028::REGISTER, 28, 1);
    check_field(set::reg028::CHRM_KLUT_WGT3, 28, 0, 18, false, true, &[]);
    check_field(set::reg028::CHRM_KLUT_WGT4_L9, 28, 23, 9, false, true, &[]);
    check_reg(set::reg029::REGISTER, 29, 1);
    check_field(set::reg029::CHRM_KLUT_WGT4_H9, 29, 0, 9, false, true, &[]);
    check_field(set::reg029::CHRM_KLUT_WGT5, 29, 14, 18, false, true, &[]);
    check_reg(set::reg030::REGISTER, 30, 1);
    check_field(set::reg030::CHRM_KLUT_WGT6, 30, 0, 18, false, true, &[]);
    check_field(set::reg030::CHRM_KLUT_WGT7_L9, 30, 23, 9, false, true, &[]);
    check_reg(set::reg031::REGISTER, 31, 1);
    check_field(set::reg031::CHRM_KLUT_WGT7_H9, 31, 0, 9, false, true, &[]);
    check_field(set::reg031::CHRM_KLUT_WGT8, 31, 14, 18, false, true, &[]);
    check_reg(set::reg032::REGISTER, 32, 1);
    check_field(set::reg032::CHRM_KLUT_WGT9, 32, 0, 18, false, true, &[]);
    check_field(set::reg032::CHRM_KLUT_WGT10_L9, 32, 23, 9, false, true, &[]);
    check_reg(set::reg033::REGISTER, 33, 1);
    check_field(set::reg033::CHRM_KLUT_WGT10_H9, 33, 0, 9, false, true, &[]);
    check_field(set::reg033::CHRM_KLUT_WGT11, 33, 14, 18, false, true, &[]);
    check_reg(set::reg034::REGISTER, 34, 1);
    check_field(set::reg034::CHRM_KLUT_WGT12, 34, 0, 18, false, true, &[]);
    check_field(set::reg034::CHRM_KLUT_WGT13_L9, 34, 23, 9, false, true, &[]);
    check_reg(set::reg035::REGISTER, 35, 1);
    check_field(set::reg035::CHRM_KLUT_WGT13_H9, 35, 0, 9, false, true, &[]);
    check_field(set::reg035::CHRM_KLUT_WGT14, 35, 14, 18, false, true, &[]);
    check_reg(set::reg036::REGISTER, 36, 1);
    check_field(set::reg036::CHRM_KLUT_WGT15, 36, 0, 18, false, true, &[]);
    check_field(set::reg036::CHRM_KLUT_WGT16_L9, 36, 23, 9, false, true, &[]);
    check_reg(set::reg037::REGISTER, 37, 1);
    check_field(set::reg037::CHRM_KLUT_WGT16_H9, 37, 0, 9, false, true, &[]);
    check_field(set::reg037::CHRM_KLUT_WGT17, 37, 14, 18, false, true, &[]);
    check_reg(set::reg038::REGISTER, 38, 1);
    check_field(set::reg038::CHRM_KLUT_WGT18, 38, 0, 18, false, true, &[]);
    check_field(set::reg038::CHRM_KLUT_WGT19_L9, 38, 23, 9, false, true, &[]);
    check_reg(set::reg039::REGISTER, 39, 1);
    check_field(set::reg039::CHRM_KLUT_WGT19_H9, 39, 0, 9, false, true, &[]);
    check_field(set::reg039::CHRM_KLUT_WGT20, 39, 14, 18, false, true, &[]);
    check_reg(set::reg040::REGISTER, 40, 1);
    check_field(set::reg040::CHRM_KLUT_WGT21, 40, 0, 18, false, true, &[]);
    check_field(set::reg040::CHRM_KLUT_WGT22_L9, 40, 23, 9, false, true, &[]);
    check_reg(set::reg041::REGISTER, 41, 1);
    check_field(set::reg041::CHRM_KLUT_WGT22_H9, 41, 0, 9, false, true, &[]);
    check_field(set::reg041::CHRM_KLUT_WGT23, 41, 14, 18, false, true, &[]);
    check_reg(set::reg042::REGISTER, 42, 1);
    check_field(set::reg042::CHRM_KLUT_WGT24, 42, 0, 18, false, true, &[]);
    check_field(set::reg042::CHRM_KLUT_WGT25_L9, 42, 23, 9, false, true, &[]);
    check_reg(set::reg043::REGISTER, 43, 1);
    check_field(set::reg043::CHRM_KLUT_WGT25_H9, 43, 0, 9, false, true, &[]);
    check_field(set::reg043::CHRM_KLUT_WGT26, 43, 14, 18, false, true, &[]);
    check_reg(set::reg044::REGISTER, 44, 1);
    check_field(set::reg044::CHRM_KLUT_WGT27, 44, 0, 18, false, true, &[]);
    check_field(set::reg044::CHRM_KLUT_WGT28_L9, 44, 23, 9, false, true, &[]);
    check_reg(set::reg045::REGISTER, 45, 1);
    check_field(set::reg045::CHRM_KLUT_WGT28_H9, 45, 0, 9, false, true, &[]);
    check_field(set::reg045::CHRM_KLUT_WGT29, 45, 14, 18, false, true, &[]);
    check_reg(set::reg046::REGISTER, 46, 1);
    check_field(set::reg046::CHRM_KLUT_WGT30, 46, 0, 18, false, true, &[]);
    check_field(set::reg046::CHRM_KLUT_WGT31_L9, 46, 23, 9, false, true, &[]);
    check_reg(set::reg047::REGISTER, 47, 1);
    check_field(set::reg047::CHRM_KLUT_WGT31_H9, 47, 0, 9, false, true, &[]);
    check_field(set::reg047::CHRM_KLUT_WGT32, 47, 14, 18, false, true, &[]);
    check_reg(set::reg048::REGISTER, 48, 1);
    check_field(set::reg048::CHRM_KLUT_WGT33, 48, 0, 18, false, true, &[]);
    check_field(set::reg048::CHRM_KLUT_WGT34_L9, 48, 23, 9, false, true, &[]);
    check_reg(set::reg049::REGISTER, 49, 1);
    check_field(set::reg049::CHRM_KLUT_WGT34_H9, 49, 0, 9, false, true, &[]);
    check_reg(set::reg050::REGISTER, 50, 1);
    check_field(set::reg050::RC_EN, 50, 0, 1, false, true, &[]);
    check_field(set::reg050::AQ_EN, 50, 1, 1, false, true, &[]);
    check_field(set::reg050::AQ_MODE, 50, 2, 1, false, true, &[]);
    check_field(set::reg050::RC_CTU_NUM, 50, 16, 16, false, true, &[]);
    check_reg(set::reg051::REGISTER, 51, 1);
    check_field(set::reg051::RC_QP_RANGE, 51, 16, 4, true, true, &[]);
    check_field(set::reg051::RC_MAX_QP, 51, 20, 6, false, true, &[]);
    check_field(set::reg051::RC_MIN_QP, 51, 26, 6, false, true, &[]);
    check_reg(set::reg052::REGISTER, 52, 1);
    check_field(set::reg052::CTU_EBIT, 52, 0, 20, false, true, &[]);
    check_reg(set::reg053::REGISTER, 53, 1);
    check_field(set::reg053::QP_ADJ0, 53, 0, 5, true, true, &[]);
    check_field(set::reg053::QP_ADJ1, 53, 5, 5, true, true, &[]);
    check_field(set::reg053::QP_ADJ2, 53, 10, 5, true, true, &[]);
    check_field(set::reg053::QP_ADJ3, 53, 15, 5, true, true, &[]);
    check_field(set::reg053::QP_ADJ4, 53, 20, 5, true, true, &[]);
    check_reg(set::reg054::REGISTER, 54, 1);
    check_field(set::reg054::QP_ADJ5, 54, 0, 5, true, true, &[]);
    check_field(set::reg054::QP_ADJ6, 54, 5, 5, true, true, &[]);
    check_field(set::reg054::QP_ADJ7, 54, 10, 5, true, true, &[]);
    check_field(set::reg054::QP_ADJ8, 54, 15, 5, true, true, &[]);
    check_reg(set::reg055_063::REGISTER, 55, 9);
    check_field(set::reg055_063::RC_DTHD[0], 55, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[1], 56, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[2], 57, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[3], 58, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[4], 59, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[5], 60, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[6], 61, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[7], 62, 0, 32, false, true, &[]);
    check_field(set::reg055_063::RC_DTHD[8], 63, 0, 32, false, true, &[]);
    check_reg(set::reg064::REGISTER, 64, 1);
    check_field(set::reg064::QPMIN_AREA0, 64, 0, 6, false, true, &[]);
    check_field(set::reg064::QPMAX_AREA0, 64, 6, 6, false, true, &[]);
    check_field(set::reg064::QPMIN_AREA1, 64, 12, 6, false, true, &[]);
    check_field(set::reg064::QPMAX_AREA1, 64, 18, 6, false, true, &[]);
    check_field(set::reg064::QPMIN_AREA2, 64, 24, 6, false, true, &[]);
    check_reg(set::reg065::REGISTER, 65, 1);
    check_field(set::reg065::QPMAX_AREA2, 65, 0, 6, false, true, &[]);
    check_field(set::reg065::QPMIN_AREA3, 65, 6, 6, false, true, &[]);
    check_field(set::reg065::QPMAX_AREA3, 65, 12, 6, false, true, &[]);
    check_field(set::reg065::QPMIN_AREA4, 65, 18, 6, false, true, &[]);
    check_field(set::reg065::QPMAX_AREA4, 65, 24, 6, false, true, &[]);
    check_reg(set::reg066::REGISTER, 66, 1);
    check_field(set::reg066::QPMIN_AREA5, 66, 0, 6, false, true, &[]);
    check_field(set::reg066::QPMAX_AREA5, 66, 6, 6, false, true, &[]);
    check_field(set::reg066::QPMIN_AREA6, 66, 12, 6, false, true, &[]);
    check_field(set::reg066::QPMAX_AREA6, 66, 18, 6, false, true, &[]);
    check_field(set::reg066::QPMIN_AREA7, 66, 24, 6, false, true, &[]);
    check_reg(set::reg067::REGISTER, 67, 1);
    check_field(set::reg067::QPMAX_AREA7, 67, 0, 6, false, true, &[]);
    check_field(set::reg067::QPMAP_MODE, 67, 30, 2, false, true, &[]);
    check_reg(set::reg068::REGISTER, 68, 1);
    check_field(set::reg068::PIC_OFST_Y, 68, 0, 13, false, true, &[]);
    check_field(set::reg068::PIC_OFST_X, 68, 16, 13, false, true, &[]);
    check_reg(set::reg069::REGISTER, 69, 1);
    check_field(set::reg069::SRC_STRD0, 69, 0, 16, false, true, &[]);
    check_field(set::reg069::SRC_STRD1, 69, 16, 16, false, true, &[]);
    check_reg(set::reg070::REGISTER, 70, 1);
    check_addr(set::reg070::ADR_SRC0, 70, 1);
    check_reg(set::reg071::REGISTER, 71, 1);
    check_addr(set::reg071::ADR_SRC1, 71, 1);
    check_reg(set::reg072::REGISTER, 72, 1);
    check_addr(set::reg072::ADR_SRC2, 72, 1);
    check_reg(set::reg073::REGISTER, 73, 1);
    check_addr(set::reg073::ROI_ADDR, 73, 16);
    check_reg(set::reg074::REGISTER, 74, 1);
    check_addr(set::reg074::RFPW_H_ADDR, 74, 4096);
    check_reg(set::reg075::REGISTER, 75, 1);
    check_addr(set::reg075::RFPW_B_ADDR, 75, 4096);
    check_reg(set::reg076::REGISTER, 76, 1);
    check_addr(set::reg076::RFPR_H_ADDR, 76, 4096);
    check_reg(set::reg077::REGISTER, 77, 1);
    check_addr(set::reg077::RFPR_B_ADDR, 77, 4096);
    check_reg(set::reg078::REGISTER, 78, 1);
    check_addr(set::reg078::CMVW_ADDR, 78, 1024);
    check_reg(set::reg079::REGISTER, 79, 1);
    check_addr(set::reg079::CMVR_ADDR, 79, 1024);
    check_reg(set::reg080::REGISTER, 80, 1);
    check_addr(set::reg080::DSPW_ADDR, 80, 1024);
    check_reg(set::reg081::REGISTER, 81, 1);
    check_addr(set::reg081::DSPR_ADDR, 81, 1024);
    check_reg(set::reg082::REGISTER, 82, 1);
    check_addr(set::reg082::MEIW_ADDR, 82, 1024);
    check_reg(set::reg083::REGISTER, 83, 1);
    check_addr(set::reg083::BSBT_ADDR, 83, 128);
    check_reg(set::reg084::REGISTER, 84, 1);
    check_addr(set::reg084::BSBB_ADDR, 84, 128);
    check_reg(set::reg085::REGISTER, 85, 1);
    check_addr(set::reg085::BSBR_ADDR, 85, 128);
    check_reg(set::reg086::REGISTER, 86, 1);
    check_addr(set::reg086::ADR_BSBS, 86, 1);
    check_reg(set::reg087::REGISTER, 87, 1);
    check_field(set::reg087::SLI_SPLT, 87, 0, 1, false, true, &[]);
    check_field(set::reg087::SLI_SPLT_MODE, 87, 1, 1, false, true, &[]);
    check_field(set::reg087::SLI_SPLT_CPST, 87, 2, 1, false, true, &[]);
    check_field(set::reg087::SLI_MAX_NUM_M1, 87, 3, 10, false, true, &[]);
    check_field(set::reg087::SLI_FLSH, 87, 13, 1, false, true, &[]);
    check_field(set::reg087::SLI_SPLT_CNUM_M1, 87, 16, 16, false, true, &[]);
    check_reg(set::reg088::REGISTER, 88, 1);
    check_field(set::reg088::SLI_SPLT_BYTE, 88, 0, 18, false, true, &[]);
    check_reg(set::reg089::REGISTER, 89, 1);
    check_field(set::reg089::CME_SRCH_H, 89, 0, 4, false, true, &[]);
    check_field(set::reg089::CME_SRCH_V, 89, 4, 4, false, true, &[]);
    check_field(set::reg089::RME_SRCH_H, 89, 8, 3, false, true, &[]);
    check_field(set::reg089::RME_SRCH_V, 89, 11, 3, false, true, &[]);
    check_field(set::reg089::DLT_FRM_NUM, 89, 16, 16, false, true, &[]);
    check_reg(set::reg090::REGISTER, 90, 1);
    check_field(set::reg090::PMV_MDST_H, 90, 0, 8, false, true, &[]);
    check_field(set::reg090::PMV_MDST_V, 90, 8, 8, false, true, &[]);
    check_field(set::reg090::MV_LIMIT, 90, 16, 2, false, true, &[]);
    check_field(set::reg090::PMV_NUM, 90, 18, 2, false, true, &[]);
    check_field(set::reg090::COLMV_STOR, 90, 20, 1, false, true, &[]);
    check_field(set::reg090::COLMV_LOAD, 90, 21, 1, false, true, &[]);
    check_field(set::reg090::RME_DIS, 90, 22, 5, false, true, &[]);
    check_field(set::reg090::FME_DIS, 90, 27, 5, false, true, &[]);
    check_reg(set::reg091::REGISTER, 91, 1);
    check_field(set::reg091::CME_RAMA_MAX, 91, 0, 11, false, true, &[]);
    check_field(set::reg091::CME_RAMA_H, 91, 11, 5, false, true, &[]);
    check_field(set::reg091::CACH_L2_MAP, 91, 16, 2, false, true, &[]);
    check_field(set::reg091::CME_LINEBUF_W, 91, 18, 8, false, true, &[]);
    check_reg(set::reg092::REGISTER, 92, 1);
    check_field(set::reg092::POC_LSB_LT1, 92, 0, 16, false, true, &[]);
    check_field(set::reg092::POC_LSB_LT2, 92, 16, 16, false, true, &[]);
    check_reg(set::reg093::REGISTER, 93, 1);
    check_field(set::reg093::DLT_POC_MSB_CYCL1, 93, 0, 16, false, true, &[]);
    check_field(set::reg093::DLT_POC_MSB_CYCL2, 93, 16, 16, false, true, &[]);
    check_reg(set::reg094::REGISTER, 94, 1);
    check_field(set::reg094::OSD_CH_INV_EN, 94, 0, 8, false, true, &[]);
    check_field(set::reg094::OSD_ITYPE, 94, 8, 8, false, true, &[]);
    check_field(set::reg094::OSD_LU_INV_MSK, 94, 16, 8, false, true, &[]);
    check_field(set::reg094::OSD_CH_INV_MSK, 94, 24, 8, false, true, &[]);
    check_reg(set::reg_095_100::REGISTER, 95, 6);
    check_field(
        set::reg_095_100::REG_095_100[0],
        95,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_095_100::REG_095_100[1],
        96,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_095_100::REG_095_100[2],
        97,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_095_100::REG_095_100[3],
        98,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_095_100::REG_095_100[4],
        99,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_095_100::REG_095_100[5],
        100,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(set::reg101::REGISTER, 101, 1);
    check_field(set::reg101::VTHD_Y, 101, 0, 12, false, true, &[]);
    check_field(set::reg101::VTHD_C, 101, 16, 12, false, true, &[]);
    check_reg(set::reg102::REGISTER, 102, 1);
    check_field(set::reg102::RECT_SIZE, 102, 0, 1, false, true, &[]);
    check_field(set::reg102::INTER_4X4, 102, 1, 1, false, true, &[]);
    check_field(set::reg102::ARB_SEL, 102, 2, 1, false, true, &[]);
    check_field(set::reg102::VLC_LMT, 102, 3, 1, false, true, &[]);
    check_field(set::reg102::CHRM_SPCL, 102, 4, 1, false, true, &[]);
    check_field(set::reg102::RDO_MASK, 102, 5, 8, false, true, &[]);
    check_field(set::reg102::CCWA_E, 102, 13, 1, false, true, &[]);
    check_field(set::reg102::SCL_LST_SEL, 102, 14, 1, false, true, &[]);
    check_field(set::reg102::ATR_E, 102, 15, 1, false, true, &[]);
    check_field(set::reg102::ATF_EDG, 102, 16, 2, false, true, &[]);
    check_field(set::reg102::ATF_LVL_E, 102, 18, 1, false, true, &[]);
    check_field(set::reg102::ATF_INTRA_E, 102, 19, 1, false, true, &[]);
    check_field(set::reg102::SCL_LST_SEL_, 102, 20, 2, false, true, &[]);
    check_field(set::reg102::SATD_BYPS_FLG, 102, 31, 1, false, true, &[]);
    check_reg(set::reg103::REGISTER, 103, 1);
    check_field(set::reg103::NAL_REF_IDC, 103, 0, 2, false, true, &[]);
    check_field(set::reg103::NAL_UNIT_TYPE, 103, 2, 5, false, true, &[]);
    check_reg(set::reg104::REGISTER, 104, 1);
    check_field(set::reg104::MAX_FNUM, 104, 0, 4, false, true, &[]);
    check_field(set::reg104::DRCT_8X8, 104, 4, 1, false, true, &[]);
    check_field(set::reg104::MPOC_LM4, 104, 5, 4, false, true, &[]);
    check_reg(set::reg105::REGISTER, 105, 1);
    check_field(set::reg105::ETPY_MODE, 105, 0, 1, false, true, &[]);
    check_field(set::reg105::TRNS_8X8, 105, 1, 1, false, true, &[]);
    check_field(set::reg105::CSIP_FLAG, 105, 2, 1, false, true, &[]);
    check_field(set::reg105::NUM_REF0_IDX, 105, 3, 2, false, true, &[]);
    check_field(set::reg105::NUM_REF1_IDX, 105, 5, 2, false, true, &[]);
    check_field(set::reg105::PIC_INIT_QP, 105, 7, 6, false, true, &[]);
    check_field(set::reg105::CB_OFST, 105, 13, 5, false, true, &[]);
    check_field(set::reg105::CR_OFST, 105, 18, 5, false, true, &[]);
    check_field(set::reg105::WGHT_PRED, 105, 23, 1, false, true, &[]);
    check_field(set::reg105::DBF_CP_FLG, 105, 24, 1, false, true, &[]);
    check_reg(set::reg106::REGISTER, 106, 1);
    check_field(set::reg106::SLI_TYPE, 106, 0, 2, false, true, &[]);
    check_field(set::reg106::PPS_ID, 106, 2, 8, false, true, &[]);
    check_field(set::reg106::DRCT_SMVP, 106, 10, 1, false, true, &[]);
    check_field(set::reg106::NUM_REF_OVRD, 106, 11, 1, false, true, &[]);
    check_field(set::reg106::CBC_INIT_IDC, 106, 12, 2, false, true, &[]);
    check_field(set::reg106::FRM_NUM, 106, 16, 16, false, true, &[]);
    check_reg(set::reg107::REGISTER, 107, 1);
    check_field(set::reg107::IDR_PIC_ID, 107, 0, 16, false, true, &[]);
    check_field(set::reg107::POC_LSB, 107, 16, 16, false, true, &[]);
    check_reg(set::reg108::REGISTER, 108, 1);
    check_field(set::reg108::RODR_PIC_IDX, 108, 0, 2, false, true, &[]);
    check_field(set::reg108::REF_LIST0_RODR, 108, 2, 1, false, true, &[]);
    check_field(set::reg108::SLI_BETA_OFST, 108, 3, 4, false, true, &[]);
    check_field(set::reg108::SLI_ALPH_OFST, 108, 7, 4, false, true, &[]);
    check_field(set::reg108::DIS_DBLK_IDC, 108, 11, 2, false, true, &[]);
    check_field(set::reg108::RODR_PIC_NUM, 108, 16, 16, false, true, &[]);
    check_reg(set::reg109::REGISTER, 109, 1);
    check_field(set::reg109::NOPP_FLG, 109, 0, 1, false, true, &[]);
    check_field(set::reg109::LTRF_FLG, 109, 1, 1, false, true, &[]);
    check_field(set::reg109::ARPM_FLG, 109, 2, 1, false, true, &[]);
    check_field(set::reg109::MMCO4_PRE, 109, 3, 1, false, true, &[]);
    check_field(set::reg109::MMCO_TYPE0, 109, 4, 3, false, true, &[]);
    check_field(set::reg109::MMCO_PARM0, 109, 7, 16, false, true, &[]);
    check_field(set::reg109::MMCO_TYPE1, 109, 23, 3, false, true, &[]);
    check_field(set::reg109::MMCO_TYPE2, 109, 26, 3, false, true, &[]);
    check_reg(set::reg110::REGISTER, 110, 1);
    check_field(set::reg110::MMCO_PARM1, 110, 0, 16, false, true, &[]);
    check_field(set::reg110::MMCO_PARM2, 110, 16, 16, false, true, &[]);
    check_reg(set::reg_111::REGISTER, 111, 1);
    check_field(set::reg_111::REG_111, 111, 0, 32, false, false, &[]);
    check_reg(set::reg112::REGISTER, 112, 1);
    check_field(set::reg112::OSD_E, 112, 0, 8, false, true, &[]);
    check_field(set::reg112::OSD_INV_E, 112, 8, 8, false, true, &[]);
    check_field(set::reg112::OSD_PLT_CKS, 112, 16, 1, false, true, &[]);
    check_field(set::reg112::OSD_PLT_TYP, 112, 17, 1, false, true, &[]);
    check_reg(set::reg113::REGISTER, 113, 1);
    check_field(set::reg113::OSD_ITHD_R0, 113, 0, 4, false, true, &[]);
    check_field(set::reg113::OSD_ITHD_R1, 113, 4, 4, false, true, &[]);
    check_field(set::reg113::OSD_ITHD_R2, 113, 8, 4, false, true, &[]);
    check_field(set::reg113::OSD_ITHD_R3, 113, 12, 4, false, true, &[]);
    check_field(set::reg113::OSD_ITHD_R4, 113, 16, 4, false, true, &[]);
    check_field(set::reg113::OSD_ITHD_R5, 113, 20, 4, false, true, &[]);
    check_field(set::reg113::OSD_ITHD_R6, 113, 24, 4, false, true, &[]);
    check_field(set::reg113::OSD_ITHD_R7, 113, 28, 4, false, true, &[]);
    check_reg(set::reg114::REGISTER, 114, 1);
    check_field(
        set::reg114::LONG_TERM_FRAME_IDX0,
        114,
        0,
        4,
        false,
        true,
        &[],
    );
    check_field(
        set::reg114::LONG_TERM_FRAME_IDX1,
        114,
        4,
        4,
        false,
        true,
        &[],
    );
    check_field(
        set::reg114::LONG_TERM_FRAME_IDX2,
        114,
        8,
        4,
        false,
        true,
        &[],
    );
    check_reg(set::reg115::REGISTER, 115, 1);
    check_field(set::reg115::REG115, 115, 0, 32, false, true, &[]);
    check_reg(set::reg116_123::REGISTER, 116, 8);
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[0],
        116,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[1],
        117,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[2],
        118,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[3],
        119,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[4],
        120,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[5],
        121,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[6],
        122,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_X[7],
        123,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[0],
        116,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[1],
        117,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[2],
        118,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[3],
        119,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[4],
        120,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[5],
        121,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[6],
        122,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_LT_Y[7],
        123,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[0],
        116,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[1],
        117,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[2],
        118,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[3],
        119,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[4],
        120,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[5],
        121,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[6],
        122,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_X[7],
        123,
        16,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[0],
        116,
        24,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[1],
        117,
        24,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[2],
        118,
        24,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[3],
        119,
        24,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[4],
        120,
        24,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[5],
        121,
        24,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[6],
        122,
        24,
        8,
        false,
        true,
        &[],
    );
    check_field(
        set::reg116_123::OSD_POS_OSD_RB_Y[7],
        123,
        24,
        8,
        false,
        true,
        &[],
    );
    check_reg(set::reg124_131::REGISTER, 124, 8);
    check_addr(set::reg124_131::OSD_ADDR[0], 124, 16);
    check_addr(set::reg124_131::OSD_ADDR[1], 125, 16);
    check_addr(set::reg124_131::OSD_ADDR[2], 126, 16);
    check_addr(set::reg124_131::OSD_ADDR[3], 127, 16);
    check_addr(set::reg124_131::OSD_ADDR[4], 128, 16);
    check_addr(set::reg124_131::OSD_ADDR[5], 129, 16);
    check_addr(set::reg124_131::OSD_ADDR[6], 130, 16);
    check_addr(set::reg124_131::OSD_ADDR[7], 131, 16);
    check_reg(set::reg132::REGISTER, 132, 1);
    check_field(set::reg132::BS_LGTH, 132, 0, 27, false, false, &[]);
    check_reg(set::reg133::REGISTER, 133, 1);
    check_field(set::reg133::SSE_L32, 133, 0, 32, false, false, &[]);
    check_reg(set::reg134::REGISTER, 134, 1);
    check_field(set::reg134::QP_SUM, 134, 0, 22, false, false, &[]);
    check_field(set::reg134::SSE_H8, 134, 24, 8, false, false, &[]);
    check_reg(set::reg135::REGISTER, 135, 1);
    check_field(set::reg135::SAO_CNUM, 135, 0, 12, false, false, &[]);
    check_field(set::reg135::SAO_YNUM, 135, 12, 12, false, false, &[]);
    check_reg(set::reg_136_137::REGISTER, 136, 2);
    check_field(
        set::reg_136_137::REG_136_137[0],
        136,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        set::reg_136_137::REG_136_137[1],
        137,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(set::reg138::REGISTER, 138, 1);
    check_field(set::reg138::ST_ENC, 138, 0, 2, false, false, &[]);
    check_field(set::reg138::ST_SCLR, 138, 2, 1, false, false, &[]);
    check_reg(set::reg139::REGISTER, 139, 1);
    check_field(set::reg139::FNUM_ENC, 139, 0, 8, false, false, &[]);
    check_field(set::reg139::FNUM_CFG, 139, 8, 8, false, false, &[]);
    check_field(set::reg139::FNUM_INT, 139, 16, 8, false, false, &[]);
    check_reg(set::reg140::REGISTER, 140, 1);
    check_field(set::reg140::NODE_ADDR, 140, 0, 32, false, false, &[]);
    check_reg(set::reg141::REGISTER, 141, 1);
    check_field(set::reg141::BSBW_ADDR, 141, 0, 32, false, false, &[]);
    check_reg(set::reg142::REGISTER, 142, 1);
    check_field(set::reg142::AXIB_IDL, 142, 0, 7, false, false, &[]);
    check_field(set::reg142::AXIB_OVFL, 142, 7, 7, false, false, &[]);
    check_field(set::reg142::AXIB_ERR, 142, 14, 7, false, false, &[]);
    check_field(set::reg142::AXIR_ERR, 142, 21, 6, false, false, &[]);
    check_reg(set::reg143::REGISTER, 143, 1);
    check_field(set::reg143::SLI_NUM, 143, 0, 6, false, false, &[]);
    check_reg(set::reg144::REGISTER, 144, 1);
    check_field(set::reg144::SLI_LEN, 144, 0, 25, false, false, &[]);
    check_reg(set::reg145::REGISTER, 145, 1);
    check_field(set::reg145::PNUM_P64, 145, 0, 12, false, false, &[]);
    check_reg(set::reg146::REGISTER, 146, 1);
    check_field(set::reg146::PNUM_P32, 146, 0, 14, false, false, &[]);
    check_reg(set::reg147::REGISTER, 147, 1);
    check_field(set::reg147::PNUM_P16, 147, 0, 16, false, false, &[]);
    check_reg(set::reg148::REGISTER, 148, 1);
    check_field(set::reg148::PNUM_P8, 148, 0, 18, false, false, &[]);
    check_reg(set::reg149::REGISTER, 149, 1);
    check_field(set::reg149::PNUM_I32, 149, 0, 14, false, false, &[]);
    check_reg(set::reg150::REGISTER, 150, 1);
    check_field(set::reg150::PNUM_I16, 150, 0, 16, false, false, &[]);
    check_reg(set::reg151::REGISTER, 151, 1);
    check_field(set::reg151::PNUM_I8, 151, 0, 18, false, false, &[]);
    check_reg(set::reg152::REGISTER, 152, 1);
    check_field(set::reg152::PNUM_I4, 152, 0, 20, false, false, &[]);
    check_reg(set::reg153_204::REGISTER, 153, 52);
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[0],
        153,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[1],
        154,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[2],
        155,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[3],
        156,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[4],
        157,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[5],
        158,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[6],
        159,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[7],
        160,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[8],
        161,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[9],
        162,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[10],
        163,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[11],
        164,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[12],
        165,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[13],
        166,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[14],
        167,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[15],
        168,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[16],
        169,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[17],
        170,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[18],
        171,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[19],
        172,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[20],
        173,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[21],
        174,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[22],
        175,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[23],
        176,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[24],
        177,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[25],
        178,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[26],
        179,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[27],
        180,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[28],
        181,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[29],
        182,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[30],
        183,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[31],
        184,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[32],
        185,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[33],
        186,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[34],
        187,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[35],
        188,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[36],
        189,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[37],
        190,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[38],
        191,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[39],
        192,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[40],
        193,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[41],
        194,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[42],
        195,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[43],
        196,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[44],
        197,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[45],
        198,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[46],
        199,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[47],
        200,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[48],
        201,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[49],
        202,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[50],
        203,
        0,
        18,
        false,
        false,
        &[],
    );
    check_field(
        set::reg153_204::NUM_QP_B8NUM_QP[51],
        204,
        0,
        18,
        false,
        false,
        &[],
    );
    check_reg(set::reg205::REGISTER, 205, 1);
    check_field(set::reg205::MADP, 205, 0, 32, false, false, &[]);
    check_reg(set::reg206::REGISTER, 206, 1);
    check_field(set::reg206::NUM_CTU, 206, 0, 16, false, false, &[]);
    check_reg(set::reg207::REGISTER, 207, 1);
    check_field(set::reg207::MADI, 207, 0, 32, false, false, &[]);
    check_reg(set::reg208::REGISTER, 208, 1);
    check_field(set::reg208::NUM_B16, 208, 0, 32, false, false, &[]);
}

#[test]
fn ret_pins() {
    check_reg(ret::hw_status::REGISTER, 7, 1);
    check_field(ret::hw_status::ENC_DONE_STA, 7, 0, 1, false, true, &[]);
    check_field(ret::hw_status::LKT_DONE_STA, 7, 1, 1, false, true, &[]);
    check_field(ret::hw_status::SCLR_DONE_STA, 7, 2, 1, false, true, &[]);
    check_field(
        ret::hw_status::ENC_SLICE_DONE_STA,
        7,
        3,
        1,
        false,
        true,
        &[],
    );
    check_field(ret::hw_status::OFLW_DONE_STA, 7, 4, 1, false, true, &[]);
    check_field(ret::hw_status::BRSP_DONE_STA, 7, 5, 1, false, true, &[]);
    check_field(ret::hw_status::BERR_DONE_STA, 7, 6, 1, false, true, &[]);
    check_field(ret::hw_status::RERR_DONE_STA, 7, 7, 1, false, true, &[]);
    check_field(ret::hw_status::WDG_DONE_STA, 7, 8, 1, false, true, &[]);
    check_reg(ret::st_bsl::REGISTER, 132, 1);
    check_field(ret::st_bsl::BS_LGTH, 132, 0, 27, false, false, &[]);
    check_reg(ret::st_sse_l32::REGISTER, 133, 1);
    check_field(ret::st_sse_l32::SSE_L32, 133, 0, 32, false, false, &[]);
    check_reg(ret::st_sse_qp::REGISTER, 134, 1);
    check_field(ret::st_sse_qp::QP_SUM, 134, 0, 22, false, false, &[]);
    check_field(ret::st_sse_qp::SSE_H8, 134, 24, 8, false, false, &[]);
    check_reg(ret::st_sao::REGISTER, 135, 1);
    check_field(ret::st_sao::SAO_CNUM, 135, 0, 12, false, false, &[]);
    check_field(ret::st_sao::SAO_YNUM, 135, 12, 12, false, false, &[]);
    check_reg(ret::reg_136_137::REGISTER, 136, 2);
    check_field(
        ret::reg_136_137::REG_136_137[0],
        136,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::reg_136_137::REG_136_137[1],
        137,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_enc::REGISTER, 138, 1);
    check_field(ret::st_enc::ST_ENC, 138, 0, 2, false, false, &[]);
    check_field(ret::st_enc::ST_SCLR, 138, 2, 1, false, false, &[]);
    check_reg(ret::st_lkt::REGISTER, 139, 1);
    check_field(ret::st_lkt::FNUM_ENC, 139, 0, 8, false, false, &[]);
    check_field(ret::st_lkt::FNUM_CFG, 139, 8, 8, false, false, &[]);
    check_field(ret::st_lkt::FNUM_INT, 139, 16, 8, false, false, &[]);
    check_reg(ret::st_nadr::REGISTER, 140, 1);
    check_field(ret::st_nadr::NODE_ADDR, 140, 0, 32, false, false, &[]);
    check_reg(ret::st_bsb::REGISTER, 141, 1);
    check_field(ret::st_bsb::BSBW_ADDR, 141, 0, 32, false, false, &[]);
    check_reg(ret::st_bus::REGISTER, 142, 1);
    check_field(ret::st_bus::AXIB_IDL, 142, 0, 7, false, false, &[]);
    check_field(ret::st_bus::AXIB_OVFL, 142, 7, 7, false, false, &[]);
    check_field(ret::st_bus::AXIB_ERR, 142, 14, 7, false, false, &[]);
    check_field(ret::st_bus::AXIR_ERR, 142, 21, 6, false, false, &[]);
    check_reg(ret::st_slice_number::REGISTER, 143, 1);
    check_field(
        ret::st_slice_number::ST_SLICE_NUMBER,
        143,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_slice_length::REGISTER, 144, 1);
    check_field(
        ret::st_slice_length::ST_SLICE_LENGTH,
        144,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl64_inter_num::REGISTER, 145, 1);
    check_field(
        ret::st_lvl64_inter_num::ST_LVL64_INTER_NUM,
        145,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl32_inter_num::REGISTER, 146, 1);
    check_field(
        ret::st_lvl32_inter_num::ST_LVL32_INTER_NUM,
        146,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl16_inter_num::REGISTER, 147, 1);
    check_field(
        ret::st_lvl16_inter_num::ST_LVL16_INTER_NUM,
        147,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl8_inter_num::REGISTER, 148, 1);
    check_field(
        ret::st_lvl8_inter_num::ST_LVL8_INTER_NUM,
        148,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl32_intra_num::REGISTER, 149, 1);
    check_field(
        ret::st_lvl32_intra_num::ST_LVL32_INTRA_NUM,
        149,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl16_intra_num::REGISTER, 150, 1);
    check_field(
        ret::st_lvl16_intra_num::ST_LVL16_INTRA_NUM,
        150,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl8_intra_num::REGISTER, 151, 1);
    check_field(
        ret::st_lvl8_intra_num::ST_LVL8_INTRA_NUM,
        151,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_lvl4_intra_num::REGISTER, 152, 1);
    check_field(
        ret::st_lvl4_intra_num::ST_LVL4_INTRA_NUM,
        152,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_cu_num_qp::REGISTER, 153, 52);
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[0],
        153,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[1],
        154,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[2],
        155,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[3],
        156,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[4],
        157,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[5],
        158,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[6],
        159,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[7],
        160,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[8],
        161,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[9],
        162,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[10],
        163,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[11],
        164,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[12],
        165,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[13],
        166,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[14],
        167,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[15],
        168,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[16],
        169,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[17],
        170,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[18],
        171,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[19],
        172,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[20],
        173,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[21],
        174,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[22],
        175,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[23],
        176,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[24],
        177,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[25],
        178,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[26],
        179,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[27],
        180,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[28],
        181,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[29],
        182,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[30],
        183,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[31],
        184,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[32],
        185,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[33],
        186,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[34],
        187,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[35],
        188,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[36],
        189,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[37],
        190,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[38],
        191,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[39],
        192,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[40],
        193,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[41],
        194,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[42],
        195,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[43],
        196,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[44],
        197,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[45],
        198,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[46],
        199,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[47],
        200,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[48],
        201,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[49],
        202,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[50],
        203,
        0,
        32,
        false,
        false,
        &[],
    );
    check_field(
        ret::st_cu_num_qp::ST_CU_NUM_QP[51],
        204,
        0,
        32,
        false,
        false,
        &[],
    );
    check_reg(ret::st_madp::REGISTER, 205, 1);
    check_field(ret::st_madp::ST_MADP, 205, 0, 32, false, false, &[]);
    check_reg(ret::st_ctu_num::REGISTER, 206, 1);
    check_field(ret::st_ctu_num::ST_CTU_NUM, 206, 0, 32, false, false, &[]);
    check_reg(ret::st_madi::REGISTER, 207, 1);
    check_field(ret::st_madi::ST_MADI, 207, 0, 32, false, false, &[]);
    check_reg(ret::st_mb_num::REGISTER, 208, 1);
    check_field(ret::st_mb_num::ST_MB_NUM, 208, 0, 32, false, false, &[]);
}

#[test]
fn l2_pins() {
    check_reg(l2::iprd_tthdy4::REGISTER, 1, 2);
    check_field(l2::iprd_tthdy4::IPRD_TTHDY4[0], 1, 0, 16, false, true, &[]);
    check_field(l2::iprd_tthdy4::IPRD_TTHDY4[1], 1, 16, 16, false, true, &[]);
    check_field(l2::iprd_tthdy4::IPRD_TTHDY4[2], 2, 0, 16, false, true, &[]);
    check_field(l2::iprd_tthdy4::IPRD_TTHDY4[3], 2, 16, 16, false, true, &[]);
    check_reg(l2::iprd_tthdc8::REGISTER, 3, 2);
    check_field(l2::iprd_tthdc8::IPRD_TTHDC8[0], 3, 0, 16, false, true, &[]);
    check_field(l2::iprd_tthdc8::IPRD_TTHDC8[1], 3, 16, 16, false, true, &[]);
    check_field(l2::iprd_tthdc8::IPRD_TTHDC8[2], 4, 0, 16, false, true, &[]);
    check_field(l2::iprd_tthdc8::IPRD_TTHDC8[3], 4, 16, 16, false, true, &[]);
    check_reg(l2::iprd_tthdy8::REGISTER, 5, 2);
    check_field(l2::iprd_tthdy8::IPRD_TTHDY8[0], 5, 0, 16, false, true, &[]);
    check_field(l2::iprd_tthdy8::IPRD_TTHDY8[1], 5, 16, 16, false, true, &[]);
    check_field(l2::iprd_tthdy8::IPRD_TTHDY8[2], 6, 0, 16, false, true, &[]);
    check_field(l2::iprd_tthdy8::IPRD_TTHDY8[3], 6, 16, 16, false, true, &[]);
    check_reg(l2::iprd_tthd_ul::REGISTER, 7, 1);
    check_field(l2::iprd_tthd_ul::IPRD_TTHD_UL, 7, 0, 32, false, true, &[]);
    check_reg(l2::iprd_wgty8::REGISTER, 8, 1);
    check_field(l2::iprd_wgty8::IPRD_WGTY8[0], 8, 0, 8, false, true, &[]);
    check_field(l2::iprd_wgty8::IPRD_WGTY8[1], 8, 8, 8, false, true, &[]);
    check_field(l2::iprd_wgty8::IPRD_WGTY8[2], 8, 16, 8, false, true, &[]);
    check_field(l2::iprd_wgty8::IPRD_WGTY8[3], 8, 24, 8, false, true, &[]);
    check_reg(l2::iprd_wgty4::REGISTER, 9, 1);
    check_field(l2::iprd_wgty4::IPRD_WGTY4[0], 9, 0, 8, false, true, &[]);
    check_field(l2::iprd_wgty4::IPRD_WGTY4[1], 9, 8, 8, false, true, &[]);
    check_field(l2::iprd_wgty4::IPRD_WGTY4[2], 9, 16, 8, false, true, &[]);
    check_field(l2::iprd_wgty4::IPRD_WGTY4[3], 9, 24, 8, false, true, &[]);
    check_reg(l2::iprd_wgty16::REGISTER, 10, 1);
    check_field(l2::iprd_wgty16::IPRD_WGTY16[0], 10, 0, 8, false, true, &[]);
    check_field(l2::iprd_wgty16::IPRD_WGTY16[1], 10, 8, 8, false, true, &[]);
    check_field(l2::iprd_wgty16::IPRD_WGTY16[2], 10, 16, 8, false, true, &[]);
    check_field(l2::iprd_wgty16::IPRD_WGTY16[3], 10, 24, 8, false, true, &[]);
    check_reg(l2::iprd_wgtc8::REGISTER, 11, 1);
    check_field(l2::iprd_wgtc8::IPRD_WGTC8[0], 11, 0, 8, false, true, &[]);
    check_field(l2::iprd_wgtc8::IPRD_WGTC8[1], 11, 8, 8, false, true, &[]);
    check_field(l2::iprd_wgtc8::IPRD_WGTC8[2], 11, 16, 8, false, true, &[]);
    check_field(l2::iprd_wgtc8::IPRD_WGTC8[3], 11, 24, 8, false, true, &[]);
    check_reg(l2::qnt_bias_comb::REGISTER, 12, 1);
    check_field(l2::qnt_bias_comb::QNT_BIAS_I, 12, 0, 10, false, true, &[]);
    check_field(l2::qnt_bias_comb::QNT_BIAS_P, 12, 10, 10, false, true, &[]);
    check_reg(l2::atr_thd0_h264::REGISTER, 13, 1);
    check_field(l2::atr_thd0_h264::ATR_THD0, 13, 0, 12, false, true, &[]);
    check_field(l2::atr_thd0_h264::ATR_THD1, 13, 16, 12, false, true, &[]);
    check_reg(l2::atr_thd1_h264::REGISTER, 14, 1);
    check_field(l2::atr_thd1_h264::ATR_THD2, 14, 0, 12, false, true, &[]);
    check_field(l2::atr_thd1_h264::ATR_QP, 14, 16, 6, false, true, &[]);
    check_reg(l2::atr_wgt16_h264::REGISTER, 15, 1);
    check_field(
        l2::atr_wgt16_h264::ATR_LV16_WGT0,
        15,
        0,
        8,
        false,
        true,
        &[],
    );
    check_field(
        l2::atr_wgt16_h264::ATR_LV16_WGT1,
        15,
        8,
        8,
        false,
        true,
        &[],
    );
    check_field(
        l2::atr_wgt16_h264::ATR_LV16_WGT2,
        15,
        16,
        8,
        false,
        true,
        &[],
    );
    check_reg(l2::atr_wgt8_h264::REGISTER, 16, 1);
    check_field(l2::atr_wgt8_h264::ATR_LV8_WGT0, 16, 0, 8, false, true, &[]);
    check_field(l2::atr_wgt8_h264::ATR_LV8_WGT1, 16, 8, 8, false, true, &[]);
    check_field(l2::atr_wgt8_h264::ATR_LV8_WGT2, 16, 16, 8, false, true, &[]);
    check_reg(l2::atr_wgt4_h264::REGISTER, 17, 1);
    check_field(l2::atr_wgt4_h264::ATR_LV4_WGT0, 17, 0, 8, false, true, &[]);
    check_field(l2::atr_wgt4_h264::ATR_LV4_WGT1, 17, 8, 8, false, true, &[]);
    check_field(l2::atr_wgt4_h264::ATR_LV4_WGT2, 17, 16, 8, false, true, &[]);
    check_reg(l2::atf_tthd::REGISTER, 18, 2);
    check_field(l2::atf_tthd::ATF_TTHD[0], 18, 0, 16, false, true, &[]);
    check_field(l2::atf_tthd::ATF_TTHD[1], 18, 16, 16, false, true, &[]);
    check_field(l2::atf_tthd::ATF_TTHD[2], 19, 0, 16, false, true, &[]);
    check_field(l2::atf_tthd::ATF_TTHD[3], 19, 16, 16, false, true, &[]);
    check_reg(l2::atf_sthd0_h264::REGISTER, 20, 1);
    check_field(l2::atf_sthd0_h264::ATF_STHD_10, 20, 0, 14, false, true, &[]);
    check_field(
        l2::atf_sthd0_h264::ATF_STHD_MAX,
        20,
        16,
        14,
        false,
        true,
        &[],
    );
    check_reg(l2::atf_sthd1_h264::REGISTER, 21, 1);
    check_field(l2::atf_sthd1_h264::ATF_STHD_11, 21, 0, 14, false, true, &[]);
    check_field(
        l2::atf_sthd1_h264::ATF_STHD_20,
        21,
        16,
        14,
        false,
        true,
        &[],
    );
    check_reg(l2::atf_wgt0_h264::REGISTER, 22, 1);
    check_field(l2::atf_wgt0_h264::ATF_WGT10, 22, 0, 9, false, true, &[]);
    check_field(l2::atf_wgt0_h264::ATF_WGT11, 22, 16, 9, false, true, &[]);
    check_reg(l2::atf_wgt1_h264::REGISTER, 23, 1);
    check_field(l2::atf_wgt1_h264::ATF_WGT12, 23, 0, 9, false, true, &[]);
    check_field(l2::atf_wgt1_h264::ATF_WGT20, 23, 16, 9, false, true, &[]);
    check_reg(l2::atf_wgt2_h264::REGISTER, 24, 1);
    check_field(l2::atf_wgt2_h264::ATF_WGT21, 24, 0, 9, false, true, &[]);
    check_field(l2::atf_wgt2_h264::ATF_WGT30, 24, 16, 9, false, true, &[]);
    check_reg(l2::atf_ofst0_h264::REGISTER, 25, 1);
    check_field(l2::atf_ofst0_h264::ATF_OFST10, 25, 0, 14, false, true, &[]);
    check_field(l2::atf_ofst0_h264::ATF_OFST11, 25, 16, 14, false, true, &[]);
    check_reg(l2::atf_ofst1_h264::REGISTER, 26, 1);
    check_field(l2::atf_ofst1_h264::ATF_OFST12, 26, 0, 14, false, true, &[]);
    check_field(l2::atf_ofst1_h264::ATF_OFST20, 26, 16, 14, false, true, &[]);
    check_reg(l2::atf_ofst2_h264::REGISTER, 27, 1);
    check_field(l2::atf_ofst2_h264::ATF_OFST21, 27, 0, 14, false, true, &[]);
    check_field(l2::atf_ofst2_h264::ATF_OFST30, 27, 16, 14, false, true, &[]);
    check_reg(l2::iprd_wgt_qp::REGISTER, 28, 52);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[0], 28, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[1], 29, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[2], 30, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[3], 31, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[4], 32, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[5], 33, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[6], 34, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[7], 35, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[8], 36, 0, 32, false, true, &[]);
    check_field(l2::iprd_wgt_qp::IPRD_WGT_QP[9], 37, 0, 32, false, true, &[]);
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[10],
        38,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[11],
        39,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[12],
        40,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[13],
        41,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[14],
        42,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[15],
        43,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[16],
        44,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[17],
        45,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[18],
        46,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[19],
        47,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[20],
        48,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[21],
        49,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[22],
        50,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[23],
        51,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[24],
        52,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[25],
        53,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[26],
        54,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[27],
        55,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[28],
        56,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[29],
        57,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[30],
        58,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[31],
        59,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[32],
        60,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[33],
        61,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[34],
        62,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[35],
        63,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[36],
        64,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[37],
        65,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[38],
        66,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[39],
        67,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[40],
        68,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[41],
        69,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[42],
        70,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[43],
        71,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[44],
        72,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[45],
        73,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[46],
        74,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[47],
        75,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[48],
        76,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[49],
        77,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[50],
        78,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::iprd_wgt_qp::IPRD_WGT_QP[51],
        79,
        0,
        32,
        false,
        true,
        &[],
    );
    check_reg(l2::wgt_qp_grpa::REGISTER, 80, 52);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[0], 80, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[1], 81, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[2], 82, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[3], 83, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[4], 84, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[5], 85, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[6], 86, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[7], 87, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[8], 88, 0, 32, false, true, &[]);
    check_field(l2::wgt_qp_grpa::WGT_QP_GRPA[9], 89, 0, 32, false, true, &[]);
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[10],
        90,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[11],
        91,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[12],
        92,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[13],
        93,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[14],
        94,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[15],
        95,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[16],
        96,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[17],
        97,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[18],
        98,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[19],
        99,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[20],
        100,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[21],
        101,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[22],
        102,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[23],
        103,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[24],
        104,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[25],
        105,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[26],
        106,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[27],
        107,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[28],
        108,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[29],
        109,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[30],
        110,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[31],
        111,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[32],
        112,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[33],
        113,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[34],
        114,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[35],
        115,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[36],
        116,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[37],
        117,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[38],
        118,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[39],
        119,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[40],
        120,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[41],
        121,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[42],
        122,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[43],
        123,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[44],
        124,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[45],
        125,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[46],
        126,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[47],
        127,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[48],
        128,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[49],
        129,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[50],
        130,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpa::WGT_QP_GRPA[51],
        131,
        0,
        32,
        false,
        true,
        &[],
    );
    check_reg(l2::wgt_qp_grpb::REGISTER, 132, 52);
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[0],
        132,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[1],
        133,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[2],
        134,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[3],
        135,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[4],
        136,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[5],
        137,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[6],
        138,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[7],
        139,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[8],
        140,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[9],
        141,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[10],
        142,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[11],
        143,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[12],
        144,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[13],
        145,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[14],
        146,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[15],
        147,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[16],
        148,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[17],
        149,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[18],
        150,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[19],
        151,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[20],
        152,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[21],
        153,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[22],
        154,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[23],
        155,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[24],
        156,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[25],
        157,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[26],
        158,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[27],
        159,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[28],
        160,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[29],
        161,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[30],
        162,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[31],
        163,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[32],
        164,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[33],
        165,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[34],
        166,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[35],
        167,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[36],
        168,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[37],
        169,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[38],
        170,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[39],
        171,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[40],
        172,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[41],
        173,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[42],
        174,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[43],
        175,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[44],
        176,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[45],
        177,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[46],
        178,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[47],
        179,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[48],
        180,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[49],
        181,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[50],
        182,
        0,
        32,
        false,
        true,
        &[],
    );
    check_field(
        l2::wgt_qp_grpb::WGT_QP_GRPB[51],
        183,
        0,
        32,
        false,
        true,
        &[],
    );
    check_reg(l2::madi_mode::REGISTER, 184, 1);
    check_field(l2::madi_mode::MADI_MODE, 184, 0, 32, false, true, &[]);
    check_reg(l2::aq_tthd::REGISTER, 185, 4);
    check_field(l2::aq_tthd::AQ_TTHD[0], 185, 0, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[1], 185, 8, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[2], 185, 16, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[3], 185, 24, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[4], 186, 0, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[5], 186, 8, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[6], 186, 16, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[7], 186, 24, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[8], 187, 0, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[9], 187, 8, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[10], 187, 16, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[11], 187, 24, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[12], 188, 0, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[13], 188, 8, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[14], 188, 16, 8, false, true, &[]);
    check_field(l2::aq_tthd::AQ_TTHD[15], 188, 24, 8, false, true, &[]);
    check_reg(l2::aq_step::REGISTER, 189, 4);
    check_field(l2::aq_step::AQ_STEP[0], 189, 0, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[1], 189, 8, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[2], 189, 16, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[3], 189, 24, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[4], 190, 0, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[5], 190, 8, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[6], 190, 16, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[7], 190, 24, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[8], 191, 0, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[9], 191, 8, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[10], 191, 16, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[11], 191, 24, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[12], 192, 0, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[13], 192, 8, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[14], 192, 16, 8, true, true, &[]);
    check_field(l2::aq_step::AQ_STEP[15], 192, 24, 8, true, true, &[]);
    check_reg(l2::rme_mvd_penalty::REGISTER, 193, 1);
    check_field(l2::rme_mvd_penalty::MVD_PNLT_E, 193, 0, 1, false, true, &[]);
    check_field(
        l2::rme_mvd_penalty::MVD_PNLT_COEF,
        193,
        1,
        5,
        false,
        true,
        &[],
    );
    check_field(
        l2::rme_mvd_penalty::MVD_PNLT_CNST,
        193,
        6,
        14,
        false,
        true,
        &[],
    );
    check_field(
        l2::rme_mvd_penalty::MVD_PNLT_LTHD,
        193,
        20,
        4,
        false,
        true,
        &[],
    );
    check_field(
        l2::rme_mvd_penalty::MVD_PNLT_HTHD,
        193,
        24,
        4,
        false,
        true,
        &[],
    );
    check_reg(l2::atr1_thd0_h264::REGISTER, 194, 1);
    check_field(l2::atr1_thd0_h264::ATR1_THD0, 194, 0, 12, false, true, &[]);
    check_field(l2::atr1_thd0_h264::ATR1_THD1, 194, 16, 12, false, true, &[]);
    check_reg(l2::atr1_thd1_h264::REGISTER, 195, 1);
    check_field(l2::atr1_thd1_h264::ATR1_THD2, 195, 0, 12, false, true, &[]);
}
