//! The B2 check build: the FL1 stage's own USB device path on the board, with no card code at all.
//!
//! The board run that uses it is `testing/board-runs/K230-USB.md`, "B2". The build is the FL1 stage
//! ([`crate::fl1::run::stage_with`]) over the dwc2 device path, with these differences, each a rule of
//! the first run:
//!
//! - **No card.** The storage is [`Loopback`]: a RAM window of [`CHECK_SECTORS`] sectors at
//!   [`CHECK_LBA`], and one read-only record sector at [`RECORD_LBA`]. Any other sector is a storage
//!   failure (status 2), and a write to the record is too. The image built for this run has no SD host
//!   code linked in (`drivers/k230-usbboot-image/check.sh --b2` checks that), so no call reaches a card.
//! - **Its own identity.** [`CHECK_BUILD_ID`] differs from the card build's, so a host tells the two
//!   apart before it sends anything but an identify.
//! - **One period bounds the run.** The watchdogs are never fed ([`crate::fl1::run::Profile::feeds`] is
//!   false): the period chosen at the start, at the clock measured at the start ([`measure`]), covers
//!   [`RUN_BUDGET_US`], and the board is reset at most one period after the arming whatever the host does.
//!   The session's END, idle limit, or failure reports first and takes the SoC reset; the period is the
//!   backstop behind them.
//! - **A record.** What the bench must see (the speed, the SET_ADDRESS order, the bulk OUT re-arm after
//!   each frame, the watchdog clock) is kept in [`record`] words, printed on the console at the end and
//!   readable over USB at [`RECORD_LBA`].

use lemnos_flash_stage::link::{IoError, Storage};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::{MAX_SECTORS, SECTOR};
use lemnos_flash_wire::identity::{IDENTITY_LEN, identity_record};

use crate::fl1::REGIONS;
use crate::sysctl::STC_ENABLES;
use crate::wdt::{BASES, CCVR, RegAccess};

/// The check build's id ("K230" "B2", serial 1). Not the card build's [`crate::fl1::BUILD_ID`].
pub const CHECK_BUILD_ID: u64 = 0x4b32_3330_4232_0001;

/// The identity record of the check build: its own id, the K230's regions.
pub const CHECK_IDENTITY: [u8; IDENTITY_LEN] = identity_record(CHECK_BUILD_ID, &REGIONS);

/// The first sector of the RAM window. Past every region the stage polices (a plain sector), so a write
/// there needs no compared mark. It is a sector number only: nothing here is a card.
pub const CHECK_LBA: u32 = 1_048_576;

/// The window's length in sectors: one largest request.
pub const CHECK_SECTORS: u32 = MAX_SECTORS as u32;

/// The window's length in bytes.
pub const WINDOW_LEN: usize = CHECK_SECTORS as usize * SECTOR;

/// The record sector: read only, one sector, right after the window.
pub const RECORD_LBA: u32 = CHECK_LBA + CHECK_SECTORS;

/// The core's timebase, the `time` CSR's rate: 27 MHz (`timebase-frequency = <27000000>`, Linux 7.3.3
/// `arch/riscv/boot/dts/canaan/k230.dtsi:20`). UNVERIFIED in the boot ROM's state: the run measures it
/// against the console's stamps (the record's `ticks` words).
pub const TIMEBASE_HZ: u64 = 27_000_000;

/// What one run of the check build may take, from the arming: the stage's init budget, the idle limit
/// (a host that never asks), and the host's whole check after it. The period chosen covers it, so the
/// watchdog does not cut a healthy run short, and nothing after it is left running.
pub const RUN_BUDGET_US: u64 = 60_000_000;

/// The longest watchdog setting the check build may use (the block's last, `2^31` cycles).
pub const MAX_TOP: u8 = 15;

/// How long the watchdog clock is measured for, on the timebase.
pub const MEASURE_US: u64 = 20_000;

/// The reads the measurement makes at most while it waits on the timebase: a timebase that does not
/// move ends the measurement as a failure, not a hang.
pub const MEASURE_READS: u32 = 20_000_000;

