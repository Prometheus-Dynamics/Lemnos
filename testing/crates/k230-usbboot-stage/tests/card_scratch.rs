//! The B2 card writes' model (`testing/board-runs/K230-USB.md`, "B2 card writes"): a card that holds the
//! scratch range and the regions the run must never change, served by the stage's real session step
//! (`lemnos_flash_stage::serve::step`) over the real card adapter (`fl1::storage::Sectors`).
//!
//! The scratch stage (`fl1::scratch::Scratch` over the bounded card storage) is run against it: every write and
//! read outside the range is refused whole, marked or not, and the card is not touched; writes inside it are
//! written and read back equal. What the card stages did before CARD-STAGE-BOUNDS, and its fail-before, is in
//! `tests/card_bounds.rs`.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use lemnos_flash_stage::guard::Watchdog;
use lemnos_flash_stage::link::{IoError, Link, Storage};
use lemnos_flash_stage::serve::{BUFFER_LEN, Context, Progress, step};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::{
    MAX_SECTORS, Request, SECTOR, STATUS_NOT_COMPARED, STATUS_OK, STATUS_STORAGE, payload_crc,
};
use lemnos_k230_usbboot_stage::bounds::CARD_DECLARED;
use lemnos_k230_usbboot_stage::fl1::scratch::{SCRATCH_FIRST, SCRATCH_LAST, Scratch};
use lemnos_k230_usbboot_stage::fl1::storage::{Blocks, Sectors};
use lemnos_k230_usbboot_stage::fl1::{IDENTITY, REGIONS};
use lemnos_k230_usbboot_stage::format::crc32;
use lemnos_k230_usbboot_stage::sd::BLOCK;

/// The card's size of record (124,735,488 sectors).
const CARD_BLOCKS: u64 = 124_735_488;

/// The regions the run must leave as they are, from the card's own layout (the survey of 2026-11-18): the primary
/// GPT, the boot area with both SPL copies and U-Boot, the backup GPT, and the card's last 33 sectors.
const PROTECTED: [(u64, u64); 4] = [
    (0, 34),
    (34, 8192),
    (415_711, 415_744),
    (CARD_BLOCKS - 33, CARD_BLOCKS),
];

/// A card in memory: every sector ever written is kept, and every block write is logged.
#[derive(Clone, Default)]
struct ModelCard(Rc<RefCell<CardState>>);

#[derive(Default)]
struct CardState {
    sectors: BTreeMap<u64, [u8; BLOCK]>,
    writes: Vec<u64>,
}

impl ModelCard {
    /// A card whose protected regions hold a pattern of their own (each sector its number), the rest zero.
    fn seeded() -> Self {
        let card = Self::default();
        {
            let mut s = card.0.borrow_mut();
            for (first, end) in PROTECTED {
                for lba in first..end {
                    s.sectors.insert(lba, pattern(lba, 0x5a));
                }
            }
            s.sectors.insert(1, gpt_header());
        }
        card
    }

    fn sector(&self, lba: u64) -> [u8; BLOCK] {
        self.0
            .borrow()
            .sectors
            .get(&lba)
            .copied()
            .unwrap_or([0; BLOCK])
    }

    fn writes(&self) -> Vec<u64> {
        self.0.borrow().writes.clone()
    }

    fn protected_unchanged(&self) -> bool {
        PROTECTED.iter().all(|&(first, end)| {
            (first..end).all(|lba| {
                let want = if lba == 1 {
                    gpt_header()
                } else {
                    pattern(lba, 0x5a)
                };
                self.sector(lba) == want
            })
        })
    }
}

impl Blocks for ModelCard {
    fn blocks(&self) -> u64 {
        CARD_BLOCKS
    }

    fn read_block(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), IoError> {
        if lba >= CARD_BLOCKS {
            return Err(IoError::Failed);
        }
        *out = self.sector(lba);
        Ok(())
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), IoError> {
        if lba >= CARD_BLOCKS {
            return Err(IoError::Failed);
        }
        let mut s = self.0.borrow_mut();
        s.writes.push(lba);
        s.sectors.insert(lba, *data);
        Ok(())
    }
}

