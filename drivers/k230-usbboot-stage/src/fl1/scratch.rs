//! The card build's storage for the B2 card writes: the card, reachable only inside one scratch range.
//!
//! The range is the one chosen from the card's own layout (`testing/board-runs/K230-USB.md`, "B2 card
//! writes"): sectors [`SCRATCH_FIRST`] to [`SCRATCH_LAST`], far past the partition table, its backup and
//! the boot copies. A request that names any sector outside it, in part or whole, read or write, is refused
//! whole before the card is touched: the stage answers it as a storage failure (status 2), whatever the
//! request's compared mark says. The card storage under it ([`crate::fl1::storage::Sectors`]) holds the same
//! range as its declared one and the protected sectors besides ([`crate::bounds`]); this limit adds reads, so
//! the scratch build cannot read the card outside the range either.

use core::cell::Cell;

use crate::bounds::CARD_DECLARED;
use crate::fl1::REGIONS;
use crate::fl1::guard::Dogs;
use crate::fl1::run::Profile;
use crate::format::crc32;
use crate::wdt::{BASES, CRR, KICK, RegAccess};
use lemnos_flash_stage::guard::{Cause, Watchdog, start};
use lemnos_flash_stage::link::{IoError, Storage};
use lemnos_flash_stage::session::{self, SessionStop, Setup};
use lemnos_flash_stage::usb::{Controller, Ep0};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::{ResetKind, SECTOR};
use lemnos_flash_wire::identity::{IDENTITY_LEN, identity_record};

/// The scratch build's id ("K230" "B2", serial 2): neither the check build's
/// ([`crate::fl1::check::CHECK_BUILD_ID`], serial 1) nor the full card build's ([`crate::fl1::BUILD_ID`]), so a host
/// tells the three apart before it sends anything but an identify.
pub const SCRATCH_BUILD_ID: u64 = 0x4b32_3330_4232_0002;

/// The identity record of the scratch build: its own id, the K230's regions.
pub const SCRATCH_IDENTITY: [u8; IDENTITY_LEN] = identity_record(SCRATCH_BUILD_ID, &REGIONS);

/// The card of record's CID, as the SD host reads it (the R2 reply's 128 bits with the CRC byte, which the
/// controller does not keep, as 0): the card the survey read (`testing/board-runs/K230-USB.md`, "B2 card
/// writes"). The scratch build writes no other card.
pub const CARD_OF_RECORD_CID: [u8; 16] = [
    0x03, 0x53, 0x44, 0x53, 0x52, 0x36, 0x34, 0x47, 0x80, 0x14, 0xa7, 0xc9, 0xab, 0x01, 0x51, 0x00,
];

/// The card of record's size in 512-byte blocks.
pub const CARD_OF_RECORD_BLOCKS: u64 = 124_735_488;

/// Whether a card is the card of record: its CID (the CRC byte not compared, as the piece path compares it)
/// and its size.
pub fn card_of_record(cid: &[u8; 16], blocks: u64) -> bool {
    cid[..15] == CARD_OF_RECORD_CID[..15] && blocks == CARD_OF_RECORD_BLOCKS
}

/// The first sector of the scratch range (the 512 MiB mark): the card builds' declared range.
pub const SCRATCH_FIRST: u32 = CARD_DECLARED.first as u32;
/// The last sector of the scratch range, inclusive (2048 sectors, 1 MiB).
pub const SCRATCH_LAST: u32 = CARD_DECLARED.last as u32;

/// Whether `bytes` bytes at `lba` are whole sectors, at least one, all inside the scratch range.
pub fn inside(lba: u32, bytes: usize) -> bool {
    if bytes == 0 || !bytes.is_multiple_of(SECTOR) {
        return false;
    }
    let Ok(sectors) = u32::try_from(bytes / SECTOR) else {
        return false;
    };
    let Some(last) = lba.checked_add(sectors - 1) else {
        return false;
    };
    lba >= SCRATCH_FIRST && last <= SCRATCH_LAST
}

/// A storage reachable only inside the scratch range.
pub struct Scratch<S: Storage> {
    inner: S,
}

