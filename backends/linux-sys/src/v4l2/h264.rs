//! The stateless H.264 decoder's controls (`V4L2_CID_STATELESS_H264_*`): the parameter-set,
//! slice and decoded-picture structures of `linux/v4l2-controls.h`, with the layout checked
//! against the header in the tests. A decoder that takes these controls takes its headers as
//! the caller parsed them: nothing here parses a bitstream.
//!
//! Each structure is `repr(C)` with no padding (the header's `reserved` fields fill the gaps),
//! so its bytes are its fields, and [`Sps::as_bytes`] and the others give the bytes a
//! `VIDIOC_S_EXT_CTRLS` payload takes. `as_bytes_mut` gives the same bytes as the exclusive
//! slice [`super::device::ExtControl::payload`] borrows, so a decoder sets them without copying.
//!
//! The controls are the H.264 side of the same stateless interface as [`super::hevc`], and
//! take the same job shape: the caller's bytes by control id, a media request per frame, the
//! bitstream on the OUTPUT queue and the picture on the CAPTURE queue. A reference picture is
//! named in [`DpbEntry::reference_ts`] by the timestamp of the capture buffer that holds it.
//!
//! Provenance: the Linux uAPI headers are GPL-2.0 WITH Linux-syscall-note. The names, numbers
//! and layouts below are interface facts transcribed from `linux/v4l2-controls.h` and
//! `linux/videodev2.h` of the Linux 7.2 headers (`V4L2_CID_STATELESS_H264_*`,
//! `V4L2_STATELESS_H264_*`, `V4L2_H264_*`, `V4L2_PIX_FMT_H264_SLICE`); no kernel source is
//! copied. The profile and level controls (`V4L2_CID_MPEG_VIDEO_H264_PROFILE` and `_LEVEL`)
//! are not part of the stateless path: the SPS carries both, so they are left out here.

use super::hevc::{bytes_of, bytes_of_mut};

/// `V4L2_CID_STATELESS_H264_DECODE_MODE`: an integer control, one of the `DECODE_MODE_*`.
pub const CID_DECODE_MODE: u32 = 0x00a4_0900;
/// `V4L2_CID_STATELESS_H264_START_CODE`: an integer control, one of the `START_CODE_*`.
pub const CID_START_CODE: u32 = 0x00a4_0901;
/// `V4L2_CID_STATELESS_H264_SPS`.
pub const CID_SPS: u32 = 0x00a4_0902;
/// `V4L2_CID_STATELESS_H264_PPS`.
pub const CID_PPS: u32 = 0x00a4_0903;
/// `V4L2_CID_STATELESS_H264_SCALING_MATRIX`.
pub const CID_SCALING_MATRIX: u32 = 0x00a4_0904;
/// `V4L2_CID_STATELESS_H264_PRED_WEIGHTS`.
pub const CID_PRED_WEIGHTS: u32 = 0x00a4_0905;
/// `V4L2_CID_STATELESS_H264_SLICE_PARAMS`.
pub const CID_SLICE_PARAMS: u32 = 0x00a4_0906;
/// `V4L2_CID_STATELESS_H264_DECODE_PARAMS`.
pub const CID_DECODE_PARAMS: u32 = 0x00a4_0907;

/// `V4L2_STATELESS_H264_DECODE_MODE_SLICE_BASED`: one slice per OUTPUT buffer, with the
/// slice's parameters in [`CID_SLICE_PARAMS`].
pub const DECODE_MODE_SLICE_BASED: i32 = 0;
/// `V4L2_STATELESS_H264_DECODE_MODE_FRAME_BASED`: the OUTPUT buffer holds the whole frame,
/// and [`CID_SLICE_PARAMS`] is not used.
pub const DECODE_MODE_FRAME_BASED: i32 = 1;
/// `V4L2_STATELESS_H264_START_CODE_NONE`: the slices carry no start code.
pub const START_CODE_NONE: i32 = 0;
/// `V4L2_STATELESS_H264_START_CODE_ANNEX_B`: the slices carry an Annex B start code.
pub const START_CODE_ANNEX_B: i32 = 1;

