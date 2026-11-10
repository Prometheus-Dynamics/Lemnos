//! The ISP600's load image and statistics buffer: where each part lies, the register offsets and
//! bits the vendor kernel names, and the one check a load image passes before it goes anywhere.
//!
//! # The load image
//!
//! [`LOAD_BYTES`] bytes the kernel copies whole and gives the block at its next parameter load:
//! the register image ([`LOAD_REGIONS`]`[0]`, laid out as the block's register space from its
//! base, the per-ISP control blocks at [`AHB_OFFSET`] and the loaded registers at
//! [`LOAD_REGISTERS_OFFSET`]), then the front end, Bayer, RGB and YUV tables. The order of the
//! five is the source's: the kernel itself finds the Bayer table's lens shading tables at
//! `ISP_LOAD_REG_SIZE + ISP_FE_TBL_SIZE` and after.
//!
//! # The statistics buffer
//!
//! [`STATISTICS_BYTES`] bytes: the buffer the block saves each frame ([`SAVE_BYTES`]: the saved
//! registers, then the statistics), then the save-load statistics the kernel copies after it.
//! The source gives each part's size and one offset, the first histogram's
//! (`ISP_STAT_HIST0_MEM_OFS`). The parts before that offset are exactly the ones whose sizes add
//! up to it, and those after it the ones that fill the rest, so which part lies on which side is
//! fixed; within each side the order is the one the header lists them in, which the source does
//! not otherwise confirm. `testing/board-runs/E13.md` checks it on the block.
//!
//! # What a load image may not hold
//!
//! The kernel names the words of the register image that hold a memory address
//! ([`ADDRESS_WORDS`]): each control block's load, save and save-load addresses, which the kernel
//! writes to the block's own registers, and the temporal denoise buffers' four addresses, which
//! it writes into each image it hands the block (or turns temporal denoise off in it when built
//! without it). None of them is the caller's to set, so [`check_load`] refuses an image with any
//! of them other than zero: an image never names memory, whatever the kernel does with those
//! words. The kernel also writes the input and valid sizes into each image itself.
//!
//! # Source
//!
//! Interface facts only, no code copied, from `radxa/allwinner-bsp` on GitHub, branch
//! `cubie-aiot-v1.4.6` at `2045a3ca2a01f088c0314dc924bda59d154e363e` (GPL-2.0):
//! `drivers/vin/vin-isp/isp600/isp600_reg_cfg.h` (every size, the statistics configuration
//! offsets, the module and table update bits), `drivers/vin/vin-isp/isp600/isp600_reg.h` (the
//! register offsets and the control register's bits), `drivers/vin/vin-isp/isp600/isp600_reg_cfg.c`
//! (which register each function writes, and in which image), `drivers/vin/vin-isp/sunxi_isp.c`
//! (`__isp_set_load_reg`, `isp_3d_pingpong_update`, `sunxi_isp_set_load_ddr`, the parameter load
//! in `isp_isr`, and stream on in `sunxi_isp_subdev_s_stream`) and `drivers/vin/vin-stat/vin_h3a.c`
//! (`isp_stat_buf_get`). The chip is the one built without `CONFIG_ARCH_SUN55IW6` and
//! `CONFIG_ARCH_SUN55IW3`. `layout-check/run.sh <allwinner-bsp checkout>` compiles a C program
//! against the two headers and diffs what it prints against the block below.

pub use lemnos_driver_isp600_sys::{
    LOAD_BYTES, SAVE_BYTES, SAVE_LOAD_STATISTICS_BYTES, STATISTICS_BYTES,
};

/// The arrangement of [`LOAD_REGIONS`] and [`STATISTICS_REGIONS`], as `isp.isp600/1` names it.
pub const LAYOUT_VERSION: u32 = 1;

/// One part of a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub offset: usize,
    pub bytes: usize,
}

impl Region {
    /// The first byte past it.
    #[must_use]
    pub const fn end(self) -> usize {
        self.offset + self.bytes
    }