/// The watchdog clocks the check build accepts. Outside this range the measurement is taken as wrong
/// and the stage resets without bringing USB up. The bench measured 750 kHz under U-Boot's clocks
/// (`K230-USB.md`, C1b).
pub const CLOCK_HZ_LOWEST: u64 = 100_000;
/// See [`CLOCK_HZ_LOWEST`].
pub const CLOCK_HZ_HIGHEST: u64 = 200_000_000;

/// The margin added to the measured clock before the periods are computed: a period is never taken as
/// longer than it is, so the chosen one covers the budget at a clock up to 5 % faster than measured.
pub const CLOCK_MARGIN_PERCENT: u64 = 5;

/// The watchdog clock, as measured against the timebase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// `wdt0`'s counter at the start and at the end (it counts down).
    pub first: u32,
    pub last: u32,
    /// The microseconds between the two reads, on the timebase.
    pub elapsed_us: u64,
    /// The clock: the counts over the time.
    pub hz: u64,
}

/// Why the measurement was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasureError {
    /// The timebase did not move [`MEASURE_US`] within [`MEASURE_READS`] reads.
    TimebaseStuck,
    /// The counter did not count down.
    CounterStuck,
    /// The clock is outside [`CLOCK_HZ_LOWEST`]..=[`CLOCK_HZ_HIGHEST`].
    OutOfRange(u64),
}

/// Starts the `time` CSR: the system timer's clock for both harts and its first counter, written as the
/// vendor's SPL writes them before it relies on `time` ([`STC_ENABLES`]). The boot ROM leaves `time` at 0 and
/// not moving (B2 run 1, `testing/board-runs/K230-USB.md`). Nothing is read back here: [`timebase_runs`] is the
/// check that it worked.
pub fn enable_timebase<R: RegAccess>(regs: &mut R) {
    for (addr, value) in STC_ENABLES {
        regs.write32(addr, value);
    }
}

/// The watchdog counts [`CROSS_COUNTS`] for the cross-check (10 ms at the 750 kHz C1b measured).
pub const CROSS_COUNTS: u32 = 7_500;

/// What the cross-check saw: the watchdog's counts and the microseconds `clock` gave for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cross {
    pub counts: u32,
    pub us: u64,
}

/// Whether `clock` (the `time` CSR) moves, judged by a clock that is known to run: `wdt0`'s counter, which
/// the entry armed and read back moving. Waits for the counter to count [`CROSS_COUNTS`] (at most
/// [`MEASURE_READS`] reads) and requires the timebase to have moved over them. `time` is trusted for nothing
/// before this holds.
pub fn timebase_runs<R: RegAccess, K: Clock>(
    regs: &mut R,
    clock: &mut K,
) -> Result<Cross, MeasureError> {
    let start = clock.now_us();
    let first = regs.read32(BASES[0] + CCVR);
    let mut reads = 0;
    loop {
        let now = regs.read32(BASES[0] + CCVR);
        let counts = first.saturating_sub(now);
        if counts >= CROSS_COUNTS {
            let us = clock.now_us().saturating_sub(start);
            if us == 0 {
                return Err(MeasureError::TimebaseStuck);
            }
            return Ok(Cross { counts, us });
        }
        reads += 1;
        if reads >= MEASURE_READS {
            return Err(MeasureError::CounterStuck);
        }
    }
}

/// Measures `wdt0`'s clock against `clock`. The entry armed `wdt0` before this runs, so the counter is
/// running; the wait is bounded by [`MEASURE_READS`] and by the armed watchdog behind it.
pub fn measure<R: RegAccess, K: Clock>(
    regs: &mut R,
    clock: &mut K,
) -> Result<Measured, MeasureError> {
    let start = clock.now_us();
    let first = regs.read32(BASES[0] + CCVR);
    let mut now = start;
    let mut reads = 0;
    while now.saturating_sub(start) < MEASURE_US {
        if reads >= MEASURE_READS {
            return Err(MeasureError::TimebaseStuck);
        }
        reads += 1;
        now = clock.now_us();
    }
    let last = regs.read32(BASES[0] + CCVR);
    let elapsed_us = now - start;
    if last >= first {
        return Err(MeasureError::CounterStuck);
    }
    let hz = u64::from(first - last) * 1_000_000 / elapsed_us.max(1);
    if !(CLOCK_HZ_LOWEST..=CLOCK_HZ_HIGHEST).contains(&hz) {
        return Err(MeasureError::OutOfRange(hz));
    }
    Ok(Measured {
        first,
        last,
        elapsed_us,
        hz,
    })
}

