//! The pad rule, enforced in code. The stage writes no iomux, pad-control or bank-voltage register at all:
//! not the SD function select, not a pull, not a bank supply. The stage reads all 64 pad registers and
//! reports them (the ROM's state, as it was left), and it reaches the SD card only through what the ROM
//! configured. [`PadGuard`] refuses any write into the pad ranges, so the rule holds whatever the caller
//! does; the simulator's recorder test proves no write reaches them.
//!
//! Provenance (kendryte/k230_sdk 7e302f7):
//! - iomux base `0x91105000`, 2 KiB (`board.h:165-166`, `IOMUX_BASE_ADDR`, `IOMUX_IO_SIZE`). The device tree's
//!   node `iomux@91105000` is 64 KiB (`k230.dtsi:279-286`), which overlaps other blocks, so the guard refuses
//!   the 2 KiB pad block only.
//! - pmu iomux `0x91000080`, 32 bytes (`k230.dtsi:288-290`): refused too.
//! - per-pad layout, one 32-bit register per pad at `n*4` (`include/dt-bindings/pinctrl/k230_evb.h`, the
//!   `IOn` macros and the field bit numbers `ST 0, DS 1, PD 5, PU 6, OE 7, IE 8, MSC 9, SL 10, SEL 11`). The
//!   widths of multi-bit fields (drive strength, the function select) are UNVERIFIED; the report prints the raw
//!   word and the single-bit fields only.
//! - the bank voltage (`MSC`) is per pad in that header's layout; the vendor's per-board bank tables are not
//!   copied into this stage (they are another board's, and a wrong bank voltage damages the chip).

use crate::wdt::RegAccess;

/// The iomux node's base.
pub const IOMUX: usize = 0x9110_5000;
/// The iomux block's length: 2 KiB (`board.h:166`, `IOMUX_IO_SIZE`). The device tree's node is 64 KiB
/// (`k230.dtsi:281`), but its upper part overlaps other blocks (the watchdogs at `0x91106000`), so the guard
/// refuses the 2 KiB block the pads are in, and nothing else.
pub const IOMUX_LEN: usize = 0x800;
/// The pmu iomux node's base and window.
pub const PMU_IOMUX: usize = 0x9100_0080;
pub const PMU_IOMUX_LEN: usize = 0x20;
/// The pads the dump reads: IO0 to IO63.
pub const PAD_COUNT: usize = 64;

/// The register of pad `n`.
pub fn pad_register(n: usize) -> usize {
    IOMUX + 4 * n
}

/// Whether `addr` is in a pad range (any width, any register): the guard's test.
pub fn is_pad_address(addr: usize) -> bool {
    (IOMUX..IOMUX + IOMUX_LEN).contains(&addr) || (PMU_IOMUX..PMU_IOMUX + PMU_IOMUX_LEN).contains(&addr)
}

/// A register access that refuses every write into the pad ranges and counts the refusals. Reads pass.
pub struct PadGuard<R: RegAccess> {
    inner: R,
    refused: u32,
}

impl<R: RegAccess> PadGuard<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, refused: 0 }
    }

    /// The bus behind the guard, for a test to read what was written through it.
    pub fn inner_for_test(&self) -> &R {
        &self.inner
    }

    /// The writes refused so far (a write into a pad range is never forwarded).
    pub fn refused(&self) -> u32 {
        self.refused
    }
}

impl<R: RegAccess> RegAccess for PadGuard<R> {
    fn read32(&mut self, addr: usize) -> u32 {
        self.inner.read32(addr)
    }

    fn write32(&mut self, addr: usize, value: u32) {
        if is_pad_address(addr) {
            self.refused += 1;
            return;
        }
        self.inner.write32(addr, value);
    }
}

/// The raw pad registers, IO0 to IO63, as the ROM left them. A read only.
pub fn dump<R: RegAccess>(regs: &mut R) -> [u32; PAD_COUNT] {
    let mut out = [0u32; PAD_COUNT];
    for (n, word) in out.iter_mut().enumerate() {
        *word = regs.read32(pad_register(n));
    }
    out
}