    /// Its bytes in `buffer`, or `None` when `buffer` is shorter.
    #[must_use]
    pub fn of(self, buffer: &[u8]) -> Option<&[u8]> {
        buffer.get(self.offset..self.end())
    }
}

const fn after(previous: Region, bytes: usize) -> Region {
    Region {
        offset: previous.end(),
        bytes,
    }
}

/// `ISP_LOAD_REG_SIZE`: the register image.
pub const REGISTERS: Region = Region {
    offset: 0,
    bytes: 0x1000,
};
/// `ISP_FE_TBL_SIZE`: the front end's table.
pub const FRONT_END_TABLE: Region = after(REGISTERS, 0x1800);
/// `ISP_BAYER_TABLE_SIZE`: the Bayer tables.
pub const BAYER_TABLE: Region = after(FRONT_END_TABLE, 0x2600);
/// `ISP_RGB_TABLE_SIZE`: the RGB tables.
pub const RGB_TABLE: Region = after(BAYER_TABLE, 0x1200);
/// `ISP_YUV_TABLE_SIZE`: the YUV tables.
pub const YUV_TABLE: Region = after(RGB_TABLE, 0x1700);

/// The load image's regions, in the order of `isp.isp600/1`'s `LOAD_*` numbers.
pub const LOAD_REGIONS: [Region; 5] = [
    REGISTERS,
    FRONT_END_TABLE,
    BAYER_TABLE,
    RGB_TABLE,
    YUV_TABLE,
];

/// The Bayer table's lens shading table (`ISP_RSC_TBL_SIZE`), where the kernel finds it.
pub const LENS_SHADING_TABLE: Region = Region {
    offset: BAYER_TABLE.offset,
    bytes: 0x800,
};
/// The Bayer table's second shading table, the same size, which the kernel copies the first over
/// when two ISPs share a frame.
pub const MSC_TABLE: Region = after(LENS_SHADING_TABLE, 0x800);
/// The noise reduction shading table (`ISP_MSC_NR_TBL_SIZE`).
pub const NR_MSC_TABLE: Region = after(MSC_TABLE, 0x3c8);
/// Sizes of the Bayer table's other tables (`ISP_D3D_DK_TBL_SIZE`, `ISP_PLTM_TM_TBL_SIZE`,
/// `ISP_PLTM_LM_TBL_SIZE`), whose places the source does not give.
pub const D3D_DK_TABLE_BYTES: usize = 0x800;
pub const PLTM_TM_TABLE_BYTES: usize = 0x800;
pub const PLTM_LM_TABLE_BYTES: usize = 0x200;
/// Sizes of the RGB table's two tables (`ISP_RGB_DRC_TBL_SIZE`, `ISP_RGB_GAMMA_TBL_SIZE`), which
/// fill it.
pub const DRC_TABLE_BYTES: usize = 0x200;
pub const GAMMA_TABLE_BYTES: usize = 0x1000;
/// Sizes of the YUV table's two tables (`ISP_CEM_TBL0_SIZE`, `ISP_CEM_TBL1_SIZE`), which fill it.
pub const CEM_TABLE0_BYTES: usize = 0xcc0;
pub const CEM_TABLE1_BYTES: usize = 0xa40;

