//! The VEPU580 H.264 PIC (kernel class 1): the buffer and video syntax registers (reg_base).
//!
//! Words 160 to 253 of the register block; writable. Generated from the vendor header; see [`super`].

/// Register `reg0160` at 0x0280: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.adr_src0`.
pub mod reg0160 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.adr_src0",
        160,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ADR_SRC0: AddressReg = AddressReg::new(Region::Class(1), 160, 1);
}

/// Register `reg0161` at 0x0284: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.adr_src1`.
pub mod reg0161 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.adr_src1",
        161,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ADR_SRC1: AddressReg = AddressReg::new(Region::Class(1), 161, 1);
}

/// Register `reg0162` at 0x0288: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.adr_src2`.
pub mod reg0162 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.adr_src2",
        162,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ADR_SRC2: AddressReg = AddressReg::new(Region::Class(1), 162, 1);
}

/// Register `reg0163` at 0x028c: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.rfpw_h_addr`.
pub mod reg0163 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.rfpw_h_addr",
        163,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const RFPW_H_ADDR: AddressReg = AddressReg::new(Region::Class(1), 163, 1);
}

/// Register `reg0164` at 0x0290: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.rfpw_b_addr`.
pub mod reg0164 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.rfpw_b_addr",
        164,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const RFPW_B_ADDR: AddressReg = AddressReg::new(Region::Class(1), 164, 1);
}

/// Register `reg0165` at 0x0294: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.rfpr_h_addr`.
pub mod reg0165 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.rfpr_h_addr",
        165,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const RFPR_H_ADDR: AddressReg = AddressReg::new(Region::Class(1), 165, 1);
}

/// Register `reg0166` at 0x0298: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.rfpr_b_addr`.
pub mod reg0166 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.rfpr_b_addr",
        166,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const RFPR_B_ADDR: AddressReg = AddressReg::new(Region::Class(1), 166, 1);
}

/// Register `reg0167` at 0x029c: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.cmvw_addr`.
pub mod reg0167 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.cmvw_addr",
        167,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const CMVW_ADDR: AddressReg = AddressReg::new(Region::Class(1), 167, 1);
}

/// Register `reg0168` at 0x02a0: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.cmvr_addr`.
pub mod reg0168 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.cmvr_addr",
        168,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const CMVR_ADDR: AddressReg = AddressReg::new(Region::Class(1), 168, 1);
}

/// Register `reg0169` at 0x02a4: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.dspw_addr`.
pub mod reg0169 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.dspw_addr",
        169,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const DSPW_ADDR: AddressReg = AddressReg::new(Region::Class(1), 169, 1);
}

/// Register `reg0170` at 0x02a8: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.dspr_addr`.
pub mod reg0170 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.dspr_addr",
        170,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const DSPR_ADDR: AddressReg = AddressReg::new(Region::Class(1), 170, 1);
}

/// Register `reg0171` at 0x02ac: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.meiw_addr`.
pub mod reg0171 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.meiw_addr",
        171,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const MEIW_ADDR: AddressReg = AddressReg::new(Region::Class(1), 171, 1);
}

/// Register `reg0172` at 0x02b0: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.bsbt_addr`.
pub mod reg0172 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.bsbt_addr",
        172,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const BSBT_ADDR: AddressReg = AddressReg::new(Region::Class(1), 172, 1);
}

/// Register `reg0173` at 0x02b4: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.bsbb_addr`.
pub mod reg0173 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.bsbb_addr",
        173,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const BSBB_ADDR: AddressReg = AddressReg::new(Region::Class(1), 173, 1);
}

/// Register `reg0174` at 0x02b8: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.bsbr_addr`.
pub mod reg0174 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.bsbr_addr",
        174,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const BSBR_ADDR: AddressReg = AddressReg::new(Region::Class(1), 174, 1);
}

/// Register `reg0175` at 0x02bc: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.adr_bsbs`.
pub mod reg0175 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.adr_bsbs",
        175,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ADR_BSBS: AddressReg = AddressReg::new(Region::Class(1), 175, 1);
}

/// Register `reg0176` at 0x02c0: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.lpfw_addr`.
pub mod reg0176 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.lpfw_addr",
        176,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const LPFW_ADDR: AddressReg = AddressReg::new(Region::Class(1), 176, 1);
}

/// Register `reg0177` at 0x02c4: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.lpfr_addr`.
pub mod reg0177 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.lpfr_addr",
        177,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const LPFR_ADDR: AddressReg = AddressReg::new(Region::Class(1), 177, 1);
}

