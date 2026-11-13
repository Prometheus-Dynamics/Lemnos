//! The lens shading tables as register writes: the layout the ISP8000 reads, and the builder
//! that turns a caller's tables into the writes that load them. Pure: no device, no allocation.
//!
//! Layout, from the sources listed below:
//! - Four sample tables, in the register order R, GR, GB, B. Each is 17 rows of 17 samples, and a
//!   sample is 12 bits (0 to 4095). The caller's values are these register values; a caller that
//!   holds another scale converts first. Values outside the range are refused, not clipped.
//! - A row is nine words. Word `w` holds samples `2w` (bits 0 to 11) and `2w + 1` (bits 12 to 23),
//!   for `w` below 8; word 8 holds sample 16 alone, and its upper half is written as 0 (rkisp1
//!   `rkisp1_lsc_matrix_config_v10`: "2nd value of last DWORD unused").
//! - Two banks of 153 words. Bank `k` starts at address `153 * k`, and `LSC_TABLE_SEL` (0 or 1)
//!   names the bank the ISP reads. Which bank a load goes to is the caller's choice: read
//!   `LSC_STATUS` and pass the bank that is not in use.
//! - Each table has its own address pointer: it is written once, then each data write advances it
//!   (auto-increment; rkisp1 writes 153 words after one address write).
//! - Eight X and eight Y sector sizes (10 bits, at least 1) and eight X and eight Y gradients
//!   (12 bits), each set packed two to a register at bits 0 and 16, the first of the pair low.
//!
//! The mapping of the 17 samples to the image's sectors is not in these sources, so the builder
//! takes sample tables as the registers hold them and takes no position of its own. The board run
//! `testing/board-runs/U2b.md` checks the mapping on a frame.
//!
//! Sources (all licensed for use here, read as raw files; no code is copied):
//! - `vvcam/isp/mrv_all_regs.h` and `mrv_all_bits.h` (VeriSilicon, MIT), in `nxp-imx/isp-vvcam` at
//!   `e6453233e67f5d2f180d6ec277158dd54fa67a25`: the register offsets from `LSC_BASE`, the field
//!   masks and shifts, and the bank addresses (`MRV_LSC_R_RAM_ADDR_MAX_BANK0` 0x98, bank 1 from
//!   0x99).
//! - `include/uapi/linux/rkisp1-config.h` and `drivers/media/platform/rockchip/rkisp1/rkisp1-regs.h`
//!   and `rkisp1-params.c` (GPL-2.0, Linux `v6.12`): the same block at `0x2200`, the same offsets
//!   and masks, the 17 x 17 tables, the eight sector sizes and gradients, the table select, and the
//!   write order used here (`rkisp1_lsc_matrix_config_v10`, lines 236 to 276).
//! - `buildroot-overlay/package/vvcam/v4l2/isp/isp_ctrl/lsc/lsc_v1` of `kendryte/k230_linux_sdk` at
//!   `fc6cc21` (dual MIT and GPL): the tool's own tables, four of 17 x 17, eight sizes per axis.
//!   Its values run 1024 to 16383, which is the caller's scale, not the register's.

use lemnos_driver_k230isp_sys::regs::{
    LSC_B_TABLE_ADDR, LSC_B_TABLE_DATA, LSC_CTRL, LSC_EN, LSC_GB_TABLE_ADDR, LSC_GB_TABLE_DATA,
    LSC_GR_TABLE_ADDR, LSC_GR_TABLE_DATA, LSC_PAIR_SHIFT, LSC_R_TABLE_ADDR, LSC_R_TABLE_DATA,
    LSC_SAMPLE_1_SHIFT, LSC_TABLE_SEL, LSC_XGRAD_0, LSC_XSIZE_0, LSC_YGRAD_0, LSC_YSIZE_0,
};

