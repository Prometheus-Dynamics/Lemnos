//! Controller bring-up for the 472 loader: DWC3 in device mode and dw_mmc reset, written against a
//! register-access trait. The -sys crate implements the trait with volatile MMIO; the host tests use
//! a fake that clears self-clearing bits when they are read, as the controllers do.
//!
//! Every wait is bounded (see [`crate::wait`]). A reset bit that never clears is reported as a
//! timeout, and the caller resets the board.
//!
//! The base addresses are not in this file: they come from the board's device tree at bench time
//! ("to check on bench"). The register offsets and bits are from the local kernel tree (see
//! [`crate::regs`]).

use crate::regs::{dw_mmc, dwc3};
use crate::wdt::LONGEST_PERIOD_US;
use crate::wdt::{
    BASE as WDT_BASE, CR_ENABLE, CR_OFFSET, CRR_OFFSET, KICK, MAX_TOP, TORR_OFFSET, torr_for_top,
};
use lemnos_flash_stage::wait::{Clock, WaitError, wait_for};

/// The watchdog's base (rk356x-base.dtsi:1377, `wdt: watchdog@fe600000`).
pub const WATCHDOG: usize = WDT_BASE;
/// The DWC3 block the device mode runs on (rk356x-base.dtsi:265, `usb@fcc00000`). The board's UDC is
/// `fcc00000.usb` (window-200053.txt). The device mode's own register base is the same block.
pub const DWC3: usize = 0xfcc0_0000;
/// The SD host, sdmmc0 (rk356x-base.dtsi:1063, `mmc@fe2b0000`). That the SD card is on sdmmc0 is NOT
/// read from the running system: the live DT was not read. TO CHECK ON BENCH.
pub const SDMMC0: usize = 0xfe2b_0000;

/// Arms the watchdog at the longest period: the same three writes as the 471 stub, in its order.
pub fn arm_watchdog(regs: &mut dyn RegAccess) {
    regs.write32(WATCHDOG + TORR_OFFSET, torr_for_top(MAX_TOP));
    regs.write32(WATCHDOG + CRR_OFFSET, KICK);
    regs.write32(WATCHDOG + CR_OFFSET, CR_ENABLE);
}

/// Lowers the watchdog to `top` for the session, restarting the counter with it.
pub fn set_session_top(regs: &mut dyn RegAccess, top: u8) {
    regs.write32(WATCHDOG + TORR_OFFSET, torr_for_top(top));
    regs.write32(WATCHDOG + CRR_OFFSET, KICK);
}

/// Feeds the watchdog: restarts its counter. Called only after a verified chunk.
pub fn feed(regs: &mut dyn RegAccess) {
    regs.write32(WATCHDOG + CRR_OFFSET, KICK);
}

/// The board's watchdog as the shared stage sees it: arming, the read-back, the period and the feed,
/// through the same register writes as [`arm_watchdog`], [`set_session_top`] and [`feed`].
pub struct Dog<'r> {
    pub regs: &'r mut dyn RegAccess,
}

impl lemnos_flash_stage::guard::Watchdog for Dog<'_> {
    fn feed(&mut self) {
        feed(self.regs);
    }
}

impl lemnos_flash_stage::guard::Guard for Dog<'_> {
    fn arm(&mut self) {
        arm_watchdog(self.regs);
    }

    fn armed(&mut self) -> bool {
        self.regs.read32(WATCHDOG + CR_OFFSET) & CR_ENABLE != 0
    }

    fn max_top(&self) -> u8 {
        MAX_TOP
    }

    fn period_us(&self, top: u8) -> u64 {
        crate::wdt::period_us(top)
    }

    fn set_top(&mut self, top: u8) {
        set_session_top(self.regs, top);
    }
}

/// Register access, implemented by the -sys crate for the board and by the tests for the sim.
pub trait RegAccess {
    fn read32(&mut self, addr: usize) -> u32;
    fn write32(&mut self, addr: usize, value: u32);
}

/// The budget of one bounded register wait, in microseconds. Well below the watchdog's period.
pub const REG_WAIT_US: u64 = 1_000_000;

/// Waits until the bits in `mask` at `addr` are clear, or the budget runs out.
fn wait_clear<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    addr: usize,
    mask: u32,
) -> Result<(), WaitError> {
    wait_for(clock, REG_WAIT_US, LONGEST_PERIOD_US, || {
        (regs.read32(addr) & mask == 0).then_some(())
    })
}

/// Brings a DWC3 controller up in device mode: a core soft reset, a device soft reset, then RUN_STOP.
/// `base` is the controller's register base.
pub fn dwc3_start_device<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
) -> Result<(), WaitError> {
    let gctl = base + dwc3::GCTL;
    let dctl = base + dwc3::DCTL;
    let value = regs.read32(gctl);
    regs.write32(gctl, value | dwc3::GCTL_CORESOFTRESET);
    wait_clear(regs, clock, gctl, dwc3::GCTL_CORESOFTRESET)?;

    let value = regs.read32(dctl);
    regs.write32(dctl, value | dwc3::DCTL_CSFTRST);
    wait_clear(regs, clock, dctl, dwc3::DCTL_CSFTRST)?;

    let value = regs.read32(dctl);
    regs.write32(dctl, value | dwc3::DCTL_RUN_STOP);
    Ok(())
}