/// The clock the periods are computed at: the measured one with [`CLOCK_MARGIN_PERCENT`] added.
pub fn period_clock_hz(measured: &Measured) -> u64 {
    measured.hz + measured.hz * CLOCK_MARGIN_PERCENT / 100 + 1
}

/// The record's layout: 32-bit little-endian words at the start of the record sector, the same words
/// the console prints at the end (`b2 <name> <hex>`). A host checks [`record::MAGIC`] first.
pub mod record {
    /// The record's first word: the bytes `LKB2`.
    pub const MAGIC: u32 = 0x3242_4b4c;
    /// The words, by index, and their names on the console. Every count saturates.
    pub const NAMES: [&str; WORDS] = [
        "magic",
        "gsnpsid",
        "ghwcfg3",
        "gahbcfg-found",
        "reset-done",
        "dsts",
        "speed",
        "bus-resets",
        "setups",
        "set-address",
        "dcfg-after-address",
        "address-status-in",
        "ep0-status-out",
        "stalls",
        "configured",
        "out-frames",
        "out-rearms",
        "out-continued",
        "out-last-len",
        "out-longest",
        "out-oversize",
        "in-transfers",
        "in-bytes",
        "in-fifo-waits",
        "wdt-first",
        "wdt-last",
        "wdt-elapsed-us",
        "wdt-hz",
        "wdt-top",
        "rounds",
        "ticks-arm-lo",
        "ticks-now-lo",
        "tb-cross-counts",
        "tb-cross-us",
    ];
    /// How many words the record has.
    pub const WORDS: usize = 34;

    /// The index of a named word.
    pub const fn index(name: &str) -> usize {
        let mut i = 0;
        while i < WORDS {
            if eq(NAMES[i].as_bytes(), name.as_bytes()) {
                return i;
            }
            i += 1;
        }
        panic!("no such record word")
    }

