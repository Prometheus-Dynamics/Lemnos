//! The K230 watchdogs: two DesignWare WDTs (`snps,dw-wdt`), their register map, the arming
//! sequence with its readback, the feed rule, and a model of their counters for the simulator.
//!
//! Provenance (read, nothing copied):
//! - Bases: `wdt0` `0x91106000` and `wdt1` `0x91106800`. `k230.dtsi` at kendryte/k230_sdk 7e302f7
//!   (`src/little/linux/arch/riscv/boot/dts/kendryte/k230.dtsi`, lines 513-518) gives `wdt0`, IRQ 107;
//!   `board.h:171-175` (`src/big/rt-smart/kernel/bsp/maix3/board/board.h`) gives both bases.
//! - Registers: CR `0x00` (WDT_EN bit 0, RESP_MODE bit 1), TORR `0x04` (TOP_INIT in bits 7:4, TOP in
//!   bits 3:0), CCVR `0x08` (current counter value), CRR `0x0c` (restart; `0x76` restarts the
//!   counter). `drv_wdt.h` lines 37-72 (the SDK's own map). `drv_wdt.c` lines 86-135 and 179-199 write
//!   the same registers.
//! - The period of TOP `n` is `2^(16 + n)` cycles of the WDT clock (`DW_WDT_FIX_TOP`, `dw_wdt.c`).
//! - What a WDT reset reaches: UNVERIFIED. The sources do not say whether a WDT system reset resets
//!   the whole SoC or one core. The stage therefore arms both WDTs, and the SoC reset in
//!   [`crate::sysctl`] is the second path that does not depend on either.
//!
//! UNVERIFIED: the WDT clock (see [`CLOCK_HZ_MIN`]/[`CLOCK_HZ_MAX`]). The bench reads it (step 2).

/// `wdt0`'s base address.
pub const BASE0: usize = 0x9110_6000;
/// `wdt1`'s base address (the big core's block in the SDK's RT-Thread driver).
pub const BASE1: usize = 0x9110_6800;
/// The bases the stage arms, in order.
pub const BASES: [usize; 2] = [BASE0, BASE1];
/// Control register: bit 0 enables the counter, bit 1 selects the response (0: system reset).
pub const CR: usize = 0x00;
/// Timeout-range register: TOP_INIT in bits 7:4, TOP in bits 3:0.
pub const TORR: usize = 0x04;
/// Current counter value (read only).
pub const CCVR: usize = 0x08;
/// Counter-restart register: writing [`KICK`] reloads the counter from TOP.
pub const CRR: usize = 0x0c;
/// The value that restarts the counter (`drv_wdt.c`, `reg->crr = 0x76`).
pub const KICK: u32 = 0x76;
/// CR bit 0: the counter runs.
pub const CR_ENABLE: u32 = 1 << 0;
/// The TOP the stage arms with: a period of `2^27` cycles.
pub const TOP: u8 = 11;
/// The TOP for a fast reset: `2^16` cycles, the shortest period.
pub const FAST_TOP: u8 = 0;
/// Reads of CCVR the readback makes before it calls the counter stuck.
pub const COUNTER_READS: u32 = 16;
/// The lowest WDT clock the design accepts (UNVERIFIED; see the module docs).
pub const CLOCK_HZ_MIN: u64 = 1_000_000;
/// The highest WDT clock the design accepts (UNVERIFIED; see the module docs).
pub const CLOCK_HZ_MAX: u64 = 100_000_000;
/// The backstop after the last kick, in microseconds: twice the longest armed period. A WDT that
/// resets the board does so first; this only fires when the reset did not reach the board.
pub const DEADLINE_US: u64 = 2 * 134_217_728;
/// The same deadline in ticks of the core's `time` CSR, for the entry's spin. The timebase is
/// taken as 1 MHz, which makes the tick count equal to [`DEADLINE_US`]; the bench confirms it.
pub const SPIN_DEADLINE_TICKS: u64 = DEADLINE_US;

/// The TORR value for `top`: the same TOP in TOP_INIT and TOP, as `dw_wdt_set_timeout` writes it.
pub fn torr(top: u8) -> u32 {
    let top = u32::from(top & 0x0f);
    top | (top << 4)
}

/// The period of `top` in counter cycles.
pub fn period_cycles(top: u8) -> u64 {
    1u64 << (16 + u32::from(top & 0x0f))
}