/// Rows of the sample tables.
pub const LENS_ROWS: usize = 17;
/// Samples per row (and columns of the table).
pub const LENS_COLS: usize = 17;
/// Sector sizes and gradients per axis.
pub const LENS_SECTORS: usize = 8;
/// Words per table row: two samples to a word, the last word holding one.
pub const LENS_WORDS_PER_ROW: usize = 9;
/// Words per table bank: `LENS_ROWS * LENS_WORDS_PER_ROW`.
pub const LENS_BANK_WORDS: u32 = 153;
/// The largest sample, 12 bits.
pub const LENS_SAMPLE_MAX: u16 = 4095;
/// The largest sector size, 10 bits.
pub const LENS_SECT_SIZE_MAX: u16 = 1023;
/// The largest gradient, 12 bits.
pub const LENS_GRAD_MAX: u16 = 4095;
/// Writes in one load: 4 table address pointers, 4 x 153 sample words, 16 sector and gradient
/// words, the table select and the control. `4 + 612 + 16 + 2 = 634`.
pub const LENS_WRITES: usize = 4 + 4 * LENS_BANK_WORDS as usize + 16 + 2;

/// A table bank: the address its samples start at, and the `LSC_TABLE_SEL` value that selects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bank {
    /// Bank 0: addresses 0 to 152, selected by 0.
    Zero,
    /// Bank 1: addresses 153 to 305, selected by 1.
    One,
}

impl Bank {
    fn base(self) -> u32 {
        match self {
            Self::Zero => 0,
            Self::One => LENS_BANK_WORDS,
        }
    }

    fn select(self) -> u32 {
        match self {
            Self::Zero => 0,
            Self::One => 1,
        }
    }
}

/// The four sample tables, each `[row][col]`, in the register order: red, green (red), green
/// (blue), blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LensTables {
    /// Red samples.
    pub r: [[u16; LENS_COLS]; LENS_ROWS],
    /// Green samples on the red row.
    pub gr: [[u16; LENS_COLS]; LENS_ROWS],
    /// Green samples on the blue row.
    pub gb: [[u16; LENS_COLS]; LENS_ROWS],
    /// Blue samples.
    pub b: [[u16; LENS_COLS]; LENS_ROWS],
}

/// The sector geometry: sizes and gradients per axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LensGeometry {
    /// X sector sizes (10 bits, at least 1).
    pub x_size: [u16; LENS_SECTORS],
    /// Y sector sizes (10 bits, at least 1).
    pub y_size: [u16; LENS_SECTORS],
    /// X gradients (12 bits).
    pub x_grad: [u16; LENS_SECTORS],
    /// Y gradients (12 bits).
    pub y_grad: [u16; LENS_SECTORS],
}

/// A value the registers cannot hold, so no writes are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensError {
    /// A sample past 12 bits. `table` is 0 for red, 1 for green (red), 2 for green (blue), 3 for
    /// blue.
    Sample {
        /// The table, in register order.
        table: u8,
        /// The row.
        row: u8,
        /// The column.
        col: u8,
        /// The value given.
        value: u16,
    },
    /// A sector size of 0 or past 10 bits.
    SectorSize {
        /// The value given.
        value: u16,
    },
    /// A gradient past 12 bits.
    Gradient {
        /// The value given.
        value: u16,
    },
}

/// The word of one sample pair (`first` at bit 0, `second` at bit 12).
fn pair(first: u16, second: u16) -> u32 {
    u32::from(first) | (u32::from(second) << LSC_SAMPLE_1_SHIFT)
}

/// The sample word `w` of `row`: two samples, or the last sample alone in word 8.
fn sample_word(row: &[u16; LENS_COLS], w: usize) -> u32 {
    if w + 1 < LENS_WORDS_PER_ROW {
        pair(row[2 * w], row[2 * w + 1])
    } else {
        pair(row[2 * w], 0)
    }
}

/// Two packed sectors: `lo` at bit 0 and `hi` at bit 16.
fn sector_pair(lo: u16, hi: u16) -> u32 {
    u32::from(lo) | (u32::from(hi) << LSC_PAIR_SHIFT)
}

