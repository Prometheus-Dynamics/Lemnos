//! The JPEG blocks' register files, as far as the vendor kernel knows them.
//!
//! Transcribed from rockchip-linux/kernel `develop-6.1` at commit
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`, `drivers/video/rockchip/mpp/mpp_jpgdec.c` and
//! `mpp_jpgenc.c` (`GPL-2.0+ OR MIT`). No code is copied.
//!
//! A job on either block is a whole register file: the kernel copies it from the caller, turns the
//! registers of its address table from descriptors into bus addresses, writes the file to the
//! block and starts it. The kernel names only the registers it touches itself; which of the
//! others set the picture's size, strides and formats, and so how far the block reads and writes
//! from each address, is not in its source.

/// Which block, and which generation of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `rockchip,rkv-jpeg-decoder-v1`.
    DecoderV1,
    /// `rockchip,rkv-jpeg-decoder-v2` (the VPU730 decoder).
    DecoderV2,
    /// `rockchip,rkv-jpeg-encoder-v1`.
    EncoderV1,
    /// `rockchip,rkv-jpeg-encoder-v2` (the VPU730 encoder).
    EncoderV2,
}

impl Variant {
    /// Every variant.
    pub const ALL: [Self; 4] = [
        Self::DecoderV1,
        Self::DecoderV2,
        Self::EncoderV1,
        Self::EncoderV2,
    ];

    /// The device tree compatible string the vendor driver binds.
    #[must_use]
    pub const fn compatible(self) -> &'static str {
        match self {
            Self::DecoderV1 => "rockchip,rkv-jpeg-decoder-v1",
            Self::DecoderV2 => "rockchip,rkv-jpeg-decoder-v2",
            Self::EncoderV1 => "rockchip,rkv-jpeg-encoder-v1",
            Self::EncoderV2 => "rockchip,rkv-jpeg-encoder-v2",
        }
    }

    /// The variant a compatible string names.
    #[must_use]
    pub fn from_compatible(compatible: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.compatible() == compatible)
    }

    /// The service device type a session binds to.
    #[must_use]
    pub const fn device_type(self) -> u32 {
        match self {
            Self::DecoderV1 | Self::DecoderV2 => crate::mpp::DEVICE_RKJPEGD,
            Self::EncoderV1 | Self::EncoderV2 => crate::mpp::DEVICE_RKJPEGE,
        }
    }

    /// Registers in the file (`reg_num`); a job's reads and writes stay inside it.
    #[must_use]
    pub const fn registers(self) -> u32 {
        match self {
            Self::DecoderV1 => dec::REG_NUM_V1,
            Self::DecoderV2 => dec::REG_NUM_V2,
            Self::EncoderV1 => enc::REG_NUM_V1,
            Self::EncoderV2 => enc::REG_NUM_V2,
        }
    }

    /// The registers whose descriptors the kernel turns into addresses (its `trans_tbl`).
    #[must_use]
    pub const fn address_registers(self) -> &'static [u32] {
        match self {
            Self::DecoderV1 | Self::DecoderV2 => &dec::ADDRESS_REGISTERS,
            Self::EncoderV1 | Self::EncoderV2 => &enc::ADDRESS_REGISTERS,
        }
    }

    /// The register that starts the block, which the kernel writes last itself (`reg_en`).
    #[must_use]
    pub const fn start_register(self) -> u32 {
        match self {
            Self::DecoderV1 => dec::INT_EN_INDEX,
            Self::DecoderV2 => dec::V2_START_EN_INDEX,
            Self::EncoderV1 | Self::EncoderV2 => enc::START_EN_INDEX,
        }
    }
}

/// The decoder (`mpp_jpgdec.c`).
pub mod dec {
    /// `JPGDEC_REG_NUM`: registers of the v1 file.
    pub const REG_NUM_V1: u32 = 42;
    /// `JPGDEC_VPU730_REG_NUM`: registers of the v2 file.
    pub const REG_NUM_V2: u32 = 70;
    /// `JPGDEC_HWID_VPU720`.
    pub const HWID_VPU720: u32 = 0xdb1f_0006;
    /// `JPGDEC_HWID_VPU730`.
    pub const HWID_VPU730: u32 = 0xdb1f_0100;
    /// `JPGDEC_REG_HW_ID_INDEX`.
    pub const HW_ID_INDEX: u32 = 0;
    /// `JPGDEC_REG_INT_EN_INDEX` (byte offset `0x004`): start, interrupt enables and status on
    /// v1. Read back, it holds the job's interrupt status.
    pub const INT_EN_INDEX: u32 = 1;
    /// `JPGDEC_REG_SYS_BASE`.
    pub const SYS_BASE: u32 = 0x008;
    /// `JPGDEC_REG_PIC_INFO_INDEX` (`0x00c`): width minus one in bits 0..16, height minus one in
    /// bits 16..32.
    pub const PIC_INFO_INDEX: u32 = 3;
    /// `JPGDEC_REG_STREAM_RLC_BASE_INDEX` (`0x030`): the stream's address. Read back, it holds
    /// the bytes of stream the block consumed, shifted left by 10.
    pub const STREAM_RLC_BASE_INDEX: u32 = 12;
    /// `JPGDEC_REG_PERF_WORKING_CNT`.
    pub const PERF_WORKING_CNT: u32 = 0x9c;
    /// `trans_tbl_jpgdec`: the address registers, on both versions.
    pub const ADDRESS_REGISTERS: [u32; 5] = [9, 10, 11, 12, 13];

    /// `JPGDEC_START_EN`.
    pub const START_EN: u32 = 1 << 0;
    /// `JPGDEC_IRQ_DIS`.
    pub const IRQ_DIS: u32 = 1 << 1;
    /// `JPGDEC_TIMEOUT_EN`.
    pub const TIMEOUT_EN: u32 = 1 << 2;
    /// `JPGDEC_BUF_EMPTY_EN`.
    pub const BUF_EMPTY_EN: u32 = 1 << 3;
    /// `JPGDEC_BUF_EMPTY_RELOAD_EN`.
    pub const BUF_EMPTY_RELOAD_EN: u32 = 1 << 4;
    /// `JPGDEC_SOFT_REST_EN`.
    pub const SOFT_REST_EN: u32 = 1 << 5;
    /// `JPGDEC_IRQ_RAW`.
    pub const IRQ_RAW: u32 = 1 << 6;
    /// `JPGDEC_WAIT_RESET_EN`.
    pub const WAIT_RESET_EN: u32 = 1 << 7;
    /// `JPGDEC_IRQ`.
    pub const IRQ: u32 = 1 << 8;
    /// `JPGDEC_REDAY_STA`: the frame decoded.
    pub const READY_STA: u32 = 1 << 9;
    /// `JPGDEC_BUS_STA`.
    pub const BUS_STA: u32 = 1 << 10;
    /// `JPGDEC_ERROR_STA`.
    pub const ERROR_STA: u32 = 1 << 11;
    /// `JPGDEC_TIMEOUT_STA`.
    pub const TIMEOUT_STA: u32 = 1 << 12;
    /// `JPGDEC_BUF_EMPTY_STA`.
    pub const BUF_EMPTY_STA: u32 = 1 << 13;
    /// `JPGDEC_SOFT_RSET_READY`.
    pub const SOFT_RSET_READY: u32 = 1 << 14;
    /// `JPGDEC_EMPTY_FORCE_END`.
    pub const EMPTY_FORCE_END: u32 = 1 << 15;
    /// `JPGDEC_CARE_STREAM_ERROR_EN`.
    pub const CARE_STREAM_ERROR_EN: u32 = 1 << 16;
    /// The v1 status bits after which the kernel resets the block.
    pub const V1_ERROR_MASK: u32 = BUS_STA | ERROR_STA | TIMEOUT_STA | BUF_EMPTY_STA;

    /// `JPGDEC_VPU730_START_EN_INDEX` (`0x004`): a command in bits 8.. starts the block.
    pub const V2_START_EN_INDEX: u32 = 1;
    /// `JPGDEC_VPU730_REG_INT_EN_IDX` (`0x108`).
    pub const V2_INT_EN_INDEX: u32 = 66;
    /// `JPGDEC_VPU730_REG_INT_MASK_IDX` (`0x10c`).
    pub const V2_INT_MASK_INDEX: u32 = 67;
    /// `JPGDEC_VPU730_REG_INT_CLR_IDX` (`0x110`).
    pub const V2_INT_CLR_INDEX: u32 = 68;
    /// `JPGDEC_VPU730_REG_INT_STA_IDX` (`0x114`). Read back, it holds the job's status.
    pub const V2_INT_STA_INDEX: u32 = 69;
    /// `JPGDEC_VPU730_INT_ST_DEC_DONE`.
    pub const V2_DEC_DONE: u32 = 1 << 0;
    /// `JPGDEC_VPU730_INT_ST_SAFE_RST_DONE`.
    pub const V2_SAFE_RST_DONE: u32 = 1 << 1;
    /// The v2 status bits after which the kernel resets the block (`err_mask`).
    pub const V2_ERROR_MASK: u32 = 0x3c;
    /// `VPU730_CMD_ONE_FRAME`, written to bits 8.. of the start register.
    pub const V2_CMD_ONE_FRAME: u32 = 1;
}

/// The encoder (`mpp_jpgenc.c`).
pub mod enc {
    /// `JPGENC_REG_NUM`: registers of the v1 file.
    pub const REG_NUM_V1: u32 = 79;
    /// `JPGENC_VPU730_REG_NUM`: registers of the v2 file.
    pub const REG_NUM_V2: u32 = 241;
    /// `JPGENC_REG_HW_ID_INDEX`.
    pub const HW_ID_INDEX: u32 = 0;
    /// `JPGENC_REG_START_EN_INDEX` (`0x004`).
    pub const START_EN_INDEX: u32 = 1;
    /// `JPGENC_REG_INT_EN_INDEX` (`0x010`); the kernel sets it to `0xff` itself.
    pub const INT_EN_INDEX: u32 = 4;
    /// `JPGENC_REG_INT_MASK_INDEX` (`0x014`); the kernel sets it to `0xff` itself.
    pub const INT_MASK_INDEX: u32 = 5;
    /// `JPGENC_REG_INT_CLR_INDEX` (`0x018`).
    pub const INT_CLR_INDEX: u32 = 6;
    /// `JPGENC_REG_INT_STATUS_INDEX` (`0x01c`). Read back, it holds the job's status.
    pub const INT_STATUS_INDEX: u32 = 7;
    /// `JPGENC_REG_ENC_RSL_INDEX`: width in 8-pixel units minus one in bits 0..13, height the same
    /// in bits 16..29.
    pub const ENC_RSL_INDEX: u32 = 29;
    /// `JPGENC_REG_PERF_WORKING_CNT`.
    pub const PERF_WORKING_CNT: u32 = 0xe4;
    /// `JPGENC_START_EN`.
    pub const START_EN: u32 = 1 << 8;
    /// `JPGENC_INT_ST_ENC_DONE`.
    pub const ENC_DONE: u32 = 1 << 0;
    /// The status bits after which the kernel resets the block (`err_mask`).
    pub const ERROR_MASK: u32 = 0x340;
    /// `VEPU_CMD_ONE_FRAME`.
    pub const CMD_ONE_FRAME: u32 = 1;

    /// `trans_tbl_jpgenc`: the address registers, on both versions.
    pub const ADDRESS_REGISTERS: [u32; 10] = [
        LINK_TABLE, QTABLE, BS_TOP, BS_BOTTOM, BS_READ, BS_START, ECS_LEN, SRC0, SRC1, SRC2,
    ];
    /// The link table's address: in link mode the block fetches register lists from memory.
    pub const LINK_TABLE: u32 = 9;
    /// The quantisation table's address.
    pub const QTABLE: u32 = 16;
    /// The top of the bitstream buffer.
    pub const BS_TOP: u32 = 17;
    /// The bottom of the bitstream buffer.
    pub const BS_BOTTOM: u32 = 18;
    /// The bitstream read address.
    pub const BS_READ: u32 = 19;
    /// The bitstream start address.
    pub const BS_START: u32 = 20;
    /// The ECS length buffer.
    pub const ECS_LEN: u32 = 21;
    /// The source picture's first plane.
    pub const SRC0: u32 = 22;
    /// The source picture's second plane.
    pub const SRC1: u32 = 23;
    /// The source picture's third plane.
    pub const SRC2: u32 = 24;
}
