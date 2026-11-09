//! The stateless HEVC decoder's controls (`V4L2_CID_STATELESS_HEVC_*`): the
//! parameter-set and slice structures of `linux/v4l2-controls.h`, with the
//! layout checked against the header in the tests. A decoder that takes these
//! controls takes its headers as the caller parsed them.
//!
//! Each structure has no padding (the header's `reserved` fields fill the
//! gaps), so its bytes are its fields, and [`Sps::as_bytes`] and the others
//! give the bytes a `VIDIOC_S_EXT_CTRLS` payload takes. `as_bytes_mut` gives the same
//! bytes as the exclusive slice [`super::device::ExtControl::payload`] borrows, so a
//! decoder sets them without copying.
//!
//! Provenance: the Linux uAPI headers are GPL-2.0 WITH Linux-syscall-note. The names,
//! numbers and layouts below are interface facts transcribed from them; no kernel
//! source is copied.

/// `V4L2_CID_STATELESS_HEVC_SPS`.
pub const CID_SPS: u32 = 0x00a4_0a90;
/// `V4L2_CID_STATELESS_HEVC_PPS`.
pub const CID_PPS: u32 = 0x00a4_0a91;
/// `V4L2_CID_STATELESS_HEVC_SLICE_PARAMS`.
pub const CID_SLICE_PARAMS: u32 = 0x00a4_0a92;
/// `V4L2_CID_STATELESS_HEVC_SCALING_MATRIX`.
pub const CID_SCALING_MATRIX: u32 = 0x00a4_0a93;
/// `V4L2_CID_STATELESS_HEVC_DECODE_PARAMS`.
pub const CID_DECODE_PARAMS: u32 = 0x00a4_0a94;
/// `V4L2_CID_STATELESS_HEVC_DECODE_MODE`.
pub const CID_DECODE_MODE: u32 = 0x00a4_0a95;
/// `V4L2_CID_STATELESS_HEVC_START_CODE`.
pub const CID_START_CODE: u32 = 0x00a4_0a96;

/// `V4L2_STATELESS_HEVC_DECODE_MODE_SLICE_BASED`.
pub const DECODE_MODE_SLICE_BASED: i32 = 0;
/// `V4L2_STATELESS_HEVC_DECODE_MODE_FRAME_BASED`.
pub const DECODE_MODE_FRAME_BASED: i32 = 1;
/// `V4L2_STATELESS_HEVC_START_CODE_NONE`.
pub const START_CODE_NONE: i32 = 0;
/// `V4L2_STATELESS_HEVC_START_CODE_ANNEX_B`: the buffer's slices carry start codes.
pub const START_CODE_ANNEX_B: i32 = 1;

/// `V4L2_PIX_FMT_HEVC_SLICE` (`S265`): parsed slices, for a stateless decoder's
/// OUTPUT queue.
pub const PIX_FMT_HEVC_SLICE: u32 = u32::from_le_bytes(*b"S265");

/// `V4L2_HEVC_SLICE_TYPE_I`.
pub const SLICE_TYPE_I: u8 = 2;

