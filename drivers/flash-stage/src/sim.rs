//! A simulated device for the stage, and the conformance list a host runs against it.
//!
//! - [`Transport`] is the frame-level view a host drives: it sends one frame (one transfer) and takes one reply.
//!   A PC side implements it over its own transport (USB), and runs the same [`SCENARIOS`] against that
//!   transport, against a real board or against [`Device`].
//! - [`Device`] is the stage's real serve step running over a queue, with a sparse card in memory. It is a model:
//!   it holds no timing, and a frame it is not sent is an idle, not a fault.
//! - [`model`] is a scripted controller for the device side's enumeration tests.

use std::collections::{BTreeMap, VecDeque};
use std::vec::Vec;

use crate::guard::Watchdog;
use crate::link::{IoError, Link, Storage};
use crate::region::Region;
use crate::serve::{Progress, ServeError, step};
use lemnos_flash_wire::frame::{
    MAX_CHUNK, Request, ResetKind, SECTOR, STATUS_BAD_REQUEST, STATUS_CRC, STATUS_NOT_COMPARED,
    STATUS_OK, payload_crc,
};
use lemnos_flash_wire::identity::{IDENTITY_LEN, Peer, check, identity_record};
use lemnos_flash_wire::region::region_digest;

pub mod model;

/// What a host sees from a transport: one frame at a time, in both directions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The stage ended the session with an END, which asked for this reset.
    Ended(ResetKind),
    /// The stage ended the session without an END: a cut, a reset, or the idle limit.
    Stopped(ServeError),
}

/// The frame-level view of the link, from the host's side.
pub trait Transport {
    /// Sends one frame, as one transfer. A frame is never split across transfers.
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), TransportError>;
    /// Takes the next reply, running the stage as far as it needs to. `Ok(None)` means the stage has nothing
    /// more to say until the host sends again. The host can act on each reply before it asks for the next.
    fn take_reply(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
    /// The watchdog's feed count, when the transport can see it (the simulated device can; a board cannot).
    fn feeds(&self) -> Option<u32> {
        None
    }
}

/// The queues of the simulated link.
#[derive(Default)]
struct Queues {
    inbound: VecDeque<Vec<u8>>,
    outbound: VecDeque<Vec<u8>>,
}

impl Link for Queues {
    fn recv(&mut self, buf: &mut [u8], _budget_us: u64) -> Result<usize, IoError> {
        let frame = self.inbound.pop_front().ok_or(IoError::Timeout)?;
        if frame.len() > buf.len() {
            return Err(IoError::Failed);
        }
        buf[..frame.len()].copy_from_slice(&frame);
        Ok(frame.len())
    }

    fn send(&mut self, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
        self.outbound.push_back(data.to_vec());
        Ok(())
    }
}

/// The card: a sparse map of written sectors. A sector nobody wrote reads as zeros.
#[derive(Default)]
pub struct Card {
    sectors: u32,
    written: BTreeMap<u32, [u8; SECTOR]>,
}

impl Card {
    /// A card of `sectors` sectors, all zero.
    pub fn new(sectors: u32) -> Self {
        Self {
            sectors,
            written: BTreeMap::new(),
        }
    }

    /// The card's size in sectors.
    pub fn sectors(&self) -> u32 {
        self.sectors
    }

    /// One sector's bytes.
    pub fn sector(&self, lba: u32) -> [u8; SECTOR] {
        self.written.get(&lba).copied().unwrap_or([0; SECTOR])
    }

    fn range(&self, lba: u32, bytes: usize) -> Result<usize, IoError> {
        let sectors = bytes.div_ceil(SECTOR) as u64;
        if u64::from(lba) + sectors > u64::from(self.sectors) {
            return Err(IoError::Failed);
        }
        Ok(lba as usize)
    }
}

impl Storage for Card {
    fn write(&mut self, lba: u32, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
        self.range(lba, data.len())?;
        for (i, chunk) in data.chunks(SECTOR).enumerate() {
            let mut sector = [0u8; SECTOR];
            sector[..chunk.len()].copy_from_slice(chunk);
            self.written.insert(lba + i as u32, sector);
        }
        Ok(())
    }