/// The statistics buffer's parts, in the order of `isp.isp600/1`'s `STATISTICS_*` numbers.
pub const STATISTICS_REGIONS: [Region; 15] = {
    // `ISP_SAVE_REG_SIZE`, then `ISP_STAT_*_SIZE` as the header lists them.
    const SAVED: Region = Region {
        offset: 0,
        bytes: 0x100,
    };
    const AE: Region = after(SAVED, 0x360);
    const RES: Region = after(AE, 0x20);
    const AF_IIR_SUM: Region = after(RES, 0xc00);
    const AF_FIR_SUM: Region = after(AF_IIR_SUM, 0xc00);
    const AF_IIR_COUNT: Region = after(AF_FIR_SUM, 0xc00);
    const AF_FIR_COUNT: Region = after(AF_IIR_COUNT, 0xc00);
    const AF_HIGHLIGHT_COUNT: Region = after(AF_FIR_COUNT, 0xc00);
    const AWB: Region = after(AF_HIGHLIGHT_COUNT, 0x1800);
    const AFS: Region = after(AWB, 0x200);
    const HISTOGRAM_0: Region = after(AFS, 0x400);
    const HISTOGRAM_1: Region = after(HISTOGRAM_0, 0x400);
    const TONE_LUMINANCE: Region = after(HISTOGRAM_1, 0x780);
    // The save-load statistics, after the saved buffer: `ISP_STAT_PLTM_PKX_SIZE`,
    // `ISP_STAT_D3D_K_SIZE`.
    const TONE_PKX: Region = after(TONE_LUMINANCE, 0x3000);
    const DENOISE_K: Region = after(TONE_PKX, 0x300);
    [
        SAVED,
        AE,
        RES,
        AF_IIR_SUM,
        AF_FIR_SUM,
        AF_IIR_COUNT,
        AF_FIR_COUNT,
        AF_HIGHLIGHT_COUNT,
        AWB,
        AFS,
        HISTOGRAM_0,
        HISTOGRAM_1,
        TONE_LUMINANCE,
        TONE_PKX,
        DENOISE_K,
    ]
};

/// `ISP_STAT_HIST0_MEM_OFS`: the one statistics offset the source states.
pub const HISTOGRAM_0_OFFSET: usize = 0x5a80;

/// Where the per-ISP control blocks start in the register image (`ISP_AHB_REG_OFFSET`).
pub const AHB_OFFSET: usize = 0x20;
/// Bytes between one ISP's control block and the next (`ISP_AHB_AMONG_OFFSET`).
pub const AHB_STRIDE: usize = 0x30;
/// Control blocks: one per virtual ISP (`ISP_VIRT_NUM`).
pub const VIRTUAL_ISPS: usize = 4;
/// Where the loaded registers start in the register image (`ISP_LOAD_REG_OFFSET`).
pub const LOAD_REGISTERS_OFFSET: usize = 0x100;

/// Registers of a control block, from its start (`ISP_*_REG` of the `ahb reg` list).
pub mod ahb {
    /// Control: capture, parameters ready, the table update bits ([`super::update`]).
    pub const CFG0: usize = 0x000;
    /// The load image's address. The kernel's.
    pub const LOAD_ADDR: usize = 0x004;
    pub const INT_BYPASS: usize = 0x008;
    pub const INT_STATUS: usize = 0x00c;
    /// The saved statistics' address. The kernel's.
    pub const SAVE_ADDR: usize = 0x01c;
    /// The save-load buffer's address. The kernel's.
    pub const SAVE_LOAD_ADDR: usize = 0x020;
}

/// Loaded registers, from [`LOAD_REGISTERS_OFFSET`] (`ISP_*_REG` of the `load reg` list).
pub mod reg {
    pub const GLOBAL_CFG0: usize = 0x000;
    pub const GLOBAL_CFG1: usize = 0x004;
    pub const LBC_TIME_CYCLE: usize = 0x008;
    /// Input size. Written by the kernel into each image.
    pub const INPUT_SIZE: usize = 0x020;
    /// Valid size. Written by the kernel into each image.
    pub const VALID_SIZE: usize = 0x024;
    /// Valid start. Written by the kernel into each image.
    pub const VALID_START: usize = 0x028;
    pub const MODULE_BYPASS0: usize = 0x030;
    /// The module enables ([`super::module`]).
    pub const MODULE_BYPASS1: usize = 0x034;
    pub const MODULE_MODE0: usize = 0x038;
    pub const MODULE_MODE1: usize = 0x03c;
    /// Temporal denoise's buffers. The kernel's.
    pub const D3D_BAYER_ADDR: usize = 0x040;
    pub const D3D_K0_ADDR: usize = 0x044;
    pub const D3D_K1_ADDR: usize = 0x048;
    pub const D3D_STATUS_ADDR: usize = 0x04c;
    pub const VIN_CFG0: usize = 0x070;
    pub const CH0_EXPAND_CFG0: usize = 0x098;
    pub const CH1_EXPAND_CFG0: usize = 0x0b8;
    pub const CH2_EXPAND_CFG0: usize = 0x0d8;
    pub const D3D_CFG0: usize = 0x220;
    pub const D3D_LBC_CFG0: usize = 0x280;
    pub const D3D_LBC_CFG5: usize = 0x294;
}

