//! The IEP2 block's registers, as the vendor driver programs them from a job's parameters.
//!
//! Transcribed from rockchip-linux/kernel `develop-6.1` at commit
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`, `drivers/video/rockchip/mpp/rockchip_iep2_regs.h`
//! and `mpp_iep2.c` (`GPL-2.0+ OR MIT`). A caller never writes these: the kernel builds them from
//! [`Params`](crate::Params). They are here for the address table the kernel keys offsets by, for
//! the field widths the job checks lean on, and for reading the block on a board.

/// `IEP2_REG_FRM_START`.
pub const FRM_START: u32 = 0x0000;
/// `IEP2_REG_IEP_CONFIG0`: formats and chroma order.
pub const IEP_CONFIG0: u32 = 0x0004;
/// `IEP2_REG_WORK_MODE`.
pub const WORK_MODE: u32 = 0x0008;
/// `IEP2_REG_INT_EN`.
pub const INT_EN: u32 = 0x0020;
/// `IEP2_REG_INT_STS`.
pub const INT_STS: u32 = 0x0028;
/// `IEP2_REG_VIR_SRC_IMG_WIDTH`: luma stride in bits 0..16, chroma stride in bits 16..32.
pub const VIR_SRC_IMG_WIDTH: u32 = 0x0030;
/// `IEP2_REG_VIR_DST_IMG_WIDTH`: output stride in bits 0..16.
pub const VIR_DST_IMG_WIDTH: u32 = 0x0034;
/// `IEP2_REG_SRC_IMG_SIZE`: width minus one in bits 0..11, height minus one in bits 16..27.
pub const SRC_IMG_SIZE: u32 = 0x0038;
/// `IEP2_REG_DIL_CONFIG0`.
pub const DIL_CONFIG0: u32 = 0x0040;
/// `IEP2_REG_TIMEOUT_CFG`.
pub const TIMEOUT_CFG: u32 = 0x0050;
/// `IEP2_REG_SRC_ADDR_CURY`.
pub const SRC_ADDR_CURY: u32 = 0x0060;
/// `IEP2_REG_SRC_ADDR_NXTY`.
pub const SRC_ADDR_NXTY: u32 = 0x0064;
/// `IEP2_REG_SRC_ADDR_PREY`.
pub const SRC_ADDR_PREY: u32 = 0x0068;
/// `IEP2_REG_SRC_ADDR_CURUV`.
pub const SRC_ADDR_CURUV: u32 = 0x006c;
/// `IEP2_REG_SRC_ADDR_CURV`.
pub const SRC_ADDR_CURV: u32 = 0x0070;
/// `IEP2_REG_SRC_ADDR_NXTUV`.
pub const SRC_ADDR_NXTUV: u32 = 0x0074;
/// `IEP2_REG_SRC_ADDR_NXTV`.
pub const SRC_ADDR_NXTV: u32 = 0x0078;
/// `IEP2_REG_SRC_ADDR_PREUV`.
pub const SRC_ADDR_PREUV: u32 = 0x007c;
/// `IEP2_REG_SRC_ADDR_PREV`.
pub const SRC_ADDR_PREV: u32 = 0x0080;
/// `IEP2_REG_SRC_ADDR_MD`: motion data read; the same buffer is written through `DST_ADDR_MD`.
pub const SRC_ADDR_MD: u32 = 0x0084;
/// `IEP2_REG_SRC_ADDR_MV`: motion vectors read; written through `DST_ADDR_MV`.
pub const SRC_ADDR_MV: u32 = 0x0088;
/// `IEP2_REG_ROI_ADDR`: the kernel's own buffer.
pub const ROI_ADDR: u32 = 0x008c;
/// `IEP2_REG_DST_ADDR_TOPY`.
pub const DST_ADDR_TOPY: u32 = 0x00b0;
/// `IEP2_REG_DST_ADDR_BOTY`.
pub const DST_ADDR_BOTY: u32 = 0x00b4;
/// `IEP2_REG_DST_ADDR_TOPC`.
pub const DST_ADDR_TOPC: u32 = 0x00b8;
/// `IEP2_REG_DST_ADDR_BOTC`.
pub const DST_ADDR_BOTC: u32 = 0x00bc;
/// `IEP2_REG_DST_ADDR_MD`.
pub const DST_ADDR_MD: u32 = 0x00c0;
/// `IEP2_REG_DST_ADDR_MV`.
pub const DST_ADDR_MV: u32 = 0x00c4;
/// `IEP2_REG_OSD_AREA_CONF(0)`: eight registers, one per OSD area.
pub const OSD_AREA_CONF: u32 = 0x0100;
/// `IEP2_REG_DIL_MTN_TAB(0)`: sixteen registers.
pub const DIL_MTN_TAB: u32 = 0x0140;
/// `IEP2_REG_RO_PD_TCNT`: the first result register.
pub const RO_PD_TCNT: u32 = 0x0400;
/// `IEP2_REG_RO_MV_HIST_BIN(0)`: the motion-vector histogram, two bins per register.
pub const RO_MV_HIST_BIN: u32 = 0x0480;

/// `IEP2_REG_FRM_DONE_EN` and the status bit of the same place.
pub const INT_FRM_DONE: u32 = 1 << 0;
/// `IEP2_REG_OSD_MAX_EN`.
pub const INT_OSD_MAX: u32 = 1 << 1;
/// `IEP2_REG_BUS_ERROR_EN`.
pub const INT_BUS_ERROR: u32 = 1 << 4;
/// `IEP2_REG_TIMEOUT_EN`.
pub const INT_TIMEOUT: u32 = 1 << 5;

/// Bits of the width and height fields of `SRC_IMG_SIZE` (`& 0x7ff`).
pub const SIZE_FIELD_BITS: u32 = 11;
/// Bits of each stride field (`& 0xffff`).
pub const STRIDE_FIELD_BITS: u32 = 16;
/// Bits of the OSD area count field (`& 0xf`).
pub const OSD_AREA_NUM_FIELD_BITS: u32 = 4;

/// The register each address word of [`Params`](crate::Params) lands in, by its index (byte
/// offset over four), in the order of the words: `src[0..3]` then `dst[0..2]`, each as `y`,
/// `cbcr`, `cr`, then `mv_addr` and `md_addr` (`iep2_addr_rnum`). `None` is a word the kernel
/// neither translates nor writes (the outputs have no third plane).
///
/// `src[0]` is the current frame, `src[1]` the next and `src[2]` the previous: the kernel's table
/// and its register writes say so, whatever the comment in its structure says.
pub const ADDRESS_REGISTERS: [Option<u32>; ADDRESS_WORDS] = [
    Some(SRC_ADDR_CURY / 4),
    Some(SRC_ADDR_CURUV / 4),
    Some(SRC_ADDR_CURV / 4),
    Some(SRC_ADDR_NXTY / 4),
    Some(SRC_ADDR_NXTUV / 4),
    Some(SRC_ADDR_NXTV / 4),
    Some(SRC_ADDR_PREY / 4),
    Some(SRC_ADDR_PREUV / 4),
    Some(SRC_ADDR_PREV / 4),
    Some(DST_ADDR_TOPY / 4),
    Some(DST_ADDR_TOPC / 4),
    None,
    Some(DST_ADDR_BOTY / 4),
    Some(DST_ADDR_BOTC / 4),
    None,
    Some(SRC_ADDR_MV / 4),
    Some(SRC_ADDR_MD / 4),
];

/// The address words of [`Params`](crate::Params), from `src[0].y` to `md_addr`.
pub const ADDRESS_WORDS: usize = 17;