    fn read(&mut self, lba: u32, out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
        self.range(lba, out.len())?;
        for (i, chunk) in out.chunks_mut(SECTOR).enumerate() {
            let sector = self.sector(lba + i as u32);
            chunk.copy_from_slice(&sector[..chunk.len()]);
        }
        Ok(())
    }
}

/// The watchdog's counter.
#[derive(Default)]
struct Counter(u32);

impl Watchdog for Counter {
    fn feed(&mut self) {
        self.0 += 1;
    }
}

/// A simulated stage: the real serve step, a queue link, a sparse card and a watchdog counter.
pub struct Device {
    link: Queues,
    card: Card,
    dog: Counter,
    regions: Vec<Region>,
    identity: [u8; IDENTITY_LEN],
    buf: Vec<u8>,
    scratch: Vec<u8>,
    ended: Option<ResetKind>,
    stopped: Option<ServeError>,
}

impl Device {
    /// A device with a card of `sectors` sectors, the chip's `regions`, and the build id its identity record
    /// carries.
    pub fn new(sectors: u32, build_id: u64, regions: &[Region]) -> Self {
        let identity = identity_record(build_id, regions);
        Self {
            link: Queues::default(),
            card: Card::new(sectors),
            dog: Counter::default(),
            regions: regions.to_vec(),
            identity,
            buf: vec![0; MAX_CHUNK + 1],
            scratch: vec![0; MAX_CHUNK],
            ended: None,
            stopped: None,
        }
    }

    /// Runs the stage's serve step once, over what the host has queued.
    pub fn step(&mut self) -> Result<Progress, ServeError> {
        step(
            &mut self.link,
            &mut self.card,
            &mut self.dog,
            &self.regions,
            &self.identity,
            &mut self.buf,
            &mut self.scratch,
        )
    }

    /// The card, read as the stage sees it.
    pub fn card(&self) -> &Card {
        &self.card
    }

    /// The watchdog's feeds so far.
    pub fn feeds(&self) -> u32 {
        self.dog.0
    }
}

impl Transport for Device {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        if let Some(kind) = self.ended {
            return Err(TransportError::Ended(kind));
        }
        if let Some(e) = self.stopped {
            return Err(TransportError::Stopped(e));
        }
        self.link.inbound.push_back(frame.to_vec());
        Ok(())
    }

    fn take_reply(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        loop {
            if let Some(reply) = self.link.outbound.pop_front() {
                return Ok(Some(reply));
            }
            if let Some(kind) = self.ended {
                return Err(TransportError::Ended(kind));
            }
            if let Some(e) = self.stopped {
                return Err(TransportError::Stopped(e));
            }
            if self.link.inbound.is_empty() {
                return Ok(None);
            }
            match self.step() {
                Ok(Progress::Ended(kind)) => self.ended = Some(kind),
                Ok(_) => {}
                Err(e) => self.stopped = Some(e),
            }
        }
    }

    fn feeds(&self) -> Option<u32> {
        Some(self.dog.0)
    }
}

/// The outcome a scenario expects of one reply.
fn expect_status(reply: Option<Vec<u8>>, status: u8, what: &str) -> Result<(), String> {
    match reply {
        Some(bytes) if bytes == [status] => Ok(()),
        other => Err(format!("{what}: expected status {status}, got {other:?}")),
    }
}

fn next(t: &mut dyn Transport, what: &str) -> Result<Option<Vec<u8>>, String> {
    t.take_reply().map_err(|e| format!("{what}: {e:?}"))
}

fn chunk(payload: &[u8], crc_flip: bool) -> Vec<u8> {
    let mut out = payload.to_vec();
    let mut crc = payload_crc(payload);
    if crc_flip {
        crc[1] ^= 1;
    }
    out.extend_from_slice(&crc);
    out
}

/// The sectors a scenario writes to, read from the device's own identity record. A host that runs the list
/// against a real stage gets the same answers: the scenario asks the stage for its regions.
struct Layout {
    peer: Peer,
}