fn check_samples(table: u8, t: &[[u16; LENS_COLS]; LENS_ROWS]) -> Result<(), LensError> {
    for (row, cols) in t.iter().enumerate() {
        for (col, &value) in cols.iter().enumerate() {
            if value > LENS_SAMPLE_MAX {
                return Err(LensError::Sample {
                    table,
                    row: row as u8,
                    col: col as u8,
                    value,
                });
            }
        }
    }
    Ok(())
}

fn check_geometry(g: &LensGeometry) -> Result<(), LensError> {
    for &value in &g.x_size {
        if value == 0 || value > LENS_SECT_SIZE_MAX {
            return Err(LensError::SectorSize { value });
        }
    }
    for &value in &g.y_size {
        if value == 0 || value > LENS_SECT_SIZE_MAX {
            return Err(LensError::SectorSize { value });
        }
    }
    for &value in g.x_grad.iter().chain(&g.y_grad) {
        if value > LENS_GRAD_MAX {
            return Err(LensError::Gradient { value });
        }
    }
    Ok(())
}

/// Builds the writes that load a lens shading correction into `bank` and set the control to
/// `enable`, in the order the ISP needs: the four address pointers; then, row by row, each word's
/// four colours (red, green (red), green (blue), blue); then the X and Y sector sizes and the X and
/// Y gradients, two to a register; then the table select, then the control.
///
/// Every value is checked before any write is made: a sample past 12 bits, a sector size of 0 or
/// past 10 bits, or a gradient past 12 bits is refused with the first such value.
pub fn lens_shading_writes(
    tables: &LensTables,
    geometry: &LensGeometry,
    bank: Bank,
    enable: bool,
) -> Result<[(u32, u32); LENS_WRITES], LensError> {
    check_samples(0, &tables.r)?;
    check_samples(1, &tables.gr)?;
    check_samples(2, &tables.gb)?;
    check_samples(3, &tables.b)?;
    check_geometry(geometry)?;

    let base = bank.base();
    let mut out = [(0u32, 0u32); LENS_WRITES];
    let mut n = 0;
    let mut put = |out: &mut [(u32, u32); LENS_WRITES], addr: u32, value: u32| {
        out[n] = (addr, value);
        n += 1;
    };

    // The address pointers: one per table, in the same order as the data.
    for addr in [
        LSC_R_TABLE_ADDR,
        LSC_GR_TABLE_ADDR,
        LSC_GB_TABLE_ADDR,
        LSC_B_TABLE_ADDR,
    ] {
        put(&mut out, addr, base);
    }
    let data = [
        LSC_R_TABLE_DATA,
        LSC_GR_TABLE_DATA,
        LSC_GB_TABLE_DATA,
        LSC_B_TABLE_DATA,
    ];
    let tables4 = [&tables.r, &tables.gr, &tables.gb, &tables.b];
    for row in 0..LENS_ROWS {
        for w in 0..LENS_WORDS_PER_ROW {
            for (addr, table) in data.iter().zip(tables4) {
                put(&mut out, *addr, sample_word(&table[row], w));
            }
        }
    }

    // Sector sizes and gradients: four registers per axis, two sectors to each.
    let sectors = [
        (LSC_XSIZE_0, &geometry.x_size),
        (LSC_YSIZE_0, &geometry.y_size),
        (LSC_XGRAD_0, &geometry.x_grad),
        (LSC_YGRAD_0, &geometry.y_grad),
    ];
    for (first, values) in sectors {
        for k in 0..LENS_SECTORS / 2 {
            let addr = first + 4 * k as u32;
            put(
                &mut out,
                addr,
                sector_pair(values[2 * k], values[2 * k + 1]),
            );
        }
    }

    put(&mut out, LSC_TABLE_SEL, bank.select());
    put(&mut out, LSC_CTRL, if enable { LSC_EN } else { 0 });
    debug_assert_eq!(n, LENS_WRITES);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use lemnos_driver_k230isp_sys::regs::{
        LSC_B_TABLE_DATA, LSC_CTRL, LSC_EN, LSC_GB_TABLE_DATA, LSC_GR_TABLE_DATA, LSC_R_TABLE_ADDR,
        LSC_R_TABLE_DATA, LSC_TABLE_SEL, LSC_XSIZE_0, LSC_YGRAD_0, LSC_YSIZE_0,
    };

    use super::*;
    use crate::config::{self, CONFIG_BYTES};

    /// The table as the configuration list encodes it: a count, then (address, value) pairs.
    fn config_bytes(writes: &[(u32, u32)]) -> Vec<u8> {
        let mut out = vec![0u8; CONFIG_BYTES];
        out[..4].copy_from_slice(&(writes.len() as u32).to_le_bytes());
        for (i, (addr, value)) in writes.iter().enumerate() {
            out[4 + 8 * i..8 + 8 * i].copy_from_slice(&addr.to_le_bytes());
            out[8 + 8 * i..12 + 8 * i].copy_from_slice(&value.to_le_bytes());
        }
        out
    }

    /// Sample `(table, row, col)` as a distinct value: each table's own offset plus its position.
    fn sample(table: u16, row: usize, col: usize) -> u16 {
        (table * 1000 + row as u16 * 17 + col as u16) % 4096
    }

    fn tables() -> LensTables {
        let mut t = LensTables {
            r: [[0; LENS_COLS]; LENS_ROWS],
            gr: [[0; LENS_COLS]; LENS_ROWS],
            gb: [[0; LENS_COLS]; LENS_ROWS],
            b: [[0; LENS_COLS]; LENS_ROWS],
        };
        for row in 0..LENS_ROWS {
            for col in 0..LENS_COLS {
                t.r[row][col] = sample(0, row, col);
                t.gr[row][col] = sample(1, row, col);
                t.gb[row][col] = sample(2, row, col);
                t.b[row][col] = sample(3, row, col);
            }
        }
        t
    }

    fn geometry() -> LensGeometry {
        LensGeometry {
            x_size: [100, 101, 102, 103, 104, 105, 106, 107],
            y_size: [200, 201, 202, 203, 204, 205, 206, 207],
            x_grad: [1, 2, 3, 4, 5, 6, 7, 8],
            y_grad: [11, 12, 13, 14, 15, 16, 17, 18],
        }
    }

    #[test]
    fn the_write_count_is_634() {
        // 4 address pointers, 4 tables of 153 words, 16 sector words, the select and the control.
        assert_eq!(LENS_WRITES, 634);
        assert_eq!(4 + 4 * 153 + 16 + 2, 634);
        assert_eq!(LENS_ROWS * LENS_WORDS_PER_ROW, 153);
        let w = lens_shading_writes(&tables(), &geometry(), Bank::Zero, true).unwrap();
        assert_eq!(w.len(), 634);
    }

    #[test]
    fn a_known_grid_is_written_in_the_order_the_isp_takes_it() {
        let w = lens_shading_writes(&tables(), &geometry(), Bank::Zero, true).unwrap();
        // The four address pointers, each at bank 0.
        assert_eq!(
            w[..4],
            [
                (LSC_R_TABLE_ADDR, 0),
                (lemnos_driver_k230isp_sys::regs::LSC_GR_TABLE_ADDR, 0),
                (lemnos_driver_k230isp_sys::regs::LSC_GB_TABLE_ADDR, 0),
                (lemnos_driver_k230isp_sys::regs::LSC_B_TABLE_ADDR, 0),
            ]
        );
        // The first row's first word: samples 0 and 1 of each colour, colours interleaved.
        let pair = |a: u16, b: u16| u32::from(a) | (u32::from(b) << 12);
        assert_eq!(
            w[4],
            (LSC_R_TABLE_DATA, pair(sample(0, 0, 0), sample(0, 0, 1)))
        );
        assert_eq!(
            w[5],
            (LSC_GR_TABLE_DATA, pair(sample(1, 0, 0), sample(1, 0, 1)))
        );
        assert_eq!(
            w[6],
            (LSC_GB_TABLE_DATA, pair(sample(2, 0, 0), sample(2, 0, 1)))
        );
        assert_eq!(
            w[7],
            (LSC_B_TABLE_DATA, pair(sample(3, 0, 0), sample(3, 0, 1)))
        );
        // Row 0, word 8: sample 16 alone; the upper half is written as 0.
        let at = 4 + 4 * 8;
        assert_eq!(w[at], (LSC_R_TABLE_DATA, u32::from(sample(0, 0, 16))));
        // Row 1 starts after the 9 words of row 0, which is 36 writes.
        assert_eq!(w[4 + 36].1, pair(sample(0, 1, 0), sample(0, 1, 1)));
        // Sector registers: X sizes then Y sizes, then gradients; two to a register.
        let sectors = 4 + 612;
        assert_eq!(w[sectors], (LSC_XSIZE_0, 100 | (101 << 16)));
        assert_eq!(w[sectors + 3], (LSC_XSIZE_0 + 12, 106 | (107 << 16)));
        assert_eq!(w[sectors + 4], (LSC_YSIZE_0, 200 | (201 << 16)));
        assert_eq!(w[sectors + 7], (LSC_YSIZE_0 + 12, 206 | (207 << 16)));
        assert_eq!(
            w[sectors + 8].0,
            lemnos_driver_k230isp_sys::regs::LSC_XGRAD_0
        );
        assert_eq!(w[sectors + 12], (LSC_YGRAD_0, 11 | (12 << 16)));
        assert_eq!(w[sectors + 15], (LSC_YGRAD_0 + 12, 17 | (18 << 16)));
        // The select, then the control.
        assert_eq!(w[632], (LSC_TABLE_SEL, 0));
        assert_eq!(w[633], (LSC_CTRL, LSC_EN));
    }

    #[test]
    fn bank_one_starts_at_153_and_is_selected() {
        let w = lens_shading_writes(&tables(), &geometry(), Bank::One, false).unwrap();
        assert_eq!(w[0], (LSC_R_TABLE_ADDR, 153));
        assert_eq!(w[1].1, 153);
        assert_eq!(w[632], (LSC_TABLE_SEL, 1));
        assert_eq!(w[633], (LSC_CTRL, 0));
    }

    #[test]
    fn the_list_is_admitted_by_the_driver_s_writable_table() {
        // Every write is one the configuration session may make: each register's field bits
        // hold the value. A rejected write would drop the whole list.
        let w = lens_shading_writes(&tables(), &geometry(), Bank::One, true).unwrap();
        let config = config_bytes(&w);
        let admitted = config::check(&config).expect("the lens list is refused");
        assert_eq!(admitted.count(), 634);
    }

    #[test]
    fn a_sample_past_12_bits_is_refused_and_no_writes_are_made() {
        let mut t = tables();
        t.gb[3][16] = 4096;
        assert_eq!(
            lens_shading_writes(&t, &geometry(), Bank::Zero, true),
            Err(LensError::Sample {
                table: 2,
                row: 3,
                col: 16,
                value: 4096
            })
        );
        // The largest value fits.
        t.gb[3][16] = LENS_SAMPLE_MAX;
        assert!(lens_shading_writes(&t, &geometry(), Bank::Zero, true).is_ok());
    }

    #[test]
    fn a_sector_size_of_zero_or_past_10_bits_is_refused() {
        for bad in [0u16, 1024] {
            let mut g = geometry();
            g.y_size[5] = bad;
            assert_eq!(
                lens_shading_writes(&tables(), &g, Bank::Zero, true),
                Err(LensError::SectorSize { value: bad })
            );
        }
        let mut g = geometry();
        g.x_size = [LENS_SECT_SIZE_MAX; LENS_SECTORS];
        assert!(lens_shading_writes(&tables(), &g, Bank::Zero, true).is_ok());
    }

    #[test]
    fn a_gradient_past_12_bits_is_refused() {
        let mut g = geometry();
        g.x_grad[7] = 4096;
        assert_eq!(
            lens_shading_writes(&tables(), &g, Bank::Zero, true),
            Err(LensError::Gradient { value: 4096 })
        );
    }
}
