//! Chip descriptors for Lemnos, as data: each SoC's IO blocks, what each block
//! can do, whether Lemnos uses it, and the software path it would replace.
//!
//! [`CHIPS`] holds one [`Chip`] per SoC, with its [`Entry`] table (the
//! `docs/io-blocks.md` tables, as code). Nothing here runs on a board: the
//! descriptors come from the public device trees and the drivers' register
//! layouts, the sources are named per chip, and every chip is untested on
//! board. The [`rockchip_pwm`], [`rockchip_timer`] and [`clint`] modules hold
//! the register layouts of the blocks a user-space path could reach (through
//! `/dev/mem` or a kernel interface), as typed constants.
//!
//! The crate is `no_std` with no allocation: a descriptor is a `static`, so
//! it costs nothing in an image that does not read it.

#![no_std]
#![forbid(unsafe_code)]

mod chips;

pub use chips::CHIPS;

/// The kind of IO block an entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Gpio,
    Pwm,
    Timer,
    Watchdog,
    I2c,
    Spi,
    Uart,
    I2s,
    Pdm,
    Adc,
    Thermal,
    Dma,
    /// An on-chip CPU that is not the application cores (a microcontroller).
    Mcu,
    /// The interrupt controller and the CPU-local timer (CLINT, PLIC).
    Interrupt,
    /// The boot-time clock tree and pin mux: not a data path (kept for the
    /// base addresses a board needs).
    Platform,
}

/// Whether a capability is used, not used, or cannot be used from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// Code in the tree runs it on the chip (none of the chips here yet).
    Used,
    /// The block can do it; nothing runs it.
    Unused,
    /// The block or its kernel interface cannot do it.
    Impossible,
}

/// One capability of one IO block of a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    /// The block's name in the chip's description (`pwm0..pwm15`).
    pub block: &'static str,
    /// What the block does for this row (`PWM output`, `PWM input capture`).
    pub function: &'static str,
    /// Register base addresses, one per controller of this kind in the
    /// description (empty when the mainline description has none).
    pub bases: &'static [u64],
    /// The compatible string the description binds the block with.
    pub compatible: &'static str,
    pub status: Use,
    /// The software path this capability would replace or serve.
    pub software: &'static str,
    /// What decides the status, and what is not verified.
    pub note: &'static str,
}

/// One SoC: its cores, the sources its table is built from, and its table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chip {
    pub name: &'static str,
    pub cores: &'static str,
    pub sources: &'static [&'static str],
    pub entries: &'static [Entry],
}

impl Chip {
    /// The chip named `name` (case-sensitive, as in [`CHIPS`]).
    pub fn find(name: &str) -> Option<&'static Chip> {
        CHIPS.iter().find(|c| c.name == name)
    }

    /// How many controllers of `kind` the description gives: the base
    /// addresses of its entries (rows without bases count nothing).
    pub fn count(&self, kind: Kind) -> usize {
        self.entries
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.bases.len())
            .sum()
    }
}

/// The Rockchip PWM register layout of the `rockchip,rk3328-pwm` binding,
/// which the RK3562, RK3566 and RK3588 PWM nodes use (the `pwm_data_v3` of the
/// mainline driver). One channel per register block.
pub mod rockchip_pwm {
    /// Counter (read back, the running count).
    pub const CNTR: usize = 0x00;
    /// Period, in prescaled clock ticks.
    pub const PERIOD: usize = 0x04;
    /// Duty, in prescaled clock ticks.
    pub const DUTY: usize = 0x08;
    /// Control.
    pub const CTRL: usize = 0x0c;
    /// The channel is enabled.
    pub const CTRL_ENABLE: u32 = 1 << 0;
    /// Continuous output (not one period).
    pub const CTRL_CONTINUOUS: u32 = 1 << 1;
    /// Active-high duty.
    pub const CTRL_DUTY_POSITIVE: u32 = 1 << 3;
    /// Idle level high.
    pub const CTRL_INACTIVE_POSITIVE: u32 = 1 << 4;
    /// Lock the period and duty while changing them.
    pub const CTRL_LOCK: u32 = 1 << 6;
    /// The bits that must all be set for the output to run.
    pub const RUN: u32 = CTRL_ENABLE | CTRL_CONTINUOUS;
    /// The size of one channel's register block.
    pub const BLOCK_BYTES: usize = 0x10;
}

