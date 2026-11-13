//! The frame path's registers in the ISP8000's window: the input formatter and the ISP core's
//! acquisition and output windows, the data path selector, the main resizer's format conversion,
//! and the second-generation memory interface's main path (`MIV2`, "MP"), which writes frames to
//! memory.
//!
//! # Sources
//!
//! Interface facts only; no code is copied.
//!
//! - The window, `ISP_MIS`, `MIV2_CTRL`, `MIV2_MIS` and `MIV2_ICR` and the main path's frame-end
//!   bits (`MIV2_MIS_MP_FRAME_END_MASK`): `buildroot-overlay/package/vvcam/isp/vvcam_isp_hal.h` of
//!   the Kendryte K230 Linux SDK (`github.com/kendryte/k230_linux_sdk` at
//!   `fc6cc2151d54cb307be47f81815023f7ffb0eae6`; VeriSilicon, dual MIT and GPL, used under MIT).
//! - Every other offset and field: the VeriSilicon ISP8000 register structure and field masks as
//!   NXP publishes them for its own ISP8000 (`github.com/nxp-imx/isp-vvcam` at
//!   `e6453233e67f5d2f180d6ec277158dd54fa67a25`, `vvcam/isp/mrv_all_regs.h` and `mrv_all_bits.h`,
//!   VeriSilicon, MIT), and the order the same repository's `vvcam/isp/isp_miv2.c` and
//!   `isp_ioctl.c` program them in (`set_data_path`, `set_yuv_buffer`, `set_raw_buffer`,
//!   `isp_s_input`, `isp_s_mux`, `isp_start_stream`). The offsets are the struct's own comments,
//!   which match its gap arithmetic.
//!
//! The SDK carries no register map of its ISP's memory interface: the vendor programs it from a
//! binary-only daemon. The NXP map is for the same VeriSilicon block in another chip. Where the
//! two sources overlap they agree: `MIV2_CTRL` at `0x1300`, `MIV2_MIS` at `0x16D0`, `MIV2_ICR` at
//! `0x16D8`, `ISP_MIS` at `0x5C4` (`MRV_ISP_BASE + 0x1C4`), and the main path's frame-end bits.
//! Everything else here is unconfirmed on this chip until the board run
//! (`testing/board-runs/E30.md`) reads it back; constants with a known conflict say so.

/// Bytes of the register window the vendor driver maps (the ISP node's `reg` size,
/// `arch/riscv/boot/dts/canaan/k230.dtsi` of `ruyisdk/linux-xuantie-kernel` at
/// `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`). The same value as the k230isp sys crate's.
pub const WINDOW_BYTES: u32 = lemnos_driver_k230isp_sys::regs::WINDOW_BYTES;

// The input formatter (`MRV_BASE`).

/// Data path control (`vi_dpcl`): the main path's selector and the Y/C splitter's channel mode.
pub const VI_DPCL: u32 = 0x0018;
/// `vi_mp_mux` in [`VI_DPCL`]: what feeds the main path.
pub const VI_DPCL_MP_MUX_MASK: u32 = 0x0000_0003;
/// `MRV_VI_MP_MUX_MP` / `_RAW`: the main resizer to the memory interface, uncompressed (also the
/// raw bypass).
pub const VI_DPCL_MP_MUX_MI: u32 = 0x1;
/// `vi_chan_mode` in [`VI_DPCL`]. CONFLICT: the header has this at bits 3..2, or at bits 14..12
/// when built with `ISPVI_EXPAND_CHAN`; which build this chip is, is not known. Bits 3..2 are
/// used. Unconfirmed.
pub const VI_DPCL_CHAN_MODE_MASK: u32 = 0x0000_000C;
/// `vi_chan_mode` field shift.
pub const VI_DPCL_CHAN_MODE_SHIFT: u32 = 2;
/// `MRV_VI_CHAN_MODE_MP`: the main path (and raw) only.
pub const VI_CHAN_MODE_MP: u32 = 1;

// The ISP core (`MRV_ISP_BASE` = 0x400).