impl<S: Storage> Scratch<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// The storage behind the limit (for a test's look at the card).
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: Storage> Storage for Scratch<S> {
    fn write(&mut self, lba: u32, data: &[u8], budget_us: u64) -> Result<(), IoError> {
        if !inside(lba, data.len()) {
            return Err(IoError::Failed);
        }
        self.inner.write(lba, data, budget_us)
    }

    fn read(&mut self, lba: u32, out: &mut [u8], budget_us: u64) -> Result<(), IoError> {
        if !inside(lba, out.len()) {
            return Err(IoError::Failed);
        }
        self.inner.read(lba, out, budget_us)
    }
}

/// What the scratch build's storage did last, as the feed gate reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Last {
    /// Nothing the gate counts: a read that was not a write's read-back, a failed call, or a feed that used it.
    Nothing,
    /// A write the storage took whole: where, how long, and the CRC of what was written.
    Written { lba: u32, len: usize, crc: u32 },
    /// The read-back of that write: the same sectors, read whole, equal by CRC to what was written.
    ReadBackEqual,
}

/// The scratch build's record of verified progress: a write the storage took whole, then a read of the same
/// sectors that came back equal. [`VerifiedFeed`] feeds only on it, once.
pub struct Progress(Cell<Last>);

impl Progress {
    /// Nothing done yet.
    pub const fn new() -> Self {
        Self(Cell::new(Last::Nothing))
    }

    /// Whether the last storage calls were a write and its equal read-back; true once, then nothing again.
    fn take_verified(&self) -> bool {
        self.0.replace(Last::Nothing) == Last::ReadBackEqual
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

/// The storage, with each call noted in [`Progress`]: the gate's own evidence of a verified round. The shared
/// round writes, reads the same sectors back and compares them, then calls the feed; this wrapper sees the write and
/// the read-back itself and compares their CRCs, so the feed holds only where the round's own compare held. A read
/// round (the host reading) calls the same feed, but its read follows no write here, so it never feeds.
pub struct Watched<'p, S: Storage> {
    inner: S,
    progress: &'p Progress,
}

impl<'p, S: Storage> Watched<'p, S> {
    pub fn new(inner: S, progress: &'p Progress) -> Self {
        Self { inner, progress }
    }

    /// The storage under the record (for a test's look at it).
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: Storage> Storage for Watched<'_, S> {
    fn write(&mut self, lba: u32, data: &[u8], budget_us: u64) -> Result<(), IoError> {
        let result = self.inner.write(lba, data, budget_us);
        self.progress.0.set(match result {
            Ok(()) => Last::Written {
                lba,
                len: data.len(),
                crc: crc32(data),
            },
            Err(_) => Last::Nothing,
        });
        result
    }

    fn read(&mut self, lba: u32, out: &mut [u8], budget_us: u64) -> Result<(), IoError> {
        let result = self.inner.read(lba, out, budget_us);
        let was = self.progress.0.get();
        self.progress.0.set(match (result, was) {
            (Ok(()), Last::Written { lba: at, len, crc })
                if at == lba && len == out.len() && crc32(out) == crc =>
            {
                Last::ReadBackEqual
            }
            _ => Last::Nothing,
        });
        result
    }
}

/// Kicks both watchdogs. The scratch build's only feed: called from one place, [`VerifiedFeed`]'s gate.
/// `check.sh --b2-card` checks the image has exactly one call to it.
#[inline(never)]
pub fn kick<R: RegAccess>(regs: &mut R) {
    for base in BASES {
        regs.write32(base + CRR, KICK);
    }
}

/// The scratch build's watchdog during the session: the shared loop calls its feed after a write round's compare
/// held and after a read round's reply was sent; it kicks the dogs only when [`Progress`] shows a write and its equal
/// read-back since the last kick. Each kick is counted in `rounds`.
pub struct VerifiedFeed<'a, R: RegAccess> {
    regs: R,
    progress: &'a Progress,
    rounds: Option<&'a Cell<u32>>,
}