/// The period of `top` at `clock_hz`, in microseconds, rounded up: a counter that runs this long
/// has run a whole period.
pub fn period_us(top: u8, clock_hz: u64) -> u64 {
    let cycles = u128::from(period_cycles(top)) * 1_000_000;
    cycles.div_ceil(u128::from(clock_hz)) as u64
}

/// The longest any stall can last before the reset: the armed period at the lowest clock.
pub fn longest_period_us() -> u64 {
    period_us(TOP, CLOCK_HZ_MIN)
}

/// The shortest period the armed watchdog can have: the armed period at the highest clock. A
/// verified chunk, written and read back, must take less than this.
pub fn shortest_period_us() -> u64 {
    period_us(TOP, CLOCK_HZ_MAX)
}

/// Register access. The `-sys` crate implements it with volatile MMIO; the simulator implements it
/// with [`Model`]. The stage code is the same in both.
pub trait RegAccess {
    fn read32(&mut self, addr: usize) -> u32;
    fn write32(&mut self, addr: usize, value: u32);
}

/// Proof that both watchdogs were armed and read back running. It cannot be made except by
/// [`arm`], so a [`Dog`] exists only after the arming and its readback ran.
#[derive(Debug)]
pub struct Armed(());

/// A watchdog that did not take its arming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmError {
    /// CR reads back with the enable bit clear: the arming did not take.
    NotEnabled(usize),
    /// CR reads enabled, but CCVR did not change over [`COUNTER_READS`] reads: the counter is not
    /// running.
    NotCounting(usize),
}

impl core::fmt::Display for ArmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotEnabled(base) => write!(f, "watchdog {base:#x} did not enable"),
            Self::NotCounting(base) => write!(f, "watchdog {base:#x} is not counting"),
        }
    }
}

/// Arms both watchdogs: TORR (the period), then the kick (loads the counter from it), then CR
/// (enable), for each. Then the readback: CR must read enabled and CCVR must move. A reset response,
/// not an interrupt. If either fails, the caller takes the SoC reset ([`crate::sysctl`]); it does
/// not spin.
pub fn arm<R: RegAccess + ?Sized>(regs: &mut R) -> Result<Armed, ArmError> {
    for base in BASES {
        regs.write32(base + TORR, torr(TOP));
        regs.write32(base + CRR, KICK);
        regs.write32(base + CR, CR_ENABLE);
    }
    for base in BASES {
        if regs.read32(base + CR) & CR_ENABLE == 0 {
            return Err(ArmError::NotEnabled(base));
        }
        let first = regs.read32(base + CCVR);
        let moved = (0..COUNTER_READS).any(|_| regs.read32(base + CCVR) != first);
        if !moved {
            return Err(ArmError::NotCounting(base));
        }
    }
    Ok(Armed(()))
}

/// What the stage does to the watchdogs once it runs.
pub trait Feed {
    /// Restarts both counters. The stage calls it only after a verified chunk.
    fn feed(&mut self);
    /// The feeds made so far, for a test to read (0 where the feed does not count).
    fn count_for_test(&self) -> u32 {
        0
    }
    /// Shortens the period to the fast one and restarts it, so the board resets within
    /// `period_us(FAST_TOP, clock)`. Used by the reboot command.
    fn fast_reset(&mut self);
}

/// The watchdogs, armed. Feeding them needs the proof [`Armed`], which only [`arm`] makes.
pub struct Dog<R: RegAccess> {
    regs: R,
    _armed: Armed,
}

impl<R: RegAccess> Dog<R> {
    /// Wraps the registers of armed watchdogs.
    pub fn new(regs: R, armed: Armed) -> Self {
        Self {
            regs,
            _armed: armed,
        }
    }
}

impl<R: RegAccess> Feed for Dog<R> {
    fn feed(&mut self) {
        for base in BASES {
            self.regs.write32(base + CRR, KICK);
        }
    }

    fn fast_reset(&mut self) {
        for base in BASES {
            self.regs.write32(base + TORR, torr(FAST_TOP));
            self.regs.write32(base + CRR, KICK);
        }
    }
}

/// A fault a model watchdog can have. Each is a way the real block can disagree with its arming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The block behaves as the register map says.
    None,
    /// CR writes are dropped: the arming does not take, and CR reads disabled.
    CrIgnored,
    /// CR reads enabled, but the counter is frozen: CCVR does not move and nothing fires.
    CounterStuck,
}