/// Global control (`isp_ctrl`).
pub const ISP_CTRL: u32 = 0x0400;
/// `isp_enable`.
pub const ISP_CTRL_ENABLE: u32 = 0x0000_0001;
/// `isp_mode` (bits 3..1).
pub const ISP_CTRL_MODE_MASK: u32 = 0x0000_000E;
/// `isp_mode` field shift.
pub const ISP_CTRL_MODE_SHIFT: u32 = 1;
/// `MRV_ISP_ISP_MODE_RAW`: raw picture with BT.601 sync, the ISP bypassed.
pub const ISP_MODE_RAW: u32 = 0;
/// `MRV_ISP_ISP_MODE_RGB`: Bayer RGB processing with H and V sync.
pub const ISP_MODE_RGB: u32 = 3;
/// `isp_inform_enable`: the input formatter passes frames on.
pub const ISP_CTRL_INFORM_ENABLE: u32 = 0x0000_0010;
/// `isp_cfg_upd`: the shadow registers take their values now.
pub const ISP_CTRL_CFG_UPD: u32 = 0x0000_0200;
/// `isp_gen_cfg_upd`: the shadow registers take their values at the next frame.
pub const ISP_CTRL_GEN_CFG_UPD: u32 = 0x0000_0400;

/// Acquisition properties (`isp_acq_prop`).
pub const ISP_ACQ_PROP: u32 = 0x0404;
/// `bayer_pat` (bits 4..3): the colour of the first two pixels.
pub const ACQ_BAYER_PAT_MASK: u32 = 0x0000_0018;
/// `bayer_pat` field shift.
pub const ACQ_BAYER_PAT_SHIFT: u32 = 3;
/// `input_selection` (bits 14..12): the input bus width.
pub const ACQ_INPUT_SELECTION_MASK: u32 = 0x0000_7000;
/// `input_selection` field shift.
pub const ACQ_INPUT_SELECTION_SHIFT: u32 = 12;
/// `MRV_ISP_INPUT_SELECTION_12EXT`.
pub const INPUT_12BIT: u32 = 0;
/// `MRV_ISP_INPUT_SELECTION_10ZERO`: 10 bits, two zeroes appended.
pub const INPUT_10BIT: u32 = 1;
/// `MRV_ISP_INPUT_SELECTION_8ZERO`: 8 bits, four zeroes appended.
pub const INPUT_8BIT: u32 = 3;
/// `latency_fifo_selection`: 0 takes the input formatter, 1 the DMA read port. The driver
/// keeps it at 0, as `set_data_path` does.
pub const ACQ_LATENCY_FIFO_MASK: u32 = 0x0000_8000;
/// Horizontal input offset (`isp_acq_h_offs`).
pub const ISP_ACQ_H_OFFS: u32 = 0x0408;
/// Vertical input offset (`isp_acq_v_offs`).
pub const ISP_ACQ_V_OFFS: u32 = 0x040C;
/// Horizontal input size (`isp_acq_h_size`).
pub const ISP_ACQ_H_SIZE: u32 = 0x0410;
/// Vertical input size (`isp_acq_v_size`).
pub const ISP_ACQ_V_SIZE: u32 = 0x0414;
/// Frames to capture (`isp_acq_nr_frames`), 0 for no end.
pub const ISP_ACQ_NR_FRAMES: u32 = 0x0418;
/// `acq_nr_frames` field.
pub const ACQ_NR_FRAMES_MASK: u32 = 0x0000_03FF;
/// Output window (`isp_out_h_offs`, `_v_offs`, `_h_size`, `_v_size`, `MRV_ISP_BASE + 0x194..`).
pub const ISP_OUT_H_OFFS: u32 = 0x0594;
/// See [`ISP_OUT_H_OFFS`].
pub const ISP_OUT_V_OFFS: u32 = 0x0598;
/// See [`ISP_OUT_H_OFFS`].
pub const ISP_OUT_H_SIZE: u32 = 0x059C;
/// See [`ISP_OUT_H_OFFS`].
pub const ISP_OUT_V_SIZE: u32 = 0x05A0;
/// Demosaic parameters (`isp_demosaic`).
pub const ISP_DEMOSAIC: u32 = 0x05A4;
/// `demosaic_bypass` (bit 10).
pub const DEMOSAIC_BYPASS: u32 = 0x0000_0400;
/// `ISP_MIS` (`vvcam_isp_hal.h`): the ISP core's masked interrupt status. Read only.
pub const ISP_MIS: u32 = 0x05C4;

/// Window sizes and offsets: 14 bits in every source; written whole.
pub const WINDOW_MASK: u32 = 0x0000_3FFF;

// Image stabilisation (`MRV_IS_BASE` = 0x2300): its window feeds the resizer.

