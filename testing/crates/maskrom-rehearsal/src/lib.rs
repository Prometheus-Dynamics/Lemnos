//! A same-rev stand-in for the boot ROM of the maskrom path of the MR1 board, and the rehearsal script that runs against it.
//!
//! The stand-in models only what the MR1 script depends on: the ROM's maskrom entry, the 471 stub's watchdog (arm
//! and spin, or arm and return), the 472 loader's acceptance, and the watchdog reset that ends each stage. The 472
//! session itself is the chip stage's own serve loop, run through `lemnos-flash-stage`'s simulated device, with the
//! stage's own regions and build id. Nothing here touches a board.
//!
//! Each [`Behaviour`] states what the ROM does with the two unknowns the plan names. [`Behaviour::Faithful`] is the
//! plan's expectation. The other two are the failures the script must detect: a ROM whose watchdog never resets after
//! a 471 spin (the plan's U2 case), and a ROM that refuses a 472 image (U1's neighbour). The rehearsal passes only
//! when the script reports the failure it is given.
//!
//! The stand-in's clock is the stand-in's own; it advances only in [`Stand::observe`]. Timing is the plan's numbers,
//! not a measurement.

use lemnos_flash_stage::sim::{Device, Transport, TransportError};
use lemnos_flash_wire::frame::{Request, ResetKind, SECTOR, STATUS_CRC, STATUS_OK, payload_crc};
use lemnos_rockchip_maskrom_stage::entry472::{BUILD_ID, REGIONS};

/// The 471 stub's watchdog period: TOP 15 at 24 MHz, 2^31 / 24e6 us (plan section 1).
pub const WDT_471_US: u64 = 89_478_485;
/// The 472 session's watchdog period: TOP 12 at 24 MHz, 2^28 / 24e6 us (plan section 3.1).
pub const WDT_472_US: u64 = 11_184_810;
/// The window the script gives a 471 to reset the board (plan step 1a: within 90 s).
pub const RESET_WINDOW_US: u64 = 90_000_000;
/// The scratch card's size in sectors. The rehearsal writes only past the boot area (sector 32768).
pub const CARD_SECTORS: u32 = 40_000;
/// The first plain sector the rehearsal writes: past the boot area, where a write needs no compared mark.
pub const TEST_LBA: u32 = 40_000 - 8;

/// What the stand-in's ROM does with the plan's unknowns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behaviour {
    /// The plan's expectation: the 471 spin resets, the 471 return keeps the watchdog running, and the 472 is accepted
    /// after a 471 that returned.
    Faithful,
    /// The 471 arm-and-spin never resets the board. This is the case the script must report as a failure at step 1a.
    NoResetAfterSpin,
    /// The ROM refuses a 472 image of ours. The script must report it as a failure at step 2.
    Refuses472,
}

/// One of the two 471 entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stub {
    /// Arms the watchdog and spins. Never returns.
    ArmAndSpin,
    /// Arms the watchdog and returns to the ROM.
    ArmAndReturn,
}

/// A refusal the ROM gives. The board stays in maskrom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A 471 was sent when the ROM was not in maskrom.
    NotInMaskrom,
    /// A 472 was sent when the ROM had not returned from a 471.
    NoStubReturned,
    /// The ROM refused the 472 image.
    Rom472,
    /// A frame was sent before a 472 session was loaded.
    NoSession,
}

/// What a window of time showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    /// The watchdog reset the board; it now runs the system from its SD card.
    Reset,
    /// No reset within the window.
    NoReset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Power {
    /// The system is running from the SD card.
    Booted,
    /// The ROM is in maskrom, waiting for a host.
    Maskrom,
    /// A 471 spins; only its watchdog can end it.
    Spinning,
    /// A 471 returned to the ROM; a 472 may follow.
    Returned,
}

/// The stand-in board: the ROM, its watchdog, and a 472 session when one is loaded.
pub struct Stand {
    behaviour: Behaviour,
    power: Power,
    /// Absolute time of the next watchdog reset, in microseconds from the stand-in's start, if armed.
    reset_at: Option<u64>,
    now: u64,
    session: Option<Device>,
    feeds_seen: u32,
}

impl Stand {
    /// The board as the running system leaves it, before the maskrom step (plan step E).
    pub fn from_running_system(behaviour: Behaviour) -> Self {
        Self {
            behaviour,
            power: Power::Booted,
            reset_at: None,
            now: 0,
            session: None,
            feeds_seen: 0,
        }
    }