/// `V4L2_PIX_FMT_H264_SLICE` (`S264`): parsed slices, for a stateless decoder's OUTPUT queue.
pub const PIX_FMT_H264_SLICE: u32 = u32::from_le_bytes(*b"S264");

/// `V4L2_H264_SLICE_TYPE_P`.
pub const SLICE_TYPE_P: u8 = 0;
/// `V4L2_H264_SLICE_TYPE_B`.
pub const SLICE_TYPE_B: u8 = 1;
/// `V4L2_H264_SLICE_TYPE_I`.
pub const SLICE_TYPE_I: u8 = 2;
/// `V4L2_H264_SLICE_TYPE_SP`.
pub const SLICE_TYPE_SP: u8 = 3;
/// `V4L2_H264_SLICE_TYPE_SI`.
pub const SLICE_TYPE_SI: u8 = 4;

/// `V4L2_H264_NUM_DPB_ENTRIES`: the decoded picture buffer's size (level limits, A.3.1).
pub const NUM_DPB_ENTRIES: usize = 16;
/// `V4L2_H264_REF_LIST_LEN`: a reference picture list's length.
pub const REF_LIST_LEN: usize = 2 * NUM_DPB_ENTRIES;

/// `V4L2_H264_TOP_FIELD_REF`.
pub const TOP_FIELD_REF: u8 = 0x1;
/// `V4L2_H264_BOTTOM_FIELD_REF`.
pub const BOTTOM_FIELD_REF: u8 = 0x2;
/// `V4L2_H264_FRAME_REF`.
pub const FRAME_REF: u8 = 0x3;

/// `V4L2_H264_SPS_CONSTRAINT_SET*_FLAG`, the bits of [`Sps::constraint_set_flags`].
pub mod constraint_set {
    /// `V4L2_H264_SPS_CONSTRAINT_SET0_FLAG`.
    pub const SET0: u8 = 0x01;
    /// `V4L2_H264_SPS_CONSTRAINT_SET1_FLAG`.
    pub const SET1: u8 = 0x02;
    /// `V4L2_H264_SPS_CONSTRAINT_SET2_FLAG`.
    pub const SET2: u8 = 0x04;
    /// `V4L2_H264_SPS_CONSTRAINT_SET3_FLAG`.
    pub const SET3: u8 = 0x08;
    /// `V4L2_H264_SPS_CONSTRAINT_SET4_FLAG`.
    pub const SET4: u8 = 0x10;
    /// `V4L2_H264_SPS_CONSTRAINT_SET5_FLAG`.
    pub const SET5: u8 = 0x20;
}

/// `V4L2_H264_SPS_FLAG_*`, the bits of [`Sps::flags`].
pub mod sps_flags {
    /// `V4L2_H264_SPS_FLAG_SEPARATE_COLOUR_PLANE`.
    pub const SEPARATE_COLOUR_PLANE: u32 = 0x01;
    /// `V4L2_H264_SPS_FLAG_QPPRIME_Y_ZERO_TRANSFORM_BYPASS`.
    pub const QPPRIME_Y_ZERO_TRANSFORM_BYPASS: u32 = 0x02;
    /// `V4L2_H264_SPS_FLAG_DELTA_PIC_ORDER_ALWAYS_ZERO`.
    pub const DELTA_PIC_ORDER_ALWAYS_ZERO: u32 = 0x04;
    /// `V4L2_H264_SPS_FLAG_GAPS_IN_FRAME_NUM_VALUE_ALLOWED`.
    pub const GAPS_IN_FRAME_NUM_VALUE_ALLOWED: u32 = 0x08;
    /// `V4L2_H264_SPS_FLAG_FRAME_MBS_ONLY`.
    pub const FRAME_MBS_ONLY: u32 = 0x10;
    /// `V4L2_H264_SPS_FLAG_MB_ADAPTIVE_FRAME_FIELD`.
    pub const MB_ADAPTIVE_FRAME_FIELD: u32 = 0x20;
    /// `V4L2_H264_SPS_FLAG_DIRECT_8X8_INFERENCE`.
    pub const DIRECT_8X8_INFERENCE: u32 = 0x40;
}