/// `V4L2_HEVC_SPS_FLAG_*`.
pub mod sps_flags {
    /// `V4L2_HEVC_SPS_FLAG_SEPARATE_COLOUR_PLANE`.
    pub const SEPARATE_COLOUR_PLANE: u64 = 1 << 0;
    /// `V4L2_HEVC_SPS_FLAG_SCALING_LIST_ENABLED`.
    pub const SCALING_LIST_ENABLED: u64 = 1 << 1;
    /// `V4L2_HEVC_SPS_FLAG_AMP_ENABLED`.
    pub const AMP_ENABLED: u64 = 1 << 2;
    /// `V4L2_HEVC_SPS_FLAG_SAMPLE_ADAPTIVE_OFFSET`.
    pub const SAMPLE_ADAPTIVE_OFFSET: u64 = 1 << 3;
    /// `V4L2_HEVC_SPS_FLAG_PCM_ENABLED`.
    pub const PCM_ENABLED: u64 = 1 << 4;
    /// `V4L2_HEVC_SPS_FLAG_PCM_LOOP_FILTER_DISABLED`.
    pub const PCM_LOOP_FILTER_DISABLED: u64 = 1 << 5;
    /// `V4L2_HEVC_SPS_FLAG_LONG_TERM_REF_PICS_PRESENT`.
    pub const LONG_TERM_REF_PICS_PRESENT: u64 = 1 << 6;
    /// `V4L2_HEVC_SPS_FLAG_SPS_TEMPORAL_MVP_ENABLED`.
    pub const TEMPORAL_MVP_ENABLED: u64 = 1 << 7;
    /// `V4L2_HEVC_SPS_FLAG_STRONG_INTRA_SMOOTHING_ENABLED`.
    pub const STRONG_INTRA_SMOOTHING_ENABLED: u64 = 1 << 8;
}

/// `V4L2_HEVC_PPS_FLAG_*`.
pub mod pps_flags {
    /// `V4L2_HEVC_PPS_FLAG_DEPENDENT_SLICE_SEGMENT_ENABLED`.
    pub const DEPENDENT_SLICE_SEGMENT_ENABLED: u64 = 1 << 0;
    /// `V4L2_HEVC_PPS_FLAG_OUTPUT_FLAG_PRESENT`.
    pub const OUTPUT_FLAG_PRESENT: u64 = 1 << 1;
    /// `V4L2_HEVC_PPS_FLAG_SIGN_DATA_HIDING_ENABLED`.
    pub const SIGN_DATA_HIDING_ENABLED: u64 = 1 << 2;
    /// `V4L2_HEVC_PPS_FLAG_CABAC_INIT_PRESENT`.
    pub const CABAC_INIT_PRESENT: u64 = 1 << 3;
    /// `V4L2_HEVC_PPS_FLAG_CONSTRAINED_INTRA_PRED`.
    pub const CONSTRAINED_INTRA_PRED: u64 = 1 << 4;
    /// `V4L2_HEVC_PPS_FLAG_TRANSFORM_SKIP_ENABLED`.
    pub const TRANSFORM_SKIP_ENABLED: u64 = 1 << 5;
    /// `V4L2_HEVC_PPS_FLAG_CU_QP_DELTA_ENABLED`.
    pub const CU_QP_DELTA_ENABLED: u64 = 1 << 6;
    /// `V4L2_HEVC_PPS_FLAG_PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT`.
    pub const SLICE_CHROMA_QP_OFFSETS_PRESENT: u64 = 1 << 7;
    /// `V4L2_HEVC_PPS_FLAG_WEIGHTED_PRED`.
    pub const WEIGHTED_PRED: u64 = 1 << 8;
    /// `V4L2_HEVC_PPS_FLAG_WEIGHTED_BIPRED`.
    pub const WEIGHTED_BIPRED: u64 = 1 << 9;
    /// `V4L2_HEVC_PPS_FLAG_TRANSQUANT_BYPASS_ENABLED`.
    pub const TRANSQUANT_BYPASS_ENABLED: u64 = 1 << 10;
    /// `V4L2_HEVC_PPS_FLAG_TILES_ENABLED`.
    pub const TILES_ENABLED: u64 = 1 << 11;
    /// `V4L2_HEVC_PPS_FLAG_ENTROPY_CODING_SYNC_ENABLED`.
    pub const ENTROPY_CODING_SYNC_ENABLED: u64 = 1 << 12;
    /// `V4L2_HEVC_PPS_FLAG_LOOP_FILTER_ACROSS_TILES_ENABLED`.
    pub const LOOP_FILTER_ACROSS_TILES_ENABLED: u64 = 1 << 13;
    /// `V4L2_HEVC_PPS_FLAG_PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED`.
    pub const LOOP_FILTER_ACROSS_SLICES_ENABLED: u64 = 1 << 14;
    /// `V4L2_HEVC_PPS_FLAG_DEBLOCKING_FILTER_OVERRIDE_ENABLED`.
    pub const DEBLOCKING_FILTER_OVERRIDE_ENABLED: u64 = 1 << 15;
    /// `V4L2_HEVC_PPS_FLAG_PPS_DISABLE_DEBLOCKING_FILTER`.
    pub const DISABLE_DEBLOCKING_FILTER: u64 = 1 << 16;
    /// `V4L2_HEVC_PPS_FLAG_LISTS_MODIFICATION_PRESENT`.
    pub const LISTS_MODIFICATION_PRESENT: u64 = 1 << 17;
    /// `V4L2_HEVC_PPS_FLAG_SLICE_SEGMENT_HEADER_EXTENSION_PRESENT`.
    pub const SLICE_SEGMENT_HEADER_EXTENSION_PRESENT: u64 = 1 << 18;
    /// `V4L2_HEVC_PPS_FLAG_DEBLOCKING_FILTER_CONTROL_PRESENT`.
    pub const DEBLOCKING_FILTER_CONTROL_PRESENT: u64 = 1 << 19;
    /// `V4L2_HEVC_PPS_FLAG_UNIFORM_SPACING`.
    pub const UNIFORM_SPACING: u64 = 1 << 20;
}