    /// Plan step E: the running system enters maskrom. Valid only from the running system or from maskrom.
    pub fn enter_maskrom(&mut self) -> Result<(), Refused> {
        match self.power {
            Power::Booted | Power::Maskrom => {
                self.power = Power::Maskrom;
                self.reset_at = None;
                self.session = None;
                Ok(())
            }
            _ => Err(Refused::NotInMaskrom),
        }
    }

    /// Sends a 471 stub. It is accepted only in maskrom.
    pub fn send_471(&mut self, stub: Stub) -> Result<(), Refused> {
        if self.power != Power::Maskrom {
            return Err(Refused::NotInMaskrom);
        }
        match stub {
            Stub::ArmAndSpin => {
                self.power = Power::Spinning;
                self.reset_at = match self.behaviour {
                    Behaviour::NoResetAfterSpin => None,
                    _ => Some(self.now + WDT_471_US),
                };
            }
            Stub::ArmAndReturn => {
                self.power = Power::Returned;
                self.reset_at = Some(self.now + WDT_471_US);
            }
        }
        Ok(())
    }

    /// Sends a 472 image after a 471 returned. The stage arms its watchdog first (TOP 12), then connects as a device.
    pub fn send_472(&mut self) -> Result<(), Refused> {
        if self.power != Power::Returned {
            return Err(Refused::NoStubReturned);
        }
        if self.behaviour == Behaviour::Refuses472 {
            return Err(Refused::Rom472);
        }
        let mut device = Device::new(CARD_SECTORS, BUILD_ID, &REGIONS);
        device
            .connect()
            .map_err(|_: TransportError| Refused::NoSession)?;
        self.session = Some(device);
        self.feeds_seen = 0;
        self.reset_at = Some(self.now + WDT_472_US);
        Ok(())
    }

    /// Advances the stand-in's clock by `window_us`. A watchdog reset inside the window ends the stage and boots the
    /// system; otherwise the board is still where it was.
    pub fn observe(&mut self, window_us: u64) -> Observed {
        let end = self.now + window_us;
        match self.reset_at {
            Some(at) if at <= end => {
                self.now = at;
                self.power = Power::Booted;
                self.reset_at = None;
                self.session = None;
                Observed::Reset
            }
            _ => {
                self.now = end;
                Observed::NoReset
            }
        }
    }

    /// Whether the system is running from the SD card.
    pub fn booted(&self) -> bool {
        self.power == Power::Booted
    }

    /// Whether the ROM is in maskrom.
    pub fn in_maskrom(&self) -> bool {
        self.power == Power::Maskrom
    }

    /// The 472 session's feeds so far.
    pub fn feed_count(&self) -> u32 {
        self.session.as_ref().map_or(0, |s| s.feeds())
    }

    /// Re-arms the watchdog at the stage's period whenever the session's feed count rises: the feed rule.
    fn sync_feeds(&mut self) {
        let feeds = self.feed_count();
        if feeds > self.feeds_seen {
            self.feeds_seen = feeds;
            self.reset_at = Some(self.now + WDT_472_US);
        }
    }
}

impl Transport for Stand {
    fn connect(&mut self) -> Result<(), TransportError> {
        match self.session.as_mut() {
            Some(device) => device.connect(),
            None => Err(TransportError::NotConnected),
        }
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        match self.session.as_mut() {
            Some(device) => device.send_frame(frame),
            None => Err(TransportError::NotConnected),
        }
    }

    fn take_reply(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        let reply = match self.session.as_mut() {
            Some(device) => device.take_reply(),
            None => Err(TransportError::NotConnected),
        };
        self.sync_feeds();
        reply
    }

    fn feeds(&self) -> Option<u32> {
        self.session.as_ref().map(|s| s.feeds())
    }
}

/// A step of the script that did not do what the plan says it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub step: &'static str,
    pub reason: String,
}

fn fail(step: &'static str, reason: impl Into<String>) -> Failure {
    Failure {
        step,
        reason: reason.into(),
    }
}

/// The steps the script ran, each with its observed result, in order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<String>,
}

fn exchange(t: &mut dyn Transport, frame: &[u8]) -> Result<Option<Vec<u8>>, String> {
    t.send_frame(frame).map_err(|e| format!("send: {e:?}"))?;
    t.take_reply().map_err(|e| format!("reply: {e:?}"))
}

