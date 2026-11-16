//! The RK3566 watchdog: a DesignWare WDT, and a model of it for the sim.
//!
//! Provenance (all read locally, no vendor code copied):
//! - base 0xfe600000, register size 0x100, compatible rockchip,rk3568-wdt + snps,dw-wdt, clocks
//!   TCLK_WDT_NS and PCLK_WDT_NS: `arch/arm64/boot/dts/rockchip/rk356x-base.dtsi:1377` (Linux tree
//!   linux-rpi-7.2 at 53679a578).
//! - TCLK_WDT_NS is parented to xin24m, 24 MHz: `drivers/clk/rockchip/clk-rk3568.c:1208`.
//! - Registers: CR at 0x00 (WDT_EN bit 0, RESP_MODE bit 1, clear for system reset), TORR at 0x04
//!   (TOP in bits 3:0, TOP_INIT in bits 7:4), CRR at 0x0c with the kick value 0x76:
//!   `drivers/watchdog/dw_wdt.c` lines 32-39, 108-119, 190-195, 225-226.
//! - Period: 2^(16 + TOP) cycles, so TOP 15 at 24 MHz is 2^31 / 24e6 s, 89.48 s; the largest.
//!
//! Not from the RK3566 TRM (not read): whether the boot ROM leaves the watchdog running between the
//! 471 and 472 stages. That is a bench check.

/// The watchdog's base address on the RK3566.
pub const BASE: usize = 0xfe60_0000;
/// Control register offset (WDT_EN, RESP_MODE).
pub const CR_OFFSET: usize = 0x00;
/// Timeout-range register offset (TOP, TOP_INIT).
pub const TORR_OFFSET: usize = 0x04;
/// Counter-restart register offset (the kick).
pub const CRR_OFFSET: usize = 0x0c;
/// The value written to the restart register to restart the counter.
pub const KICK: u32 = 0x76;
/// CR: enable the watchdog. RESP_MODE stays clear: a system reset.
pub const CR_ENABLE: u32 = 1 << 0;
/// CR: the interrupt-then-reset response mode, which this code does not use.
pub const CR_RESP_MODE_IRQ: u32 = 1 << 1;
/// TCLK_WDT_NS, from xin24m.
pub const CLOCK_HZ: u64 = 24_000_000;
/// The largest TOP the counter takes.
pub const MAX_TOP: u8 = 15;

/// The TORR value for `top`: the same TOP in both fields, as `dw_wdt_set_timeout` writes it.
pub fn torr_for_top(top: u8) -> u32 {
    let top = u32::from(top & 0x0F);
    top | (top << 4)
}

/// The period of `top`, in microseconds.
pub const fn period_us(top: u8) -> u64 {
    (1u64 << (16 + (top & 0x0F) as u32)) * 1_000_000 / CLOCK_HZ
}

/// The longest period the watchdog offers: the arming's period, and the bound every wait stays under.
pub const LONGEST_PERIOD_US: u64 = period_us(MAX_TOP);

/// The watchdog as the sim sees it: its registers, the counter, and whether it has fired. It
/// models the reset response only, and a kick that is not the kick value does nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    cr: u32,
    torr: u32,
    active_top: u8,
    elapsed_us: u64,
    fired: bool,
}

impl Model {
    /// A watchdog in its reset state: disabled, TOP 0.
    pub const fn new() -> Self {
        Self {
            cr: 0,
            torr: 0,
            active_top: 0,
            elapsed_us: 0,
            fired: false,
        }
    }

    /// A register write at `offset`.
    pub fn write(&mut self, offset: usize, value: u32) {
        match offset {
            CR_OFFSET => {
                self.cr = value;
                if value & CR_ENABLE != 0 && !self.enabled_now() {
                    self.active_top = (self.torr & 0x0F) as u8;
                    self.elapsed_us = 0;
                }
            }
            TORR_OFFSET => self.torr = value,
            CRR_OFFSET if value == KICK => {
                self.active_top = (self.torr & 0x0F) as u8;
                self.elapsed_us = 0;
            }
            _ => {}
        }
    }

    /// Time passes: the counter runs, and fires when it reaches the period.
    pub fn advance(&mut self, us: u64) {
        if !self.enabled_now() || self.fired {
            return;
        }
        self.elapsed_us = self.elapsed_us.saturating_add(us);
        if self.elapsed_us >= period_us(self.active_top) {
            self.fired = true;
        }
    }

    /// Whether the watchdog has reset the board.
    pub fn reset_fired(&self) -> bool {
        self.fired
    }

    /// Whether the enable bit is set.
    pub fn enabled_now(&self) -> bool {
        self.cr & CR_ENABLE != 0
    }