/// `V4L2_H264_PPS_FLAG_*`, the bits of [`Pps::flags`].
pub mod pps_flags {
    /// `V4L2_H264_PPS_FLAG_ENTROPY_CODING_MODE`.
    pub const ENTROPY_CODING_MODE: u16 = 0x0001;
    /// `V4L2_H264_PPS_FLAG_BOTTOM_FIELD_PIC_ORDER_IN_FRAME_PRESENT`.
    pub const BOTTOM_FIELD_PIC_ORDER_IN_FRAME_PRESENT: u16 = 0x0002;
    /// `V4L2_H264_PPS_FLAG_WEIGHTED_PRED`.
    pub const WEIGHTED_PRED: u16 = 0x0004;
    /// `V4L2_H264_PPS_FLAG_DEBLOCKING_FILTER_CONTROL_PRESENT`.
    pub const DEBLOCKING_FILTER_CONTROL_PRESENT: u16 = 0x0008;
    /// `V4L2_H264_PPS_FLAG_CONSTRAINED_INTRA_PRED`.
    pub const CONSTRAINED_INTRA_PRED: u16 = 0x0010;
    /// `V4L2_H264_PPS_FLAG_REDUNDANT_PIC_CNT_PRESENT`.
    pub const REDUNDANT_PIC_CNT_PRESENT: u16 = 0x0020;
    /// `V4L2_H264_PPS_FLAG_TRANSFORM_8X8_MODE`.
    pub const TRANSFORM_8X8_MODE: u16 = 0x0040;
    /// `V4L2_H264_PPS_FLAG_SCALING_MATRIX_PRESENT`: a non-flat scaling matrix applies, given in
    /// [`super::ScalingMatrix`].
    pub const SCALING_MATRIX_PRESENT: u16 = 0x0080;
}

/// `V4L2_H264_SLICE_FLAG_*`, the bits of [`SliceParams::flags`].
pub mod slice_flags {
    /// `V4L2_H264_SLICE_FLAG_DIRECT_SPATIAL_MV_PRED`.
    pub const DIRECT_SPATIAL_MV_PRED: u32 = 0x01;
    /// `V4L2_H264_SLICE_FLAG_SP_FOR_SWITCH`.
    pub const SP_FOR_SWITCH: u32 = 0x02;
}

/// `V4L2_H264_DPB_ENTRY_FLAG_*`, the bits of [`DpbEntry::flags`].
pub mod dpb_flags {
    /// `V4L2_H264_DPB_ENTRY_FLAG_VALID`.
    pub const VALID: u32 = 0x01;
    /// `V4L2_H264_DPB_ENTRY_FLAG_ACTIVE`.
    pub const ACTIVE: u32 = 0x02;
    /// `V4L2_H264_DPB_ENTRY_FLAG_LONG_TERM`.
    pub const LONG_TERM: u32 = 0x04;
    /// `V4L2_H264_DPB_ENTRY_FLAG_FIELD`.
    pub const FIELD: u32 = 0x08;
}

/// `V4L2_H264_DECODE_PARAM_FLAG_*`, the bits of [`DecodeParams::flags`].
pub mod decode_flags {
    /// `V4L2_H264_DECODE_PARAM_FLAG_IDR_PIC`.
    pub const IDR_PIC: u32 = 0x01;
    /// `V4L2_H264_DECODE_PARAM_FLAG_FIELD_PIC`.
    pub const FIELD_PIC: u32 = 0x02;
    /// `V4L2_H264_DECODE_PARAM_FLAG_BOTTOM_FIELD`.
    pub const BOTTOM_FIELD: u32 = 0x04;
    /// `V4L2_H264_DECODE_PARAM_FLAG_PFRAME`.
    pub const PFRAME: u32 = 0x08;
    /// `V4L2_H264_DECODE_PARAM_FLAG_BFRAME`.
    pub const BFRAME: u32 = 0x10;
}

