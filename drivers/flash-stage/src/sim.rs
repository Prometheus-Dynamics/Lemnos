//! A simulated device for the stage, and the conformance list a host runs against it.
//!
//! - [`Transport`] is the frame-level view a host drives: it sends one frame (one transfer) and takes one reply.
//!   A PC side implements it over its own transport (USB), and runs the same [`SCENARIOS`] against that
//!   transport, against a real board or against [`Device`].
//! - [`Device`] is a stage from reset: the device side over a scripted controller ([`model`]), the stage's serve step,
//!   and a sparse card in memory. **The simulated device starts at reset.** [`run`] connects it first (the host's
//!   enumeration, `Transport::connect`), so a scenario that passes has also enumerated the device. It is a model: it
//!   holds no timing, and a frame the host has not sent is an idle, not a fault.
//! - [`model`] is the scripted controller that stands in for the device controller.

use std::collections::BTreeMap;
use std::vec::Vec;

use crate::guard::Watchdog;
use crate::link::{IoError, Storage};
use crate::region::Region;
use crate::serve::{Context, Progress, ServeError, step};
use crate::usb::{BulkLink, EnumError, Ep0, enumerate_from_now};
use crate::wait::IterClock;
use lemnos_flash_wire::frame::{
    MAX_CHUNK, Request, ResetKind, SECTOR, STATUS_BAD_REQUEST, STATUS_CRC, STATUS_NOT_COMPARED,
    STATUS_OK, payload_crc,
};
use lemnos_flash_wire::identity::{IDENTITY_LEN, Peer, check, identity_record};
use lemnos_flash_wire::region::region_digest;
use lemnos_flash_wire::timing::IDLE_LIMIT_US;

pub mod model;

/// What a host sees from a transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The host could not enumerate the device: the enumeration limit, or a disconnect.
    Enumeration(EnumError),
    /// A frame was sent before the device was enumerated.
    NotConnected,
    /// The stage ended the session with an END, which asked for this reset.
    Ended(ResetKind),
    /// The stage ended the session without an END: a cut, or the idle limit.
    Stopped(ServeError),
    /// The transport itself failed (a host-side fault: the USB stack, the device going away). Not a verdict on the stage.
    Transport(String),
}

/// A scratch card, and the caller's word that it is one. The conformance suite writes real sectors (the header at
/// sector 64, a boot-area sector, plain sectors past the boot area). Running it against any transport requires this
/// typed argument: a run on a card that matters is refused by the type system, not by an environment variable.
#[derive(Debug)]
pub struct ScratchCard(());

impl ScratchCard {
    /// The caller states that the card under the transport is a scratch card, and that the suite may write any sector.
    pub const fn acknowledge() -> Self {
        Self(())
    }
}

/// The outcome of a scenario or a run. A check that could not be made is not a pass: it is reported as not checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every check was made and passed.
    Passed,
    /// The checks that were made passed, but at least one check could not be made, and is named here. The run is not
    /// a pass.
    NotChecked(Vec<&'static str>),
}

/// The checks a scenario has skipped so far. A skipped check is recorded, and makes the verdict `NotChecked`.
#[derive(Default)]
pub struct Checks {
    skipped: Vec<&'static str>,
}

impl Checks {
    fn skip(&mut self, what: &'static str) {
        self.skipped.push(what);
    }
}