/// Statistics windows in the register image (`ISP_AE_REG_OFS` and the rest, with their sizes).
pub const AE_WINDOW: Region = Region {
    offset: 0x650,
    bytes: 0x8,
};
pub const AF_WINDOW: Region = Region {
    offset: 0x670,
    bytes: 0xc,
};
pub const AWB_WINDOW: Region = Region {
    offset: 0x6e0,
    bytes: 0x8,
};
pub const HISTOGRAM_WINDOW: Region = Region {
    offset: 0x710,
    bytes: 0x20,
};

/// Module enables in `MODULE_BYPASS1` (`*_EN` of the `ISP Module enable` list).
pub mod module {
    pub const WDR_STITCH: u32 = 1 << 0;
    pub const DPC: u32 = 1 << 1;
    pub const CTC: u32 = 1 << 2;
    pub const GCA: u32 = 1 << 3;
    pub const D2D: u32 = 1 << 4;
    pub const D3D: u32 = 1 << 5;
    pub const BLC: u32 = 1 << 6;
    pub const WB: u32 = 1 << 7;
    pub const DG: u32 = 1 << 8;
    pub const RSC: u32 = 1 << 9;
    pub const MSC: u32 = 1 << 10;
    pub const PLTM: u32 = 1 << 11;
    pub const LCA: u32 = 1 << 12;
    pub const SHARP: u32 = 1 << 13;
    pub const CCM: u32 = 1 << 14;
    pub const CNR: u32 = 1 << 15;
    pub const DRC: u32 = 1 << 16;
    pub const GAMMA: u32 = 1 << 17;
    pub const CEM: u32 = 1 << 18;
    pub const AE: u32 = 1 << 24;
    pub const AF: u32 = 1 << 25;
    pub const AWB: u32 = 1 << 26;
    pub const AFS: u32 = 1 << 27;
    pub const HIST0: u32 = 1 << 28;
    pub const HIST1: u32 = 1 << 29;
}

/// Bits of a control block's `CFG0` (`ISP_AHB_CFG0_REG_t` and the `*_UPDATE` list): which
/// tables the block takes from the image.
pub mod update {
    pub const CAPTURE: u32 = 1 << 0;
    pub const PARA_READY: u32 = 1 << 1;
    pub const LENS: u32 = 1 << 8;
    pub const GAMMA: u32 = 1 << 9;
    pub const DRC: u32 = 1 << 10;
    pub const D3D: u32 = 1 << 13;
    pub const PLTM: u32 = 1 << 14;
    pub const CEM: u32 = 1 << 15;
    pub const MSC: u32 = 1 << 16;
    pub const FE0_MSC: u32 = 1 << 17;
    pub const NR_MSC: u32 = 1 << 18;
    pub const FE1_MSC: u32 = 1 << 19;
    pub const FE2_MSC: u32 = 1 << 20;
}