/// Image stabilisation control (`isp_is_ctrl`): bit 0 enables it. The driver keeps it off.
pub const ISP_IS_CTRL: u32 = 0x2300;
/// Image stabilisation window (`isp_is_h_offs`, `_v_offs`, `_h_size`, `_v_size`).
pub const ISP_IS_H_OFFS: u32 = 0x2308;
/// See [`ISP_IS_H_OFFS`].
pub const ISP_IS_V_OFFS: u32 = 0x230C;
/// See [`ISP_IS_H_OFFS`].
pub const ISP_IS_H_SIZE: u32 = 0x2310;
/// See [`ISP_IS_H_OFFS`].
pub const ISP_IS_V_SIZE: u32 = 0x2314;

// The main resizer (`MRV_MRSZ_BASE` = 0xC00).

/// Main resizer control (`mrsz_ctrl`). Only the scale enables and directions (bits 7..0) are
/// written, all to 0: no scaling. Its update bits are not written: the header places
/// `mrsz_cfg_upd` at bit 8 or bit 9 depending on the build, and which this chip is, is not known.
pub const MRSZ_CTRL: u32 = 0x0C00;
/// The scale enables (`scale_hy`, `_hc`, `_vy`, `_vc`) and their up/down bits.
pub const MRSZ_SCALE_MASK: u32 = 0x0000_00FF;
/// Format conversion (`mrsz_format_conv_ctrl`, `MRV_MRSZ_BASE + 0x6C`).
pub const MRSZ_FORMAT_CONV_CTRL: u32 = 0x0C6C;
/// `mrsz_covert_output` (bits 4..2).
pub const CONV_OUTPUT_MASK: u32 = 0x0000_001C;
/// `mrsz_covert_output` field shift.
pub const CONV_OUTPUT_SHIFT: u32 = 2;
/// `mrsz_covert_input` (bits 1..0).
pub const CONV_INPUT_MASK: u32 = 0x0000_0003;
/// The output code `set_data_path` writes for RGB888.
pub const CONV_OUTPUT_RGB888: u32 = 6;
/// The input code `set_data_path` writes with RGB888 and YUV 4:4:4 output.
pub const CONV_INPUT_RGB888: u32 = 2;

// The second-generation memory interface (`MIV2`): global control and the main path.

/// `MIV2_CTRL`: which of the memory interface's paths run. Written by
/// [`Capture`](crate::Capture) only, whole, so every path but the one armed is off.
pub const MIV2_CTRL: u32 = 0x1300;
/// `mp_ycbcr_path_enable`.
pub const MIV2_MP_YCBCR_PATH: u32 = 0x0000_0001;
/// `mp_raw_path_enable`.
pub const MIV2_MP_RAW_PATH: u32 = 0x0000_0002;

/// Main path control (`miv2_mp_ctrl`). Written by [`Capture`](crate::Capture) only.
pub const MP_CTRL: u32 = 0x1310;
/// `mp_init_offset_en`: the offset counters take their init values on an update.
pub const MP_CTRL_INIT_OFFSET_EN: u32 = 0x0000_0020;
/// `mp_init_base_en`: the base addresses and sizes take their init values on an update.
pub const MP_CTRL_INIT_BASE_EN: u32 = 0x0000_0010;
/// `mp_mi_cfg_upd`: update the shadow registers now.
pub const MP_CTRL_CFG_UPD: u32 = 0x0000_0008;
/// `mp_auto_update`: update the shadow registers at each frame end.
pub const MP_CTRL_AUTO_UPDATE: u32 = 0x0000_0002;

/// Main path format (`miv2_mp_fmt`).
pub const MP_FMT: u32 = 0x1314;
/// Every field of [`MP_FMT`] `set_data_path` names (bits 14..0).
pub const MP_FMT_MASK: u32 = 0x0000_7FFF;
/// `nyv`: the order of an interleaved 4:2:2 line (bits 14..13). 0 is taken as Y, U, Y, V.
/// Unconfirmed.
pub const FMT_NYV_SHIFT: u32 = 13;
/// `nv12`: semi-planar chroma order (bit 12). 1 is taken as U before V. Unconfirmed.
pub const FMT_NV12: u32 = 1 << 12;
/// `raw_aligned` (bits 11..10).
pub const FMT_RAW_ALIGNED_SHIFT: u32 = 10;
/// `ISP_MI_DATA_UNALIGN_MODE`: raw pixels packed back to back.
pub const RAW_UNALIGNED: u32 = 0;
/// `ISP_MI_DATA_ALIGN_16BIT_MODE`: each raw pixel in a 16-bit word.
pub const RAW_ALIGNED_16: u32 = 2;
/// `yuv_aligned` (bit 9): each 10-bit sample in a 16-bit word.
pub const FMT_YUV_ALIGNED: u32 = 1 << 9;
/// `raw_bit` (bits 8..6): 0 for 8 bits, 1 for 10, 2 for 12 (`set_data_path`).
pub const FMT_RAW_BIT_SHIFT: u32 = 6;
/// `yuv_str` (bits 5..4): 0 semi-planar, 1 interleaved, 2 planar (`set_data_path`).
pub const FMT_YUV_STR_SHIFT: u32 = 4;
/// `yuv_fmt` (bits 3..2): 0 for 4:2:0, 1 for 4:2:2, 2 for 4:4:4 (`set_data_path`).
pub const FMT_YUV_FMT_SHIFT: u32 = 2;
/// `yuv_bit` (bit 1): 10-bit samples. `set_data_path` always writes 0; 1 is taken from the field's
/// name. Unconfirmed.
pub const FMT_YUV_BIT_10: u32 = 1 << 1;