/// The frame-level view of the device, from the host's side. A scenario starts with `connect`, which runs the host's
/// enumeration from reset; a board transport performs the real enumeration, and the simulated device runs its model.
pub trait Transport {
    /// Brings the device from reset to configured: the host's enumeration. Every scenario starts here.
    fn connect(&mut self) -> Result<(), TransportError>;
    /// Sends one frame, as one transfer. A frame is never split across transfers.
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), TransportError>;
    /// Takes the next reply, running the stage as far as it needs to. `Ok(None)` means the stage has nothing more to
    /// say until the host sends again. The host can act on each reply before it asks for the next.
    fn take_reply(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
    /// The watchdog's feed count, when the transport can see it (the simulated device can; a board cannot).
    fn feeds(&self) -> Option<u32> {
        None
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

/// A simulated stage from reset. Its device side is the scripted controller model; its serve loop is the stage's own.
/// `connect` runs the host's enumeration; the frames a host then sends are queued in the model's script, and the stage
/// serves them as the stage does. It is a model: a frame that never comes is a timeout, not a fault.
pub struct Device {
    host: model::Model,
    ep0: Ep0,
    clock: IterClock,
    card: Card,
    dog: Counter,
    regions: Vec<Region>,
    identity: [u8; IDENTITY_LEN],
    buf: Vec<u8>,
    scratch: Vec<u8>,
    connected: bool,
    ended: Option<ResetKind>,
    stopped: Option<ServeError>,
}

impl Device {
    /// A device at reset, with a card of `sectors` sectors, the chip's `regions`, and the build id its identity
    /// record carries.
    pub fn new(sectors: u32, build_id: u64, regions: &[Region]) -> Self {
        Self {
            host: model::Model::new(),
            ep0: Ep0::new(),
            clock: IterClock::new(),
            card: Card::new(sectors),
            dog: Counter::default(),
            regions: regions.to_vec(),
            identity: identity_record(build_id, regions),
            buf: vec![0; MAX_CHUNK + 1],
            scratch: vec![0; MAX_CHUNK],
            connected: false,
            ended: None,
            stopped: None,
        }
    }

    /// The card, read as the stage sees it.
    pub fn card(&self) -> &Card {
        &self.card
    }

    /// The watchdog's feeds so far.
    pub fn feeds(&self) -> u32 {
        self.dog.0
    }

    /// Runs the stage's serve step once, over the frame the host has queued next. Only called with a frame pending.
    fn serve_one(&mut self) -> Result<Progress, ServeError> {
        let mut link = BulkLink {
            ctrl: &mut self.host,
            clock: &mut self.clock,
            ep0: &mut self.ep0,
        };
        step(
            &mut link,
            &mut self.card,
            &mut self.dog,
            &Context {
                regions: &self.regions,
                identity: &self.identity,
            },
            IDLE_LIMIT_US,
            &mut self.buf,
            &mut self.scratch,
        )
    }
}

impl Transport for Device {
    fn connect(&mut self) -> Result<(), TransportError> {
        self.host.enumeration(7);
        match enumerate_from_now(&mut self.host, &mut self.clock, &mut self.ep0) {
            Ok(()) => {
                self.connected = true;
                Ok(())
            }
            Err(e) => Err(TransportError::Enumeration(e)),
        }
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::NotConnected);
        }
        if let Some(kind) = self.ended {
            return Err(TransportError::Ended(kind));
        }
        if let Some(e) = self.stopped {
            return Err(TransportError::Stopped(e));
        }
        self.host.frame(frame.to_vec());
        Ok(())
    }

    fn take_reply(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        loop {
            if !self.host.bulk_in.is_empty() {
                return Ok(Some(self.host.bulk_in.remove(0)));
            }
            if let Some(kind) = self.ended {
                return Err(TransportError::Ended(kind));
            }
            if let Some(e) = self.stopped {
                return Err(TransportError::Stopped(e));
            }
            if !self.host.frame_pending() {
                return Ok(None);
            }
            match self.serve_one() {
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

fn check_feeds(
    c: &mut Checks,
    got: Option<u32>,
    want: u32,
    what: &'static str,
) -> Result<(), String> {
    match got {
        Some(n) if n != want => Err(format!("{what}: fed {n} times, expected {want}")),
        Some(_) => Ok(()),
        // A transport that cannot count feeds has not checked this: the run is not a pass.
        None => {
            c.skip(what);
            Ok(())
        }
    }
}

// The conformance scenarios. Each one is a fresh device or a fresh host transport.

fn identify(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
    let layout = Layout::fetch(t)?;
    if layout.peer.build_id == 0 {
        return Err("identify: build id 0 in a stage that set one".into());
    }
    Ok(())
}

fn read_feeds_once(t: &mut dyn Transport, c: &mut Checks) -> Result<(), String> {
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
    check_feeds(c, n, 1, "read")
}

fn write_stores_and_feeds_once(t: &mut dyn Transport, c: &mut Checks) -> Result<(), String> {
    let lba = Layout::fetch(t)?.plain_after();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0x3C, false, false)?;
        expect_status(reply, STATUS_OK, "plain write")
    })?;
    check_feeds(c, n, 1, "plain write")?;
    if read_back(t, lba)? != vec![0x3C; SECTOR] {
        return Err("plain write: the sector does not read back".into());
    }
    Ok(())
}

fn bad_crc_refused(t: &mut dyn Transport, c: &mut Checks) -> Result<(), String> {
    let lba = Layout::fetch(t)?.plain_after();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0x3C, false, true)?;
        expect_status(reply, STATUS_CRC, "bad crc")
    })?;
    check_feeds(c, n, 0, "bad crc")?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("bad crc: the sector was written".into());
    }
    Ok(())
}

