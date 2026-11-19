//! The B2 check run: the FL1 stage's own USB device path with a RAM window and no card code
//! (`lemnos_k230_usbboot_stage::fl1::check`). The image's entry calls [`check`] then [`end`] with the
//! board's buses; the PC rehearsal (`testing/crates/k230-usbboot`, `b2sim`) calls the same two over a
//! register model of the controller, the watchdogs and the console. Nothing here is unsafe and nothing
//! here names an address: the buses do.
//!
//! The order, after the entry armed both watchdogs (before any of this runs):
//!
//! 1. the banner on the console;
//! 2. the timebase started ([`check::enable_timebase`]: the boot ROM leaves `time` stopped) and checked
//!    to move against the watchdog's counter ([`check::timebase_runs`]); then the watchdog clock measured
//!    against it ([`check::measure`]). Either failing ends the run before USB is touched;
//! 3. the shared entry: arm at the longest setting, read back, lower to the smallest period covering
//!    [`RUN_BUDGET_US`] at the measured clock ([`stage_with`], never fed);
//! 4. the controller's core reset ([`Dwc2`]), the FL1 device, the [`Loopback`] storage;
//! 5. the session, to its END, its idle limit or a failure;
//! 6. [`end`]: the record on the console, the cause, the SoC reset.

use core::cell::Cell;

use crate::dwc2::{Dwc2, Dwc2Regs, Stats};
use lemnos_flash_stage::guard::{Cause, finish};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::MAX_CHUNK;
use lemnos_k230_usbboot_stage::fl1::check::{
    self, CHECK_IDENTITY, Cross, Loopback, MAX_TOP, Measured, RUN_BUDGET_US, record,
};
use lemnos_k230_usbboot_stage::fl1::report::Uart;
use lemnos_k230_usbboot_stage::fl1::run::{Profile, stage_with};
use lemnos_k230_usbboot_stage::fl1::usb::Device;
use lemnos_k230_usbboot_stage::uart;
use lemnos_k230_usbboot_stage::usb_dev::UsbHw;
use lemnos_k230_usbboot_stage::wdt::{RegAccess, period_us};

/// The bulk OUT buffer: the largest frame and one byte more, in whole packets, so a frame one byte
/// too long is taken whole and refused by its length (status 5), never cut to fit.
pub const RX_LEN: usize = (MAX_CHUNK + 1).div_ceil(512) * 512;

/// The banner, the first line after the entry's own.
pub const BANNER: &str = "lemnos stage: check (B2: the USB device path, a RAM window, no card code; watchdog armed before this line)";

/// The last line before the cause.
pub const END_LINE: &str = "lemnos stage: end";

/// The line printed when the watchdog clock cannot be measured.
pub const NO_CLOCK_LINE: &str = "b2 the watchdog clock could not be measured; no USB bring-up";

/// The line printed when the timebase does not move against the watchdog's counter after its enable.
pub const TIMEBASE_STOPPED_LINE: &str =
    "b2 the timebase does not move against the watchdog counter after its enable; no USB bring-up";

/// What the run knows besides the driver's counts.
pub struct Known {
    /// The timebase cross-check against the watchdog's counter, once it passed.
    pub cross: Option<Cross>,
    /// The watchdog clock, once measured.
    pub measured: Option<Measured>,
    /// The setting chosen (0xff: none covers the budget; the shared entry then refuses).
    pub top: u32,
    /// The verified rounds (feeds the shared rule asks for; this build makes none).
    pub rounds: Cell<u32>,
    /// The timebase at the start of the check.
    pub arm_ticks: u64,
}

impl Known {
    /// Nothing measured yet, from `arm_ticks`.
    pub const fn new(arm_ticks: u64) -> Self {
        Self {
            cross: None,
            measured: None,
            top: 0,
            rounds: Cell::new(0),
            arm_ticks,
        }
    }
}

/// The record's words, now.
pub fn words(stats: &Stats, known: &Known, now_ticks: u64) -> [u32; record::WORDS] {
    let mut words = [0u32; record::WORDS];
    words[0] = record::MAGIC;
    for (name, value) in stats.words() {
        words[record::index(name)] = value;
    }
    if let Some(c) = known.cross {
        words[record::index("tb-cross-counts")] = c.counts;
        words[record::index("tb-cross-us")] = c.us as u32;
    }
    if let Some(m) = known.measured {
        words[record::index("wdt-first")] = m.first;
        words[record::index("wdt-last")] = m.last;
        words[record::index("wdt-elapsed-us")] = m.elapsed_us as u32;
        words[record::index("wdt-hz")] = m.hz as u32;
    }
    words[record::index("wdt-top")] = known.top;
    words[record::index("rounds")] = known.rounds.get();
    words[record::index("ticks-arm-lo")] = known.arm_ticks as u32;
    words[record::index("ticks-now-lo")] = now_ticks as u32;
    words
}

