//! CARD-STAGE-BOUNDS: no card stage writes or erases outside its declared range, or touches the primary GPT
//! (sectors 0 to 33), the backup GPT, or sectors 2048 and 3072, whatever the host sends. The compared mark never
//! overrides a protected sector, and an erase or write that touches one is refused whole.
//!
//! The model is the card of record as the stage's own SD code sees it: a sparse SD host (`ModelHost`) with the
//! card's 124,735,488 blocks, read through the real `sd::Card`. It holds the image's GPT (a valid primary header in
//! sector 1 that puts the backup at 415,711..415,743) and a pattern in every protected sector. Both card stages run
//! over it through their real code:
//! - the legacy stage (`stage::Stage` over `sd::SdMedium`), the card image's `stage` mode;
//! - the FL1 card version (the shared session step over `fl1::storage::Sectors`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use lemnos_flash_stage::guard::Watchdog;
use lemnos_flash_stage::link::{IoError, Link};
use lemnos_flash_stage::serve::{BUFFER_LEN, Context, step};
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::frame::{Request, SECTOR, STATUS_OK, payload_crc};
use lemnos_k230_usbboot_stage::bounds::{CARD_DECLARED, Declared};
use lemnos_k230_usbboot_stage::fl1::storage::Sectors;
use lemnos_k230_usbboot_stage::fl1::{IDENTITY, REGIONS};
use lemnos_k230_usbboot_stage::format::{self, cmd, packet};
use lemnos_k230_usbboot_stage::sd::{BLOCK, Card, SdMedium};
use lemnos_k230_usbboot_stage::stage::Stage;
use lemnos_k230_usbboot_stage::wdt::Feed;

mod card_model;
use card_model::{BLOCKS, ModelHost, card, untouched};

// ---- the legacy stage (the card image's `stage` mode) ----

/// The constructor under test: the card's medium, with the declared range.
fn legacy_medium(host: &ModelHost, declared: Declared) -> SdMedium<ModelHost> {
    SdMedium::new(card(host), declared)
}

struct NoFeed;
impl Feed for NoFeed {
    fn feed(&mut self) {}
    fn fast_reset(&mut self) {}
}