/// Register `reg0178` at 0x02c8: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.roi_addr`.
pub mod reg0178 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.roi_addr",
        178,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ROI_ADDR: AddressReg = AddressReg::new(Region::Class(1), 178, 1);
}

/// Register `reg0179` at 0x02cc: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.roi_qp_addr`.
pub mod reg0179 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.roi_qp_addr",
        179,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ROI_QP_ADDR: AddressReg = AddressReg::new(Region::Class(1), 179, 1);
}

/// Register `reg0180` at 0x02d0: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.roi_amv_addr`.
pub mod reg0180 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.roi_amv_addr",
        180,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ROI_AMV_ADDR: AddressReg = AddressReg::new(Region::Class(1), 180, 1);
}

/// Register `reg0181` at 0x02d4: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.roi_mv_addr`.
pub mod reg0181 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.roi_mv_addr",
        181,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const ROI_MV_ADDR: AddressReg = AddressReg::new(Region::Class(1), 181, 1);
}

/// Register `reg0182` at 0x02d8: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.ebuft_addr`.
pub mod reg0182 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.ebuft_addr",
        182,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const EBUFT_ADDR: AddressReg = AddressReg::new(Region::Class(1), 182, 1);
}

/// Register `reg0183` at 0x02dc: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.ebufb_addr`.
pub mod reg0183 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.ebufb_addr",
        183,
        1,
        Access::ReadWrite,
        1,
    );
    /// A buffer's bus address: the driver writes it from the caller's buffer.
    pub const EBUFB_ADDR: AddressReg = AddressReg::new(Region::Class(1), 183, 1);
}

/// Register `reg0192` at 0x0300: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.enc_pic`.
pub mod reg0192 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.enc_pic",
        192,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ENC_STND: Field = Field::new(Region::Class(1), 192, 0, 1, false, true, &[]);
    pub const ROI_EN: Field = Field::new(Region::Class(1), 192, 1, 1, false, true, &[]);
    pub const CUR_FRM_REF: Field = Field::new(Region::Class(1), 192, 2, 1, false, true, &[]);
    pub const MEI_STOR: Field = Field::new(Region::Class(1), 192, 3, 1, false, true, &[]);
    pub const BS_SCP: Field = Field::new(Region::Class(1), 192, 4, 1, false, true, &[]);
    pub const PIC_QP: Field = Field::new(Region::Class(1), 192, 8, 6, false, true, &[]);
    pub const NUM_PIC_TOT_CUR: Field = Field::new(Region::Class(1), 192, 14, 5, false, true, &[]);
    pub const LOG2_CTU_NUM: Field = Field::new(Region::Class(1), 192, 19, 5, false, true, &[]);
    pub const SLEN_FIFO: Field = Field::new(Region::Class(1), 192, 30, 1, false, true, &[]);
    pub const REC_FBC_DIS: Field = Field::new(Region::Class(1), 192, 31, 1, false, true, &[]);
}

/// Register `reg0193` at 0x0304: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.dual_core`.
pub mod reg0193 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.dual_core",
        193,
        1,
        Access::ReadWrite,
        1,
    );
    pub const DCHS_TXID: Field = Field::new(Region::Class(1), 193, 0, 2, false, true, &[]);
    pub const DCHS_RXID: Field = Field::new(Region::Class(1), 193, 2, 2, false, true, &[]);
    pub const DCHS_TXE: Field = Field::new(Region::Class(1), 193, 4, 1, false, true, &[]);
    pub const DCHS_RXE: Field = Field::new(Region::Class(1), 193, 5, 1, false, true, &[]);
    pub const DCHS_OFST: Field = Field::new(Region::Class(1), 193, 16, 11, false, true, &[]);
}

/// Register `reg0196` at 0x0310: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.enc_rsl`.
pub mod reg0196 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.enc_rsl",
        196,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PIC_WD8_M1: Field = Field::new(Region::Class(1), 196, 0, 11, false, true, &[]);
    pub const PIC_HD8_M1: Field = Field::new(Region::Class(1), 196, 16, 11, false, true, &[]);
}

/// Register `reg0197` at 0x0314: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_fill`.
pub mod reg0197 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_fill",
        197,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PIC_WFILL: Field = Field::new(Region::Class(1), 197, 0, 6, false, true, &[]);
    pub const PIC_HFILL: Field = Field::new(Region::Class(1), 197, 16, 6, false, true, &[]);
}

