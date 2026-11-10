//! The vendor kernel's AI-ISP uAPI, transcribed.
//!
//! Source: rockchip-linux/kernel, branch `develop-6.1`, commit
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`:
//!
//! - `include/uapi/linux/rk-aiisp-config.h`: every structure, constant, request and the event
//!   below, except where noted.
//! - `include/uapi/linux/rk-isp2-config.h`: [`AibnrQueue`] (`struct rkisp_aiisp_st`),
//!   [`IspBuf`] (`struct rkisp_buf_info`), [`BnrBufInfo`] (`struct rkisp_bnr_buf_info`),
//!   `RKISP_BUFFER_MAX`, `VPSL_YRAW_CHN_MAX`, `VPSL_SIG_CHN_MAX`.
//! - `drivers/media/platform/rockchip/aiisp/aiisp.c` and `aiisp.h`: the node's format and its
//!   buffer counts (`RKAIISP_REQ_BUFS_MIN`/`_MAX`, the `queue_setup` size).
//!
//! Every structure is `__attribute__((packed))` in C and `#[repr(C, packed)]` here, so the layout
//! is the same on every target; `src/layout.rs` pins it against the C compiler. C `enum` members
//! are `u32` here (GCC gives a non-negative enum `unsigned int`), C `int` is `i32`.
//!
//! The header's two unions are not unions here. `union rkaiisp_queue_buf` is one of
//! [`AibnrQueue`] (its largest member, which sets its size) or [`AirmsQueue`] (a prefix of it),
//! and [`QUEUE_BUF_BYTES`] is its size. The union inside `struct rkisp_bnr_buf_info` is its `v35`
//! member, the larger one and the one the AI-ISP driver reads ([`BnrBufInfo`]).

use core::mem::size_of;

/// `RKAIISP_PYRAMID_LAYER_NUM`.
pub const PYRAMID_LAYER_NUM: u32 = 4;
/// `RKAIISP_AIYNR_LAYER_NUM`.
pub const AIYNR_LAYER_NUM: u32 = 5;
/// `RKAIISP_MAX_RUNCNT`: model runs per frame at most, and the length of every per-run array.
pub const MAX_RUNCNT: u32 = 8;
/// `RKAIISP_MAX_ISPBUF`.
pub const MAX_ISPBUF: u32 = 8;
/// `RKAIISP_MODEL_UPDATE`: a `module_update` bit, the buffer carries a new model.
pub const MODEL_UPDATE: u32 = 0x01;
/// `RKAIISP_OTHER_UPDATE`: a `module_update` bit, the buffer carries new [`OtherCfg`].
pub const OTHER_UPDATE: u32 = 0x02;
/// `RKAIISP_AIYNR_YBUF_NUM_MAX`.
pub const AIYNR_YBUF_NUM_MAX: u32 = 8;
/// `RKISP_BUFFER_MAX` (`rk-isp2-config.h`): buffers in each set of [`BnrBufInfo`].
pub const ISP_BUFFER_MAX: u32 = 8;
/// `VPSL_YRAW_CHN_MAX` (`rk-isp2-config.h`).
pub const VPSL_YRAW_CHN_MAX: usize = 6;
/// `VPSL_SIG_CHN_MAX` (`rk-isp2-config.h`).
pub const VPSL_SIG_CHN_MAX: usize = 5;
/// Read channels of one run (`RKAIISP_MAX_CHANNEL` in `aiisp.h`, and the length of the
/// per-channel arrays of [`ModelCfg`]).
pub const CHANNELS: usize = 7;
/// Levels of one run: the length of `sw_aiisp_lv_active` and `sw_aiisp_lv_mode`.
pub const LEVELS: usize = 16;

/// Parameter buffers the node's queue takes at least and at most (`RKAIISP_REQ_BUFS_MIN`,
/// `RKAIISP_REQ_BUFS_MAX` in `aiisp.c`; `queue_setup` clamps the count to them).
pub const PARAM_BUFFERS_MIN: u32 = 2;
/// See [`PARAM_BUFFERS_MIN`].
pub const PARAM_BUFFERS_MAX: u32 = 8;
/// The parameter node's metadata format, `V4L2_META_FMT_RK_ISP1_PARAMS` (`RK1P`), which the
/// AI-ISP node reports for its `V4L2_BUF_TYPE_META_OUTPUT` queue.
pub const META_FORMAT: u32 = u32::from_le_bytes(*b"RK1P");