/// Writes the payload accessors of each structure of this module: its bytes, shared and
/// exclusive. Each structure is padding-free with integer fields only (checked in the tests),
/// so the bytes are its fields and any bytes written leave a valid value.
macro_rules! payload {
    ($($ty:ty),* $(,)?) => {$(
        impl $ty {
            /// The control's payload.
            pub fn as_bytes(&self) -> &[u8] {
                bytes_of(self)
            }

            /// The control's payload, for [`super::device::ExtControl::payload`].
            pub fn as_bytes_mut(&mut self) -> &mut [u8] {
                bytes_of_mut(self)
            }
        }
    )*};
}

/// `struct v4l2_ctrl_h264_sps` (`V4L2_CID_STATELESS_H264_SPS`): the sequence parameter set,
/// as its syntax elements.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sps {
    /// `profile_idc`.
    pub profile_idc: u8,
    /// `constraint_set_flags`: [`constraint_set`].
    pub constraint_set_flags: u8,
    /// `level_idc`.
    pub level_idc: u8,
    /// `seq_parameter_set_id`.
    pub seq_parameter_set_id: u8,
    /// `chroma_format_idc`.
    pub chroma_format_idc: u8,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u8,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u8,
    /// `log2_max_frame_num_minus4`.
    pub log2_max_frame_num_minus4: u8,
    /// `pic_order_cnt_type`.
    pub pic_order_cnt_type: u8,
    /// `log2_max_pic_order_cnt_lsb_minus4`.
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    /// `max_num_ref_frames`.
    pub max_num_ref_frames: u8,
    /// `num_ref_frames_in_pic_order_cnt_cycle`.
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,
    /// `offset_for_ref_frame`, one per cycle entry used.
    pub offset_for_ref_frame: [i32; 255],
    /// `offset_for_non_ref_pic`.
    pub offset_for_non_ref_pic: i32,
    /// `offset_for_top_to_bottom_field`.
    pub offset_for_top_to_bottom_field: i32,
    /// `pic_width_in_mbs_minus1`.
    pub pic_width_in_mbs_minus1: u16,
    /// `pic_height_in_map_units_minus1`.
    pub pic_height_in_map_units_minus1: u16,
    /// [`sps_flags`].
    pub flags: u32,
}

impl Sps {
    /// Whether the profile carries `chroma_format_idc` and the bit depths (the header's
    /// `V4L2_H264_SPS_HAS_CHROMA_FORMAT`): the High-family profiles and the few others that
    /// have them.
    #[must_use]
    pub fn has_chroma_format(&self) -> bool {
        matches!(
            self.profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        )
    }
}

impl Default for Sps {
    fn default() -> Self {
        Self {
            profile_idc: 0,
            constraint_set_flags: 0,
            level_idc: 0,
            seq_parameter_set_id: 0,
            chroma_format_idc: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            max_num_ref_frames: 0,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            offset_for_ref_frame: [0; 255],
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            pic_width_in_mbs_minus1: 0,
            pic_height_in_map_units_minus1: 0,
            flags: 0,
        }
    }
}

/// `struct v4l2_ctrl_h264_pps` (`V4L2_CID_STATELESS_H264_PPS`): the picture parameter set.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pps {
    /// `pic_parameter_set_id`.
    pub pic_parameter_set_id: u8,
    /// `seq_parameter_set_id`.
    pub seq_parameter_set_id: u8,
    /// `num_slice_groups_minus1`.
    pub num_slice_groups_minus1: u8,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// `weighted_bipred_idc`.
    pub weighted_bipred_idc: u8,
    /// `pic_init_qp_minus26`.
    pub pic_init_qp_minus26: i8,
    /// `pic_init_qs_minus26`.
    pub pic_init_qs_minus26: i8,
    /// `chroma_qp_index_offset`.
    pub chroma_qp_index_offset: i8,
    /// `second_chroma_qp_index_offset`.
    pub second_chroma_qp_index_offset: i8,
    /// [`pps_flags`].
    pub flags: u16,
}