/// The Rockchip timer layout the `rockchip,rk3288-timer` fallback uses (the
/// RK3576 and RK3588 timer nodes bind with it). A 64-bit down counter.
pub mod rockchip_timer {
    pub const LOAD_COUNT0: usize = 0x00;
    pub const LOAD_COUNT1: usize = 0x04;
    pub const CURRENT_VALUE0: usize = 0x08;
    pub const CURRENT_VALUE1: usize = 0x0c;
    pub const CONTROL: usize = 0x10;
    pub const INT_STATUS: usize = 0x18;
    /// The timer runs.
    pub const CONTROL_ENABLE: u32 = 1 << 0;
    /// User-defined count (otherwise free running).
    pub const CONTROL_USER_DEFINED: u32 = 1 << 1;
    /// Interrupt not masked.
    pub const CONTROL_INT_UNMASK: u32 = 1 << 2;
    /// The size of one timer's register block.
    pub const BLOCK_BYTES: usize = 0x20;
}

/// The CLINT layout the kernel's `timer-clint` driver uses (the K230's
/// `canaan,k230-clint`, bound with `thead,c900-clint`).
pub mod clint {
    /// Software-interrupt register of hart 0.
    pub const MSIP: usize = 0x0000;
    /// The hart 0 timer compare (`mtimecmp`).
    pub const TIMER_CMP: usize = 0x4000;
    /// The machine time (`mtime`), the counter the timebase drives.
    pub const TIMER_VAL: usize = 0xbff8;
    /// The K230's timebase (`timebase-frequency` in `k230.dtsi`).
    pub const K230_TIMEBASE_HZ: u32 = 27_000_000;
}

const _: () = assert!(rockchip_timer::INT_STATUS < rockchip_timer::BLOCK_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwm_registers_are_distinct_and_inside_one_block() {
        let regs = [
            rockchip_pwm::CNTR,
            rockchip_pwm::PERIOD,
            rockchip_pwm::DUTY,
            rockchip_pwm::CTRL,
        ];
        for (i, a) in regs.iter().enumerate() {
            assert!(*a < rockchip_pwm::BLOCK_BYTES);
            assert!(regs[i + 1..].iter().all(|b| b != a));
        }
        // The run bits are the two low bits of the v3 control word.
        assert_eq!(rockchip_pwm::RUN, 0b11);
    }

    #[test]
    fn timer_registers_follow_the_rk3288_layout() {
        assert_eq!(rockchip_timer::CONTROL, 0x10);
        assert_eq!(rockchip_timer::INT_STATUS, 0x18);
        assert_eq!(
            rockchip_timer::CONTROL_ENABLE | rockchip_timer::CONTROL_USER_DEFINED,
            0b11
        );
    }

    #[test]
    fn every_chip_names_its_sources_and_explains_its_unused_and_impossible_rows() {
        for chip in CHIPS {
            assert!(!chip.sources.is_empty(), "{} has no sources", chip.name);
            assert!(!chip.entries.is_empty(), "{} has no entries", chip.name);
            for e in chip.entries {
                assert!(!e.function.is_empty() && !e.software.is_empty());
                if e.status == Use::Impossible {
                    assert!(
                        !e.note.is_empty(),
                        "{}: {} has no reason",
                        chip.name,
                        e.function
                    );
                }
            }
        }
    }

    #[test]
    fn block_counts_match_the_descriptions() {
        let rk3588s = Chip::find("RK3588S").expect("RK3588S");
        assert_eq!(rk3588s.count(Kind::Gpio), 5);
        assert_eq!(rk3588s.count(Kind::Pwm), 16);
        assert_eq!(rk3588s.count(Kind::I2c), 9);
        assert_eq!(rk3588s.count(Kind::Uart), 10);
        assert_eq!(rk3588s.count(Kind::I2s), 7);
        let rk3562 = Chip::find("RK3562").expect("RK3562");
        assert_eq!(rk3562.count(Kind::Pwm), 16);
        assert_eq!(rk3562.count(Kind::Adc), 2);
        let k230d = Chip::find("K230D").expect("K230D");
        assert_eq!(k230d.count(Kind::Uart), 5);
        let rv1126b = Chip::find("RV1126B").expect("RV1126B");
        assert_eq!(rv1126b.count(Kind::Gpio), 8);
        assert_eq!(rv1126b.count(Kind::Pwm), 28);
        assert_eq!(rv1126b.count(Kind::I2c), 6);
        assert_eq!(rv1126b.count(Kind::Uart), 8);
        assert_eq!(rv1126b.count(Kind::Adc), 3);
        let a733 = Chip::find("A733").expect("A733");
        assert_eq!(a733.count(Kind::Pwm), 3);
        assert_eq!(a733.count(Kind::I2c), 16);
        assert_eq!(a733.count(Kind::Spi), 6);
        assert_eq!(a733.count(Kind::Uart), 9);
        assert_eq!(a733.count(Kind::I2s), 5);
        assert!(Chip::find("RK9999").is_none());
    }
}