/// The bytes of a parameter buffer the kernel allocates (`queue_setup` in `aiisp.c`):
/// `sizeof(struct rkaiisp_params) + para_size * max_runcnt`, or `None` past `u32` (the kernel
/// computes it in 32 bits and would wrap).
#[must_use]
pub const fn param_buffer_bytes(para_size: u32, max_runcnt: u32) -> Option<u32> {
    match para_size.checked_mul(max_runcnt) {
        Some(weights) => weights.checked_add(size_of::<Params>() as u32),
        None => None,
    }
}

// Requests (asm-generic encoding, as on arm64): `_IOC(dir, 'V', BASE_VIDIOC_PRIVATE + n, size)`.
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr
}

/// `BASE_VIDIOC_PRIVATE` (`videodev2.h`).
pub const BASE_VIDIOC_PRIVATE: u32 = 192;
/// `RKAIISP_CMD_SET_PARAM_INFO`: `_IOW`, [`ParamInfo`].
pub const CMD_SET_PARAM_INFO: u32 = ioc(IOC_WRITE, BASE_VIDIOC_PRIVATE, size_of::<ParamInfo>());
/// `RKAIISP_CMD_INIT_BUFPOOL`: `_IOW`, [`IspbufInfo`].
pub const CMD_INIT_BUFPOOL: u32 = ioc(IOC_WRITE, BASE_VIDIOC_PRIVATE + 1, size_of::<IspbufInfo>());
/// `RKAIISP_CMD_FREE_BUFPOOL`: `_IO`.
pub const CMD_FREE_BUFPOOL: u32 = ioc(0, BASE_VIDIOC_PRIVATE + 2, 0);
/// `RKAIISP_CMD_QUEUE_BUF`: `_IOW`, `union rkaiisp_queue_buf` ([`QUEUE_BUF_BYTES`]).
pub const CMD_QUEUE_BUF: u32 = ioc(IOC_WRITE, BASE_VIDIOC_PRIVATE + 3, QUEUE_BUF_BYTES);
/// `RKAIISP_CMD_INIT_AIRMS_BUFPOOL`: `_IOWR`, [`RmsbufInfo`].
pub const CMD_INIT_AIRMS_BUFPOOL: u32 = ioc(
    IOC_READ | IOC_WRITE,
    BASE_VIDIOC_PRIVATE + 4,
    size_of::<RmsbufInfo>(),
);
/// `RKAIISP_CMD_GET_YNRBUF_INFO`: `_IOR`, [`YnrbufInfo`].
pub const CMD_GET_YNRBUF_INFO: u32 =
    ioc(IOC_READ, BASE_VIDIOC_PRIVATE + 5, size_of::<YnrbufInfo>());
/// `RKAIISP_CMD_SET_MEMORY_MODE`: `_IOW`, `enum rkaiisp_mem_mode` (four bytes).
pub const CMD_SET_MEMORY_MODE: u32 = ioc(IOC_WRITE, BASE_VIDIOC_PRIVATE + 6, size_of::<u32>());
/// `RKAIISP_CMD_CLEAR_IQPARAMS`: `_IO`.
pub const CMD_CLEAR_IQPARAMS: u32 = ioc(0, BASE_VIDIOC_PRIVATE + 7, 0);

/// `V4L2_EVENT_PRIVATE_START` (`videodev2.h`).
pub const V4L2_EVENT_PRIVATE_START: u32 = 0x0800_0000;
/// `RKAIISP_V4L2_EVENT_AIISP_DONE`: queued on the AI-ISP's subdevice when a frame's runs are
/// done, with the frame's `union rkaiisp_queue_buf` at the start of the event's data.
pub const EVENT_AIISP_DONE: u32 = V4L2_EVENT_PRIVATE_START + 1;