/// Writes one sector. The reply is the stage's status and nothing else.
fn write_sector(
    t: &mut dyn Transport,
    lba: u32,
    bytes: &[u8],
    crc_flip: bool,
) -> Result<u8, String> {
    t.send_frame(&Request::write(lba, 1, false).encode())
        .map_err(|e| format!("write request: {e:?}"))?;
    let mut chunk = bytes.to_vec();
    let mut crc = payload_crc(bytes);
    if crc_flip {
        crc[1] ^= 1;
    }
    chunk.extend_from_slice(&crc);
    t.send_frame(&chunk).map_err(|e| format!("chunk: {e:?}"))?;
    match t.take_reply().map_err(|e| format!("write reply: {e:?}"))? {
        Some(reply) if reply.len() == 1 => Ok(reply[0]),
        other => Err(format!("write reply: not one status byte: {other:?}")),
    }
}

fn read_sector(t: &mut dyn Transport, lba: u32) -> Result<Vec<u8>, String> {
    match exchange(t, &Request::read(lba, 1).encode())? {
        Some(reply) if reply.first() == Some(&STATUS_OK) && reply.len() == 1 + SECTOR + 2 => {
            Ok(reply[1..1 + SECTOR].to_vec())
        }
        other => Err(format!("read {lba}: not an ok reply: {other:?}")),
    }
}