/// Register `reg0198` at 0x0318: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_fmt`.
pub mod reg0198 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_fmt",
        198,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ALPHA_SWAP: Field = Field::new(Region::Class(1), 198, 0, 1, false, true, &[]);
    pub const RBUV_SWAP: Field = Field::new(Region::Class(1), 198, 1, 1, false, true, &[]);
    pub const SRC_CFMT: Field = Field::new(Region::Class(1), 198, 2, 4, false, true, &[]);
    pub const SRC_RANGE: Field = Field::new(Region::Class(1), 198, 6, 1, false, true, &[]);
    pub const OUT_FMT: Field = Field::new(Region::Class(1), 198, 7, 1, false, true, &[]);
}

/// Register `reg0199` at 0x031c: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_udfy`.
pub mod reg0199 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_udfy",
        199,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CSC_WGT_B2Y: Field = Field::new(Region::Class(1), 199, 0, 9, false, true, &[]);
    pub const CSC_WGT_G2Y: Field = Field::new(Region::Class(1), 199, 9, 9, false, true, &[]);
    pub const CSC_WGT_R2Y: Field = Field::new(Region::Class(1), 199, 18, 9, false, true, &[]);
}

/// Register `reg0200` at 0x0320: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_udfu`.
pub mod reg0200 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_udfu",
        200,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CSC_WGT_B2U: Field = Field::new(Region::Class(1), 200, 0, 9, false, true, &[]);
    pub const CSC_WGT_G2U: Field = Field::new(Region::Class(1), 200, 9, 9, false, true, &[]);
    pub const CSC_WGT_R2U: Field = Field::new(Region::Class(1), 200, 18, 9, false, true, &[]);
}

/// Register `reg0201` at 0x0324: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_udfv`.
pub mod reg0201 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_udfv",
        201,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CSC_WGT_B2V: Field = Field::new(Region::Class(1), 201, 0, 9, false, true, &[]);
    pub const CSC_WGT_G2V: Field = Field::new(Region::Class(1), 201, 9, 9, false, true, &[]);
    pub const CSC_WGT_R2V: Field = Field::new(Region::Class(1), 201, 18, 9, false, true, &[]);
}

/// Register `reg0202` at 0x0328: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_udfo`.
pub mod reg0202 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_udfo",
        202,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CSC_OFST_V: Field = Field::new(Region::Class(1), 202, 0, 8, false, true, &[]);
    pub const CSC_OFST_U: Field = Field::new(Region::Class(1), 202, 8, 8, false, true, &[]);
    pub const CSC_OFST_Y: Field = Field::new(Region::Class(1), 202, 16, 5, false, true, &[]);
}

/// Register `reg0203` at 0x032c: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_proc`.
pub mod reg0203 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_proc",
        203,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SRC_MIRR: Field = Field::new(Region::Class(1), 203, 26, 1, false, true, &[]);
    pub const SRC_ROT: Field = Field::new(Region::Class(1), 203, 27, 2, false, true, &[]);
    pub const TXA_EN: Field = Field::new(Region::Class(1), 203, 29, 1, false, true, &[]);
    pub const AFBCD_EN: Field = Field::new(Region::Class(1), 203, 30, 1, false, true, &[]);
}

/// Register `reg0204` at 0x0330: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.pic_ofst`.
pub mod reg0204 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.pic_ofst",
        204,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PIC_OFST_X: Field = Field::new(Region::Class(1), 204, 0, 14, false, true, &[]);
    pub const PIC_OFST_Y: Field = Field::new(Region::Class(1), 204, 16, 14, false, true, &[]);
}

/// Register `reg0205` at 0x0334: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_strd0`.
pub mod reg0205 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_strd0",
        205,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SRC_STRD0: Field = Field::new(Region::Class(1), 205, 0, 17, false, true, &[]);
}

/// Register `reg0206` at 0x0338: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.src_strd1`.
pub mod reg0206 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.src_strd1",
        206,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SRC_STRD1: Field = Field::new(Region::Class(1), 206, 0, 16, false, true, &[]);
}