/// `enum rkaiisp_chn_src`: where a read channel takes its data.
pub mod chn_src {
    #![allow(missing_docs)]
    pub const ISP_IIR: u32 = 0;
    pub const VPSL_YRAW_CHN0: u32 = 1;
    pub const VPSL_YRAW_CHN1: u32 = 2;
    pub const VPSL_YRAW_CHN2: u32 = 3;
    pub const VPSL_YRAW_CHN3: u32 = 4;
    pub const VPSL_YRAW_CHN4: u32 = 5;
    pub const VPSL_YRAW_CHN5: u32 = 6;
    pub const VPSL_SIG_CHN0: u32 = 7;
    pub const VPSL_SIG_CHN1: u32 = 8;
    pub const VPSL_SIG_CHN2: u32 = 9;
    pub const VPSL_SIG_CHN3: u32 = 10;
    pub const VPSL_SIG_CHN4: u32 = 11;
    pub const ISP_AIPRE_NARMAP: u32 = 12;
    pub const AIISP_LAST_OUT: u32 = 13;
    pub const VICAP_BAYER_RAW: u32 = 14;
    pub const ALLZERO_SIGMA: u32 = 15;
    pub const ALLZERO_NARMAP: u32 = 16;
    pub const ISP_FINAL_Y: u32 = 17;
    pub const VICAP_BAYER_RAW_DOWN: u32 = 18;
}

/// `enum rkaiisp_exealgo`: which kind of work the node is set up for.
pub mod exealgo {
    #![allow(missing_docs)]
    pub const AIBNR: u32 = 0;
    pub const AIRMS: u32 = 1;
    pub const AIYNR: u32 = 2;
}

/// `enum rkaiisp_model_mode`.
pub mod model_mode {
    #![allow(missing_docs)]
    pub const SINGLE_MODE: u32 = 0;
    pub const COMBO_MODE: u32 = 1;
    pub const SINGLEX2_MODE: u32 = 2;
    pub const REMOSAIC_MODE: u32 = 3;
    pub const AIYNR_MODE: u32 = 4;
}

/// `enum rkaiisp_exemode`: who hands the node each frame's buffers.
pub mod exemode {
    #![allow(missing_docs)]
    pub const BOTHEVENT_TO_AIQ: u32 = 0;
    pub const ISPEVENT_IN_KERNEL: u32 = 1;
    pub const BOTHEVENT_IN_KERNEL: u32 = 2;
}

/// `enum rkaiisp_mem_mode`.
pub mod mem_mode {
    #![allow(missing_docs)]
    pub const SINGLE_MEMODE: u32 = 0;
    pub const COMBO_MEMODE: u32 = 1;
}

/// `struct rkaiisp_airms_st`: one frame for the remosaic work.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct AirmsQueue {
    pub sequence: i32,
    pub inbuf_idx: i32,
    pub outbuf_idx: i32,
}

/// `struct rkisp_aiisp_st` (`rk-isp2-config.h`): one frame for the denoise work, as indices into
/// the buffer sets of [`BnrBufInfo`].
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct AibnrQueue {
    pub timestamp: u64,
    pub sequence: i32,
    pub iir_index: i32,
    pub gain_index: i32,
    pub aiisp_index: i32,
    pub aipre_gain_index: i32,
    pub vpsl_index: i32,
    pub y_src_index: i32,
    pub y_dest_index: i32,
}

/// `sizeof(union rkaiisp_queue_buf)`: its largest member, [`AibnrQueue`].
pub const QUEUE_BUF_BYTES: usize = size_of::<AibnrQueue>();

/// `struct rkaiisp_param_info`.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParamInfo {
    /// One of [`exealgo`].
    pub exealgo: u32,
    /// One of [`exemode`].
    pub exemode: u32,
    /// Weight bytes per run the parameter buffers have room for.
    pub para_size: u32,
    /// Runs per frame at most, 1 to [`MAX_RUNCNT`].
    pub max_runcnt: u32,
}

/// `struct rkisp_buf_info` (`rk-isp2-config.h`): one set of buffers the ISP shares.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct IspBuf {
    pub buf_cnt: i32,
    pub buf_size: i32,
    pub buf_stride: i32,
    pub buf_fd: [i32; ISP_BUFFER_MAX as usize],
}

/// `struct rkisp_bnr_buf_info` (`rk-isp2-config.h`) with its union as the `v35` member.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct BnrBufInfo {
    pub iir: IspBuf,
    pub ds: IspBuf,
    pub wgt: IspBuf,
    pub aiisp: IspBuf,
    pub gain: IspBuf,
    pub aipre_gain: IspBuf,
    pub vpsl: IspBuf,
    pub y_src: IspBuf,
    pub iir_rw_fmt: u8,
    pub gain_mode: u8,
    pub yraw_sel: u8,
    pub aibnr_l2: u8,
    pub vpsl_yraw_offs: [u32; VPSL_YRAW_CHN_MAX],
    pub vpsl_yraw_stride: [u32; VPSL_YRAW_CHN_MAX],
    pub vpsl_sig_offs: [u32; VPSL_SIG_CHN_MAX],
    pub vpsl_sig_stride: [u32; VPSL_SIG_CHN_MAX],
}

