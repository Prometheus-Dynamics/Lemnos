//! The ISP35 buffers' `bytemuck` impls, and the C compiler's sizes and offsets for the same
//! headers (checked at compile time: the numbers come from a C program that prints `sizeof` and
//! `offsetof` for `headers/develop-6.1/rk-isp35-config.h`).

use super::isp35::*;

// SAFETY: a bitfield unit is one `Storage` field and nothing else, so it has no padding and is
// `Zeroable` wherever `Storage` is. Derive refuses a generic structure, so the impl is written
// here; the units are the only generic types of the bindgen layout.
unsafe impl<Storage: bytemuck::Pod> bytemuck::Zeroable for __BindgenBitfieldUnit<Storage> {}

// SAFETY: as for `Zeroable` above: one `Storage` field, no padding, `Copy` and `'static` through
// `Storage: Pod`, so every bit pattern of the unit is a value.
unsafe impl<Storage: bytemuck::Pod> bytemuck::Pod for __BindgenBitfieldUnit<Storage> {}

const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<isp35_isp_params_cfg>() == 13065);
    assert!(offset_of!(isp35_isp_params_cfg, module_ens) == 8);
    assert!(offset_of!(isp35_isp_params_cfg, module_cfg_update) == 16);
    assert!(offset_of!(isp35_isp_params_cfg, frame_id) == 24);
    assert!(offset_of!(isp35_isp_params_cfg, meas) == 28);
    assert!(offset_of!(isp35_isp_params_cfg, others) == 1665);
    assert!(offset_of!(isp35_isp_params_cfg, exposure) == 12969);
    assert!(size_of::<isp35_isp_meas_cfg>() == 1637);
    assert!(size_of::<isp35_isp_other_cfg>() == 11304);
    assert!(offset_of!(isp35_isp_other_cfg, bls_cfg) == 80);
    assert!(offset_of!(isp35_isp_other_cfg, sharp_cfg) == 7291);
    assert!(offset_of!(isp35_isp_other_cfg, enh_cfg) == 7760);
    assert!(size_of::<isp35_stat>() == 14530);
    assert!(offset_of!(isp35_stat, rawae3) == 0);
    assert!(offset_of!(isp35_stat, rawhist3) == 992);
    assert!(offset_of!(isp35_stat, rawae0) == 2016);
    assert!(offset_of!(isp35_stat, rawhist0) == 3008);
    assert!(offset_of!(isp35_stat, rawaf) == 4032);
    assert!(offset_of!(isp35_stat, rawawb) == 8864);
    assert!(offset_of!(isp35_stat, bay3d) == 10992);
    assert!(offset_of!(isp35_stat, sharp) == 11036);
    assert!(offset_of!(isp35_stat, enh) == 11070);
    assert!(offset_of!(isp35_stat, hist) == 11838);
    assert!(size_of::<rkisp35_stat_buffer>() == 14542);
    assert!(offset_of!(rkisp35_stat_buffer, meas_type) == 14530);
    assert!(offset_of!(rkisp35_stat_buffer, frame_id) == 14534);
    assert!(offset_of!(rkisp35_stat_buffer, params_id) == 14538);
};