/// `V4L2_HEVC_SLICE_PARAMS_FLAG_*`.
pub mod slice_flags {
    /// `V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_SAO_LUMA`.
    pub const SAO_LUMA: u64 = 1 << 0;
    /// `V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_SAO_CHROMA`.
    pub const SAO_CHROMA: u64 = 1 << 1;
    /// `V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_DEBLOCKING_FILTER_DISABLED`.
    pub const DEBLOCKING_FILTER_DISABLED: u64 = 1 << 7;
    /// `V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED`.
    pub const LOOP_FILTER_ACROSS_SLICES_ENABLED: u64 = 1 << 8;
    /// `V4L2_HEVC_SLICE_PARAMS_FLAG_DEPENDENT_SLICE_SEGMENT`.
    pub const DEPENDENT_SLICE_SEGMENT: u64 = 1 << 9;
}

/// `V4L2_HEVC_DECODE_PARAM_FLAG_*`.
pub mod decode_flags {
    /// `V4L2_HEVC_DECODE_PARAM_FLAG_IRAP_PIC`.
    pub const IRAP_PIC: u64 = 1 << 0;
    /// `V4L2_HEVC_DECODE_PARAM_FLAG_IDR_PIC`.
    pub const IDR_PIC: u64 = 1 << 1;
    /// `V4L2_HEVC_DECODE_PARAM_FLAG_NO_OUTPUT_OF_PRIOR`.
    pub const NO_OUTPUT_OF_PRIOR: u64 = 1 << 2;
}

/// `V4L2_HEVC_DPB_ENTRIES_NUM_MAX`.
pub const DPB_ENTRIES_MAX: usize = 16;

/// Views a structure of this module as its bytes. Only for the structures
/// here: each has no padding, so every byte is a field's.
fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: `T` is one of this module's structures, all `repr(C)` integer
    // arrays with no padding (checked by the layout tests), so every byte of
    // the value is initialised; the slice covers exactly `value` and borrows it.
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

/// [`bytes_of`] for writing: as there, every byte of `T` is a field's, and every field is
/// an integer, so any bytes written leave a valid value.
fn bytes_of_mut<T: Copy>(value: &mut T) -> &mut [u8] {
    let len = size_of::<T>();
    let ptr = std::ptr::from_mut(value).cast::<u8>();
    // SAFETY: `ptr` is the exclusive borrow of `value` for `len` bytes; `T` is one of this
    // module's padding-free integer structures, so every byte is initialised and any
    // byte is a valid value, and the slice borrows `value`.
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}