/// The SD pads: IO26 clk, IO27 cmd, IO28 to IO31 data (`genimage` and the other board's table, reference).
pub const SD_PADS: [usize; 6] = [26, 27, 28, 29, 30, 31];
/// The function-select value of the SD function, as the other board's pad table sets it (SEL=1 on IO26 to
/// IO31, `k230d_canmv_bpi-pins.txt`, a reference from another board model). The value on this SoC is UNVERIFIED.
pub const SD_FUNCTION: u32 = 1;
/// The function-select field's position (`SEL` bit 11 in `k230_evb.h`); its width is UNVERIFIED, taken as 3 bits.
pub const SEL_SHIFT: u32 = 11;
pub const SEL_MASK: u32 = 0x7;

/// The gate before any SD access: every SD pad must be on the SD function as the ROM left it. A pad that is
/// not is reported by its number; the stage then makes no SD access at all (no clock, no command).
pub fn sd_pads_on_sd_function(dump: &[u32; PAD_COUNT]) -> Result<(), usize> {
    for &pad in &SD_PADS {
        let sel = (dump[pad] >> SEL_SHIFT) & SEL_MASK;
        if sel != SD_FUNCTION {
            return Err(pad);
        }
    }
    Ok(())
}

/// The single-bit fields of a pad word, by the bit numbers `k230_evb.h` gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadBits {
    pub st: bool,
    pub pd: bool,
    pub pu: bool,
    pub oe: bool,
    pub ie: bool,
    pub msc: bool,
    pub sl: bool,
}

pub fn decode(raw: u32) -> PadBits {
    let bit = |n: u32| raw & (1 << n) != 0;
    PadBits {
        st: bit(0),
        pd: bit(5),
        pu: bit(6),
        oe: bit(7),
        ie: bit(8),
        msc: bit(9),
        sl: bit(10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A register file that records every write address.
    #[derive(Default)]
    struct Recorder {
        writes: Vec<usize>,
        reads: Vec<usize>,
    }

    impl RegAccess for Recorder {
        fn read32(&mut self, addr: usize) -> u32 {
            self.reads.push(addr);
            0x0000_0800
        }
        fn write32(&mut self, addr: usize, _value: u32) {
            self.writes.push(addr);
        }
    }

    #[test]
    fn the_guard_refuses_every_write_into_the_pad_ranges_and_forwards_the_rest() {
        let mut guard = PadGuard::new(Recorder::default());
        guard.write32(IOMUX, 1);
        guard.write32(pad_register(63), 1);
        guard.write32(IOMUX + IOMUX_LEN - 4, 1);
        guard.write32(PMU_IOMUX, 1);
        guard.write32(0x9110_6000, 1); // wdt0: not a pad, forwarded
        assert_eq!(guard.refused(), 4);
        assert_eq!(guard.inner.writes, vec![0x9110_6000]);
    }

    #[test]
    fn the_pad_dump_reads_sixty_four_registers_and_writes_none() {
        let mut rec = Recorder::default();
        let words = dump(&mut rec);
        assert_eq!(rec.reads.len(), PAD_COUNT);
        assert_eq!(rec.reads[0], IOMUX);
        assert_eq!(rec.reads[63], pad_register(63));
        assert!(rec.writes.is_empty());
        assert!(words.iter().all(|&w| w == 0x800));
    }

    #[test]
    fn the_sd_gate_passes_only_when_every_sd_pad_is_on_the_sd_function() {
        let mut dump = [0u32; PAD_COUNT];
        for &pad in &SD_PADS {
            dump[pad] = SD_FUNCTION << SEL_SHIFT;
        }
        assert_eq!(sd_pads_on_sd_function(&dump), Ok(()));
        // One pad left on GPIO (function 0): the gate names it and the stage makes no SD access.
        dump[29] = 0;
        assert_eq!(sd_pads_on_sd_function(&dump), Err(29));
    }

    #[test]
    fn the_single_bit_fields_follow_the_header_bit_numbers() {
        let bits = decode((1 << 7) | (1 << 9));
        assert!(bits.oe && bits.msc && !bits.ie && !bits.st);
    }
}