/// Register `reg0212` at 0x0350: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.rc_cfg`.
pub mod reg0212 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.rc_cfg",
        212,
        1,
        Access::ReadWrite,
        1,
    );
    pub const RC_EN: Field = Field::new(Region::Class(1), 212, 0, 1, false, true, &[]);
    pub const AQ_EN: Field = Field::new(Region::Class(1), 212, 1, 1, false, true, &[]);
    pub const AQ_MODE: Field = Field::new(Region::Class(1), 212, 2, 1, false, true, &[]);
    pub const RC_CTU_NUM: Field = Field::new(Region::Class(1), 212, 12, 20, false, true, &[]);
}

/// Register `reg0213` at 0x0354: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.rc_qp`.
pub mod reg0213 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.rc_qp",
        213,
        1,
        Access::ReadWrite,
        1,
    );
    pub const RC_QP_RANGE: Field = Field::new(Region::Class(1), 213, 16, 4, false, true, &[]);
    pub const RC_MAX_QP: Field = Field::new(Region::Class(1), 213, 20, 6, false, true, &[]);
    pub const RC_MIN_QP: Field = Field::new(Region::Class(1), 213, 26, 6, false, true, &[]);
}

/// Register `reg0214` at 0x0358: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.rc_tgt`.
pub mod reg0214 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.rc_tgt",
        214,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CTU_EBIT: Field = Field::new(Region::Class(1), 214, 0, 20, false, true, &[]);
}

/// Register `reg0216` at 0x0360: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.sli_splt`.
pub mod reg0216 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.sli_splt",
        216,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SLI_SPLT: Field = Field::new(Region::Class(1), 216, 0, 1, false, true, &[]);
    pub const SLI_SPLT_MODE: Field = Field::new(Region::Class(1), 216, 1, 1, false, true, &[]);
    pub const SLI_SPLT_CPST: Field = Field::new(Region::Class(1), 216, 2, 1, false, true, &[]);
    pub const SLI_FLSH: Field = Field::new(Region::Class(1), 216, 15, 1, false, true, &[]);
    pub const SLI_MAX_NUM_M1: Field = Field::new(Region::Class(1), 216, 16, 15, false, true, &[]);
}

/// Register `reg0217` at 0x0364: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.sli_byte`.
pub mod reg0217 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.sli_byte",
        217,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SLI_SPLT_BYTE: Field = Field::new(Region::Class(1), 217, 0, 20, false, true, &[]);
}

/// Register `reg0218` at 0x0368: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.sli_cnum`.
pub mod reg0218 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.sli_cnum",
        218,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SLI_SPLT_CNUM_M1: Field = Field::new(Region::Class(1), 218, 0, 20, false, true, &[]);
}

/// Register `reg0220` at 0x0370: 1 word, ReadWrite.
///
/// The C path is `reg_base.common.me_rnge`.
pub mod reg0220 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.common.me_rnge",
        220,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CME_SRCH_H: Field = Field::new(Region::Class(1), 220, 0, 4, false, true, &[]);
    pub const CME_SRCH_V: Field = Field::new(Region::Class(1), 220, 4, 4, false, true, &[]);
    pub const RME_SRCH_H: Field = Field::new(Region::Class(1), 220, 8, 3, false, true, &[]);
    pub const RME_SRCH_V: Field = Field::new(Region::Class(1), 220, 11, 3, false, true, &[]);
    pub const DLT_FRM_NUM: Field = Field::new(Region::Class(1), 220, 16, 16, false, true, &[]);
}

/// Register `reg0221` at 0x0374: 1 word, ReadWrite.
///
/// The C path is `reg_base.me_cfg`.
pub mod reg0221 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.me_cfg",
        221,
        1,
        Access::ReadWrite,
        1,
    );
    pub const PMV_MDST_H: Field = Field::new(Region::Class(1), 221, 0, 8, false, true, &[]);
    pub const PMV_MDST_V: Field = Field::new(Region::Class(1), 221, 8, 8, false, true, &[]);
    pub const MV_LIMIT: Field = Field::new(Region::Class(1), 221, 16, 2, false, true, &[]);
    pub const PMV_NUM: Field = Field::new(Region::Class(1), 221, 18, 2, false, true, &[]);
    pub const COLMV_STOR: Field = Field::new(Region::Class(1), 221, 20, 1, false, true, &[]);
    pub const COLMV_LOAD: Field = Field::new(Region::Class(1), 221, 21, 1, false, true, &[]);
    pub const RME_DIS: Field = Field::new(Region::Class(1), 221, 22, 3, false, true, &[]);
    pub const FME_DIS: Field = Field::new(Region::Class(1), 221, 27, 3, false, true, &[]);
    pub const LVL4_OVRD_EN: Field = Field::new(Region::Class(1), 221, 31, 1, false, true, &[]);
}