impl<R: RegAccess> Watchdog for VerifiedFeed<'_, R> {
    // Not inlined: the gate is one function in the image, and it holds the only call to `kick`.
    #[inline(never)]
    fn feed(&mut self) {
        if self.progress.take_verified() {
            if let Some(rounds) = self.rounds {
                rounds.set(rounds.get().saturating_add(1));
            }
            kick(&mut self.regs);
        }
    }
}

/// The scratch build's entry, as [`crate::fl1::run::stage_with`] with one difference: the session's watchdog is
/// [`VerifiedFeed`], so the dogs are kicked only after a verified write round. `bring_up` must give the storage
/// wrapped in [`Watched`] over `progress`; with any other storage nothing is ever fed, and the period ends the run.
/// `profile.feeds` is not read: the gate decides.
pub fn stage_scratch<C, S, K, R, B>(
    regs: &mut R,
    clock: &mut K,
    buf: &mut [u8],
    scratch: &mut [u8],
    profile: Profile<'_>,
    progress: &Progress,
    bring_up: B,
) -> Result<ResetKind, Cause>
where
    C: Controller,
    S: Storage,
    K: Clock,
    R: RegAccess,
    B: FnOnce(&mut R, &mut K) -> Result<(C, S), Cause>,
{
    let armed_at_us = clock.now_us();
    start(
        &mut Dogs::with_clock(&mut *regs, profile.clock_hz, profile.max_top),
        profile.budget_us,
    )
    .map_err(Cause::Start)?;
    let (mut ctrl, mut storage) = bring_up(regs, clock)?;
    let mut ep0 = Ep0::default();
    let mut guard = VerifiedFeed {
        regs: &mut *regs,
        progress,
        rounds: profile.rounds,
    };
    session::run(
        &mut ctrl,
        clock,
        &mut ep0,
        &mut storage,
        &mut guard,
        Setup {
            regions: &REGIONS,
            identity: profile.identity,
            buf,
            scratch,
            armed_at_us,
        },
    )
    .map_err(|stop| match stop {
        SessionStop::Enumerate(_) => Cause::Setup { step: 2 },
        SessionStop::Serve(error) => Cause::Serve(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fl1::check::CHECK_IDENTITY;
    use crate::fl1::{BUILD_ID, IDENTITY};
    use lemnos_flash_wire::identity;

    #[test]
    fn the_scratch_identity_is_neither_the_check_builds_nor_the_card_builds() {
        assert_ne!(SCRATCH_IDENTITY, CHECK_IDENTITY);
        assert_ne!(SCRATCH_IDENTITY, IDENTITY);
        assert_ne!(SCRATCH_BUILD_ID, BUILD_ID);
        let peer = identity::check(&SCRATCH_IDENTITY).unwrap();
        assert_eq!(peer.build_id, SCRATCH_BUILD_ID);
    }

    /// A RAM storage for the gate's tests, with one sector that reads back wrong when asked.
    struct Ram {
        bytes: std::vec::Vec<u8>,
        flip: Option<u32>,
    }

    impl Storage for Ram {
        fn write(&mut self, lba: u32, data: &[u8], _: u64) -> Result<(), IoError> {
            let at = lba as usize * SECTOR;
            self.bytes[at..at + data.len()].copy_from_slice(data);
            Ok(())
        }
        fn read(&mut self, lba: u32, out: &mut [u8], _: u64) -> Result<(), IoError> {
            let at = lba as usize * SECTOR;
            out.copy_from_slice(&self.bytes[at..at + out.len()]);
            if self.flip == Some(lba) {
                out[0] ^= 1;
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Bus(std::vec::Vec<(usize, u32)>);

    impl RegAccess for Bus {
        fn read32(&mut self, _: usize) -> u32 {
            0
        }
        fn write32(&mut self, addr: usize, value: u32) {
            self.0.push((addr, value));
        }
    }

    #[test]
    fn the_gate_kicks_once_per_write_and_equal_read_back_and_never_for_anything_else() {
        let progress = Progress::new();
        let rounds = Cell::new(0);
        let mut storage = Watched::new(
            Ram {
                bytes: std::vec![0; 64 * SECTOR],
                flip: Some(8),
            },
            &progress,
        );
        let mut feed = VerifiedFeed {
            regs: Bus::default(),
            progress: &progress,
            rounds: Some(&rounds),
        };
        let data = [0x5au8; 2 * SECTOR];
        let mut back = [0u8; 2 * SECTOR];
        // A write and its equal read-back: one kick of each dog, one round.
        storage.write(4, &data, 0).unwrap();
        storage.read(4, &mut back, 0).unwrap();
        feed.feed();
        assert_eq!(
            feed.regs.0,
            [(BASES[0] + CRR, KICK), (BASES[1] + CRR, KICK)]
        );
        // The same feed again with nothing new: nothing.
        feed.feed();
        // A read alone (a read round): nothing.
        storage.read(4, &mut back, 0).unwrap();
        feed.feed();
        // A write then a read of other sectors, or a shorter read: nothing.
        storage.write(4, &data, 0).unwrap();
        storage.read(5, &mut back, 0).unwrap();
        feed.feed();
        storage.write(4, &data, 0).unwrap();
        storage.read(4, &mut back[..SECTOR], 0).unwrap();
        feed.feed();
        // A read-back that differs: nothing.
        storage.write(8, &data, 0).unwrap();
        storage.read(8, &mut back, 0).unwrap();
        feed.feed();
        // A write, a read-back, then a read before the feed: the read-back is no longer the last call.
        storage.write(4, &data, 0).unwrap();
        storage.read(4, &mut back, 0).unwrap();
        storage.read(4, &mut back, 0).unwrap();
        feed.feed();
        assert_eq!(feed.regs.0.len(), 2, "{:?}", feed.regs.0);
        assert_eq!(rounds.get(), 1);
    }

    #[test]
    fn only_the_card_of_record_is_taken() {
        assert!(card_of_record(&CARD_OF_RECORD_CID, CARD_OF_RECORD_BLOCKS));
        let mut crc = CARD_OF_RECORD_CID;
        crc[15] = 0x5b;
        assert!(
            card_of_record(&crc, CARD_OF_RECORD_BLOCKS),
            "the CRC byte is not kept"
        );
        for byte in 0..15 {
            let mut other = CARD_OF_RECORD_CID;
            other[byte] ^= 1;
            assert!(
                !card_of_record(&other, CARD_OF_RECORD_BLOCKS),
                "byte {byte}"
            );
        }
        assert!(!card_of_record(
            &CARD_OF_RECORD_CID,
            CARD_OF_RECORD_BLOCKS - 1024
        ));
    }

    #[test]
    fn the_range_is_past_every_region_and_excludes_the_header_blocks() {
        let end = REGIONS.iter().map(|r| r.end).max().unwrap();
        assert!(SCRATCH_FIRST > end);
        for lba in [0, 1, 33, 2048, 3072, 415_711, 415_743] {
            assert!(!inside(lba, SECTOR), "{lba}");
        }
    }

    #[test]
    fn each_edge_is_refused_and_the_range_itself_is_not() {
        assert!(inside(SCRATCH_FIRST, SECTOR));
        assert!(inside(SCRATCH_LAST, SECTOR));
        assert!(inside(
            SCRATCH_FIRST,
            (SCRATCH_LAST - SCRATCH_FIRST + 1) as usize * SECTOR
        ));
        assert!(!inside(SCRATCH_FIRST - 1, SECTOR));
        assert!(!inside(SCRATCH_LAST + 1, SECTOR));
        assert!(
            !inside(SCRATCH_FIRST - 1, 2 * SECTOR),
            "straddles the first sector"
        );
        assert!(
            !inside(SCRATCH_LAST, 2 * SECTOR),
            "straddles the last sector"
        );
        assert!(!inside(SCRATCH_FIRST, 0));
        assert!(!inside(SCRATCH_FIRST, SECTOR + 1));
        assert!(!inside(u32::MAX, SECTOR));
        assert!(
            !inside(u32::MAX, 2 * SECTOR),
            "no wrap past the last sector number"
        );
    }
}