/// A model of one watchdog's counter for the simulator. It runs while CR's enable bit is set, a
/// kick reloads it from TOP, and it fires when it runs out. A CCVR read costs one cycle, so the
/// readback sees the counter move. Time is given to it in microseconds at a clock frequency.
#[derive(Clone, Debug)]
pub struct Model {
    base: usize,
    clock_hz: u64,
    torr: u32,
    cr: u32,
    remaining: u64,
    fired: bool,
    kicks: u64,
    fault: Fault,
}

impl Model {
    /// A watchdog at `base` that is not armed, at `clock_hz`, with `fault`.
    pub fn new(base: usize, clock_hz: u64, fault: Fault) -> Self {
        Self {
            base,
            clock_hz,
            torr: 0,
            cr: 0,
            remaining: 0,
            fired: false,
            kicks: 0,
            fault,
        }
    }

    /// Runs the counter for `us` microseconds. Returns true when it fires during that time.
    pub fn advance_us(&mut self, us: u64) -> bool {
        if self.cr & CR_ENABLE == 0 || self.fired || self.fault == Fault::CounterStuck {
            return false;
        }
        let cycles = (u128::from(us) * u128::from(self.clock_hz)).div_ceil(1_000_000);
        if cycles >= u128::from(self.remaining) {
            self.remaining = 0;
            self.fired = true;
            true
        } else {
            self.remaining -= cycles as u64;
            false
        }
    }

    /// The base address this watchdog answers at.
    pub fn base(&self) -> usize {
        self.base
    }

    /// Whether the counter has run out while enabled.
    pub fn fired(&self) -> bool {
        self.fired
    }

    /// The kicks received since the model was made.
    pub fn kicks(&self) -> u64 {
        self.kicks
    }

    /// Whether the counter is enabled.
    pub fn enabled(&self) -> bool {
        self.cr & CR_ENABLE != 0
    }

    /// The cycles left before a reset, while enabled.
    pub fn remaining_cycles(&self) -> u64 {
        self.remaining
    }
}