impl Pps {
    /// Whether a slice of `slice_type` carries a prediction weight table (the header's
    /// `V4L2_H264_CTRL_PRED_WEIGHTS_REQUIRED`): weighted prediction in a P or SP slice, or
    /// explicit weighted bi-prediction in a B slice.
    #[must_use]
    pub fn pred_weights_required(&self, slice_type: u8) -> bool {
        (self.flags & pps_flags::WEIGHTED_PRED != 0
            && matches!(slice_type, SLICE_TYPE_P | SLICE_TYPE_SP))
            || (self.weighted_bipred_idc == 1 && slice_type == SLICE_TYPE_B)
    }
}

/// `struct v4l2_ctrl_h264_scaling_matrix` (`V4L2_CID_STATELESS_H264_SCALING_MATRIX`), in raster
/// scan order. The 4x4 lists are Intra Y, Intra Cb, Intra Cr, Inter Y, Inter Cb, Inter Cr; the
/// 8x8 lists are Intra Y, Inter Y, Intra Cb, Inter Cb, Intra Cr, Inter Cr (the H.264 table 7-2
/// order, which differs).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalingMatrix {
    /// `scaling_list_4x4`.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// `scaling_list_8x8`.
    pub scaling_list_8x8: [[u8; 64]; 6],
}

impl Default for ScalingMatrix {
    fn default() -> Self {
        Self {
            scaling_list_4x4: [[0; 16]; 6],
            scaling_list_8x8: [[0; 64]; 6],
        }
    }
}

/// `struct v4l2_h264_weight_factors`: one reference list's weights and offsets.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WeightFactors {
    /// `luma_weight`.
    pub luma_weight: [i16; 32],
    /// `luma_offset`.
    pub luma_offset: [i16; 32],
    /// `chroma_weight`, Cb and Cr.
    pub chroma_weight: [[i16; 2]; 32],
    /// `chroma_offset`, Cb and Cr.
    pub chroma_offset: [[i16; 2]; 32],
}

/// `struct v4l2_ctrl_h264_pred_weights` (`V4L2_CID_STATELESS_H264_PRED_WEIGHTS`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredWeights {
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u16,
    /// `chroma_log2_weight_denom`.
    pub chroma_log2_weight_denom: u16,
    /// `weight_factors`: list 0, then list 1.
    pub weight_factors: [WeightFactors; 2],
}

/// `struct v4l2_h264_reference`: one entry of a reference picture list.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reference {
    /// How the picture is referenced: [`TOP_FIELD_REF`], [`BOTTOM_FIELD_REF`] or [`FRAME_REF`].
    pub fields: u8,
    /// The index into [`DecodeParams::dpb`].
    pub index: u8,
}

/// `struct v4l2_ctrl_h264_slice_params` (`V4L2_CID_STATELESS_H264_SLICE_PARAMS`): the slice
/// syntax elements that change from slice to slice. The invariant ones are in
/// [`DecodeParams`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SliceParams {
    /// The offset in bits to `slice_data()` from the start of this slice.
    pub header_bit_size: u32,
    /// `first_mb_in_slice`.
    pub first_mb_in_slice: u32,
    /// `slice_type`: [`SLICE_TYPE_P`] and the like.
    pub slice_type: u8,
    /// `colour_plane_id`.
    pub colour_plane_id: u8,
    /// `redundant_pic_cnt`.
    pub redundant_pic_cnt: u8,
    /// `cabac_init_idc`.
    pub cabac_init_idc: u8,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i8,
    /// `slice_qs_delta`.
    pub slice_qs_delta: i8,
    /// `disable_deblocking_filter_idc`.
    pub disable_deblocking_filter_idc: u8,
    /// `slice_alpha_c0_offset_div2`.
    pub slice_alpha_c0_offset_div2: i8,
    /// `slice_beta_offset_div2`.
    pub slice_beta_offset_div2: i8,
    /// `num_ref_idx_l0_active_minus1`.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`.
    pub num_ref_idx_l1_active_minus1: u8,
    /// Reserved, zero.
    pub reserved: u8,
    /// `ref_pic_list0`, after the slice's modifications.
    pub ref_pic_list0: [Reference; REF_LIST_LEN],
    /// `ref_pic_list1`, after the slice's modifications.
    pub ref_pic_list1: [Reference; REF_LIST_LEN],
    /// [`slice_flags`].
    pub flags: u32,
}