/// Register `reg0222` at 0x0378: 1 word, ReadWrite.
///
/// The C path is `reg_base.me_cach`.
pub mod reg0222 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.me_cach",
        222,
        1,
        Access::ReadWrite,
        1,
    );
    pub const CME_RAMA_MAX: Field = Field::new(Region::Class(1), 222, 0, 11, false, true, &[]);
    pub const CME_RAMA_H: Field = Field::new(Region::Class(1), 222, 11, 5, false, true, &[]);
    pub const CACH_L2_TAG: Field = Field::new(Region::Class(1), 222, 16, 2, false, true, &[]);
    pub const CME_LINEBUF_W: Field = Field::new(Region::Class(1), 222, 18, 9, false, true, &[]);
}

/// Register `reg0224` at 0x0380: 1 word, ReadWrite.
///
/// The C path is `reg_base.gmv`.
pub mod reg0224 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.gmv",
        224,
        1,
        Access::ReadWrite,
        1,
    );
    pub const GMV_X: Field = Field::new(Region::Class(1), 224, 0, 13, false, true, &[]);
    pub const GMV_Y: Field = Field::new(Region::Class(1), 224, 16, 13, false, true, &[]);
}

/// Register `reg0228` at 0x0390: 1 word, ReadWrite.
///
/// The C path is `reg_base.roi_en`.
pub mod reg0228 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.roi_en",
        228,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ROI_QP_EN: Field = Field::new(Region::Class(1), 228, 0, 1, false, true, &[]);
    pub const ROI_AMV_EN: Field = Field::new(Region::Class(1), 228, 1, 1, false, true, &[]);
    pub const ROI_MV_EN: Field = Field::new(Region::Class(1), 228, 2, 1, false, true, &[]);
}

/// Register `reg0232` at 0x03a0: 1 word, ReadWrite.
///
/// The C path is `reg_base.rdo_cfg`.
pub mod reg0232 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.rdo_cfg",
        232,
        1,
        Access::ReadWrite,
        1,
    );
    pub const RECT_SIZE: Field = Field::new(Region::Class(1), 232, 0, 1, false, true, &[]);
    pub const INTER_4X4: Field = Field::new(Region::Class(1), 232, 1, 1, false, true, &[]);
    pub const ARB_SEL: Field = Field::new(Region::Class(1), 232, 2, 1, false, true, &[]);
    pub const VLC_LMT: Field = Field::new(Region::Class(1), 232, 3, 1, false, true, &[]);
    pub const CHRM_SPCL: Field = Field::new(Region::Class(1), 232, 4, 1, false, true, &[]);
    pub const RDO_MASK: Field = Field::new(Region::Class(1), 232, 5, 8, false, true, &[]);
    pub const CCWA_E: Field = Field::new(Region::Class(1), 232, 13, 1, false, true, &[]);
    pub const ATR_E: Field = Field::new(Region::Class(1), 232, 15, 1, false, true, &[]);
    pub const ATF_INTRA_E: Field = Field::new(Region::Class(1), 232, 19, 1, false, true, &[]);
    pub const SCL_LST_SEL: Field = Field::new(Region::Class(1), 232, 20, 2, false, true, &[]);
}

/// Register `reg0233` at 0x03a4: 1 word, ReadWrite.
///
/// The C path is `reg_base.iprd_csts`.
pub mod reg0233 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.iprd_csts",
        233,
        1,
        Access::ReadWrite,
        1,
    );
    pub const VTHD_Y: Field = Field::new(Region::Class(1), 233, 0, 12, false, true, &[]);
    pub const VTHD_C: Field = Field::new(Region::Class(1), 233, 16, 12, false, true, &[]);
}

/// Register `reg0236` at 0x03b0: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_nal`.
pub mod reg0236 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_nal",
        236,
        1,
        Access::ReadWrite,
        1,
    );
    pub const NAL_REF_IDC: Field = Field::new(Region::Class(1), 236, 0, 2, false, true, &[]);
    pub const NAL_UNIT_TYPE: Field = Field::new(Region::Class(1), 236, 2, 5, false, true, &[]);
}