fn words(values: &[u64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn reply(stage: &mut Stage<SdMedium<ModelHost>, NoFeed>) -> u16 {
    let mut out = [0u8; 60];
    stage.on_in(&mut out).expect("a reply");
    format::le16(&out, 2)
}

/// A legacy write of `sectors` sectors at `lba`: whether the stage took it whole.
fn legacy_write(host: &ModelHost, lba: u64, sectors: u64) -> bool {
    let mut stage = Stage::new(legacy_medium(host, CARD_DECLARED), NoFeed);
    let total = sectors * BLOCK as u64;
    let _ = stage.on_out(&packet(
        cmd::WRITE,
        &words(&[lba * BLOCK as u64, total, 0, 0]),
    ));
    if reply(&mut stage) != format::RESULT_OK {
        return false;
    }
    for chunk in vec![0xc3u8; total as usize].chunks(format::CHUNK) {
        let _ = stage.on_out(chunk);
    }
    reply(&mut stage) == format::RESULT_OK
}

/// A legacy erase of `sectors` sectors at `lba`: whether the stage took it.
fn legacy_erase(host: &ModelHost, lba: u64, sectors: u64) -> bool {
    let mut stage = Stage::new(legacy_medium(host, CARD_DECLARED), NoFeed);
    let _ = stage.on_out(&packet(
        cmd::ERASE,
        &words(&[lba * BLOCK as u64, sectors * BLOCK as u64]),
    ));
    reply(&mut stage) == format::RESULT_OK
}

/// Writes and erases the legacy stage must refuse, each on the erase grid (eight sectors) so its grid rule does not
/// refuse it first.
const LEGACY_REFUSED: [(&str, u64, u64); 9] = [
    ("the header block at 2048", 2048, 8),
    ("the header block at 3072", 3072, 8),
    ("the primary GPT, 0..7", 0, 8),
    ("the primary GPT's last sectors, 32..39", 32, 8),
    ("eight sectors before the declared range", 1_048_568, 8),
    ("eight sectors after the declared range", 1_050_624, 8),
    ("straddling the declared range's start", 1_048_568, 16),
    ("the backup GPT's erase block", 415_736, 8),
    ("the card's last erase block", BLOCKS - 8, 8),
];

#[test]
fn the_legacy_stage_refuses_every_write_outside_its_range_or_on_a_protected_sector() {
    for (name, lba, sectors) in LEGACY_REFUSED {
        let host = ModelHost::card_of_record();
        assert!(!legacy_write(&host, lba, sectors), "{name}: refused");
        assert!(host.writes().is_empty(), "{name}: the card was not touched");
    }
}

#[test]
fn the_legacy_stage_refuses_every_erase_outside_its_range_or_on_a_protected_sector() {
    for (name, lba, sectors) in LEGACY_REFUSED {
        let host = ModelHost::card_of_record();
        assert!(!legacy_erase(&host, lba, sectors), "{name}: refused");
        assert!(host.writes().is_empty(), "{name}: the card was not touched");
        assert!(untouched(&host));
    }
}

#[test]
fn an_erase_straddling_a_protected_sector_is_refused_whole() {
    // 2040..2055 holds sector 2048; 3064..3079 holds 3072. Neither erases its first eight sectors either.
    for lba in [2040, 3064] {
        let host = ModelHost::card_of_record();
        assert!(!legacy_erase(&host, lba, 16), "{lba}");
        assert!(host.writes().is_empty(), "{lba}: nothing erased");
    }
}

#[test]
fn the_legacy_stage_writes_and_erases_inside_its_declared_range() {
    let host = ModelHost::card_of_record();
    assert!(legacy_write(&host, CARD_DECLARED.first, 8));
    assert!(legacy_erase(&host, CARD_DECLARED.last - 7, 8));
    assert!(
        host.writes()
            .iter()
            .all(|lba| (CARD_DECLARED.first..=CARD_DECLARED.last).contains(lba))
    );
    assert!(untouched(&host));
}

// ---- the FL1 card version ----

#[derive(Clone, Default)]
struct Tick(Rc<RefCell<u64>>);

impl Clock for Tick {
    fn now_us(&mut self) -> u64 {
        let mut t = self.0.borrow_mut();
        *t += 10;
        *t
    }
}

/// The constructor under test: the card as the FL1 storage, with the declared range.
fn fl1_storage(host: &ModelHost, declared: Declared) -> Sectors<Card<ModelHost>, Tick> {
    Sectors::new(card(host), Tick::default(), declared)
}

struct Frames {
    incoming: VecDeque<Vec<u8>>,
    replies: Vec<Vec<u8>>,
    clock: Tick,
}

impl Link for Frames {
    fn recv(&mut self, buf: &mut [u8], _budget_us: u64) -> Result<usize, IoError> {
        let frame = self.incoming.pop_front().ok_or(IoError::Timeout)?;
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

struct NoDog;
impl Watchdog for NoDog {
    fn feed(&mut self) {}
}

/// An FL1 write of `count` sectors at `lba`, marked compared or not: the status.
fn fl1_write(host: &ModelHost, lba: u32, count: u16, compared: bool) -> u8 {
    let mut storage = fl1_storage(host, CARD_DECLARED);
    let payload = vec![0xc3u8; usize::from(count) * SECTOR];
    let mut chunk = payload.clone();
    chunk.extend_from_slice(&payload_crc(&payload));
    let mut link = Frames {
        incoming: vec![
            Request::write(lba, count, compared).encode().to_vec(),
            chunk,
        ]
        .into(),
        replies: Vec::new(),
        clock: Tick::default(),
    };
    let ctx = Context {
        regions: &REGIONS,
        identity: &IDENTITY,
    };
    let mut buf = vec![0u8; BUFFER_LEN];
    let mut scratch = vec![0u8; BUFFER_LEN];
    step(
        &mut link,
        &mut storage,
        &mut NoDog,
        &ctx,
        1_000,
        &mut buf,
        &mut scratch,
    )
    .expect("the step serves the request");
    link.replies[0][0]
}

const FL1_REFUSED: [(&str, u32, u16); 12] = [
    ("sector 0", 0, 1),
    ("the primary GPT header", 1, 1),
    ("the primary GPT's last entry sector", 33, 1),
    ("sector 2048", 2048, 1),
    ("sector 3072", 3072, 1),
    ("the backup GPT's first sector", 415_711, 1),
    ("the backup GPT header", 415_743, 1),
    ("first-1", 1_048_575, 1),
    ("last+1", 1_050_624, 1),
    ("straddling first", 1_048_575, 2),
    ("straddling last", 1_050_623, 2),
    ("the card's last sector", (BLOCKS - 1) as u32, 1),
];

#[test]
fn the_fl1_card_version_refuses_every_write_outside_its_range_marked_compared_or_not() {
    for (name, lba, count) in FL1_REFUSED {
        for compared in [true, false] {
            let host = ModelHost::card_of_record();
            let status = fl1_write(&host, lba, count, compared);
            assert_ne!(status, STATUS_OK, "{name} compared {compared}: refused");
            assert!(
                host.writes().is_empty(),
                "{name} compared {compared}: untouched"
            );
        }
    }
}

#[test]
fn the_fl1_card_version_writes_inside_its_declared_range() {
    let host = ModelHost::card_of_record();
    for (lba, count) in [(1_048_576, 1), (1_050_623, 1), (1_050_496, 128)] {
        assert_eq!(
            fl1_write(&host, lba, count, false),
            STATUS_OK,
            "{lba}+{count}"
        );
    }
    assert!(untouched(&host));
}