impl Layout {
    fn fetch(t: &mut dyn Transport) -> Result<Self, String> {
        t.send_frame(&Request::identify().encode())
            .map_err(|e| format!("identify: {e:?}"))?;
        let record = next(t, "identify")?.ok_or("identify: no reply")?;
        let peer = check(&record).map_err(|e| format!("identify: {e:?}"))?;
        Ok(Self { peer })
    }

    /// The first header sector (the boot record).
    fn header(&self) -> u32 {
        self.peer
            .regions()
            .iter()
            .find(|r| r.class == crate::region::Class::Header)
            .expect("a header region")
            .first
    }

    /// A boot-area sector that is not a header.
    fn boot_only(&self) -> u32 {
        let area = self
            .peer
            .regions()
            .iter()
            .find(|r| r.class == crate::region::Class::BootArea)
            .expect("a boot area");
        area.first + 36
    }

    /// The sector just before the boot area: plain data.
    fn plain_before_boot(&self) -> u32 {
        self.peer
            .regions()
            .iter()
            .map(|r| r.first)
            .min()
            .expect("regions")
            .saturating_sub(1)
    }

    /// A sector past every region: plain data.
    fn plain_after(&self) -> u32 {
        self.peer
            .regions()
            .iter()
            .map(|r| r.end)
            .max()
            .expect("regions")
    }
}

fn write_one(
    t: &mut dyn Transport,
    lba: u32,
    fill: u8,
    compared: bool,
    crc_flip: bool,
) -> Result<Option<Vec<u8>>, String> {
    t.send_frame(&Request::write(lba, 1, compared).encode())
        .map_err(|e| format!("write request: {e:?}"))?;
    t.send_frame(&chunk(&vec![fill; SECTOR], crc_flip))
        .map_err(|e| format!("chunk: {e:?}"))?;
    next(t, "write reply")
}

fn read_back(t: &mut dyn Transport, lba: u32) -> Result<Vec<u8>, String> {
    t.send_frame(&Request::read(lba, 1).encode())
        .map_err(|e| format!("read request: {e:?}"))?;
    let reply = next(t, "read reply")?.ok_or("read: no reply")?;
    if reply.first() != Some(&STATUS_OK) || reply.len() != 1 + SECTOR + 2 {
        return Err(format!(
            "read {lba}: not an ok reply of 515 bytes: {} bytes",
            reply.len()
        ));
    }
    Ok(reply[1..1 + SECTOR].to_vec())
}

/// The feeds a transport can report, measured around `op`, or `None` when it cannot report them.
fn feeds_around(
    t: &mut dyn Transport,
    op: impl FnOnce(&mut dyn Transport) -> Result<(), String>,
) -> Result<Option<u32>, String> {
    let before = t.feeds();
    op(t)?;
    Ok(match (before, t.feeds()) {
        (Some(a), Some(b)) => Some(b - a),
        _ => None,
    })
}

fn check_feeds(got: Option<u32>, want: u32, what: &str) -> Result<(), String> {
    match got {
        Some(n) if n != want => Err(format!("{what}: fed {n} times, expected {want}")),
        _ => Ok(()),
    }
}

// The conformance scenarios. Each one is a fresh device or a fresh host transport.

fn identify(t: &mut dyn Transport) -> Result<(), String> {
    let layout = Layout::fetch(t)?;
    if layout.peer.build_id == 0 {
        return Err("identify: build id 0 in a stage that set one".into());
    }
    Ok(())
}

fn read_feeds_once(t: &mut dyn Transport) -> Result<(), String> {
    let sector = Layout::fetch(t)?.plain_after();
    let n = feeds_around(t, |t| {
        t.send_frame(&Request::read(sector, 1).encode())
            .map_err(|e| format!("{e:?}"))?;
        let reply = next(t, "read")?.ok_or("read: no reply")?;
        if reply.len() != 1 + SECTOR + 2 {
            return Err(format!("read reply length {}", reply.len()));
        }
        Ok(())
    })?;
    check_feeds(n, 1, "read")
}

