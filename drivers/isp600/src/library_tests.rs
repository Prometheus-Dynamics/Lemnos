//! The vendor user-space library's facts, checked against [`layout`] and [`tuning`]. The library
//! is `radxa/allwinner-isp6xx` at `516fd7443810e0807b289829dbae1ae5a62e0cb5`; every number here is
//! written out from the file and define named beside it, for ISP 602 (the A733's version, per the
//! library's `scripts/mksetup.sh`). The default register words are the library's own
//! `isp_default_reg` table (`isp6xx/libisp/isp_manage/isp_manage.c`, the `ISP_VERSION >= 600`
//! block with the `602` rows), read at the offsets the layout names.
//!
//! No code or comment from the library is copied. The repository has no licence; see
//! `docs/drivers.md`.

use crate::layout::{self, LOAD_BYTES, LOAD_REGISTERS_OFFSET, Refusal, reg};
use crate::tuning::{ChannelExpand, Image, expand, temporal};

// isp_comm.h, the ISP 602 branches of the load, table and statistics sizes.
const ISP_LOAD_DRAM_SIZE: usize = 0x7700;
const ISP_LOAD_REG_SIZE: usize = 0x1000;
const ISP_FE_TABLE_SIZE: usize = 0x1800;
const ISP_BAYER_TABLE_SIZE: usize = 0x2600;
const ISP_RGB_TABLE_SIZE: usize = 0x1200;
const ISP_YUV_TABLE_SIZE: usize = 0x1700;
const ISP_SAVE_DRAM_SIZE: usize = 0x6a00;
const ISP_SAVE_REG_SIZE: usize = 0x0100;
const ISP_SAVE_LOAD_DRAM_SIZE: usize = 0x3400;
const ISP_SAVE_LOAD_STATISTIC_SIZE: usize = 0x3300;
const ISP_STAT_TOTAL_SIZE: usize = 0x6a00;
const ISP_LDCI_BUFFER_OFS: usize = 0x9d00;

// The default register words of the 602 image, at the image's byte offsets.
/// `CH0_EXPAND_CFG0` to `CH2`: 12-bit input and output, black level off.
const DEFAULT_CHANNEL_EXPAND: u32 = 0x000c_00c0;
/// `D3D_CFG0`: bits 1, 3, 7 and 26 set; the record enable (bit 0) clear.
const DEFAULT_D3D_CFG0: u32 = 0x0400_008a;
/// `INPUT_SIZE` and `VALID_SIZE`: the kernel rewrites them into each image.
const DEFAULT_INPUT_SIZE: u32 = 0x0438_0780;
/// `D3D_LBC_CFG0`: a lossless group the kernel also derives.
const DEFAULT_LOSSLESS_CFG0: u32 = 0x3c24_4058;

fn word(image: &[u8; LOAD_BYTES], register: usize) -> u32 {
    let at = LOAD_REGISTERS_OFFSET + register;
    u32::from_le_bytes([image[at], image[at + 1], image[at + 2], image[at + 3]])
}