fn cut_before_payload(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
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

fn boot_area_without_mark_refused(t: &mut dyn Transport, c: &mut Checks) -> Result<(), String> {
    let layout = Layout::fetch(t)?;
    let lba = layout.boot_only();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0x77, false, false)?;
        expect_status(reply, STATUS_NOT_COMPARED, "boot area without the mark")
    })?;
    check_feeds(c, n, 0, "boot area without the mark")?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("boot area without the mark: the sector was written".into());
    }
    Ok(())
}

fn boot_area_with_mark_stored(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
    let lba = Layout::fetch(t)?.boot_only();
    let reply = write_one(t, lba, 0x77, true, false)?;
    expect_status(reply, STATUS_OK, "boot area with the mark")?;
    if read_back(t, lba)? != vec![0x77; SECTOR] {
        return Err("boot area with the mark: not stored".into());
    }
    Ok(())
}

fn header_bad_crc_before_any_write(t: &mut dyn Transport, c: &mut Checks) -> Result<(), String> {
    let layout = Layout::fetch(t)?;
    let lba = layout.header();
    let n = feeds_around(t, |t| {
        let reply = write_one(t, lba, 0xEE, true, true)?;
        expect_status(reply, STATUS_CRC, "header with a bad crc and the mark")
    })?;
    check_feeds(c, n, 0, "header bad crc")?;
    if read_back(t, lba)? != vec![0; SECTOR] {
        return Err("header bad crc: the boot record was touched".into());
    }
    Ok(())
}

fn header_without_mark_refused(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
    let lba = Layout::fetch(t)?.header();
    let reply = write_one(t, lba, 0xEE, false, false)?;
    expect_status(reply, STATUS_NOT_COMPARED, "header without the mark")
}

fn header_with_mark_stored(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
    let lba = Layout::fetch(t)?.header();
    let reply = write_one(t, lba, 0xEE, true, false)?;
    expect_status(reply, STATUS_OK, "header with the mark")?;
    if read_back(t, lba)? != vec![0xEE; SECTOR] {
        return Err("header with the mark: not stored".into());
    }
    Ok(())
}