/// The image's primary GPT header: the backup at 415,711..415,743.
fn gpt_header() -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    h[..8].copy_from_slice(b"EFI PART");
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&1u64.to_le_bytes());
    h[32..40].copy_from_slice(&415_743u64.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// One sector's bytes: its number and a tag, so two sectors never compare equal by accident.
fn pattern(lba: u64, tag: u8) -> [u8; BLOCK] {
    let mut out = [tag; BLOCK];
    out[..8].copy_from_slice(&lba.to_le_bytes());
    out
}

#[derive(Clone, Default)]
struct Tick(Rc<RefCell<u64>>);

impl Clock for Tick {
    fn now_us(&mut self) -> u64 {
        let mut t = self.0.borrow_mut();
        *t += 10;
        *t
    }
}

/// The host's side, frame by frame: the frames queued for the stage and the replies it sent.
struct Frames {
    incoming: VecDeque<Vec<u8>>,
    replies: Vec<Vec<u8>>,
    clock: Tick,
}

impl Link for Frames {
    fn recv(&mut self, buf: &mut [u8], _budget_us: u64) -> Result<usize, IoError> {
        let frame = self.incoming.pop_front().ok_or(IoError::Timeout)?;
        if frame.len() > buf.len() {
            return Err(IoError::Failed);
        }
        buf[..frame.len()].copy_from_slice(&frame);
        Ok(frame.len())
    }

    fn send(&mut self, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
        self.replies.push(data.to_vec());
        Ok(())
    }

    fn now_us(&mut self) -> u64 {
        self.clock.now_us()
    }
}

#[derive(Default)]
struct Feeds(u32);

impl Watchdog for Feeds {
    fn feed(&mut self) {
        self.0 += 1;
    }
}

/// One request served by the real session step: its reply, and the feeds it made.
fn serve_one<S: Storage>(storage: &mut S, frames: Vec<Vec<u8>>) -> (Vec<u8>, u32) {
    let mut link = Frames {
        incoming: frames.into(),
        replies: Vec::new(),
        clock: Tick::default(),
    };
    let mut feeds = Feeds::default();
    let ctx = Context {
        regions: &REGIONS,
        identity: &IDENTITY,
    };
    let mut buf = vec![0u8; BUFFER_LEN];
    let mut scratch = vec![0u8; BUFFER_LEN];
    let progress = step(
        &mut link,
        storage,
        &mut feeds,
        &ctx,
        1_000,
        &mut buf,
        &mut scratch,
    )
    .expect("the step serves the request");
    assert!(!matches!(progress, Progress::Idle), "the request was taken");
    assert_eq!(link.replies.len(), 1, "one reply per request");
    (link.replies.remove(0), feeds.0)
}

/// A write of `count` sectors at `lba` with each sector's own pattern; its one-byte status.
fn write<S: Storage>(storage: &mut S, lba: u32, count: u16, compared: bool, tag: u8) -> u8 {
    let mut payload = Vec::new();
    for i in 0..u64::from(count) {
        payload.extend_from_slice(&pattern(u64::from(lba) + i, tag));
    }
    let mut chunk = payload.clone();
    chunk.extend_from_slice(&payload_crc(&payload));
    let (reply, _) = serve_one(
        storage,
        vec![
            Request::write(lba, count, compared).encode().to_vec(),
            chunk,
        ],
    );
    assert_eq!(reply.len(), 1);
    reply[0]
}

/// A read of `count` sectors at `lba`: the sectors, or the one-byte status.
fn read<S: Storage>(storage: &mut S, lba: u32, count: u16) -> Result<Vec<u8>, u8> {
    let (reply, _) = serve_one(storage, vec![Request::read(lba, count).encode().to_vec()]);
    if reply.len() == 1 {
        return Err(reply[0]);
    }
    let n = usize::from(count) * SECTOR;
    assert_eq!(reply[0], STATUS_OK);
    assert_eq!(&reply[1 + n..], &payload_crc(&reply[..1 + n]));
    Ok(reply[1..1 + n].to_vec())
}

/// Every write outside the range the plan names: each edge, a straddle at each end, the header blocks and sector 0,
/// and the other protected regions.
const OUTSIDE: [(&str, u32, u16); 10] = [
    ("first-1", SCRATCH_FIRST - 1, 1),
    ("last+1", SCRATCH_LAST + 1, 1),
    ("straddles first", SCRATCH_FIRST - 1, 2),
    ("straddles last", SCRATCH_LAST, 2),
    ("sector 0", 0, 1),
    ("sector 2048", 2048, 1),
    ("sector 3072", 3072, 1),
    ("primary GPT header", 1, 1),
    ("backup GPT header", 415_743, 1),
    ("card's last sector", (CARD_BLOCKS - 1) as u32, 1),
];

fn limited(card: &ModelCard) -> Scratch<Sectors<ModelCard, Tick>> {
    Scratch::new(Sectors::new(card.clone(), Tick::default(), CARD_DECLARED))
}

#[test]
fn the_scratch_stage_refuses_every_write_outside_the_range_and_touches_nothing() {
    let card = ModelCard::seeded();
    let mut stage = limited(&card);
    for (name, lba, count) in OUTSIDE {
        for compared in [false, true] {
            let status = write(&mut stage, lba, count, compared, 0xc3);
            let want = if lba < 61_440 && !compared {
                // The region gate runs before the storage: an unmarked boot-area write is refused by it first.
                STATUS_NOT_COMPARED
            } else {
                STATUS_STORAGE
            };
            assert_eq!(status, want, "{name} compared {compared}");
        }
    }
    assert!(card.writes().is_empty(), "no block write reached the card");
    assert!(card.protected_unchanged());
}

#[test]
fn the_scratch_stage_refuses_reads_outside_the_range_too() {
    let card = ModelCard::seeded();
    let mut stage = limited(&card);
    for (name, lba, count) in OUTSIDE {
        assert_eq!(read(&mut stage, lba, count), Err(STATUS_STORAGE), "{name}");
    }
}

#[test]
fn the_scratch_stage_writes_the_whole_range_and_each_write_reads_back_equal() {
    let card = ModelCard::seeded();
    let mut stage = limited(&card);
    let edges = [
        (SCRATCH_FIRST, 1),
        (SCRATCH_LAST, 1),
        (SCRATCH_LAST + 1 - MAX_SECTORS as u32, MAX_SECTORS),
    ];
    let sweep = (SCRATCH_FIRST..=SCRATCH_LAST)
        .step_by(usize::from(MAX_SECTORS))
        .map(|lba| (lba, MAX_SECTORS));
    for (lba, count) in edges.into_iter().chain(sweep) {
        for compared in [false, true] {
            assert_eq!(
                write(&mut stage, lba, count, compared, 0x3c),
                STATUS_OK,
                "{lba}+{count}"
            );
            let back = read(&mut stage, lba, count).expect("an in-range read");
            let mut want = Vec::new();
            for i in 0..u64::from(count) {
                want.extend_from_slice(&pattern(u64::from(lba) + i, 0x3c));
            }
            assert!(back == want, "{lba}+{count} reads back equal");
        }
    }
    let written = card.writes();
    assert!(
        written
            .iter()
            .all(|&lba| (u64::from(SCRATCH_FIRST)..=u64::from(SCRATCH_LAST)).contains(&lba))
    );
    assert!(card.protected_unchanged());
}