/// Register `reg0237` at 0x03b4: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_sps`.
pub mod reg0237 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_sps",
        237,
        1,
        Access::ReadWrite,
        1,
    );
    pub const MAX_FNUM: Field = Field::new(Region::Class(1), 237, 0, 4, false, true, &[]);
    pub const DRCT_8X8: Field = Field::new(Region::Class(1), 237, 4, 1, false, true, &[]);
    pub const MPOC_LM4: Field = Field::new(Region::Class(1), 237, 5, 4, false, true, &[]);
}

/// Register `reg0238` at 0x03b8: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_pps`.
pub mod reg0238 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_pps",
        238,
        1,
        Access::ReadWrite,
        1,
    );
    pub const ETPY_MODE: Field = Field::new(Region::Class(1), 238, 0, 1, false, true, &[]);
    pub const TRNS_8X8: Field = Field::new(Region::Class(1), 238, 1, 1, false, true, &[]);
    pub const CSIP_FLAG: Field = Field::new(Region::Class(1), 238, 2, 1, false, true, &[]);
    pub const NUM_REF0_IDX: Field = Field::new(Region::Class(1), 238, 3, 2, false, true, &[]);
    pub const NUM_REF1_IDX: Field = Field::new(Region::Class(1), 238, 5, 2, false, true, &[]);
    pub const PIC_INIT_QP: Field = Field::new(Region::Class(1), 238, 7, 6, false, true, &[]);
    pub const CB_OFST: Field = Field::new(Region::Class(1), 238, 13, 5, false, true, &[]);
    pub const CR_OFST: Field = Field::new(Region::Class(1), 238, 18, 5, false, true, &[]);
    pub const WGHT_PRED: Field = Field::new(Region::Class(1), 238, 23, 1, false, true, &[]);
    pub const DBF_CP_FLG: Field = Field::new(Region::Class(1), 238, 24, 1, false, true, &[]);
}

/// Register `reg0239` at 0x03bc: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_sli0`.
pub mod reg0239 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_sli0",
        239,
        1,
        Access::ReadWrite,
        1,
    );
    pub const SLI_TYPE: Field = Field::new(Region::Class(1), 239, 0, 2, false, true, &[]);
    pub const PPS_ID: Field = Field::new(Region::Class(1), 239, 2, 8, false, true, &[]);
    pub const DRCT_SMVP: Field = Field::new(Region::Class(1), 239, 10, 1, false, true, &[]);
    pub const NUM_REF_OVRD: Field = Field::new(Region::Class(1), 239, 11, 1, false, true, &[]);
    pub const CBC_INIT_IDC: Field = Field::new(Region::Class(1), 239, 12, 2, false, true, &[]);
    pub const FRM_NUM: Field = Field::new(Region::Class(1), 239, 16, 16, false, true, &[]);
}

/// Register `reg0240` at 0x03c0: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_sli1`.
pub mod reg0240 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_sli1",
        240,
        1,
        Access::ReadWrite,
        1,
    );
    pub const IDR_PID: Field = Field::new(Region::Class(1), 240, 0, 16, false, true, &[]);
    pub const POC_LSB: Field = Field::new(Region::Class(1), 240, 16, 16, false, true, &[]);
}

/// Register `reg0241` at 0x03c4: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_sli2`.
pub mod reg0241 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_sli2",
        241,
        1,
        Access::ReadWrite,
        1,
    );
    pub const RODR_PIC_IDX: Field = Field::new(Region::Class(1), 241, 0, 2, false, true, &[]);
    pub const REF_LIST0_RODR: Field = Field::new(Region::Class(1), 241, 2, 1, false, true, &[]);
    pub const SLI_BETA_OFST: Field = Field::new(Region::Class(1), 241, 3, 4, false, true, &[]);
    pub const SLI_ALPH_OFST: Field = Field::new(Region::Class(1), 241, 7, 4, false, true, &[]);
    pub const DIS_DBLK_IDC: Field = Field::new(Region::Class(1), 241, 11, 2, false, true, &[]);
    pub const RODR_PIC_NUM: Field = Field::new(Region::Class(1), 241, 16, 16, false, true, &[]);
}