/// Main path bus configuration (`miv2_mp_bus_cfg`).
pub const MP_BUS_CFG: u32 = 0x1318;
/// `mp_wr_burst_len` (bits 1..0).
pub const BUS_CFG_BURST_MASK: u32 = 0x0000_0003;
/// `mp_wr_swap_raw` (bits 23..20): `set_data_path` sets 1 for raw deeper than 8 bits.
pub const BUS_CFG_SWAP_RAW_MASK: u32 = 0x00F0_0000;
/// `mp_wr_swap_raw` field shift.
pub const BUS_CFG_SWAP_RAW_SHIFT: u32 = 20;
/// Main path bus ids (`miv2_mp_bus_id`).
pub const MP_BUS_ID: u32 = 0x131C;
/// `mp_wr_id_en`.
pub const BUS_ID_WR_ID_EN: u32 = 0x0000_0100;
/// `mp_bus_sw_en`.
pub const BUS_ID_SW_EN: u32 = 0x0200_0000;

/// Y (or the only) plane: base address (`miv2_mp_y_base_ad_init`). Address register.
pub const MP_Y_BASE_AD_INIT: u32 = 0x1324;
/// Y plane size in bytes (`miv2_mp_y_size_init`): where the write wraps. Extent register.
pub const MP_Y_SIZE_INIT: u32 = 0x1328;
/// Y plane offset counter start (`miv2_mp_y_offs_cnt_init`). Extent register.
pub const MP_Y_OFFS_CNT_INIT: u32 = 0x132C;
/// Y line length in pixels (`miv2_mp_y_llength`).
pub const MP_Y_LLENGTH: u32 = 0x1330;
/// Picture width in pixels (`miv2_mp_y_pic_width`).
pub const MP_Y_PIC_WIDTH: u32 = 0x1334;
/// Picture height in lines (`miv2_mp_y_pic_height`).
pub const MP_Y_PIC_HEIGHT: u32 = 0x1338;
/// Picture size in pixels (`miv2_mp_y_pic_size`).
pub const MP_Y_PIC_SIZE: u32 = 0x133C;
/// Cb (or CbCr) plane: base, size, offset counter start. Address and extent registers.
pub const MP_CB_BASE_AD_INIT: u32 = 0x1340;
/// See [`MP_CB_BASE_AD_INIT`].
pub const MP_CB_SIZE_INIT: u32 = 0x1344;
/// See [`MP_CB_BASE_AD_INIT`].
pub const MP_CB_OFFS_CNT_INIT: u32 = 0x1348;
/// Cr plane: base, size, offset counter start. Address and extent registers.
pub const MP_CR_BASE_AD_INIT: u32 = 0x134C;
/// See [`MP_CR_BASE_AD_INIT`].
pub const MP_CR_SIZE_INIT: u32 = 0x1350;
/// See [`MP_CR_BASE_AD_INIT`].
pub const MP_CR_OFFS_CNT_INIT: u32 = 0x1354;
/// Second base addresses (`miv2_mp_{y,cb,cr}_base_ad_init2`), for a ping-pong mode no source
/// describes. Address registers: written with the same bases as the first, so whatever mode the
/// path is left in, it writes only where the first set points.
pub const MP_Y_BASE_AD_INIT2: u32 = 0x1358;
/// See [`MP_Y_BASE_AD_INIT2`].
pub const MP_CB_BASE_AD_INIT2: u32 = 0x135C;
/// See [`MP_Y_BASE_AD_INIT2`].
pub const MP_CR_BASE_AD_INIT2: u32 = 0x1360;
/// The Y base the path is writing now (`miv2_mp_y_base_ad_shd`). Read only.
pub const MP_Y_BASE_AD_SHD: u32 = 0x1370;
/// Raw plane: base, size, offset counter start. Address and extent registers.
pub const MP_RAW_BASE_AD_INIT: u32 = 0x1394;
/// See [`MP_RAW_BASE_AD_INIT`].
pub const MP_RAW_SIZE_INIT: u32 = 0x1398;
/// See [`MP_RAW_BASE_AD_INIT`].
pub const MP_RAW_OFFS_CNT_INIT: u32 = 0x139C;
/// Raw line length in bytes (`miv2_mp_raw_llength`).
pub const MP_RAW_LLENGTH: u32 = 0x13A0;
/// Raw picture width in pixels (`miv2_mp_raw_pic_width`).
pub const MP_RAW_PIC_WIDTH: u32 = 0x13A4;
/// Raw picture height in lines (`miv2_mp_raw_pic_height`).
pub const MP_RAW_PIC_HEIGHT: u32 = 0x13A8;
/// Raw picture size in bytes (`miv2_mp_raw_pic_size`).
pub const MP_RAW_PIC_SIZE: u32 = 0x13AC;
/// The raw base the path is writing now (`miv2_mp_raw_base_ad_shd`). Read only.
pub const MP_RAW_BASE_AD_SHD: u32 = 0x13B4;
/// JPEG/data path: base, size, offset counter start. Address and extent registers. The driver
/// never enables this path; its registers are pointed at the armed memory anyway.
pub const MP_JDP_BASE_AD_INIT: u32 = 0x13C0;
/// See [`MP_JDP_BASE_AD_INIT`].
pub const MP_JDP_SIZE_INIT: u32 = 0x13C4;
/// See [`MP_JDP_BASE_AD_INIT`].
pub const MP_JDP_OFFS_CNT_INIT: u32 = 0x13C8;