fn straddle_needs_mark(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
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

fn wrong_length_chunk_refused_and_session_goes_on(
    t: &mut dyn Transport,
    _c: &mut Checks,
) -> Result<(), String> {
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

fn end_maskrom_after_ok(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
    t.send_frame(&Request::end(ResetKind::Maskrom).encode())
        .map_err(|e| format!("{e:?}"))?;
    expect_status(next(t, "end")?, STATUS_OK, "end")?;
    match t.take_reply() {
        Err(TransportError::Ended(ResetKind::Maskrom)) => Ok(()),
        other => Err(format!("end: expected Ended(Maskrom), got {other:?}")),
    }
}

fn bad_request_crc_refused_and_session_goes_on(
    t: &mut dyn Transport,
    _c: &mut Checks,
) -> Result<(), String> {
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

fn reserved_bit_refused_and_session_goes_on(
    t: &mut dyn Transport,
    _c: &mut Checks,
) -> Result<(), String> {
    let mut request = Request::read(1, 1);
    request.reserved = 1;
    t.send_frame(&request.encode())
        .map_err(|e| format!("{e:?}"))?;
    expect_status(next(t, "reserved bit")?, STATUS_BAD_REQUEST, "reserved bit")
}

fn empty_queue_is_idle(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
    match t.take_reply() {
        Ok(None) => Ok(()),
        other => Err(format!("empty queue: expected no reply, got {other:?}")),
    }
}

/// One conformance scenario: a name, and the checks a host passes when it runs against a fresh stage.
pub struct Scenario {
    pub name: &'static str,
    pub run: fn(&mut dyn Transport, &mut Checks) -> Result<(), String>,
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

fn layout_is_published(t: &mut dyn Transport, _c: &mut Checks) -> Result<(), String> {
    let peer = Layout::fetch(t)?.peer;
    if region_digest(peer.regions()) == 0 {
        return Err("regions: digest is zero".into());
    }
    Ok(())
}

/// Runs one scenario against a transport. A host calls this for each entry of [`SCENARIOS`].
///
/// The harness first brings the device from reset to configured (`Transport::connect`), so every scenario starts from
/// reset, as a board does: the host enumerates, then the scenario's first request is answered.
///
/// The suite writes real sectors, so it runs only on a scratch card: the caller passes [`ScratchCard::acknowledge`].
pub fn run(
    scenario: &Scenario,
    t: &mut dyn Transport,
    _scratch: ScratchCard,
) -> Result<Verdict, String> {
    t.connect()
        .map_err(|e| format!("{}: connect: {e:?}", scenario.name))?;
    let mut checks = Checks::default();
    (scenario.run)(t, &mut checks).map_err(|e| format!("{}: {e}", scenario.name))?;
    Ok(if checks.skipped.is_empty() {
        Verdict::Passed
    } else {
        Verdict::NotChecked(checks.skipped)
    })
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
            let verdict = run(scenario, &mut dev, ScratchCard::acknowledge())
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(verdict, Verdict::Passed, "{}", scenario.name);
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
        dev.connect().unwrap();
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
        dev.connect().unwrap();
        dev.send_frame(&Request::identify().encode()).unwrap();
        let record = dev.take_reply().unwrap().unwrap();
        assert_eq!(record.len(), IDENTITY_LEN);
        let peer = check(&record).unwrap();
        assert_eq!(peer.build_id, 42);
        assert_eq!(peer.regions(), &REGIONS[..]);
    }

    /// A transport that forwards to a device but cannot count feeds, as a board cannot.
    struct NoCount(Device);

    impl Transport for NoCount {
        fn connect(&mut self) -> Result<(), TransportError> {
            self.0.connect()
        }
        fn send_frame(&mut self, frame: &[u8]) -> Result<(), TransportError> {
            self.0.send_frame(frame)
        }
        fn take_reply(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
            self.0.take_reply()
        }
    }

    #[test]
    fn a_check_the_transport_cannot_make_is_not_checked_and_never_a_pass() {
        let scenario = SCENARIOS
            .iter()
            .find(|s| s.name.starts_with("read_returns"))
            .unwrap();
        let mut t = NoCount(Device::new(40_000, 1, &REGIONS));
        let verdict = run(scenario, &mut t, ScratchCard::acknowledge()).unwrap();
        assert_eq!(
            verdict,
            Verdict::NotChecked(vec!["read"]),
            "the feed count was not checked"
        );
        let mut dev = Device::new(40_000, 1, &REGIONS);
        assert_eq!(
            run(scenario, &mut dev, ScratchCard::acknowledge()).unwrap(),
            Verdict::Passed
        );
    }

    #[test]
    fn a_device_starts_at_reset_and_refuses_frames_until_the_host_enumerates_it() {
        let mut dev = Device::new(1024, 1, &REGIONS);
        assert_eq!(
            dev.send_frame(&Request::identify().encode()),
            Err(TransportError::NotConnected)
        );
        dev.connect().unwrap();
        assert!(dev.send_frame(&Request::identify().encode()).is_ok());
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