/// `struct v4l2_h264_dpb_entry`: one decoded picture buffer entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DpbEntry {
    /// The timestamp of the capture buffer that holds the picture, as
    /// `v4l2_timeval_to_ns()` gives it: whole microseconds, in nanoseconds. This is how the
    /// kernel finds the reference, so it is the value the decoding job stamped its picture with.
    pub reference_ts: u64,
    /// `PicNum`.
    pub pic_num: u32,
    /// `frame_num`.
    pub frame_num: u16,
    /// How the entry is referenced: [`TOP_FIELD_REF`], [`BOTTOM_FIELD_REF`] or [`FRAME_REF`].
    pub fields: u8,
    /// Reserved, zero.
    pub reserved: [u8; 5],
    /// `TopFieldOrderCnt`.
    pub top_field_order_cnt: i32,
    /// `BottomFieldOrderCnt`.
    pub bottom_field_order_cnt: i32,
    /// [`dpb_flags`].
    pub flags: u32,
}

/// `struct v4l2_ctrl_h264_decode_params` (`V4L2_CID_STATELESS_H264_DECODE_PARAMS`): the
/// slice-invariant syntax elements and the decoded picture buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeParams {
    /// The decoded picture buffer.
    pub dpb: [DpbEntry; NUM_DPB_ENTRIES],
    /// `nal_ref_idc`.
    pub nal_ref_idc: u16,
    /// `frame_num`.
    pub frame_num: u16,
    /// `TopFieldOrderCnt` of the picture.
    pub top_field_order_cnt: i32,
    /// `BottomFieldOrderCnt` of the picture.
    pub bottom_field_order_cnt: i32,
    /// `idr_pic_id`.
    pub idr_pic_id: u16,
    /// `pic_order_cnt_lsb`.
    pub pic_order_cnt_lsb: u16,
    /// `delta_pic_order_cnt_bottom`.
    pub delta_pic_order_cnt_bottom: i32,
    /// `delta_pic_order_cnt0`.
    pub delta_pic_order_cnt0: i32,
    /// `delta_pic_order_cnt1`.
    pub delta_pic_order_cnt1: i32,
    /// The size in bits of `dec_ref_pic_marking()`.
    pub dec_ref_pic_marking_bit_size: u32,
    /// The size in bits of the picture order count syntax.
    pub pic_order_cnt_bit_size: u32,
    /// `slice_group_change_cycle`.
    pub slice_group_change_cycle: u32,
    /// Reserved, zero.
    pub reserved: u32,
    /// [`decode_flags`].
    pub flags: u32,
}