/// Base address fields: 16-byte aligned (`MP_Y_BASE_AD_MASK`).
pub const BASE_MASK: u32 = 0xFFFF_FFF0;
/// Size fields: 16-byte multiples below 512 MiB (`MP_Y_SIZE_MASK`).
pub const SIZE_MASK: u32 = 0x1FFF_FFF0;

/// The memory interface's interrupt mask (`miv2_imsc`).
pub const MIV2_IMSC: u32 = 0x16C0;
/// `MIV2_MIS`: masked interrupt status, read and cleared by the vendor's interrupt handler, which
/// queues it as the `VVCAM_EID_MIV2_MIS` event. Read only here.
pub const MIV2_MIS: u32 = 0x16D0;
/// `MIV2_ICR`: writing a bit clears it in [`MIV2_MIS`], as the vendor's interrupt handler does.
/// Written only by [`Capture`](crate::Capture) on the block's own window, never by a caller.
pub const MIV2_ICR: u32 = 0x16D8;
/// `mp_ycbcr_frame_end`.
pub const MIS_MP_YCBCR_FRAME_END: u32 = 0x0000_0001;
/// `mp_raw_frame_end`.
pub const MIS_MP_RAW_FRAME_END: u32 = 0x0000_0002;
/// `MIV2_MIS_MP_FRAME_END_MASK`: either of the main path's frame ends.
pub const MIS_MP_FRAME_END: u32 = MIS_MP_YCBCR_FRAME_END | MIS_MP_RAW_FRAME_END;

/// Every register [`Capture`](crate::Capture) writes an address or an extent to. None of them is
/// in [`crate::access::WRITABLE`]; the tests check that.
pub const ADDRESS_REGISTERS: [u32; 18] = [
    MP_Y_BASE_AD_INIT,
    MP_Y_SIZE_INIT,
    MP_Y_OFFS_CNT_INIT,
    MP_CB_BASE_AD_INIT,
    MP_CB_SIZE_INIT,
    MP_CB_OFFS_CNT_INIT,
    MP_CR_BASE_AD_INIT,
    MP_CR_SIZE_INIT,
    MP_CR_OFFS_CNT_INIT,
    MP_Y_BASE_AD_INIT2,
    MP_CB_BASE_AD_INIT2,
    MP_CR_BASE_AD_INIT2,
    MP_RAW_BASE_AD_INIT,
    MP_RAW_SIZE_INIT,
    MP_RAW_OFFS_CNT_INIT,
    MP_JDP_BASE_AD_INIT,
    MP_JDP_SIZE_INIT,
    MP_JDP_OFFS_CNT_INIT,
];