fn put(image: &mut [u8; LOAD_BYTES], register: usize, value: u32) {
    let at = LOAD_REGISTERS_OFFSET + register;
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn the_load_image_is_the_librarys_size_and_regions() {
    assert_eq!(LOAD_BYTES, ISP_LOAD_DRAM_SIZE);
    assert_eq!(layout::REGISTERS.bytes, ISP_LOAD_REG_SIZE);
    assert_eq!(layout::FRONT_END_TABLE.bytes, ISP_FE_TABLE_SIZE);
    assert_eq!(layout::BAYER_TABLE.bytes, ISP_BAYER_TABLE_SIZE);
    assert_eq!(layout::RGB_TABLE.bytes, ISP_RGB_TABLE_SIZE);
    assert_eq!(layout::YUV_TABLE.bytes, ISP_YUV_TABLE_SIZE);
    assert_eq!(
        ISP_LOAD_REG_SIZE
            + ISP_FE_TABLE_SIZE
            + ISP_BAYER_TABLE_SIZE
            + ISP_RGB_TABLE_SIZE
            + ISP_YUV_TABLE_SIZE,
        ISP_LOAD_DRAM_SIZE
    );
}

#[test]
fn the_bayer_table_is_at_the_librarys_places() {
    let base = layout::BAYER_TABLE.offset;
    assert_eq!(layout::LENS_SHADING_TABLE.offset, base);
    assert_eq!(layout::LENS_SHADING_TABLE.bytes, 0x800);
    assert_eq!(layout::MSC_TABLE.offset, base + 0x800);
    assert_eq!(layout::NR_MSC_TABLE.offset, base + 0x1000);
    assert_eq!(layout::NR_MSC_TABLE.bytes, 0x3c8);
    assert_eq!(layout::D3D_DK_TABLE.offset, base + 0x1400);
    assert_eq!(layout::D3D_DK_TABLE.bytes, 0x800);
    assert_eq!(layout::PLTM_TM_TABLE.offset, base + 0x1c00);
    assert_eq!(layout::PLTM_TM_TABLE.bytes, 0x800);
    assert_eq!(layout::PLTM_LM_TABLE.offset, base + 0x2400);
    assert_eq!(layout::PLTM_LM_TABLE.bytes, 0x200);
    // The last table ends the Bayer table.
    assert_eq!(layout::PLTM_LM_TABLE.end(), layout::BAYER_TABLE.end());
}

#[test]
fn the_rgb_and_yuv_tables_are_at_the_librarys_places() {
    let rgb = layout::RGB_TABLE.offset;
    assert_eq!(layout::DRC_TABLE.offset, rgb);
    assert_eq!(layout::DRC_TABLE.bytes, 0x200);
    assert_eq!(layout::GAMMA_TABLE.offset, rgb + 0x200);
    assert_eq!(layout::GAMMA_TABLE.bytes, 0x1000);
    assert_eq!(layout::GAMMA_TABLE.end(), layout::RGB_TABLE.end());
    // The two CEM tables fill the YUV table (the ISP 602 branch, not the 606 one).
    assert_eq!(layout::CEM_TABLE0_BYTES, 0xcc0);
    assert_eq!(layout::CEM_TABLE1_BYTES, 0xa40);
    assert_eq!(
        layout::CEM_TABLE0_BYTES + layout::CEM_TABLE1_BYTES,
        layout::YUV_TABLE.bytes
    );
}

#[test]
fn the_front_end_channel_tables_are_at_the_librarys_places() {
    let base = layout::FRONT_END_TABLE.offset;
    let expect = [(0x000, 0x800), (0x800, 0x800), (0x1000, 0x800)];
    for (table, (offset, bytes)) in layout::FRONT_END_MSC_TABLES.iter().zip(expect) {
        assert_eq!(table.offset, base + offset);
        assert_eq!(table.bytes, bytes);
    }
}

#[test]
fn the_statistics_regions_are_at_the_librarys_offsets() {
    // (offset, bytes) of each statistics part, from the ISP_STAT_* and ISP_SAVE_* defines.
    let expect = [
        (0x000, 0x100),   // ISP_SAVE_REG_SIZE: the saved registers
        (0x100, 0x360),   // ISP_STAT_AE_MEM_OFS, ISP_STAT_AE_MEM_SIZE
        (0x460, 0x020),   // ISP_STAT_RES_SIZE
        (0x480, 0xc00),   // ISP_STAT_AF_MEM_OFS, the AF IIR sums
        (0x1080, 0xc00),  // the AF FIR sums
        (0x1c80, 0xc00),  // the AF IIR counts
        (0x2880, 0xc00),  // the AF FIR counts
        (0x3480, 0xc00),  // the AF highlight counts, which end the AF part at 0x4080
        (0x4080, 0x1800), // ISP_STAT_AWB_MEM_OFS, ISP_STAT_AWB_RGB_MEM_SIZE
        (0x5880, 0x200),  // ISP_STAT_AFS_MEM_OFS, ISP_STAT_AFS_MEM_SIZE
        (0x5a80, 0x400),  // ISP_STAT_HIST0_MEM_OFS, ISP_STAT_HIST0_MEM_SIZE
        (0x5e80, 0x400),  // ISP_STAT_HIST1_MEM_OFS, ISP_STAT_HIST1_MEM_SIZE
        (0x6280, 0x780),  // ISP_STAT_PLTM_LUM_MEM_OFS, ISP_STAT_PLTM_LUM_SIZE
    ];
    for (index, (offset, bytes)) in expect.into_iter().enumerate() {
        assert_eq!(
            layout::STATISTICS_REGIONS[index].offset,
            offset,
            "part {index}"
        );
        assert_eq!(
            layout::STATISTICS_REGIONS[index].bytes,
            bytes,
            "part {index}"
        );
    }
    assert_eq!(layout::HISTOGRAM_0_OFFSET, 0x5a80);
    assert_eq!(layout::STATISTICS_REGIONS[12].end(), ISP_STAT_TOTAL_SIZE);
    assert_eq!(ISP_SAVE_DRAM_SIZE, layout::SAVE_BYTES);
    assert_eq!(ISP_SAVE_REG_SIZE, 0x100);
}

#[test]
fn the_save_load_statistics_are_at_the_librarys_offsets() {
    // ISP_SAVE_LOAD_PLTM_PKX_OFS and ISP_SAVE_LOAD_D3D_K_OFS, which follow the saved statistics.
    let pkx = layout::STATISTICS_REGIONS[13];
    let denoise_k = layout::STATISTICS_REGIONS[14];
    assert_eq!((pkx.offset, pkx.bytes), (ISP_STAT_TOTAL_SIZE, 0x3000));
    assert_eq!(
        (denoise_k.offset, denoise_k.bytes),
        (ISP_STAT_TOTAL_SIZE + 0x3000, 0x300)
    );
    assert_eq!(
        layout::SAVE_LOAD_STATISTICS_BYTES,
        ISP_SAVE_LOAD_STATISTIC_SIZE
    );
    // The library's save-load buffer also holds the save-load registers, which the layout's
    // statistics buffer does not, and an LDCI buffer after the statistics, which it names and the
    // layout does not hold.
    assert_eq!(
        ISP_SAVE_LOAD_DRAM_SIZE - ISP_SAVE_LOAD_STATISTIC_SIZE,
        0x100
    );
    assert_eq!(
        ISP_LDCI_BUFFER_OFS,
        ISP_STAT_TOTAL_SIZE + ISP_SAVE_LOAD_STATISTIC_SIZE
    );
    assert_eq!(
        layout::STATISTICS_BYTES,
        ISP_SAVE_DRAM_SIZE + ISP_SAVE_LOAD_STATISTIC_SIZE
    );
}

#[test]
fn the_default_channel_expand_is_twelve_bits_in_and_out() {
    let twelve = ChannelExpand {
        black_level: false,
        inverse_black_level: false,
        input_bits: 12,
        output_bits: 12,
    };
    assert_eq!(twelve.encode(), Ok(DEFAULT_CHANNEL_EXPAND));
    for channel in 0..expand::CHANNELS {
        let mut bytes = [0u8; LOAD_BYTES];
        Image::new(&mut bytes)
            .set_channel_expand(channel, twelve)
            .expect("a channel");
        assert_eq!(
            word(&bytes, expand::REGISTERS[channel]),
            DEFAULT_CHANNEL_EXPAND
        );
    }
}

#[test]
fn a_channel_expand_setter_keeps_the_bits_it_does_not_own() {
    let mut bytes = [0u8; LOAD_BYTES];
    put(&mut bytes, reg::CH1_EXPAND_CFG0, u32::MAX);
    let twelve = ChannelExpand {
        black_level: false,
        inverse_black_level: false,
        input_bits: 12,
        output_bits: 12,
    };
    Image::new(&mut bytes)
        .set_channel_expand(1, twelve)
        .expect("a channel");
    assert_eq!(
        word(&bytes, reg::CH1_EXPAND_CFG0),
        !expand::MASK | DEFAULT_CHANNEL_EXPAND
    );
}

#[test]
fn the_temporal_record_keeps_the_library_default_bits() {
    let mut bytes = [0u8; LOAD_BYTES];
    put(&mut bytes, reg::D3D_CFG0, DEFAULT_D3D_CFG0);
    Image::new(&mut bytes).set_temporal_record(false);
    assert_eq!(word(&bytes, reg::D3D_CFG0), DEFAULT_D3D_CFG0);
    Image::new(&mut bytes).set_temporal_record(true);
    assert_eq!(
        word(&bytes, reg::D3D_CFG0),
        DEFAULT_D3D_CFG0 | temporal::RECORD
    );
    Image::new(&mut bytes).set_temporal_record(false);
    assert_eq!(word(&bytes, reg::D3D_CFG0), DEFAULT_D3D_CFG0);
}

#[test]
fn the_library_default_extent_words_are_refused() {
    let mut bytes = [0u8; LOAD_BYTES];
    put(&mut bytes, reg::INPUT_SIZE, DEFAULT_INPUT_SIZE);
    assert!(matches!(
        layout::check_load(&bytes),
        Err(Refusal::Extent(at)) if at == LOAD_REGISTERS_OFFSET + reg::INPUT_SIZE
    ));
    let mut lossless = [0u8; LOAD_BYTES];
    put(&mut lossless, reg::D3D_LBC_CFG0, DEFAULT_LOSSLESS_CFG0);
    assert!(matches!(
        layout::check_load(&lossless),
        Err(Refusal::Extent(_))
    ));
}