/// The register image's words that hold a memory address: each control block's load, save and
/// save-load addresses, then temporal denoise's four. Offsets from the image's start, each the
/// first of four bytes.
pub const ADDRESS_WORDS: [usize; 3 * VIRTUAL_ISPS + 4] = {
    let mut words = [0; 3 * VIRTUAL_ISPS + 4];
    let mut v = 0;
    while v < VIRTUAL_ISPS {
        let block = AHB_OFFSET + v * AHB_STRIDE;
        words[3 * v] = block + ahb::LOAD_ADDR;
        words[3 * v + 1] = block + ahb::SAVE_ADDR;
        words[3 * v + 2] = block + ahb::SAVE_LOAD_ADDR;
        v += 1;
    }
    let d3d = [
        reg::D3D_BAYER_ADDR,
        reg::D3D_K0_ADDR,
        reg::D3D_K1_ADDR,
        reg::D3D_STATUS_ADDR,
    ];
    let mut i = 0;
    while i < d3d.len() {
        words[3 * VIRTUAL_ISPS + i] = LOAD_REGISTERS_OFFSET + d3d[i];
        i += 1;
    }
    words
};

/// Why [`check_load`] refused an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not [`LOAD_BYTES`] long.
    Length,
    /// An address word, at this offset, is not zero.
    Address(usize),
}

/// A load image that passed [`check_load`]. The only way to one, so nothing else reaches the
/// kernel.
#[derive(Debug, Clone, Copy)]
pub struct Checked<'a>(&'a [u8; LOAD_BYTES]);

impl<'a> Checked<'a> {
    /// The image, whole.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8; LOAD_BYTES] {
        self.0
    }
}

/// `image`, if it may go to the kernel: exactly [`LOAD_BYTES`] long, and zero in every one of
/// [`ADDRESS_WORDS`]. Reads only those words, allocates nothing.
pub fn check_load(image: &[u8]) -> Result<Checked<'_>, Refusal> {
    let image: &[u8; LOAD_BYTES] = image.try_into().map_err(|_| Refusal::Length)?;
    for &word in &ADDRESS_WORDS {
        let bytes = image.get(word..word + 4).ok_or(Refusal::Address(word))?;
        if bytes.iter().any(|&b| b != 0) {
            return Err(Refusal::Address(word));
        }
    }
    Ok(Checked(image))
}

// The regions tile each buffer exactly, end to end.
const _: () = {
    assert!(YUV_TABLE.end() == LOAD_BYTES);
    assert!(NR_MSC_TABLE.end() <= RGB_TABLE.offset);
    assert!(
        NR_MSC_TABLE.end() + D3D_DK_TABLE_BYTES + PLTM_TM_TABLE_BYTES + PLTM_LM_TABLE_BYTES
            <= RGB_TABLE.offset
    );
    assert!(DRC_TABLE_BYTES + GAMMA_TABLE_BYTES == RGB_TABLE.bytes);
    assert!(CEM_TABLE0_BYTES + CEM_TABLE1_BYTES == YUV_TABLE.bytes);
    let mut i = 1;
    while i < LOAD_REGIONS.len() {
        assert!(LOAD_REGIONS[i].offset == LOAD_REGIONS[i - 1].end());
        i += 1;
    }
    let mut i = 1;
    while i < STATISTICS_REGIONS.len() {
        assert!(STATISTICS_REGIONS[i].offset == STATISTICS_REGIONS[i - 1].end());
        i += 1;
    }
    assert!(STATISTICS_REGIONS[0].offset == 0);
    assert!(STATISTICS_REGIONS[10].offset == HISTOGRAM_0_OFFSET);
    assert!(STATISTICS_REGIONS[12].end() == SAVE_BYTES);
    assert!(STATISTICS_REGIONS[14].end() == STATISTICS_BYTES);
    assert!(
        STATISTICS_REGIONS[13].bytes + STATISTICS_REGIONS[14].bytes == SAVE_LOAD_STATISTICS_BYTES
    );
    // Every address word and window lies in the register image.
    let mut i = 0;
    while i < ADDRESS_WORDS.len() {
        assert!(ADDRESS_WORDS[i] + 4 <= REGISTERS.bytes);
        assert!(ADDRESS_WORDS[i].is_multiple_of(4));
        i += 1;
    }
    assert!(HISTOGRAM_WINDOW.end() <= REGISTERS.bytes);
    // The control blocks end before the loaded registers start.
    assert!(AHB_OFFSET + VIRTUAL_ISPS * AHB_STRIDE <= LOAD_REGISTERS_OFFSET);
};