/// `struct rkaiisp_ispbuf_info`: the ISP's buffers and the planes' sizes, for the denoise work.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct IspbufInfo {
    pub bnr_buf: BnrBufInfo,
    pub iir_width: u32,
    pub iir_height: u32,
    pub raw_width: [u32; 6],
    pub raw_height: [u32; 6],
    pub sig_width: [u32; 5],
    pub sig_height: [u32; 5],
    pub narmap_width: u32,
    pub narmap_height: u32,
}

/// `struct rkaiisp_rmsbuf_info`: the remosaic work's sizes in, the kernel's buffers out.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct RmsbufInfo {
    pub image_width: u32,
    pub image_height: u32,
    pub sigma_width: u32,
    pub sigma_height: u32,
    pub narmap_width: u32,
    pub narmap_height: u32,
    pub inbuf_num: u32,
    pub outbuf_num: u32,
    pub inbuf_fd: [i32; 6],
    pub outbuf_fd: [i32; 6],
}

/// `struct rkaiisp_ynrbuf_info`.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct YnrbufInfo {
    pub width: i32,
    pub height: i32,
    pub buf_cnt: u32,
    pub dma_fd: [i32; AIYNR_YBUF_NUM_MAX as usize],
}

/// `struct rkaiisp_other_cfg`: noise limits, black levels and the companding curves.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct OtherCfg {
    pub sw_neg_noiselimit: u16,
    pub sw_pos_noiselimit: u16,
    pub sw_prev_blacklvl: u16,
    pub sw_post_blacklvl: u16,
    pub sw_in_comp_y: [u16; 33],
    pub sw_out_decomp_y: [u16; 33],
}

/// `struct rkaiisp_model_cfg`: how one run of the model is laid on the block.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct ModelCfg {
    /// One of [`chn_src`] per read channel.
    pub mi_chn_src: [u32; CHANNELS],
    pub sw_aiisp_mode: u32,
    pub sw_aiisp_level_num: u32,
    pub sw_aiisp_l1_level_num: u32,
    pub sw_aiisp_op_mode: u32,
    pub sw_aiisp_drop_en: u32,
    pub sw_aiisp_lv_active: [u32; LEVELS],
    pub sw_aiisp_lv_mode: [u32; LEVELS],
    pub sw_mi_chn_en: [u32; CHANNELS],
    pub sw_mi_chn_mode: [u32; CHANNELS],
    pub sw_mi_chn_num: [u32; CHANNELS],
    pub sw_mi_chn_data_mode: [u32; CHANNELS],
    pub sw_mi_chn1_sel: u32,
    pub sw_mi_chn3_sel: u32,
    pub sw_out_d2s_en: u32,
    pub sw_out_mode: u32,
    pub sw_lastlvlm1_clip8bit: u32,
}

/// `struct rkaiisp_kwt_cfg`: where each run's weights lie in the parameter buffer (the header
/// spells the first member `kwt_offet`).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct KwtCfg {
    pub kwt_offet: [u32; MAX_RUNCNT as usize],
    pub kwt_size: [u32; MAX_RUNCNT as usize],
    pub kwt_pad_size: [u32; MAX_RUNCNT as usize],
}

/// `struct rkaiisp_params`: the head of a parameter buffer. The weights follow it, where
/// [`KwtCfg`] places them.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct Params {
    pub frame_id: u32,
    /// [`MODEL_UPDATE`] and [`OTHER_UPDATE`] bits.
    pub module_update: u32,
    pub model_runcnt: u32,
    /// One of [`model_mode`].
    pub model_mode: u32,
    pub other_cfg: OtherCfg,
    pub model_cfg: [ModelCfg; MAX_RUNCNT as usize],
    pub kwt_cfg: KwtCfg,
    pub reserved: [u8; 36],
}

/// `struct rkaiisp_model_info`: a model's description as the header lays it out. The kernel
/// takes none; it is here for its layout.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(missing_docs)]
pub struct ModelInfo {
    pub checksum: u32,
    pub model_runcnt: u32,
    pub model_qr: f32,
    pub model_mode: u32,
    pub model_cfg: [ModelCfg; MAX_RUNCNT as usize],
    pub kwt_cfg: KwtCfg,
    pub reserved: [u8; 48],
}