    /// The TOP the counter runs with.
    pub fn active_top(&self) -> u8 {
        self.active_top
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUB: &str = include_str!("../../rockchip-maskrom-sys/src/stage471.S");

    #[test]
    fn the_timeout_range_repeats_top_in_both_fields() {
        assert_eq!(torr_for_top(15), 0xFF);
        assert_eq!(torr_for_top(0), 0x00);
        assert_eq!(torr_for_top(3), 0x33);
        assert_eq!(torr_for_top(0x13), 0x33, "only the low four bits are TOP");
    }

    #[test]
    fn the_longest_period_is_two_to_the_thirty_first_over_24_mhz() {
        // 2^31 / 24e6 s = 89.478485... s
        assert_eq!(period_us(MAX_TOP), 89_478_485);
        assert_eq!(period_us(0), 2_730);
    }

    #[test]
    fn the_periods_grow_from_top_zero_to_fifteen() {
        for top in 1..=MAX_TOP {
            assert!(period_us(top) > period_us(top - 1));
        }
        assert_eq!(LONGEST_PERIOD_US, period_us(MAX_TOP));
    }

    #[test]
    fn the_stub_sequence_arms_the_longest_period_and_fires_at_it() {
        let mut wdt = Model::new();
        wdt.write(TORR_OFFSET, torr_for_top(MAX_TOP));
        wdt.write(CRR_OFFSET, KICK);
        wdt.write(CR_OFFSET, CR_ENABLE);
        assert!(wdt.enabled_now());
        assert_eq!(wdt.active_top(), MAX_TOP);
        wdt.advance(period_us(MAX_TOP) - 1);
        assert!(!wdt.reset_fired(), "not before the period");
        wdt.advance(1);
        assert!(wdt.reset_fired(), "a hang resets the board at the period");
    }

    #[test]
    fn a_kick_keeps_it_alive_and_the_wrong_kick_does_not() {
        let mut wdt = Model::new();
        wdt.write(TORR_OFFSET, torr_for_top(10));
        wdt.write(CRR_OFFSET, KICK);
        wdt.write(CR_OFFSET, CR_ENABLE);
        wdt.advance(period_us(10) - 5);
        wdt.write(CRR_OFFSET, KICK);
        wdt.advance(period_us(10) - 5);
        assert!(!wdt.reset_fired(), "the kick restarted the counter");
        wdt.write(CRR_OFFSET, 0x75);
        wdt.advance(period_us(10));
        assert!(
            wdt.reset_fired(),
            "a write other than the kick value does nothing"
        );
    }

    #[test]
    fn a_disabled_watchdog_never_fires() {
        let mut wdt = Model::new();
        wdt.write(TORR_OFFSET, torr_for_top(MAX_TOP));
        wdt.advance(u64::MAX / 2);
        assert!(!wdt.reset_fired());
    }

    #[test]
    fn a_new_torr_takes_effect_at_the_next_kick() {
        let mut wdt = Model::new();
        wdt.write(TORR_OFFSET, torr_for_top(MAX_TOP));
        wdt.write(CRR_OFFSET, KICK);
        wdt.write(CR_OFFSET, CR_ENABLE);
        wdt.write(TORR_OFFSET, torr_for_top(4));
        assert_eq!(wdt.active_top(), MAX_TOP, "not yet");
        wdt.write(CRR_OFFSET, KICK);
        assert_eq!(wdt.active_top(), 4);
    }

    #[test]
    fn the_stub_writes_its_registers_in_the_order_that_arms_it() {
        // The values and the order the assembly stores them in. Read from the stub's own source, so
        // a change to the stub without a change here fails the test.
        let base = STUB
            .find("#0xfe60, lsl #16")
            .expect("the WDT base, as an immediate");
        let torr = STUB.find("#0x04]").expect("TORR store");
        let kick = STUB.find("#0x0c]").expect("kick store");
        let enable = STUB.find("#0x00]").expect("CR store");
        let jump = STUB.find("br      x16").expect("the jump to the DDR blob");
        assert!(base < torr && torr < kick && kick < enable && enable < jump);
        assert!(
            STUB.contains("movz    w10, #0xff"),
            "TORR = 0xff: TOP_INIT and TOP at 15"
        );
        assert!(STUB.contains("movz    w10, #0x76"), "the kick value");
        assert!(
            STUB.contains("movz    w10, #0x01"),
            "CR: enable, reset mode"
        );
    }

    #[test]
    fn the_stub_is_position_independent_in_its_source() {
        // No literal pool (ldr from a label), no absolute data words: the BROM picks the load address.
        assert!(
            !STUB.contains(", ="),
            "a literal-pool load (ldr x, =sym) assumes a base"
        );
        assert!(!STUB.contains(".word") && !STUB.contains(".quad") && !STUB.contains(".xword"));
        assert!(
            STUB.contains("adr     x16, lemnos_ddr_blob"),
            "the DDR blob is reached PC-relatively"
        );
        assert!(
            STUB.contains("lemnos_arm_and_spin:"),
            "the arm-and-spin image is in the same stub"
        );
        assert!(
            STUB.contains("1:  b       1b"),
            "the spin is a branch to itself, with no feed"
        );
    }
}