/// `struct v4l2_ctrl_hevc_sps` (`V4L2_CID_STATELESS_HEVC_SPS`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sps {
    /// `video_parameter_set_id`.
    pub video_parameter_set_id: u8,
    /// `seq_parameter_set_id`.
    pub seq_parameter_set_id: u8,
    /// `pic_width_in_luma_samples`.
    pub pic_width_in_luma_samples: u16,
    /// `pic_height_in_luma_samples`.
    pub pic_height_in_luma_samples: u16,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u8,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u8,
    /// `log2_max_pic_order_cnt_lsb_minus4`.
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    /// `sps_max_dec_pic_buffering_minus1`.
    pub sps_max_dec_pic_buffering_minus1: u8,
    /// `sps_max_num_reorder_pics`.
    pub sps_max_num_reorder_pics: u8,
    /// `sps_max_latency_increase_plus1`.
    pub sps_max_latency_increase_plus1: u8,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// `log2_min_luma_transform_block_size_minus2`.
    pub log2_min_luma_transform_block_size_minus2: u8,
    /// `log2_diff_max_min_luma_transform_block_size`.
    pub log2_diff_max_min_luma_transform_block_size: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u8,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u8,
    /// `pcm_sample_bit_depth_luma_minus1`.
    pub pcm_sample_bit_depth_luma_minus1: u8,
    /// `pcm_sample_bit_depth_chroma_minus1`.
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    /// `log2_min_pcm_luma_coding_block_size_minus3`.
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_pcm_luma_coding_block_size`.
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u8,
    /// `num_long_term_ref_pics_sps`.
    pub num_long_term_ref_pics_sps: u8,
    /// `chroma_format_idc`.
    pub chroma_format_idc: u8,
    /// `sps_max_sub_layers_minus1`.
    pub sps_max_sub_layers_minus1: u8,
    /// Reserved, zero.
    pub reserved: [u8; 6],
    /// [`sps_flags`].
    pub flags: u64,
}

impl Sps {
    /// The control's payload.
    pub fn as_bytes(&self) -> &[u8] {
        bytes_of(self)
    }

    /// The control's payload, for [`super::device::ExtControl::payload`].
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        bytes_of_mut(self)
    }
}

/// `struct v4l2_ctrl_hevc_pps` (`V4L2_CID_STATELESS_HEVC_PPS`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pps {
    /// `pic_parameter_set_id`.
    pub pic_parameter_set_id: u8,
    /// `num_extra_slice_header_bits`.
    pub num_extra_slice_header_bits: u8,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// `init_qp_minus26`.
    pub init_qp_minus26: i8,
    /// `diff_cu_qp_delta_depth`.
    pub diff_cu_qp_delta_depth: u8,
    /// `pps_cb_qp_offset`.
    pub pps_cb_qp_offset: i8,
    /// `pps_cr_qp_offset`.
    pub pps_cr_qp_offset: i8,
    /// `num_tile_columns_minus1`.
    pub num_tile_columns_minus1: u8,
    /// `num_tile_rows_minus1`.
    pub num_tile_rows_minus1: u8,
    /// `column_width_minus1`.
    pub column_width_minus1: [u8; 20],
    /// `row_height_minus1`.
    pub row_height_minus1: [u8; 22],
    /// `pps_beta_offset_div2`.
    pub pps_beta_offset_div2: i8,
    /// `pps_tc_offset_div2`.
    pub pps_tc_offset_div2: i8,
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u8,
    /// Reserved, zero.
    pub reserved: u8,
    /// [`pps_flags`].
    pub flags: u64,
}

impl Pps {
    /// The control's payload.
    pub fn as_bytes(&self) -> &[u8] {
        bytes_of(self)
    }

    /// The control's payload, for [`super::device::ExtControl::payload`].
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        bytes_of_mut(self)
    }
}

/// `struct v4l2_hevc_dpb_entry`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DpbEntry {
    /// The buffer's timestamp (its key in the decoder's DPB).
    pub timestamp: u64,
    /// `V4L2_HEVC_DPB_ENTRY_LONG_TERM_REFERENCE` and the like.
    pub flags: u8,
    /// `field_pic`.
    pub field_pic: u8,
    /// Reserved, zero.
    pub reserved: u16,
    /// `pic_order_cnt_val`.
    pub pic_order_cnt_val: i32,
}

/// `struct v4l2_hevc_pred_weight_table`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredWeightTable {
    /// `delta_luma_weight_l0`.
    pub delta_luma_weight_l0: [i8; DPB_ENTRIES_MAX],
    /// `luma_offset_l0`.
    pub luma_offset_l0: [i8; DPB_ENTRIES_MAX],
    /// `delta_chroma_weight_l0`.
    pub delta_chroma_weight_l0: [[i8; 2]; DPB_ENTRIES_MAX],
    /// `chroma_offset_l0`.
    pub chroma_offset_l0: [[i8; 2]; DPB_ENTRIES_MAX],
    /// `delta_luma_weight_l1`.
    pub delta_luma_weight_l1: [i8; DPB_ENTRIES_MAX],
    /// `luma_offset_l1`.
    pub luma_offset_l1: [i8; DPB_ENTRIES_MAX],
    /// `delta_chroma_weight_l1`.
    pub delta_chroma_weight_l1: [[i8; 2]; DPB_ENTRIES_MAX],
    /// `chroma_offset_l1`.
    pub chroma_offset_l1: [[i8; 2]; DPB_ENTRIES_MAX],
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u8,
    /// `delta_chroma_log2_weight_denom`.
    pub delta_chroma_log2_weight_denom: i8,
}

/// `struct v4l2_ctrl_hevc_slice_params` (`V4L2_CID_STATELESS_HEVC_SLICE_PARAMS`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SliceParams {
    /// The slice's size in bits.
    pub bit_size: u32,
    /// The byte offset of the slice data in the buffer.
    pub data_byte_offset: u32,
    /// `num_entry_point_offsets`.
    pub num_entry_point_offsets: u32,
    /// `nal_unit_type`.
    pub nal_unit_type: u8,
    /// `nuh_temporal_id_plus1`.
    pub nuh_temporal_id_plus1: u8,
    /// `slice_type` ([`SLICE_TYPE_I`] and the like).
    pub slice_type: u8,
    /// `colour_plane_id`.
    pub colour_plane_id: u8,
    /// `slice_pic_order_cnt`.
    pub slice_pic_order_cnt: i32,
    /// `num_ref_idx_l0_active_minus1`.
    pub num_ref_idx_l0_active_minus1: u8,
    /// `num_ref_idx_l1_active_minus1`.
    pub num_ref_idx_l1_active_minus1: u8,
    /// `collocated_ref_idx`.
    pub collocated_ref_idx: u8,
    /// `five_minus_max_num_merge_cand`.
    pub five_minus_max_num_merge_cand: u8,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i8,
    /// `slice_cb_qp_offset`.
    pub slice_cb_qp_offset: i8,
    /// `slice_cr_qp_offset`.
    pub slice_cr_qp_offset: i8,
    /// `slice_act_y_qp_offset`.
    pub slice_act_y_qp_offset: i8,
    /// `slice_act_cb_qp_offset`.
    pub slice_act_cb_qp_offset: i8,
    /// `slice_act_cr_qp_offset`.
    pub slice_act_cr_qp_offset: i8,
    /// `slice_beta_offset_div2`.
    pub slice_beta_offset_div2: i8,
    /// `slice_tc_offset_div2`.
    pub slice_tc_offset_div2: i8,
    /// `pic_struct`.
    pub pic_struct: u8,
    /// Reserved, zero.
    pub reserved0: [u8; 3],
    /// `slice_segment_addr`.
    pub slice_segment_addr: u32,
    /// `ref_idx_l0`.
    pub ref_idx_l0: [u8; DPB_ENTRIES_MAX],
    /// `ref_idx_l1`.
    pub ref_idx_l1: [u8; DPB_ENTRIES_MAX],
    /// `short_term_ref_pic_set_size`, in bits.
    pub short_term_ref_pic_set_size: u16,
    /// `long_term_ref_pic_set_size`, in bits.
    pub long_term_ref_pic_set_size: u16,
    /// `pred_weight_table`.
    pub pred_weight_table: PredWeightTable,
    /// Reserved, zero.
    pub reserved1: [u8; 2],
    /// [`slice_flags`].
    pub flags: u64,
}

impl SliceParams {
    /// The control's payload (one element of the array control).
    pub fn as_bytes(&self) -> &[u8] {
        bytes_of(self)
    }

    /// The control's payload, for [`super::device::ExtControl::payload`].
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        bytes_of_mut(self)
    }
}

/// `struct v4l2_ctrl_hevc_decode_params` (`V4L2_CID_STATELESS_HEVC_DECODE_PARAMS`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeParams {
    /// `pic_order_cnt_val`.
    pub pic_order_cnt_val: i32,
    /// `short_term_ref_pic_set_size`, in bits.
    pub short_term_ref_pic_set_size: u16,
    /// `long_term_ref_pic_set_size`, in bits.
    pub long_term_ref_pic_set_size: u16,
    /// `num_active_dpb_entries`.
    pub num_active_dpb_entries: u8,
    /// `num_poc_st_curr_before`.
    pub num_poc_st_curr_before: u8,
    /// `num_poc_st_curr_after`.
    pub num_poc_st_curr_after: u8,
    /// `num_poc_lt_curr`.
    pub num_poc_lt_curr: u8,
    /// `poc_st_curr_before`: DPB indices.
    pub poc_st_curr_before: [u8; DPB_ENTRIES_MAX],
    /// `poc_st_curr_after`: DPB indices.
    pub poc_st_curr_after: [u8; DPB_ENTRIES_MAX],
    /// `poc_lt_curr`: DPB indices.
    pub poc_lt_curr: [u8; DPB_ENTRIES_MAX],
    /// `num_delta_pocs_of_ref_rps_idx`.
    pub num_delta_pocs_of_ref_rps_idx: u8,
    /// Reserved, zero.
    pub reserved: [u8; 3],
    /// `dpb`.
    pub dpb: [DpbEntry; DPB_ENTRIES_MAX],
    /// [`decode_flags`].
    pub flags: u64,
}

impl DecodeParams {
    /// The control's payload.
    pub fn as_bytes(&self) -> &[u8] {
        bytes_of(self)
    }

    /// The control's payload, for [`super::device::ExtControl::payload`].
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        bytes_of_mut(self)
    }
}

/// `struct v4l2_ctrl_hevc_scaling_matrix` (`V4L2_CID_STATELESS_HEVC_SCALING_MATRIX`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalingMatrix {
    /// `scaling_list_4x4`.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// `scaling_list_8x8`.
    pub scaling_list_8x8: [[u8; 64]; 6],
    /// `scaling_list_16x16`.
    pub scaling_list_16x16: [[u8; 64]; 6],
    /// `scaling_list_32x32`.
    pub scaling_list_32x32: [[u8; 64]; 2],
    /// `scaling_list_dc_coef_16x16`.
    pub scaling_list_dc_coef_16x16: [u8; 6],
    /// `scaling_list_dc_coef_32x32`.
    pub scaling_list_dc_coef_32x32: [u8; 2],
}

impl ScalingMatrix {
    /// The control's payload.
    pub fn as_bytes(&self) -> &[u8] {
        bytes_of(self)
    }

    /// The control's payload, for [`super::device::ExtControl::payload`].
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        bytes_of_mut(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The sizes and offsets `linux/v4l2-controls.h` gives on x86_64 and
    /// aarch64, measured with the header's own structures.
    #[test]
    fn structures_match_the_kernel_layout() {
        assert_eq!(size_of::<Sps>(), 40);
        assert_eq!(offset_of!(Sps, flags), 32);
        assert_eq!(size_of::<Pps>(), 64);
        assert_eq!(offset_of!(Pps, column_width_minus1), 10);
        assert_eq!(offset_of!(Pps, row_height_minus1), 30);
        assert_eq!(offset_of!(Pps, pps_beta_offset_div2), 52);
        assert_eq!(offset_of!(Pps, log2_parallel_merge_level_minus2), 54);
        assert_eq!(offset_of!(Pps, flags), 56);
        assert_eq!(size_of::<DpbEntry>(), 16);
        assert_eq!(offset_of!(DpbEntry, pic_order_cnt_val), 12);
        assert_eq!(size_of::<PredWeightTable>(), 194);
        assert_eq!(size_of::<SliceParams>(), 280);
        assert_eq!(offset_of!(SliceParams, slice_segment_addr), 36);
        assert_eq!(offset_of!(SliceParams, ref_idx_l0), 40);
        assert_eq!(offset_of!(SliceParams, short_term_ref_pic_set_size), 72);
        assert_eq!(offset_of!(SliceParams, pred_weight_table), 76);
        assert_eq!(offset_of!(SliceParams, reserved1), 270);
        assert_eq!(offset_of!(SliceParams, flags), 272);
        assert_eq!(offset_of!(SliceParams, slice_qp_delta), 24);
        assert_eq!(offset_of!(SliceParams, slice_beta_offset_div2), 30);
        assert_eq!(offset_of!(SliceParams, pic_struct), 32);
        assert_eq!(size_of::<DecodeParams>(), 328);
        assert_eq!(offset_of!(DecodeParams, poc_st_curr_before), 12);
        assert_eq!(offset_of!(DecodeParams, num_delta_pocs_of_ref_rps_idx), 60);
        assert_eq!(offset_of!(DecodeParams, dpb), 64);
        assert_eq!(offset_of!(DecodeParams, flags), 320);
        assert_eq!(size_of::<ScalingMatrix>(), 1000);
    }

    #[test]
    fn control_ids_and_formats_match_the_header() {
        assert_eq!(CID_SPS, 0xa40a90);
        assert_eq!(CID_START_CODE, 0xa40a96);
        assert_eq!(PIX_FMT_HEVC_SLICE, 0x3536_3253);
    }

    #[test]
    fn a_writable_payload_is_the_structure_itself() {
        let mut sps = Sps::default();
        sps.as_bytes_mut()[4..6].copy_from_slice(&0x0280u16.to_ne_bytes());
        assert_eq!(sps.pic_height_in_luma_samples, 0x0280);
        let mut scaling = ScalingMatrix {
            scaling_list_4x4: [[0; 16]; 6],
            scaling_list_8x8: [[0; 64]; 6],
            scaling_list_16x16: [[0; 64]; 6],
            scaling_list_32x32: [[0; 64]; 2],
            scaling_list_dc_coef_16x16: [0; 6],
            scaling_list_dc_coef_32x32: [0; 2],
        };
        assert_eq!(scaling.as_bytes_mut().len(), 1000);
    }

    #[test]
    fn a_payload_is_the_structure_in_order() {
        let mut p = SliceParams {
            data_byte_offset: 0x0102_0304,
            ..SliceParams::default()
        };
        p.flags = 1 << 9;
        let bytes = p.as_bytes();
        assert_eq!(bytes.len(), 280);
        assert_eq!(&bytes[4..8], &0x0102_0304u32.to_ne_bytes());
        assert_eq!(&bytes[272..280], &(1u64 << 9).to_ne_bytes());
    }
}