/// Resets a dw_mmc host: the controller reset, then the FIFO reset, each waited for. `base` is the
/// host's register base.
pub fn dw_mmc_reset<C: Clock>(
    regs: &mut dyn RegAccess,
    clock: &mut C,
    base: usize,
) -> Result<(), WaitError> {
    let ctrl = base + dw_mmc::CTRL;
    let value = regs.read32(ctrl);
    regs.write32(ctrl, value | dw_mmc::CTRL_RESET);
    wait_clear(regs, clock, ctrl, dw_mmc::CTRL_RESET)?;

    let value = regs.read32(ctrl);
    regs.write32(ctrl, value | dw_mmc::CTRL_FIFO_RESET);
    wait_clear(regs, clock, ctrl, dw_mmc::CTRL_FIFO_RESET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn the_watchdog_arm_is_the_stub_sequence_and_the_feed_is_the_kick() {
        let mut regs = Regs::default();
        arm_watchdog(&mut regs);
        assert_eq!(
            regs.writes,
            vec![
                (WATCHDOG + TORR_OFFSET, 0xFF),
                (WATCHDOG + CRR_OFFSET, KICK),
                (WATCHDOG + CR_OFFSET, CR_ENABLE),
            ],
            "the same order as stage471.S"
        );
        regs.writes.clear();
        feed(&mut regs);
        assert_eq!(regs.writes, vec![(WATCHDOG + CRR_OFFSET, KICK)]);
    }

    #[test]
    fn the_session_period_is_written_before_its_kick() {
        let mut regs = Regs::default();
        set_session_top(&mut regs, 12);
        assert_eq!(regs.writes[0], (WATCHDOG + TORR_OFFSET, torr_for_top(12)));
        assert_eq!(regs.writes[1], (WATCHDOG + CRR_OFFSET, KICK));
    }

    /// A register file that clears the self-clearing bits on read, the way the controllers do; a
    /// `stuck` bit never clears.
    #[derive(Default)]
    struct Regs {
        values: HashMap<usize, u32>,
        self_clearing: Vec<(usize, u32)>,
        stuck: Vec<(usize, u32)>,
        writes: Vec<(usize, u32)>,
    }

    impl RegAccess for Regs {
        fn read32(&mut self, addr: usize) -> u32 {
            let mut value = *self.values.get(&addr).unwrap_or(&0);
            for &(at, bit) in &self.self_clearing {
                if at == addr {
                    value &= !bit;
                }
            }
            self.values.insert(addr, value);
            value
                | self
                    .stuck
                    .iter()
                    .filter(|(at, _)| *at == addr)
                    .fold(0, |acc, (_, bit)| acc | bit)
        }

        fn write32(&mut self, addr: usize, value: u32) {
            self.writes.push((addr, value));
            self.values.insert(addr, value);
        }
    }

    /// A clock that moves 10 ms per read.
    struct Ticking(u64);

    impl Clock for Ticking {
        fn now_us(&mut self) -> u64 {
            self.0 += 10_000;
            self.0
        }
    }

    const BASE: usize = 0x1000_0000;

    #[test]
    fn dwc3_brings_the_device_up_in_order_with_its_resets_self_clearing() {
        let mut regs = Regs {
            self_clearing: vec![
                (BASE + dwc3::GCTL, dwc3::GCTL_CORESOFTRESET),
                (BASE + dwc3::DCTL, dwc3::DCTL_CSFTRST),
            ],
            ..Regs::default()
        };
        let mut clock = Ticking(0);
        dwc3_start_device(&mut regs, &mut clock, BASE).unwrap();
        assert_eq!(
            regs.values[&(BASE + dwc3::DCTL)] & dwc3::DCTL_RUN_STOP,
            dwc3::DCTL_RUN_STOP
        );
        let order: Vec<usize> = regs.writes.iter().map(|(addr, _)| *addr).collect();
        assert_eq!(
            order,
            vec![BASE + dwc3::GCTL, BASE + dwc3::DCTL, BASE + dwc3::DCTL]
        );
    }

    #[test]
    fn a_dwc3_reset_that_never_clears_times_out_and_leaves_run_stop_unset() {
        let mut regs = Regs {
            stuck: vec![(BASE + dwc3::GCTL, dwc3::GCTL_CORESOFTRESET)],
            ..Regs::default()
        };
        let mut clock = Ticking(0);
        let result = dwc3_start_device(&mut regs, &mut clock, BASE);
        assert!(
            matches!(result, Err(WaitError::Timeout { .. })),
            "{result:?}"
        );
        assert!(
            regs.writes
                .iter()
                .all(|(addr, _)| *addr != BASE + dwc3::DCTL),
            "no device step after a stuck reset"
        );
    }

    #[test]
    fn dw_mmc_resets_the_controller_then_the_fifo() {
        let mut regs = Regs {
            self_clearing: vec![(
                BASE + dw_mmc::CTRL,
                dw_mmc::CTRL_RESET | dw_mmc::CTRL_FIFO_RESET,
            )],
            ..Regs::default()
        };
        let mut clock = Ticking(0);
        dw_mmc_reset(&mut regs, &mut clock, BASE).unwrap();
        let writes: Vec<u32> = regs.writes.iter().map(|(_, value)| *value).collect();
        assert_eq!(writes[0] & dw_mmc::CTRL_RESET, dw_mmc::CTRL_RESET);
        assert_eq!(writes[1] & dw_mmc::CTRL_FIFO_RESET, dw_mmc::CTRL_FIFO_RESET);
    }

    #[test]
    fn a_dw_mmc_reset_that_never_clears_is_a_bounded_timeout() {
        let mut regs = Regs {
            stuck: vec![(BASE + dw_mmc::CTRL, dw_mmc::CTRL_RESET)],
            ..Regs::default()
        };
        let mut clock = Ticking(0);
        assert!(matches!(
            dw_mmc_reset(&mut regs, &mut clock, BASE),
            Err(WaitError::Timeout { .. })
        ));
    }
}
