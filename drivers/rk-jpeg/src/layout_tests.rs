//! The layouts and numbers against the vendor sources (`rk-mpp.h`, `mpp_common.c`,
//! `mpp_jpgdec.c`, `mpp_jpgenc.c`), sizes and offsets as `gcc` gives them on LP64.

use std::mem::{offset_of, size_of};

use crate::mpp::{self, Msg, OffsetElem};
use crate::regs::{Variant, dec, enc};

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
    assert_eq!(mpp::IOC_CFG_V1, 0x4004_7601);
    assert_eq!(
        [
            mpp::CMD_QUERY_HW_SUPPORT,
            mpp::CMD_QUERY_HW_ID,
            mpp::CMD_INIT_CLIENT_TYPE,
            mpp::CMD_INIT_TRANS_TABLE,
            mpp::CMD_SET_REG_WRITE,
            mpp::CMD_SET_REG_READ,
            mpp::CMD_SET_REG_ADDR_OFFSET,
            mpp::CMD_POLL_HW_FINISH,
            mpp::CMD_RESET_SESSION,
            mpp::CMD_RELEASE_FD,
        ],
        [0, 1, 0x100, 0x102, 0x200, 0x201, 0x202, 0x300, 0x400, 0x402]
    );
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
    assert_eq!((mpp::DEVICE_RKJPEGD, mpp::DEVICE_RKJPEGE), (13, 20));
    assert_eq!((mpp::MAX_MSGS, mpp::MAX_OFFSETS), (16, 80));
}

#[test]
fn decoder_register_file() {
    assert_eq!((dec::REG_NUM_V1, dec::REG_NUM_V2), (42, 70));
    assert_eq!(
        (dec::HWID_VPU720, dec::HWID_VPU730),
        (0xdb1f_0006, 0xdb1f_0100)
    );
    assert_eq!(dec::HW_ID_INDEX, 0);
    assert_eq!(dec::INT_EN_INDEX * 4, 0x004);
    assert_eq!(dec::SYS_BASE, 0x008);
    assert_eq!(dec::PIC_INFO_INDEX * 4, 0x00c);
    assert_eq!(dec::STREAM_RLC_BASE_INDEX * 4, 0x030);
    assert_eq!(dec::PERF_WORKING_CNT, 0x9c);
    assert_eq!(dec::ADDRESS_REGISTERS, [9, 10, 11, 12, 13]);
    // Bits 0 to 16 of the v1 control register, in order.
    assert_eq!(
        [
            dec::START_EN,
            dec::IRQ_DIS,
            dec::TIMEOUT_EN,
            dec::BUF_EMPTY_EN,
            dec::BUF_EMPTY_RELOAD_EN,
            dec::SOFT_REST_EN,
            dec::IRQ_RAW,
            dec::WAIT_RESET_EN,
            dec::IRQ,
            dec::READY_STA,
            dec::BUS_STA,
            dec::ERROR_STA,
            dec::TIMEOUT_STA,
            dec::BUF_EMPTY_STA,
            dec::SOFT_RSET_READY,
            dec::EMPTY_FORCE_END,
            dec::CARE_STREAM_ERROR_EN,
        ],
        std::array::from_fn(|bit| 1 << bit)
    );
    assert_eq!(dec::V1_ERROR_MASK, 0x3c00);
    assert_eq!(dec::V2_START_EN_INDEX * 4, 0x004);
    assert_eq!(
        [
            dec::V2_INT_EN_INDEX,
            dec::V2_INT_MASK_INDEX,
            dec::V2_INT_CLR_INDEX,
            dec::V2_INT_STA_INDEX
        ]
        .map(|i| i * 4),
        [0x108, 0x10c, 0x110, 0x114]
    );
    assert_eq!(dec::V2_INT_STA_INDEX, dec::REG_NUM_V2 - 1);
    assert_eq!(
        (dec::V2_DEC_DONE, dec::V2_SAFE_RST_DONE, dec::V2_ERROR_MASK),
        (1, 2, 0x3c)
    );
    assert_eq!(dec::V2_CMD_ONE_FRAME, 1);
}

#[test]
fn encoder_register_file() {
    assert_eq!((enc::REG_NUM_V1, enc::REG_NUM_V2), (79, 241));
    assert_eq!(enc::HW_ID_INDEX, 0);
    assert_eq!(
        [
            enc::START_EN_INDEX,
            enc::INT_EN_INDEX,
            enc::INT_MASK_INDEX,
            enc::INT_CLR_INDEX,
            enc::INT_STATUS_INDEX
        ]
        .map(|i| i * 4),
        [0x4, 0x10, 0x14, 0x18, 0x1c]
    );
    assert_eq!(enc::ENC_RSL_INDEX, 29);
    assert_eq!(enc::PERF_WORKING_CNT, 0xe4);
    assert_eq!(
        (enc::START_EN, enc::ENC_DONE, enc::ERROR_MASK),
        (0x100, 1, 0x340)
    );
    assert_eq!(enc::CMD_ONE_FRAME, 1);
    assert_eq!(
        enc::ADDRESS_REGISTERS,
        [9, 16, 17, 18, 19, 20, 21, 22, 23, 24]
    );
}

#[test]
fn variants() {
    for v in Variant::ALL {
        assert_eq!(Variant::from_compatible(v.compatible()), Some(v));
        assert!(
            v.address_registers()
                .iter()
                .all(|&r| r < v.registers() && r != v.start_register())
        );
    }
    assert_eq!(Variant::from_compatible("rockchip,iep-v2"), None);
    assert_eq!(Variant::ALL.map(Variant::registers), [42, 70, 79, 241]);
    assert_eq!(Variant::ALL.map(Variant::device_type), [13, 13, 20, 20]);
    assert_eq!(Variant::ALL.map(Variant::start_register), [1, 1, 1, 1]);
}