/// Register `reg0242` at 0x03c8: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_refm0`.
pub mod reg0242 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_refm0",
        242,
        1,
        Access::ReadWrite,
        1,
    );
    pub const NOPP_FLG: Field = Field::new(Region::Class(1), 242, 0, 1, false, true, &[]);
    pub const LTRF_FLG: Field = Field::new(Region::Class(1), 242, 1, 1, false, true, &[]);
    pub const ARPM_FLG: Field = Field::new(Region::Class(1), 242, 2, 1, false, true, &[]);
    pub const MMCO4_PRE: Field = Field::new(Region::Class(1), 242, 3, 1, false, true, &[]);
    pub const MMCO_TYPE0: Field = Field::new(Region::Class(1), 242, 4, 3, false, true, &[]);
    pub const MMCO_PARM0: Field = Field::new(Region::Class(1), 242, 7, 16, false, true, &[]);
    pub const MMCO_TYPE1: Field = Field::new(Region::Class(1), 242, 23, 3, false, true, &[]);
    pub const MMCO_TYPE2: Field = Field::new(Region::Class(1), 242, 26, 3, false, true, &[]);
}

/// Register `reg0243` at 0x03cc: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_refm1`.
pub mod reg0243 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_refm1",
        243,
        1,
        Access::ReadWrite,
        1,
    );
    pub const MMCO_PARM1: Field = Field::new(Region::Class(1), 243, 0, 16, false, true, &[]);
    pub const MMCO_PARM2: Field = Field::new(Region::Class(1), 243, 16, 16, false, true, &[]);
}

/// Register `reg0244` at 0x03d0: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_refm2`.
pub mod reg0244 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_refm2",
        244,
        1,
        Access::ReadWrite,
        1,
    );
    pub const LONG_TERM_FRAME_IDX0: Field =
        Field::new(Region::Class(1), 244, 0, 4, false, true, &[]);
    pub const LONG_TERM_FRAME_IDX1: Field =
        Field::new(Region::Class(1), 244, 4, 4, false, true, &[]);
    pub const LONG_TERM_FRAME_IDX2: Field =
        Field::new(Region::Class(1), 244, 8, 4, false, true, &[]);
}

/// Register `reg0245` at 0x03d4: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_refm3_hevc`.
pub mod reg0245 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_refm3_hevc",
        245,
        1,
        Access::ReadWrite,
        1,
    );
    pub const DLT_POC_S0_M12: Field = Field::new(Region::Class(1), 245, 0, 16, false, true, &[]);
    pub const DLT_POC_S0_M13: Field = Field::new(Region::Class(1), 245, 16, 16, false, true, &[]);
}

/// Register `reg0246` at 0x03d8: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_long_refm0`.
pub mod reg0246 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_long_refm0",
        246,
        1,
        Access::ReadWrite,
        1,
    );
    pub const POC_LSB_LT1: Field = Field::new(Region::Class(1), 246, 0, 16, false, true, &[]);
    pub const POC_LSB_LT2: Field = Field::new(Region::Class(1), 246, 16, 16, false, true, &[]);
}

/// Register `reg0247` at 0x03dc: 1 word, ReadWrite.
///
/// The C path is `reg_base.synt_long_refm1`.
pub mod reg0247 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.synt_long_refm1",
        247,
        1,
        Access::ReadWrite,
        1,
    );
    pub const DLT_POC_MSB_CYCL1: Field = Field::new(Region::Class(1), 247, 0, 16, false, true, &[]);
    pub const DLT_POC_MSB_CYCL2: Field =
        Field::new(Region::Class(1), 247, 16, 16, false, true, &[]);
}

/// Register `reg0252` at 0x03f0: 1 word, ReadWrite.
///
/// The C path is `reg_base.sli_cfg`.
pub mod reg0252 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.sli_cfg",
        252,
        1,
        Access::ReadWrite,
        1,
    );
    pub const MV_V_LMT_THD: Field = Field::new(Region::Class(1), 252, 0, 14, false, true, &[]);
    pub const MV_V_LMT_EN: Field = Field::new(Region::Class(1), 252, 15, 1, false, true, &[]);
    pub const SLI_CRS_EN: Field = Field::new(Region::Class(1), 252, 31, 1, false, true, &[]);
}

/// Register `reg0253` at 0x03f4: 1 word, ReadWrite.
///
/// The C path is `reg_base.tile_pos`.
pub mod reg0253 {
    use crate::layout::*;

    /// The register itself.
    pub const REGISTER: Register = Register::new(
        Region::Class(1),
        "reg_base.tile_pos",
        253,
        1,
        Access::ReadWrite,
        1,
    );
    pub const TILE_X: Field = Field::new(Region::Class(1), 253, 0, 8, false, true, &[]);
    pub const TILE_Y: Field = Field::new(Region::Class(1), 253, 16, 8, false, true, &[]);
}