impl RegAccess for Model {
    fn read32(&mut self, addr: usize) -> u32 {
        match addr.wrapping_sub(self.base) {
            CR => self.cr,
            TORR => self.torr,
            CCVR => {
                if self.cr & CR_ENABLE != 0 && self.fault != Fault::CounterStuck {
                    self.remaining = self.remaining.saturating_sub(1);
                }
                self.remaining as u32
            }
            _ => 0,
        }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        match addr.wrapping_sub(self.base) {
            CR if self.fault == Fault::CrIgnored => {}
            CR => self.cr = value & 0x3,
            TORR => self.torr = value,
            CRR if value == KICK => {
                self.remaining = period_cycles((self.torr & 0x0f) as u8);
                self.kicks += 1;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both models at their bases, for one clock and fault each.
    fn pair(hz: u64, faults: [Fault; 2]) -> (Model, Model) {
        (
            Model::new(BASE0, hz, faults[0]),
            Model::new(BASE1, hz, faults[1]),
        )
    }

    /// One register file over two models, as the board has it.
    struct Both(Model, Model);

    impl RegAccess for Both {
        fn read32(&mut self, addr: usize) -> u32 {
            if (BASE0..BASE0 + 0x800).contains(&addr) {
                self.0.read32(addr)
            } else {
                self.1.read32(addr)
            }
        }
        fn write32(&mut self, addr: usize, value: u32) {
            if (BASE0..BASE0 + 0x800).contains(&addr) {
                self.0.write32(addr, value)
            } else {
                self.1.write32(addr, value)
            }
        }
    }

    #[test]
    fn the_torr_encoding_repeats_the_top_in_both_fields() {
        assert_eq!(torr(TOP), 0xbb);
        assert_eq!(torr(FAST_TOP), 0x00);
        assert_eq!(torr(15), 0xff);
    }

    #[test]
    fn the_period_is_two_to_the_sixteen_plus_top_cycles() {
        assert_eq!(period_cycles(0), 1 << 16);
        assert_eq!(period_cycles(TOP), 1 << 27);
        assert_eq!(period_us(TOP, 1_000_000), 134_217_728);
        assert_eq!(period_us(TOP, 100_000_000), 1_342_178);
    }

    #[test]
    fn the_deadline_is_past_every_armed_period() {
        assert!(DEADLINE_US > longest_period_us());
        assert_eq!(SPIN_DEADLINE_TICKS, DEADLINE_US);
    }

    #[test]
    fn the_armed_period_is_bounded_across_the_plausible_clocks() {
        assert!(longest_period_us() < 180_000_000);
        assert!(shortest_period_us() >= 1_000_000);
    }

    #[test]
    fn arming_both_reads_back_running_and_each_fires_at_its_period() {
        for hz in [CLOCK_HZ_MIN, 24_000_000, CLOCK_HZ_MAX] {
            let (a, b) = pair(hz, [Fault::None; 2]);
            let mut regs = Both(a, b);
            let armed = arm(&mut regs).expect("a healthy pair arms");
            let _ = armed;
            // Each counter restarts from its period when armed (the readback spent a few cycles).
            let period = period_us(TOP, hz);
            assert!(!regs.0.advance_us(period - 1000), "{hz} Hz fired early");
            assert!(regs.0.advance_us(2000), "{hz} Hz wdt0 did not fire");
            assert!(regs.1.enabled(), "{hz} Hz wdt1 is not armed");
        }
    }

    #[test]
    fn a_cr_that_ignores_the_arming_is_refused_by_the_readback() {
        let (a, b) = pair(24_000_000, [Fault::None, Fault::CrIgnored]);
        let mut regs = Both(a, b);
        assert_eq!(arm(&mut regs).unwrap_err(), ArmError::NotEnabled(BASE1));
    }

    #[test]
    fn a_readback_that_lies_about_a_frozen_counter_is_refused() {
        // Real-failing: CR reads enabled, so the enable check passes, but CCVR never moves.
        let (a, b) = pair(24_000_000, [Fault::CounterStuck, Fault::None]);
        let mut regs = Both(a, b);
        assert_eq!(arm(&mut regs).unwrap_err(), ArmError::NotCounting(BASE0));
    }

    #[test]
    fn a_feed_restarts_both_counts_and_a_stall_fires_within_one_period() {
        let hz = 24_000_000;
        let (a, b) = pair(hz, [Fault::None; 2]);
        let mut regs = Both(a, b);
        let armed = arm(&mut regs).unwrap();
        let mut dog = Dog::new(regs, armed);
        let period = period_us(TOP, hz);
        dog.regs.0.advance_us(period - 10);
        dog.feed();
        assert!(!dog.regs.0.fired());
        assert!(dog.regs.0.advance_us(period));
        assert!(dog.regs.1.fired() || dog.regs.1.advance_us(period));
    }

    #[test]
    fn a_fast_reset_fires_in_one_short_period() {
        let hz = 1_000_000;
        let (a, b) = pair(hz, [Fault::None; 2]);
        let mut regs = Both(a, b);
        let armed = arm(&mut regs).unwrap();
        let mut dog = Dog::new(regs, armed);
        dog.fast_reset();
        assert!(dog.regs.0.advance_us(period_us(FAST_TOP, hz)));
    }

    #[test]
    fn a_kick_with_another_value_does_nothing() {
        let mut model = Model::new(BASE0, 1_000_000, Fault::None);
        model.write32(BASE0 + TORR, torr(TOP));
        model.write32(BASE0 + CRR, KICK);
        model.write32(BASE0 + CR, CR_ENABLE);
        let before = model.remaining_cycles();
        model.write32(BASE0 + CRR, 0x75);
        assert_eq!(model.remaining_cycles(), before);
        assert_eq!(model.kicks(), 1);
    }

    /// The register sequence the `-sys` crate's entry runs must be the one this module makes. The
    /// entry is assembly, so this test reads its source and checks the constants it names.
    #[test]
    fn the_sys_entry_arms_both_with_this_sequence_and_reads_back() {
        let source = include_str!("../../../../drivers/k230-usbboot-sys/src/lib.rs");
        for needle in [
            "0x91106000",
            "0x91106800",
            "li t1, 0xbb",
            "li t1, 0x76",
            "li t1, 1",
            "lw t2, 0x08(t0)",
            "0x91102060",
            "rdtime",
        ] {
            assert!(source.contains(needle), "the -sys entry lacks `{needle}`");
        }
        let at = |needle: &str| source.find(needle).unwrap_or(usize::MAX);
        assert!(at("sw t1, 0x04(t0)") < at("sw t1, 0x0c(t0)"));
        assert!(at("sw t1, 0x0c(t0)") < at("sw t1, 0x00(t0)"));
    }
}