    const fn eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The words of a record sector, if it starts with [`MAGIC`].
    pub fn parse(sector: &[u8]) -> Option<[u32; WORDS]> {
        if sector.len() < 4 * WORDS {
            return None;
        }
        let mut words = [0u32; WORDS];
        for (i, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                sector[4 * i],
                sector[4 * i + 1],
                sector[4 * i + 2],
                sector[4 * i + 3],
            ]);
        }
        (words[0] == MAGIC).then_some(words)
    }

    /// Writes `words` into a record sector (the rest of the sector is zero).
    pub fn write(words: &[u32; WORDS], sector: &mut [u8]) {
        sector.fill(0);
        for (i, word) in words.iter().enumerate() {
            sector[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
}

/// The check build's storage: a RAM window and a read-only record sector. No card.
pub struct Loopback<'a, F: FnMut(&mut [u8])> {
    window: &'a mut [u8],
    record: F,
}

impl<'a, F: FnMut(&mut [u8])> Loopback<'a, F> {
    /// The window (at least [`WINDOW_LEN`] bytes; only that much is used) and the function that fills
    /// the record sector when it is read.
    pub fn new(window: &'a mut [u8], record: F) -> Self {
        Self { window, record }
    }

    /// The byte range of `sectors` sectors at `lba` inside the window, if they are all inside it.
    fn range(&self, lba: u32, bytes: usize) -> Option<core::ops::Range<usize>> {
        if bytes == 0 || !bytes.is_multiple_of(SECTOR) || lba < CHECK_LBA {
            return None;
        }
        let at = (lba - CHECK_LBA) as usize * SECTOR;
        let end = at.checked_add(bytes)?;
        (end <= WINDOW_LEN.min(self.window.len())).then_some(at..end)
    }
}

impl<F: FnMut(&mut [u8])> Storage for Loopback<'_, F> {
    fn write(&mut self, lba: u32, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
        let range = self.range(lba, data.len()).ok_or(IoError::Failed)?;
        self.window[range].copy_from_slice(data);
        Ok(())
    }

    fn read(&mut self, lba: u32, out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
        if lba == RECORD_LBA && out.len() == SECTOR {
            (self.record)(out);
            return Ok(());
        }
        let range = self.range(lba, out.len()).ok_or(IoError::Failed)?;
        out.copy_from_slice(&self.window[range]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wdt::{Fault, Model};
    use std::cell::Cell;
    use std::rc::Rc;

    struct Fake(u64, u64);

    impl Clock for Fake {
        fn now_us(&mut self) -> u64 {
            self.0 += self.1;
            self.0
        }
    }

    /// A counter that runs at `hz` on a shared time, and the clock that moves that time.
    struct Timed {
        now: Rc<Cell<u64>>,
        hz: u64,
    }

    impl RegAccess for Timed {
        fn read32(&mut self, addr: usize) -> u32 {
            assert_eq!(addr, BASES[0] + CCVR);
            (0xFFFF_FFFF - self.now.get() * self.hz / 1_000_000) as u32
        }
        fn write32(&mut self, _addr: usize, _value: u32) {}
    }

    struct Shared(Rc<Cell<u64>>);

    impl Clock for Shared {
        fn now_us(&mut self) -> u64 {
            self.0.set(self.0.get() + 7);
            self.0.get()
        }
    }

    #[test]
    fn the_window_takes_writes_inside_it_and_refuses_every_other_sector() {
        let mut window = vec![0u8; WINDOW_LEN];
        let mut store = Loopback::new(&mut window, |s: &mut [u8]| s.fill(0xAB));
        let data = vec![0x5A; 2 * SECTOR];
        store.write(CHECK_LBA + 3, &data, 0).unwrap();
        let mut back = vec![0u8; 2 * SECTOR];
        store.read(CHECK_LBA + 3, &mut back, 0).unwrap();
        assert_eq!(back, data);
        for lba in [0, 2048, 3072, CHECK_LBA - 1, RECORD_LBA, u32::MAX] {
            assert_eq!(
                store.write(lba, &[1; SECTOR], 0),
                Err(IoError::Failed),
                "sector {lba} is not the window"
            );
        }
        assert_eq!(
            store.write(CHECK_LBA + CHECK_SECTORS - 1, &[1; 2 * SECTOR], 0),
            Err(IoError::Failed),
            "a write that runs past the window is refused whole"
        );
        let mut one = vec![0u8; SECTOR];
        store.read(RECORD_LBA, &mut one, 0).unwrap();
        assert!(
            one.iter().all(|&b| b == 0xAB),
            "the record is read from its function"
        );
        assert_eq!(store.read(0, &mut one, 0), Err(IoError::Failed));
    }

    #[test]
    fn the_clock_is_measured_from_the_counter_and_a_stuck_counter_or_timebase_is_refused() {
        let now = Rc::new(Cell::new(0));
        let mut regs = Timed {
            now: now.clone(),
            hz: 750_000,
        };
        let m = measure(&mut regs, &mut Shared(now)).expect("a running counter is measured");
        assert!(m.elapsed_us >= MEASURE_US);
        assert!((749_000..=751_000).contains(&m.hz), "{m:?}");
        let mut stuck = Model::new(BASES[0], 750_000, Fault::CounterStuck);
        crate::wdt::arm(&mut stuck).ok();
        assert_eq!(
            measure(&mut stuck, &mut Fake(0, 1)),
            Err(MeasureError::CounterStuck)
        );
        let mut regs = Model::new(BASES[0], 750_000, Fault::None);
        assert_eq!(
            measure(&mut regs, &mut Fake(0, 0)),
            Err(MeasureError::TimebaseStuck)
        );
        let now = Rc::new(Cell::new(0));
        let mut slow = Timed {
            now: now.clone(),
            hz: 10_000,
        };
        assert!(matches!(
            measure(&mut slow, &mut Shared(now)),
            Err(MeasureError::OutOfRange(_))
        ));
    }

    struct Stopped;

    impl Clock for Stopped {
        fn now_us(&mut self) -> u64 {
            0
        }
    }

    struct Writes(Vec<(usize, u32)>);

    impl RegAccess for Writes {
        fn read32(&mut self, _addr: usize) -> u32 {
            0
        }
        fn write32(&mut self, addr: usize, value: u32) {
            self.0.push((addr, value));
        }
    }

    #[test]
    fn the_timebase_enable_writes_the_three_vendor_values_in_their_order() {
        let mut bus = Writes(Vec::new());
        enable_timebase(&mut bus);
        assert_eq!(
            bus.0,
            [(0x9110_8020, 1), (0x9110_8030, 1), (0x9110_8000, 0x69)]
        );
    }

    #[test]
    fn the_timebase_is_trusted_only_when_it_moves_against_the_watchdog_counter() {
        /// A 750 kHz counter that runs off its own clock (each read moves it on 7 us of it).
        struct Counter(Shared);
        impl RegAccess for Counter {
            fn read32(&mut self, addr: usize) -> u32 {
                assert_eq!(addr, BASES[0] + CCVR);
                (0xFFFF_FFFF - self.0.now_us() * 750_000 / 1_000_000) as u32
            }
            fn write32(&mut self, _addr: usize, _value: u32) {}
        }

        // `time` on the same clock as the counter: trusted, about 10 ms for the counts.
        let now = Rc::new(Cell::new(0));
        let mut counter = Counter(Shared(now.clone()));
        let cross = timebase_runs(&mut counter, &mut Shared(now)).unwrap();
        assert!(cross.counts >= CROSS_COUNTS);
        assert!((9_900..=10_200).contains(&cross.us), "{cross:?}");

        // `time` reading 0 and never moving, as the boot ROM leaves it (B2 run 1): refused.
        let mut counter = Counter(Shared(Rc::new(Cell::new(0))));
        assert_eq!(
            timebase_runs(&mut counter, &mut Stopped),
            Err(MeasureError::TimebaseStuck)
        );

        // A counter that never counts: refused, bounded by the reads.
        let mut frozen = Writes(Vec::new());
        let mut clock = Fake(0, 7);
        assert_eq!(
            timebase_runs(&mut frozen, &mut clock),
            Err(MeasureError::CounterStuck)
        );
    }

    #[test]
    fn the_period_clock_rounds_up() {
        let m = Measured {
            first: 0,
            last: 0,
            elapsed_us: 1,
            hz: 750_000,
        };
        assert!(period_clock_hz(&m) > 787_000);
    }

    #[test]
    fn the_record_round_trips_and_its_names_are_unique() {
        let mut words = [0u32; record::WORDS];
        words[0] = record::MAGIC;
        words[record::index("out-rearms")] = 7;
        let mut sector = [0u8; SECTOR];
        record::write(&words, &mut sector);
        assert_eq!(record::parse(&sector), Some(words));
        sector[0] ^= 1;
        assert_eq!(record::parse(&sector), None);
        for (i, a) in record::NAMES.iter().enumerate() {
            assert_eq!(record::index(a), i);
        }
    }

    #[test]
    fn the_check_build_is_not_the_card_build() {
        assert_ne!(CHECK_BUILD_ID, crate::fl1::BUILD_ID);
        let peer = lemnos_flash_wire::identity::check(&CHECK_IDENTITY).expect("the record checks");
        assert_eq!(peer.regions(), &REGIONS[..]);
        assert!(RECORD_LBA > CHECK_LBA && CHECK_LBA > REGIONS.iter().map(|r| r.end).max().unwrap());
    }
}