// BEGIN GENERATED
// isp600_reg_cfg.h: sizes.
const _: () = {
    assert!(LOAD_BYTES == 0x7700);
    assert!(REGISTERS.bytes == 0x1000);
    assert!(FRONT_END_TABLE.bytes == 0x1800);
    assert!(BAYER_TABLE.bytes == 0x2600);
    assert!(RGB_TABLE.bytes == 0x1200);
    assert!(YUV_TABLE.bytes == 0x1700);
    assert!(LENS_SHADING_TABLE.offset == 0x2800);
    assert!(LENS_SHADING_TABLE.bytes == 0x800);
    assert!(MSC_TABLE.offset == 0x3000);
    assert!(NR_MSC_TABLE.offset == 0x3800);
    assert!(NR_MSC_TABLE.bytes == 0x3c8);
    assert!(D3D_DK_TABLE_BYTES == 0x800);
    assert!(PLTM_TM_TABLE_BYTES == 0x800);
    assert!(PLTM_LM_TABLE_BYTES == 0x200);
    assert!(DRC_TABLE_BYTES == 0x200);
    assert!(GAMMA_TABLE_BYTES == 0x1000);
    assert!(CEM_TABLE0_BYTES == 0xcc0);
    assert!(CEM_TABLE1_BYTES == 0xa40);
    assert!(SAVE_BYTES == 0x6a00);
    assert!(SAVE_BYTES == 0x6a00);
    assert!(STATISTICS_REGIONS[0].bytes == 0x100);
    assert!(STATISTICS_REGIONS[1].bytes == 0x360);
    assert!(STATISTICS_REGIONS[2].bytes == 0x20);
    assert!(STATISTICS_REGIONS[3].bytes == 0xc00);
    assert!(STATISTICS_REGIONS[4].bytes == 0xc00);
    assert!(STATISTICS_REGIONS[5].bytes == 0xc00);
    assert!(STATISTICS_REGIONS[6].bytes == 0xc00);
    assert!(STATISTICS_REGIONS[7].bytes == 0xc00);
    assert!(STATISTICS_REGIONS[8].bytes == 0x1800);
    assert!(STATISTICS_REGIONS[9].bytes == 0x200);
    assert!(STATISTICS_REGIONS[10].bytes == 0x400);
    assert!(STATISTICS_REGIONS[11].bytes == 0x400);
    assert!(STATISTICS_REGIONS[12].bytes == 0x780);
    assert!(STATISTICS_REGIONS[13].bytes == 0x3000);
    assert!(STATISTICS_REGIONS[14].bytes == 0x300);
    assert!(HISTOGRAM_0_OFFSET == 0x5a80);
    assert!(SAVE_LOAD_STATISTICS_BYTES == 0x3300);
    assert!(AE_WINDOW.offset == 0x650);
    assert!(AE_WINDOW.bytes == 0x8);
    assert!(AF_WINDOW.offset == 0x670);
    assert!(AF_WINDOW.bytes == 0xc);
    assert!(AWB_WINDOW.offset == 0x6e0);
    assert!(AWB_WINDOW.bytes == 0x8);
    assert!(HISTOGRAM_WINDOW.offset == 0x710);
    assert!(HISTOGRAM_WINDOW.bytes == 0x20);
    assert!(VIRTUAL_ISPS == 0x4);
};
// isp600_reg.h: register offsets.
const _: () = {
    assert!(AHB_OFFSET == 0x20);
    assert!(AHB_STRIDE == 0x30);
    assert!(LOAD_REGISTERS_OFFSET == 0x100);
    assert!(ahb::CFG0 == 0);
    assert!(ahb::LOAD_ADDR == 0x4);
    assert!(ahb::INT_BYPASS == 0x8);
    assert!(ahb::INT_STATUS == 0xc);
    assert!(ahb::SAVE_ADDR == 0x1c);
    assert!(ahb::SAVE_LOAD_ADDR == 0x20);
    assert!(reg::GLOBAL_CFG0 == 0);
    assert!(reg::GLOBAL_CFG1 == 0x4);
    assert!(reg::LBC_TIME_CYCLE == 0x8);
    assert!(reg::INPUT_SIZE == 0x20);
    assert!(reg::VALID_SIZE == 0x24);
    assert!(reg::VALID_START == 0x28);
    assert!(reg::MODULE_BYPASS0 == 0x30);
    assert!(reg::MODULE_BYPASS1 == 0x34);
    assert!(reg::MODULE_MODE0 == 0x38);
    assert!(reg::MODULE_MODE1 == 0x3c);
    assert!(reg::D3D_BAYER_ADDR == 0x40);
    assert!(reg::D3D_K0_ADDR == 0x44);
    assert!(reg::D3D_K1_ADDR == 0x48);
    assert!(reg::D3D_STATUS_ADDR == 0x4c);
    assert!(reg::VIN_CFG0 == 0x70);
    assert!(reg::CH0_EXPAND_CFG0 == 0x98);
    assert!(reg::CH1_EXPAND_CFG0 == 0xb8);
    assert!(reg::CH2_EXPAND_CFG0 == 0xd8);
    assert!(reg::D3D_CFG0 == 0x220);
    assert!(reg::D3D_LBC_CFG0 == 0x280);
    assert!(reg::D3D_LBC_CFG5 == 0x294);
};
// isp600_reg_cfg.h and isp600_reg.h: bits.
const _: () = {
    assert!(module::WDR_STITCH == 0x1);
    assert!(module::DPC == 0x2);
    assert!(module::CTC == 0x4);
    assert!(module::GCA == 0x8);
    assert!(module::D2D == 0x10);
    assert!(module::D3D == 0x20);
    assert!(module::BLC == 0x40);
    assert!(module::WB == 0x80);
    assert!(module::DG == 0x100);
    assert!(module::RSC == 0x200);
    assert!(module::MSC == 0x400);
    assert!(module::PLTM == 0x800);
    assert!(module::LCA == 0x1000);
    assert!(module::SHARP == 0x2000);
    assert!(module::CCM == 0x4000);
    assert!(module::CNR == 0x8000);
    assert!(module::DRC == 0x10000);
    assert!(module::GAMMA == 0x20000);
    assert!(module::CEM == 0x40000);
    assert!(module::AE == 0x1000000);
    assert!(module::AF == 0x2000000);
    assert!(module::AWB == 0x4000000);
    assert!(module::AFS == 0x8000000);
    assert!(module::HIST0 == 0x10000000);
    assert!(module::HIST1 == 0x20000000);
    assert!(update::LENS == 0x100);
    assert!(update::GAMMA == 0x200);
    assert!(update::DRC == 0x400);
    assert!(update::D3D == 0x2000);
    assert!(update::PLTM == 0x4000);
    assert!(update::CEM == 0x8000);
    assert!(update::MSC == 0x10000);
    assert!(update::FE0_MSC == 0x20000);
    assert!(update::NR_MSC == 0x40000);
    assert!(update::FE1_MSC == 0x80000);
    assert!(update::FE2_MSC == 0x100000);
    assert!(update::CAPTURE == 0x1);
    assert!(update::PARA_READY == 0x2);
    assert!(update::LENS == 0x100);
    assert!(update::GAMMA == 0x200);
    assert!(update::DRC == 0x400);
    assert!(update::D3D == 0x2000);
    assert!(update::PLTM == 0x4000);
    assert!(update::CEM == 0x8000);
    assert!(update::MSC == 0x10000);
    assert!(update::FE0_MSC == 0x20000);
    assert!(update::NR_MSC == 0x40000);
    assert!(update::FE1_MSC == 0x80000);
    assert!(update::FE2_MSC == 0x100000);
};
// END GENERATED