/// The rehearsal: plan steps E, 1a, 1b, 2, 3, 4 and 5, in the plan's order, against a stand-in. Step 2 and step 3 are
/// rehearsed here; their board runs stay unrun until a reviewer's word (MR1.md).
pub fn run_script(stand: &mut Stand) -> Result<Report, Failure> {
    let mut report = Report::default();

    // E: the running system enters maskrom.
    stand
        .enter_maskrom()
        .map_err(|r| fail("E", format!("{r:?}")))?;
    if !stand.in_maskrom() {
        return Err(fail("E", "not in maskrom after the magic and the reboot"));
    }
    report.lines.push("E: in maskrom".into());

    // 1a: the 471 arm-and-spin must reset the board within the window.
    stand
        .send_471(Stub::ArmAndSpin)
        .map_err(|r| fail("1a", format!("471 spin refused: {r:?}")))?;
    if stand.observe(RESET_WINDOW_US) != Observed::Reset {
        return Err(fail("1a", "no reset within 90 s after the 471 spin"));
    }
    if !stand.booted() {
        return Err(fail("1a", "reset did not boot the system"));
    }
    report
        .lines
        .push("1a: 471 spin reset the board; system booted".into());

    // 1b: the 471 arm-and-return; the watchdog must survive the ROM's return and reset the board.
    stand
        .enter_maskrom()
        .map_err(|r| fail("1b", format!("{r:?}")))?;
    stand
        .send_471(Stub::ArmAndReturn)
        .map_err(|r| fail("1b", format!("471 return refused: {r:?}")))?;
    if stand.observe(RESET_WINDOW_US) != Observed::Reset {
        return Err(fail("1b", "watchdog did not survive the 471 return"));
    }
    report
        .lines
        .push("1b: 471 return; watchdog reset the board".into());

    // 2: the 472 loader arms, brings up its session, and hangs with no frame. Its watchdog resets the board.
    stand
        .enter_maskrom()
        .map_err(|r| fail("2", format!("{r:?}")))?;
    stand
        .send_471(Stub::ArmAndReturn)
        .map_err(|r| fail("2", format!("471 return refused: {r:?}")))?;
    stand
        .send_472()
        .map_err(|r| fail("2", format!("472 refused: {r:?}")))?;
    if stand.feed_count() != 0 {
        return Err(fail(
            "2",
            "the hang fed the watchdog before any verified chunk",
        ));
    }
    if stand.observe(WDT_472_US + 1_000_000) != Observed::Reset {
        return Err(fail("2", "no reset after the 472 hang"));
    }
    report
        .lines
        .push("2: 472 hang; watchdog reset with no feed and no write".into());

    // 3: the same-bytes round trip. Read a baseline, write the same bytes back, read again, compare.
    stand
        .enter_maskrom()
        .map_err(|r| fail("3", format!("{r:?}")))?;
    stand
        .send_471(Stub::ArmAndReturn)
        .map_err(|r| fail("3", format!("471 return refused: {r:?}")))?;
    stand
        .send_472()
        .map_err(|r| fail("3", format!("472 refused: {r:?}")))?;
    let mut baseline = Vec::new();
    for lba in TEST_LBA..TEST_LBA + 4 {
        baseline.push(read_sector(stand, lba).map_err(|e| fail("3", e))?);
    }
    for (i, lba) in (TEST_LBA..TEST_LBA + 4).enumerate() {
        let status = write_sector(stand, lba, &baseline[i], false).map_err(|e| fail("3", e))?;
        if status != STATUS_OK {
            return Err(fail("3", format!("write {lba}: status {status}")));
        }
    }
    for (i, lba) in (TEST_LBA..TEST_LBA + 4).enumerate() {
        if read_sector(stand, lba).map_err(|e| fail("3", e))? != baseline[i] {
            return Err(fail(
                "3",
                format!("sector {lba} differs after the same-bytes write"),
            ));
        }
    }
    if stand.feed_count() != 12 {
        return Err(fail(
            "3",
            format!(
                "feeds {} for 4 baseline reads, 4 writes and 4 verify reads",
                stand.feed_count()
            ),
        ));
    }
    let _ = exchange(stand, &Request::end(ResetKind::Normal).encode());
    if stand.observe(WDT_472_US + 1_000_000) != Observed::Reset {
        return Err(fail("3", "no reset after the END"));
    }
    report
        .lines
        .push("3: same-bytes round trip equal (4 sectors, byte compare)".into());

    // 4: a cut mid-write: the request is sent, its payload never is. The watchdog resets; re-entry reads the baseline.
    stand
        .enter_maskrom()
        .map_err(|r| fail("4", format!("{r:?}")))?;
    stand
        .send_471(Stub::ArmAndReturn)
        .map_err(|r| fail("4", format!("471 return refused: {r:?}")))?;
    stand
        .send_472()
        .map_err(|r| fail("4", format!("472 refused: {r:?}")))?;
    stand
        .send_frame(&Request::write(TEST_LBA, 1, false).encode())
        .map_err(|e| fail("4", format!("{e:?}")))?;
    if stand.observe(WDT_472_US + 1_000_000) != Observed::Reset {
        return Err(fail("4", "no reset after the cut"));
    }
    stand
        .enter_maskrom()
        .map_err(|r| fail("4", format!("{r:?}")))?;
    stand
        .send_471(Stub::ArmAndReturn)
        .map_err(|r| fail("4", format!("471 return refused: {r:?}")))?;
    stand
        .send_472()
        .map_err(|r| fail("4", format!("472 refused: {r:?}")))?;
    for (i, lba) in (TEST_LBA..TEST_LBA + 4).enumerate() {
        if read_sector(stand, lba).map_err(|e| fail("4", e))? != baseline[i] {
            return Err(fail("4", format!("sector {lba} changed by the cut")));
        }
    }
    stand.observe(WDT_472_US + 1_000_000);
    report
        .lines
        .push("4: cut mid-write; reset; re-entry reads the baseline".into());

    // 5: a chunk with a wrong CRC is refused before any write, and does not feed.
    stand
        .enter_maskrom()
        .map_err(|r| fail("5", format!("{r:?}")))?;
    stand
        .send_471(Stub::ArmAndReturn)
        .map_err(|r| fail("5", format!("471 return refused: {r:?}")))?;
    stand
        .send_472()
        .map_err(|r| fail("5", format!("472 refused: {r:?}")))?;
    let before = stand.feed_count();
    let status = write_sector(stand, TEST_LBA, &[0xA5; SECTOR], true).map_err(|e| fail("5", e))?;
    if status != STATUS_CRC {
        return Err(fail(
            "5",
            format!("bad chunk: status {status}, expected {STATUS_CRC}"),
        ));
    }
    if stand.feed_count() != before {
        return Err(fail("5", "a refused chunk fed the watchdog"));
    }
    if read_sector(stand, TEST_LBA).map_err(|e| fail("5", e))? != baseline[0] {
        return Err(fail("5", "a refused chunk reached storage"));
    }
    report
        .lines
        .push("5: wrong-CRC chunk refused; no feed; nothing stored".into());

    // The end of the rehearsal: a normal END, and the board boots.
    let _ = exchange(stand, &Request::end(ResetKind::Normal).encode());
    stand.observe(WDT_472_US + 1_000_000);
    report.lines.push("end: rehearsal complete".into());
    Ok(report)
}