fn write_stores_and_feeds_once(t: &mut dyn Transport) -> Result<(), String> {
    let lba = Layout::fetch(t)?.plain_after();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0x3C, false, false)?;
        expect_status(reply, STATUS_OK, "plain write")
    })?;
    check_feeds(n, 1, "plain write")?;
    if read_back(t, lba)? != vec![0x3C; SECTOR] {
        return Err("plain write: the sector does not read back".into());
    }
    Ok(())
}

fn bad_crc_refused(t: &mut dyn Transport) -> Result<(), String> {
    let lba = Layout::fetch(t)?.plain_after();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0x3C, false, true)?;
        expect_status(reply, STATUS_CRC, "bad crc")
    })?;
    check_feeds(n, 0, "bad crc")?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("bad crc: the sector was written".into());
    }
    Ok(())
}

fn cut_before_payload(t: &mut dyn Transport) -> Result<(), String> {
    // A request with no chunk after it: the stage waits for the chunk, the link goes quiet, and the session
    // ends with a stop. The sector is not written.
    let lba = Layout::fetch(t)?.plain_after();
    t.send_frame(&Request::write(lba, 1, false).encode())
        .map_err(|e| format!("{e:?}"))?;
    match t.take_reply() {
        Err(TransportError::Stopped(ServeError::Link(IoError::Timeout))) => Ok(()),
        other => Err(format!("cut: expected a stop at the chunk, got {other:?}")),
    }
}

fn boot_area_without_mark_refused(t: &mut dyn Transport) -> Result<(), String> {
    let layout = Layout::fetch(t)?;
    let lba = layout.boot_only();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0x77, false, false)?;
        expect_status(reply, STATUS_NOT_COMPARED, "boot area without the mark")
    })?;
    check_feeds(n, 0, "boot area without the mark")?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("boot area without the mark: the sector was written".into());
    }
    Ok(())
}

fn boot_area_with_mark_stored(t: &mut dyn Transport) -> Result<(), String> {
    let lba = Layout::fetch(t)?.boot_only();
    let reply = write_one(t, lba, 0x77, true, false)?;
    expect_status(reply, STATUS_OK, "boot area with the mark")?;
    if read_back(t, lba)? != vec![0x77; SECTOR] {
        return Err("boot area with the mark: not stored".into());
    }
    Ok(())
}

fn header_bad_crc_before_any_write(t: &mut dyn Transport) -> Result<(), String> {
    let layout = Layout::fetch(t)?;
    let lba = layout.header();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0xEE, true, true)?;
        expect_status(reply, STATUS_CRC, "header with a bad crc and the mark")
    })?;
    check_feeds(n, 0, "header bad crc")?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("header bad crc: the boot record was touched".into());
    }
    Ok(())
}

fn header_without_mark_refused(t: &mut dyn Transport) -> Result<(), String> {
    let lba = Layout::fetch(t)?.header();
    let reply = write_one(t, lba, 0xEE, false, false)?;
    expect_status(reply, STATUS_NOT_COMPARED, "header without the mark")
}

fn header_with_mark_stored(t: &mut dyn Transport) -> Result<(), String> {
    let lba = Layout::fetch(t)?.header();
    let reply = write_one(t, lba, 0xEE, true, false)?;
    expect_status(reply, STATUS_OK, "header with the mark")?;
    if read_back(t, lba)? != vec![0xEE; SECTOR] {
        return Err("header with the mark: not stored".into());
    }
    Ok(())
}

fn straddle_needs_mark(t: &mut dyn Transport) -> Result<(), String> {
    let layout = Layout::fetch(t)?;
    let lba = layout.plain_before_boot();
    // Two sectors: the last plain sector and the first boot-area sector.
    t.send_frame(&Request::write(lba, 2, false).encode())
        .map_err(|e| format!("{e:?}"))?;
    t.send_frame(&chunk(&vec![0x11; 2 * SECTOR], false))
        .map_err(|e| format!("{e:?}"))?;
    expect_status(
        next(t, "straddle")?,
        STATUS_NOT_COMPARED,
        "straddle without the mark",
    )?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("straddle: the plain sector was written by a refused write".into());
    }
    Ok(())
}