payload!(
    Sps,
    Pps,
    ScalingMatrix,
    PredWeights,
    SliceParams,
    DecodeParams
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The sizes and offsets `linux/v4l2-controls.h` gives, measured with a C compiler on
    /// x86_64 (`gcc -m64`) against the Linux 7.2 header.
    #[test]
    fn structures_match_the_kernel_layout() {
        assert_eq!(size_of::<Sps>(), 1048);
        assert_eq!(offset_of!(Sps, offset_for_ref_frame), 12);
        assert_eq!(offset_of!(Sps, offset_for_non_ref_pic), 1032);
        assert_eq!(offset_of!(Sps, offset_for_top_to_bottom_field), 1036);
        assert_eq!(offset_of!(Sps, pic_width_in_mbs_minus1), 1040);
        assert_eq!(offset_of!(Sps, pic_height_in_map_units_minus1), 1042);
        assert_eq!(offset_of!(Sps, flags), 1044);

        assert_eq!(size_of::<Pps>(), 12);
        assert_eq!(offset_of!(Pps, weighted_bipred_idc), 5);
        assert_eq!(offset_of!(Pps, pic_init_qp_minus26), 6);
        assert_eq!(offset_of!(Pps, second_chroma_qp_index_offset), 9);
        assert_eq!(offset_of!(Pps, flags), 10);

        assert_eq!(size_of::<ScalingMatrix>(), 480);
        assert_eq!(offset_of!(ScalingMatrix, scaling_list_8x8), 96);

        assert_eq!(size_of::<WeightFactors>(), 384);
        assert_eq!(offset_of!(WeightFactors, chroma_weight), 128);
        assert_eq!(offset_of!(WeightFactors, chroma_offset), 256);
        assert_eq!(size_of::<PredWeights>(), 772);
        assert_eq!(offset_of!(PredWeights, weight_factors), 4);

        assert_eq!(size_of::<Reference>(), 2);

        assert_eq!(size_of::<SliceParams>(), 152);
        assert_eq!(offset_of!(SliceParams, header_bit_size), 0);
        assert_eq!(offset_of!(SliceParams, first_mb_in_slice), 4);
        assert_eq!(offset_of!(SliceParams, slice_type), 8);
        assert_eq!(offset_of!(SliceParams, slice_beta_offset_div2), 16);
        assert_eq!(offset_of!(SliceParams, num_ref_idx_l1_active_minus1), 18);
        assert_eq!(offset_of!(SliceParams, reserved), 19);
        assert_eq!(offset_of!(SliceParams, ref_pic_list0), 20);
        assert_eq!(offset_of!(SliceParams, ref_pic_list1), 84);
        assert_eq!(offset_of!(SliceParams, flags), 148);

        assert_eq!(size_of::<DpbEntry>(), 32);
        assert_eq!(offset_of!(DpbEntry, reference_ts), 0);
        assert_eq!(offset_of!(DpbEntry, pic_num), 8);
        assert_eq!(offset_of!(DpbEntry, frame_num), 12);
        assert_eq!(offset_of!(DpbEntry, fields), 14);
        assert_eq!(offset_of!(DpbEntry, top_field_order_cnt), 20);
        assert_eq!(offset_of!(DpbEntry, bottom_field_order_cnt), 24);
        assert_eq!(offset_of!(DpbEntry, flags), 28);

        assert_eq!(size_of::<DecodeParams>(), 560);
        assert_eq!(offset_of!(DecodeParams, dpb), 0);
        assert_eq!(offset_of!(DecodeParams, nal_ref_idc), 512);
        assert_eq!(offset_of!(DecodeParams, top_field_order_cnt), 516);
        assert_eq!(offset_of!(DecodeParams, idr_pic_id), 524);
        assert_eq!(offset_of!(DecodeParams, delta_pic_order_cnt_bottom), 528);
        assert_eq!(offset_of!(DecodeParams, dec_ref_pic_marking_bit_size), 540);
        assert_eq!(offset_of!(DecodeParams, slice_group_change_cycle), 548);
        assert_eq!(offset_of!(DecodeParams, reserved), 552);
        assert_eq!(offset_of!(DecodeParams, flags), 556);
    }

    #[test]
    fn control_ids_and_formats_match_the_header() {
        // V4L2_CID_CODEC_STATELESS_BASE is V4L2_CTRL_CLASS_CODEC_STATELESS | 0x900.
        assert_eq!(CID_DECODE_MODE, 0xa40900);
        assert_eq!(CID_START_CODE, 0xa40901);
        assert_eq!(CID_SPS, 0xa40902);
        assert_eq!(CID_PPS, 0xa40903);
        assert_eq!(CID_SCALING_MATRIX, 0xa40904);
        assert_eq!(CID_PRED_WEIGHTS, 0xa40905);
        assert_eq!(CID_SLICE_PARAMS, 0xa40906);
        assert_eq!(CID_DECODE_PARAMS, 0xa40907);
        assert_eq!(PIX_FMT_H264_SLICE, 0x3436_3253);
    }

    #[test]
    fn menu_values_match_the_header() {
        assert_eq!(DECODE_MODE_SLICE_BASED, 0);
        assert_eq!(DECODE_MODE_FRAME_BASED, 1);
        assert_eq!(START_CODE_NONE, 0);
        assert_eq!(START_CODE_ANNEX_B, 1);
        assert_eq!(
            [
                SLICE_TYPE_P,
                SLICE_TYPE_B,
                SLICE_TYPE_I,
                SLICE_TYPE_SP,
                SLICE_TYPE_SI
            ],
            [0, 1, 2, 3, 4]
        );
        assert_eq!((NUM_DPB_ENTRIES, REF_LIST_LEN, FRAME_REF), (16, 32, 0x3));
    }

    #[test]
    fn flags_are_single_bits_at_the_header_values() {
        assert_eq!(constraint_set::SET5, 0x20);
        assert_eq!(sps_flags::DIRECT_8X8_INFERENCE, 0x40);
        assert_eq!(pps_flags::SCALING_MATRIX_PRESENT, 0x80);
        assert_eq!(slice_flags::SP_FOR_SWITCH, 0x02);
        assert_eq!(dpb_flags::FIELD, 0x08);
        assert_eq!(decode_flags::BFRAME, 0x10);
    }

    #[test]
    fn a_writable_payload_is_the_structure_itself() {
        let mut sps = Sps::default();
        sps.as_bytes_mut()[1..2].copy_from_slice(&[constraint_set::SET0]);
        sps.as_bytes_mut()[1044..1048].copy_from_slice(&sps_flags::FRAME_MBS_ONLY.to_ne_bytes());
        assert_eq!(sps.constraint_set_flags, constraint_set::SET0);
        assert_eq!(sps.flags, sps_flags::FRAME_MBS_ONLY);

        let mut slice = SliceParams::default();
        slice.as_bytes_mut()[8] = SLICE_TYPE_B;
        assert_eq!(slice.slice_type, SLICE_TYPE_B);
        assert_eq!(slice.as_bytes_mut().len(), 152);
    }

    #[test]
    fn a_payload_is_the_structure_in_order() {
        let mut p = DecodeParams {
            nal_ref_idc: 3,
            ..DecodeParams::default()
        };
        p.flags = decode_flags::IDR_PIC;
        let bytes = p.as_bytes();
        assert_eq!(bytes.len(), 560);
        assert_eq!(&bytes[512..514], &3u16.to_ne_bytes());
        assert_eq!(&bytes[556..560], &1u32.to_ne_bytes());

        let pps = Pps {
            flags: pps_flags::WEIGHTED_PRED,
            ..Pps::default()
        };
        assert_eq!(&pps.as_bytes()[10..12], &4u16.to_ne_bytes());
    }

    #[test]
    fn a_prediction_weight_table_is_required_by_weighted_prediction_and_bipred() {
        let weighted = Pps {
            flags: pps_flags::WEIGHTED_PRED,
            ..Pps::default()
        };
        assert!(weighted.pred_weights_required(SLICE_TYPE_P));
        assert!(weighted.pred_weights_required(SLICE_TYPE_SP));
        assert!(!weighted.pred_weights_required(SLICE_TYPE_B));
        assert!(!weighted.pred_weights_required(SLICE_TYPE_I));

        let bipred = Pps {
            weighted_bipred_idc: 1,
            ..Pps::default()
        };
        assert!(bipred.pred_weights_required(SLICE_TYPE_B));
        assert!(!bipred.pred_weights_required(SLICE_TYPE_P));
        assert!(!Pps::default().pred_weights_required(SLICE_TYPE_B));
    }

    #[test]
    fn chroma_format_is_present_in_the_high_profiles_only() {
        let profile = |profile_idc| Sps {
            profile_idc,
            ..Sps::default()
        };
        assert!(profile(100).has_chroma_format());
        assert!(profile(244).has_chroma_format());
        assert!(!profile(66).has_chroma_format());
        assert!(!profile(77).has_chroma_format());
    }
}