/// The check's buffers, each borrowed for the whole run.
pub struct Buffers<'a> {
    /// The session's frame buffer (`serve::BUFFER_LEN`).
    pub buf: &'a mut [u8],
    /// The session's read-back scratch (`serve::BUFFER_LEN`).
    pub scratch: &'a mut [u8],
    /// The bulk OUT buffer ([`RX_LEN`]).
    pub rx: &'a mut [u8],
    /// The RAM window (`check::WINDOW_LEN`).
    pub window: &'a mut [u8],
}

/// The console: one bounded wait at most, then nothing more is tried (a dead UART costs one wait).
struct Console {
    dead: bool,
}

impl Console {
    fn line<R: RegAccess>(&mut self, regs: &mut R, parts: &[&str]) {
        if !self.dead {
            self.dead = uart::write_line_parts(regs, parts).is_err();
        }
    }
}

/// Hex digits as text (they are always ASCII).
fn text(digits: &[u8]) -> &str {
    core::str::from_utf8(digits).unwrap_or("????????")
}

/// Eight hex digits, no formatting machinery.
fn hex8(value: u32) -> [u8; 8] {
    let mut out = [b'0'; 8];
    for (i, digit) in out.iter_mut().enumerate() {
        let nibble = (value >> (28 - 4 * i)) & 0xf;
        *digit = b"0123456789abcdef"[nibble as usize];
    }
    out
}

/// Runs the check (steps 1 to 5) and returns why it stopped. It never resets: [`end`] does.
pub fn check<R, D, K, T>(
    regs: &mut R,
    dwc2: D,
    clock: &mut K,
    ticks: &T,
    bufs: Buffers<'_>,
    stats: &Stats,
    known: &mut Known,
) -> Cause
where
    R: RegAccess,
    D: Dwc2Regs,
    K: Clock + Copy,
    T: Fn() -> u64,
{
    let mut console = Console { dead: false };
    uart::clear_programmable_thre(regs);
    uart::configure_if_unset(regs);
    console.line(regs, &[BANNER]);
    check::enable_timebase(regs);
    match check::timebase_runs(regs, clock) {
        Ok(cross) => known.cross = Some(cross),
        Err(_) => {
            console.line(regs, &[TIMEBASE_STOPPED_LINE]);
            return Cause::Setup { step: 5 };
        }
    }
    let measured = match check::measure(regs, clock) {
        Ok(measured) => measured,
        Err(_) => {
            console.line(regs, &[NO_CLOCK_LINE]);
            return Cause::Setup { step: 4 };
        }
    };
    known.measured = Some(measured);
    let clock_hz = check::period_clock_hz(&measured);
    known.top = (0..=MAX_TOP)
        .find(|&top| period_us(top, clock_hz) >= RUN_BUDGET_US)
        .map_or(0xff, u32::from);
    let known: &Known = known;
    let profile = Profile {
        identity: &CHECK_IDENTITY,
        budget_us: RUN_BUDGET_US,
        clock_hz,
        max_top: MAX_TOP,
        feeds: false,
        rounds: Some(&known.rounds),
    };
    let Buffers {
        buf,
        scratch,
        rx,
        window,
    } = bufs;
    let outcome = stage_with(regs, clock, buf, scratch, profile, |_, clock| {
        let mut hw = Dwc2::with(dwc2, rx, stats);
        hw.core_reset().map_err(|_| Cause::Setup { step: 1 })?;
        let device = Device::new(hw, *clock);
        let storage = Loopback::new(window, move |sector: &mut [u8]| {
            record::write(&words(stats, known, ticks()), sector)
        });
        Ok((device, storage))
    });
    match outcome {
        Ok(kind) => Cause::Ended(kind),
        Err(cause) => cause,
    }
}

/// Step 6: the record as `b2 <name> <hex>` lines, [`END_LINE`], then the cause and the SoC reset
/// (`fl1::report::Uart`). Does not return.
pub fn end<R: RegAccess>(
    regs: &mut R,
    stats: &Stats,
    known: &Known,
    now_ticks: u64,
    cause: Cause,
) -> ! {
    let mut console = Console { dead: false };
    for (name, value) in record::NAMES.iter().zip(words(stats, known, now_ticks)) {
        let digits = hex8(value);
        let digits = core::str::from_utf8(&digits).unwrap_or("????????");
        console.line(regs, &["b2 ", name, " ", digits]);
    }
    if let Some(trace) = stats.trace {
        let kept = (trace.count() as usize).min(crate::dwc2::TRACE_LEN);
        let count = hex8(trace.count());
        console.line(
            regs,
            &[
                "b2 ep0-trace ",
                core::str::from_utf8(&count).unwrap_or("????????"),
                " entries",
            ],
        );
        for i in 0..kept {
            if let Some(entry) = trace.entry(i) {
                let n = hex8(i as u32);
                let [a, b, c, d] = entry.map(hex8);
                console.line(
                    regs,
                    &[
                        "b2 t ",
                        text(&n[5..]),
                        " ",
                        text(&a),
                        " ",
                        text(&b),
                        " ",
                        text(&c),
                        " ",
                        text(&d),
                    ],
                );
            }
        }
    }
    console.line(regs, &[END_LINE]);
    finish(&mut Uart::new(regs), cause)
}