fn wrong_length_chunk_refused_and_session_goes_on(t: &mut dyn Transport) -> Result<(), String> {
    let lba = Layout::fetch(t)?.plain_after();
    t.send_frame(&Request::write(lba, 2, false).encode())
        .map_err(|e| format!("{e:?}"))?;
    // One sector's chunk for a two-sector request: a bad length, refused with a status.
    t.send_frame(&chunk(&vec![0x3C; SECTOR], false))
        .map_err(|e| format!("{e:?}"))?;
    expect_status(next(t, "short chunk")?, STATUS_BAD_REQUEST, "short chunk")?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("short chunk: something was written".into());
    }
    Ok(())
}

fn end_maskrom_after_ok(t: &mut dyn Transport) -> Result<(), String> {
    t.send_frame(&Request::end(ResetKind::Maskrom).encode())
        .map_err(|e| format!("{e:?}"))?;
    expect_status(next(t, "end")?, STATUS_OK, "end")?;
    match t.take_reply() {
        Err(TransportError::Ended(ResetKind::Maskrom)) => Ok(()),
        other => Err(format!("end: expected Ended(Maskrom), got {other:?}")),
    }
}

fn bad_request_crc_refused_and_session_goes_on(t: &mut dyn Transport) -> Result<(), String> {
    let mut wire = Request::read(1, 1).encode();
    wire[7] ^= 0x40;
    t.send_frame(&wire).map_err(|e| format!("{e:?}"))?;
    expect_status(next(t, "bad request crc")?, STATUS_CRC, "bad request crc")?;
    // The session goes on: a good request after it is served.
    t.send_frame(&Request::identify().encode())
        .map_err(|e| format!("{e:?}"))?;
    let reply = next(t, "identify after a refusal")?.ok_or("no reply after a refusal")?;
    check(&reply)
        .map(|_| ())
        .map_err(|e| format!("identify after a refusal: {e:?}"))
}

fn reserved_bit_refused_and_session_goes_on(t: &mut dyn Transport) -> Result<(), String> {
    let mut request = Request::read(1, 1);
    request.reserved = 1;
    t.send_frame(&request.encode())
        .map_err(|e| format!("{e:?}"))?;
    expect_status(next(t, "reserved bit")?, STATUS_BAD_REQUEST, "reserved bit")
}

fn empty_queue_is_idle(t: &mut dyn Transport) -> Result<(), String> {
    match t.take_reply() {
        Ok(None) => Ok(()),
        other => Err(format!("empty queue: expected no reply, got {other:?}")),
    }
}

/// One conformance scenario: a name, and the checks a host passes when it runs against a fresh stage.
pub struct Scenario {
    pub name: &'static str,
    pub run: fn(&mut dyn Transport) -> Result<(), String>,
}

/// The conformance list. A host runs each against a fresh stage (a fresh [`Device`], or a fresh board): see
/// [`run`]. Each scenario reads the stage's own identity record for its sectors, so the list follows the stage's
/// regions rather than fixed numbers.
pub const SCENARIOS: [Scenario; 17] = [
    Scenario {
        name: "identify_answers_with_a_build_id",
        run: identify,
    },
    Scenario {
        name: "read_returns_the_sector_with_its_crc_and_feeds_once",
        run: read_feeds_once,
    },
    Scenario {
        name: "a_plain_write_stores_the_verified_chunk_and_feeds_once",
        run: write_stores_and_feeds_once,
    },
    Scenario {
        name: "a_write_with_a_bad_crc_is_refused_with_status_one_and_neither_stored_nor_fed",
        run: bad_crc_refused,
    },
    Scenario {
        name: "a_cut_before_the_payload_stops_the_session_with_nothing_stored",
        run: cut_before_payload,
    },
    Scenario {
        name: "a_boot_area_write_without_the_mark_is_refused_with_status_four",
        run: boot_area_without_mark_refused,
    },
    Scenario {
        name: "a_boot_area_write_with_the_mark_is_stored",
        run: boot_area_with_mark_stored,
    },
    Scenario {
        name: "a_header_write_with_a_bad_crc_is_refused_before_any_write_even_with_the_mark",
        run: header_bad_crc_before_any_write,
    },
    Scenario {
        name: "a_header_write_without_the_mark_is_refused_with_status_four",
        run: header_without_mark_refused,
    },
    Scenario {
        name: "a_header_write_with_the_mark_and_a_good_crc_is_stored",
        run: header_with_mark_stored,
    },
    Scenario {
        name: "a_write_that_straddles_into_the_boot_area_needs_the_mark",
        run: straddle_needs_mark,
    },
    Scenario {
        name: "a_chunk_of_the_wrong_length_is_refused_and_the_session_goes_on",
        run: wrong_length_chunk_refused_and_session_goes_on,
    },
    Scenario {
        name: "end_with_the_maskrom_reset_returns_maskrom_after_an_ok_status",
        run: end_maskrom_after_ok,
    },
    Scenario {
        name: "a_request_with_a_bad_crc_is_refused_and_the_session_goes_on",
        run: bad_request_crc_refused_and_session_goes_on,
    },
    Scenario {
        name: "a_reserved_bit_a_read_does_not_define_is_refused_and_the_session_goes_on",
        run: reserved_bit_refused_and_session_goes_on,
    },
    Scenario {
        name: "an_empty_queue_is_idle_not_a_stop",
        run: empty_queue_is_idle,
    },
    Scenario {
        name: "the_regions_the_identity_publishes_match_the_scenarios_layout",
        run: layout_is_published,
    },
];

fn layout_is_published(t: &mut dyn Transport) -> Result<(), String> {
    let peer = Layout::fetch(t)?.peer;
    if region_digest(peer.regions()) == 0 {
        return Err("regions: digest is zero".into());
    }
    Ok(())
}

/// Runs one scenario against a transport. A host calls this for each entry of [`SCENARIOS`].
pub fn run(scenario: &Scenario, t: &mut dyn Transport) -> Result<(), String> {
    (scenario.run)(t).map_err(|e| format!("{}: {e}", scenario.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Class;

    /// The chip's table in the tests: a boot area 64..32768 with a header at 64, the card larger than the area.
    const REGIONS: [Region; 2] = [
        Region {
            first: 64,
            end: 32_768,
            class: Class::BootArea,
        },
        Region {
            first: 64,
            end: 65,
            class: Class::Header,
        },
    ];

    #[test]
    fn every_scenario_passes_against_the_simulated_device() {
        for scenario in &SCENARIOS {
            let mut dev = Device::new(40_000, 0x0BAD_CAFE, &REGIONS);
            run(scenario, &mut dev).unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn the_scenario_list_has_seventeen_entries_with_unique_names() {
        let mut names: Vec<&str> = SCENARIOS.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), 17);
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 17);
    }

    #[test]
    fn a_stop_is_reported_as_a_stop_not_as_idle() {
        let mut dev = Device::new(1024, 1, &REGIONS);
        dev.send_frame(&Request::write(5, 1, false).encode())
            .unwrap();
        assert_eq!(
            dev.take_reply(),
            Err(TransportError::Stopped(ServeError::Link(IoError::Timeout)))
        );
    }

    #[test]
    fn the_record_the_device_sends_is_the_one_built_from_its_table() {
        let mut dev = Device::new(1024, 42, &REGIONS);
        dev.send_frame(&Request::identify().encode()).unwrap();
        let record = dev.take_reply().unwrap().unwrap();
        assert_eq!(record.len(), IDENTITY_LEN);
        let peer = check(&record).unwrap();
        assert_eq!(peer.build_id, 42);
        assert_eq!(peer.regions(), &REGIONS[..]);
    }

    #[test]
    fn the_card_is_sparse_and_bounded() {
        let mut card = Card::new(1 << 20);
        card.write(7, &[1u8; SECTOR], 0).unwrap();
        assert_eq!(card.written.len(), 1, "one written sector is one entry");
        assert!(
            card.write((1 << 20) - 1, &[0u8; 2 * SECTOR], 0).is_err(),
            "past the end is refused"
        );
    }
}
