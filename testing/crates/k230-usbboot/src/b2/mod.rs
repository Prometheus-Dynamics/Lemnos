//! The B2 host check: the first run of the FL1 stage's own USB device path (`testing/board-runs/K230-USB.md`,
//! "B2"). The board's check build (`lemnos_k230_usbboot_stage::fl1::check`) serves a RAM window and a record;
//! this module is the host's side of that run, over two traits so the same code talks to the board
//! ([`usb::Sysfs`]) and to the rehearsal's simulated board (`crate::b2sim`).
//!
//! The order, each step refusing or stopping before the next:
//!
//! 1. **Wait** for the stage at the port, bounded ([`Bounds::enumerate`]). Only the device at the named port
//!    is looked at; a device with the stage's ids at another port is noted and never opened, and if the
//!    bound passes with the stage only elsewhere the run is refused (wrong port).
//! 2. **Refuse** before claiming, from what the kernel read at enumeration (sysfs, no open): the product
//!    string must be the stage's and the interface string the wire's. Either differing refuses.
//! 3. **Access**: first wait, bounded ([`Bounds::udev`]), for udev to finish the device: its database
//!    entry exists and this user may read and write the node. The uaccess ACL is applied late in udev's
//!    rules, after the kernel has published the device, so an open right away is denied (B2 run 3, 0.649 s
//!    after the load). Still denied at the bound stops the run and says so. Nothing is opened before that,
//!    and nothing is changed on the host to get access.
//! 4. **Endpoint 0**: the descriptors as the device sends them, GET_STATUS, GET_CONFIGURATION, a request the
//!    device must stall, and a request after the stall.
//! 5. **Bulk**: identify (the check build's id, not the card build's), writes and reads in the RAM window at
//!    several sizes up to the largest, each checked by its CRC both ways and compared; a chunk with a bad CRC
//!    (status 1), the largest chunk and one byte (status 5, not cut to fit), 129 sectors (status 5); the
//!    record read back and checked (speed, SET_ADDRESS order, the bulk OUT re-arm after each frame); END.
//!    No request addresses a sector outside the window and the record unless [`Plan::outside_window`] is set
//!    (the rehearsal sets it; the board run does not).
//!    With [`Mode::Scratch`] step 5 is the card writes instead ([`scratch`]: the scratch build's identity, the
//!    range read before, written and read back, the deliberate writes outside it refused, the range zeroed and read
//!    back; END).
//! 6. **The way back**: the boot ROM at the port again, under a new device number, bounded ([`Bounds::back`]).
//!    Waited for after every outcome, refusals included: the board returns by itself.

pub mod scratch;
pub mod usb;

use std::time::{Duration, Instant};

use lemnos_flash_stage::usb::descriptors;
use lemnos_flash_wire::frame::{
    MAX_CHUNK, MAX_SECTORS, REQUEST_LEN, Request, ResetKind, SECTOR, STATUS_BAD_REQUEST,
    STATUS_CRC, STATUS_NOT_COMPARED, STATUS_OK, STATUS_STORAGE, crc16_ccitt, payload_crc,
};
use lemnos_flash_wire::identity;
use lemnos_flash_wire::usb::{
    BULK_IN, CONFIGURATION_VALUE, INTERFACE_NAME, LANGUAGE, PRODUCT, PRODUCT_ID, VENDOR_ID,
};
use lemnos_k230_usbboot_stage::fl1::check::{
    CHECK_BUILD_ID, CHECK_IDENTITY, CHECK_LBA, CHECK_SECTORS, CROSS_COUNTS, RECORD_LBA, record,
};

use crate::bootrom::{DOWNLOAD_PID, DOWNLOAD_VID};

/// The board's port (the bench's hub path to the board's USB port).
pub const PORT: &str = "3-4.2.1.4.1";
/// The board's lock: every contact with the board holds it.
pub const LOCK: &str = "/run/user/1000/lemnos-k230d.lock";

/// The waits, each a bound the run never passes.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// From the start command to the stage at the port. A good stage shows within a second (the
    /// rehearsal: 0.5 s); the stage itself waits for a configuration until its idle limit (30 s from
    /// its arming), then reports and resets, so a host that stops waiting earlier leaves nothing running.
    pub enumerate: Duration,
    /// One control transfer.
    pub control: Duration,
    /// One bulk transfer (`HOST_BULK_TIMEOUT_US`).
    pub bulk: Duration,
    /// From the end (or the stop) to the boot ROM back at the port.
    pub back: Duration,
    /// From the stage seen at the port (and its checks made) to udev having finished it: its database
    /// entry written and this user's access to the node granted.
    pub udev: Duration,
    /// Between two looks at the bus.
    pub poll: Duration,
}

impl Bounds {
    /// The board run's bounds. `back` covers the longest way back: the entry's own arming (setting 11) at
    /// the slowest clock the bench has measured (750 kHz, 179 s) if the stage hangs before it lowers the
    /// period, with margin. Every other way back is shorter: the stage's own report and reset (its idle
    /// limit, 30 s; its enumeration limit, 10 s) or the period it chose (at most twice the run budget).
    pub const BOARD: Bounds = Bounds {
        enumerate: Duration::from_secs(15),
        control: Duration::from_secs(1),
        bulk: Duration::from_secs(5),
        back: Duration::from_secs(200),
        udev: Duration::from_secs(5),
        poll: Duration::from_millis(100),
    };
}

/// One USB device as the kernel enumerated it (sysfs: read without opening the device).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// The port path (`3-4.2.1.4.1`).
    pub port: String,
    pub vid: u16,
    pub pid: u16,
    /// The product string the kernel read at enumeration.
    pub product: Option<String>,
    /// Interface 0's string, as the kernel read it.
    pub interface: Option<String>,
    /// The link speed in Mb/s, as the kernel states it (`480`).
    pub speed: Option<String>,
    /// The configuration the kernel set.
    pub configuration: Option<u8>,
    pub busnum: u32,
    pub devnum: u32,
}

impl Seen {
    /// The device's node (`/dev/bus/usb/BBB/DDD`).
    pub fn devnode(&self) -> String {
        format!("/dev/bus/usb/{:03}/{:03}", self.busnum, self.devnum)
    }

    fn ids(&self) -> String {
        format!("{:04x}:{:04x}", self.vid, self.pid)
    }

    fn is_stage(&self) -> bool {
        self.vid == VENDOR_ID && self.pid == PRODUCT_ID
    }

    fn is_rom(&self) -> bool {
        self.vid == DOWNLOAD_VID && self.pid == DOWNLOAD_PID
    }
}

/// Whether this user may open a device node, decided without opening it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    /// Not granted, with what the node's owner, mode and ACL say.
    Denied(String),
}

/// A transfer's failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// The device stalled the request.
    Stall,
    /// No answer within the transfer's bound.
    Timeout,
    /// The device left the bus.
    Gone,
    /// Anything else, as text.
    Other(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stall => write!(f, "stalled"),
            Self::Timeout => write!(f, "timed out"),
            Self::Gone => write!(f, "the device left the bus"),
            Self::Other(text) => write!(f, "{text}"),
        }
    }
}

/// The opened and claimed stage: endpoint 0 and the bulk pair.
pub trait Link {
    /// A control transfer with a data stage to the host (or none when `len` is 0).
    fn control_in(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, LinkError>;
    /// A control transfer with no data stage.
    fn control_none(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        timeout: Duration,
    ) -> Result<(), LinkError>;
    /// One bulk OUT transfer (one frame), on endpoint 0x01.
    fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<(), LinkError>;
    /// One bulk IN transfer, ended by a short packet, of at most `max` bytes, on endpoint 0x81.
    fn bulk_in(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>, LinkError>;
}

/// The bus as the host sees it.
pub trait Board {
    /// Every device on the bus now, as the kernel enumerated it. Opens nothing.
    fn scan(&mut self) -> Vec<Seen>;
    /// Whether this user may open `seen`'s node. Opens nothing.
    fn access(&mut self, seen: &Seen) -> Access;
    /// Whether udev has finished `seen`: its database entry exists. Opens nothing.
    fn udev_done(&mut self, seen: &Seen) -> bool;
    /// Opens `seen` and claims its interface 0. Called only after every refusal and the access check.
    fn open(&mut self, seen: &Seen) -> Result<Box<dyn Link>, String>;
}

/// Which build the run is for, and so which bulk checks it makes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The check build: the RAM window and the record (step 5 above).
    #[default]
    Check,
    /// The scratch build: the card writes inside the scratch range ([`scratch`]).
    Scratch,
}

/// What the run does.
#[derive(Clone, Debug)]
pub struct Plan {
    pub port: String,
    pub bounds: Bounds,
    /// Also send the requests that address sectors outside the window (each must be refused with status
    /// 2). The rehearsal sets it; the board's first run does not (no request outside the window). The check
    /// build's only.
    pub outside_window: bool,
    /// The build the run is for.
    pub mode: Mode,
}

impl Plan {
    /// The board's first run.
    pub fn board() -> Self {
        Self {
            port: PORT.to_string(),
            bounds: Bounds::BOARD,
            outside_window: false,
            mode: Mode::Check,
        }
    }

    /// The card write run: the scratch build at the board's port, with the board's bounds.
    pub fn scratch() -> Self {
        Self {
            mode: Mode::Scratch,
            ..Self::board()
        }
    }
}

/// How the run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every check was made and held.
    Passed,
    /// A device was refused before it was claimed (product, interface string or port).
    Refused(String),
    /// The run stopped before claiming for a reason that is the user's call (access).
    Stopped(String),
    /// The stage never showed at the port within the bound.
    GaveUp(String),
    /// A check failed; each failure is listed.
    Failed(Vec<String>),
}

/// The run's result.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub verdict: Verdict,
    /// The stage as the kernel saw it, if it showed.
    pub stage: Option<Seen>,
    /// The seconds from the start command to the stage at the port.
    pub stage_after_s: Option<f64>,
    /// The record read over USB.
    pub record: Option<[u32; record::WORDS]>,
    /// The boot ROM back at the port, and the seconds from the end (or stop) to it. `None`: not back within
    /// the bound.
    pub back: Option<(Seen, f64)>,
}

/// The host side of the run. `rom` is the boot ROM's device before the load (its number must change);
/// `started` is when the start command went out.
pub fn run(
    board: &mut dyn Board,
    plan: &Plan,
    rom: Option<&Seen>,
    started: Instant,
    log: &mut dyn FnMut(&str),
) -> Outcome {
    let mut outcome = Outcome {
        verdict: Verdict::Passed,
        stage: None,
        stage_after_s: None,
        record: None,
        back: None,
    };
    let mut old: Vec<u32> = rom.map(|r| r.devnum).into_iter().collect();
    outcome.verdict = match wait_stage(board, plan, &old, started, log) {
        Err(verdict) => verdict,
        Ok(seen) => {
            outcome.stage_after_s = Some(started.elapsed().as_secs_f64());
            log(&format!(
                "stage at {} after {:.3} s: {} devnum {} speed {:?} configuration {:?} product {:?} interface {:?}",
                seen.port,
                started.elapsed().as_secs_f64(),
                seen.ids(),
                seen.devnum,
                seen.speed,
                seen.configuration,
                seen.product,
                seen.interface
            ));
            old.push(seen.devnum);
            outcome.stage = Some(seen.clone());
            claim_and_check(board, plan, &seen, &mut outcome, log)
        }
    };
    log(&format!("verdict: {:?}", outcome.verdict));
    let from = Instant::now();
    match wait_back(board, plan, &old, from) {
        Ok(seen) => {
            let after = from.elapsed().as_secs_f64();
            log(&format!(
                "boot ROM back at {} as {} devnum {} (was {:?}) after {:.3} s",
                seen.port,
                seen.ids(),
                seen.devnum,
                old,
                after
            ));
            outcome.back = Some((seen, after));
        }
        Err(text) => log(&format!("WAY BACK: {text}")),
    }
    outcome
}

/// Step 1 and 2: the stage at the port, refused by product or interface string, or not there in time.
fn wait_stage(
    board: &mut dyn Board,
    plan: &Plan,
    old: &[u32],
    started: Instant,
    log: &mut dyn FnMut(&str),
) -> Result<Seen, Verdict> {
    let mut elsewhere: Vec<String> = Vec::new();
    let mut last_here: Option<String> = None;
    loop {
        let seen = board.scan();
        for other in seen.iter().filter(|s| s.is_stage() && s.port != plan.port) {
            if !elsewhere.contains(&other.port) {
                log(&format!(
                    "{} at {} (not the board's port {}): not ours, never opened",
                    other.ids(),
                    other.port,
                    plan.port
                ));
                elsewhere.push(other.port.clone());
            }
        }
        let here = seen.iter().find(|s| s.port == plan.port);
        let state = here.map(|s| format!("{} devnum {}", s.ids(), s.devnum));
        if state != last_here {
            log(&format!(
                "port {} at {:.3} s: {}",
                plan.port,
                started.elapsed().as_secs_f64(),
                state.as_deref().unwrap_or("empty")
            ));
            last_here = state;
        }
        // The stage's ids alone are not enough: its product (a locking attribute, read with a bound) must
        // have answered before it is judged; until then the wait goes on, within its bound.
        if let Some(here) = here.filter(|s| s.is_stage() && s.product.is_some()) {
            if here.product.as_deref() != Some(PRODUCT) {
                return Err(Verdict::Refused(format!(
                    "the device at {} has the stage's ids but the product {:?}, not {:?}: not claimed",
                    here.port, here.product, PRODUCT
                )));
            }
            let want = std::str::from_utf8(&INTERFACE_NAME).unwrap_or("");
            if here.interface.as_deref() != Some(want) {
                return Err(Verdict::Refused(format!(
                    "the device at {} has the interface string {:?}, not {:?}: another wire, not claimed",
                    here.port, here.interface, want
                )));
            }
            return Ok(here.clone());
        }
        if let Some(rom) = here.filter(|s| s.is_rom() && !old.contains(&s.devnum)) {
            return Err(Verdict::GaveUp(format!(
                "the board was back at the boot ROM (devnum {}) {:.3} s after the start, before the stage showed",
                rom.devnum,
                started.elapsed().as_secs_f64()
            )));
        }
        if started.elapsed() >= plan.bounds.enumerate {
            let text = format!(
                "no stage at {} within {:.1} s (the port shows {})",
                plan.port,
                plan.bounds.enumerate.as_secs_f64(),
                last_here.as_deref().unwrap_or("nothing")
            );
            return Err(if elsewhere.is_empty() {
                Verdict::GaveUp(text)
            } else {
                Verdict::Refused(format!(
                    "{text}; the stage's ids showed only at {elsewhere:?}: wrong port, not claimed"
                ))
            });
        }
        std::thread::sleep(plan.bounds.poll);
    }
}

/// Steps 3 to 5.
fn claim_and_check(
    board: &mut dyn Board,
    plan: &Plan,
    seen: &Seen,
    outcome: &mut Outcome,
    log: &mut dyn FnMut(&str),
) -> Verdict {
    let mut failures = Vec::new();
    if seen.speed.as_deref() != Some("480") {
        failures.push(format!("the kernel states speed {:?}, not 480", seen.speed));
    }
    if seen.configuration != Some(CONFIGURATION_VALUE) {
        failures.push(format!(
            "the kernel left configuration {:?}, not {}",
            seen.configuration, CONFIGURATION_VALUE
        ));
    }
    let asked = Instant::now();
    let access = loop {
        let done = board.udev_done(seen);
        let access = board.access(seen);
        if done && access == Access::Granted {
            break access;
        }
        if asked.elapsed() >= plan.bounds.udev {
            break match access {
                Access::Granted => Access::Denied(format!(
                    "udev has not finished the device (no database entry) within {:.1} s",
                    plan.bounds.udev.as_secs_f64()
                )),
                denied => denied,
            };
        }
        std::thread::sleep(plan.bounds.poll);
    };
    match access {
        Access::Granted => log(&format!(
            "access to {}: granted, udev done, {:.3} s after the checks",
            seen.devnode(),
            asked.elapsed().as_secs_f64()
        )),
        Access::Denied(why) => {
            return Verdict::Stopped(format!(
                "this user may not open {} ({why}); nothing on the host was changed to get access (no udev rule, \
                 group, chmod or sudo): granting it is the user's call",
                seen.devnode()
            ));
        }
    }
    let mut link = match board.open(seen) {
        Ok(link) => link,
        Err(text) => {
            return Verdict::Stopped(format!(
                "opening or claiming {} failed: {text}",
                seen.devnode()
            ));
        }
    };
    log("claimed interface 0");
    ep0_checks(&mut *link, &plan.bounds, &mut failures, log);
    let ended = match plan.mode {
        Mode::Check => bulk_checks(&mut *link, plan, seen, outcome, &mut failures, log),
        Mode::Scratch => scratch::checks(&mut *link, &plan.bounds, &mut failures, log),
    };
    if !ended {
        // Ask for the end anyway: the stage resets on it. If it cannot be sent, the stage's idle limit and its
        // watchdog are the way back.
        match end(&mut *link, &plan.bounds) {
            Ok(()) => log("END sent after the failure"),
            Err(text) => log(&format!("END after the failure: {text}")),
        }
    }
    if failures.is_empty() {
        Verdict::Passed
    } else {
        Verdict::Failed(failures)
    }
}

const GET_STATUS: u8 = 0;
const GET_DESCRIPTOR: u8 = 6;
const GET_CONFIGURATION: u8 = 8;
/// A vendor request the device does not have: it must stall.
const UNKNOWN_VENDOR_REQUEST: u8 = 0x5a;

/// Step 4.
fn ep0_checks(
    link: &mut dyn Link,
    bounds: &Bounds,
    failures: &mut Vec<String>,
    log: &mut dyn FnMut(&str),
) {
    let t = bounds.control;
    let expect = |log: &mut dyn FnMut(&str),
                  failures: &mut Vec<String>,
                  what: &str,
                  got: Result<Vec<u8>, LinkError>,
                  want: &[u8]| match got {
        Ok(bytes) if bytes == want => {
            log(&format!("ep0 {what}: {} bytes as expected", bytes.len()))
        }
        Ok(bytes) => {
            log(&format!("ep0 {what}: FAIL {}", crate::hex(&bytes)));
            failures.push(format!(
                "ep0 {what}: got {}, want {}",
                crate::hex(&bytes),
                crate::hex(want)
            ));
        }
        Err(e) => {
            log(&format!("ep0 {what}: FAIL {e}"));
            failures.push(format!("ep0 {what}: {e}"));
        }
    };
    expect(
        log,
        failures,
        "device descriptor (18)",
        link.control_in(0x80, GET_DESCRIPTOR, 0x0100, 0, 18, t),
        &descriptors::DEVICE,
    );
    expect(
        log,
        failures,
        "configuration descriptor (9)",
        link.control_in(0x80, GET_DESCRIPTOR, 0x0200, 0, 9, t),
        &descriptors::CONFIGURATION[..9],
    );
    expect(
        log,
        failures,
        "configuration descriptor (all, asked 255)",
        link.control_in(0x80, GET_DESCRIPTOR, 0x0200, 0, 255, t),
        &descriptors::CONFIGURATION,
    );
    expect(
        log,
        failures,
        "string 0 (languages)",
        link.control_in(0x80, GET_DESCRIPTOR, 0x0300, 0, 255, t),
        &descriptors::LANGUAGES,
    );
    expect(
        log,
        failures,
        "string 2 (product)",
        link.control_in(0x80, GET_DESCRIPTOR, 0x0302, LANGUAGE, 255, t),
        &descriptors::PRODUCT_STRING,
    );
    expect(
        log,
        failures,
        "string 3 (interface)",
        link.control_in(0x80, GET_DESCRIPTOR, 0x0303, LANGUAGE, 255, t),
        &descriptors::INTERFACE_STRING,
    );
    expect(
        log,
        failures,
        "GET_STATUS (device)",
        link.control_in(0x80, GET_STATUS, 0, 0, 2, t),
        &[0, 0],
    );
    expect(
        log,
        failures,
        "GET_STATUS (endpoint 0x81)",
        link.control_in(0x82, GET_STATUS, 0, u16::from(BULK_IN), 2, t),
        &[0, 0],
    );
    expect(
        log,
        failures,
        "GET_CONFIGURATION",
        link.control_in(0x80, GET_CONFIGURATION, 0, 0, 1, t),
        &[CONFIGURATION_VALUE],
    );
    match link.control_in(0xc0, UNKNOWN_VENDOR_REQUEST, 0, 0, 8, t) {
        Err(LinkError::Stall) => log("ep0 unknown vendor request: stalled, as it must"),
        other => {
            log(&format!("ep0 unknown vendor request: FAIL {other:?}"));
            failures.push(format!(
                "ep0 unknown vendor request: {other:?}, not a stall"
            ));
        }
    }
    match link.control_none(0x40, UNKNOWN_VENDOR_REQUEST, 0, 0, t) {
        Err(LinkError::Stall) => log("ep0 unknown vendor request (no data): stalled, as it must"),
        other => {
            log(&format!(
                "ep0 unknown vendor request (no data): FAIL {other:?}"
            ));
            failures.push(format!(
                "ep0 unknown vendor request (no data): {other:?}, not a stall"
            ));
        }
    }
    expect(
        log,
        failures,
        "GET_STATUS after the stalls",
        link.control_in(0x80, GET_STATUS, 0, 0, 2, t),
        &[0, 0],
    );
}

/// A payload of `sectors` sectors, different for each `seed`.
pub fn pattern(sectors: usize, seed: u8) -> Vec<u8> {
    (0..sectors * SECTOR)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) ^ ((i >> 9) as u8))
        .collect()
}

/// The largest reply: a read of the most sectors (status, payload, CRC).
pub(crate) const REPLY_MAX: usize = 1 + MAX_CHUNK;

/// Sends a write request and its chunk (`payload`, its CRC, then `extra` bytes; `corrupt` flips one payload
/// bit after the CRC is computed). Returns the status byte.
fn write_frames(
    link: &mut dyn Link,
    bounds: &Bounds,
    lba: u32,
    payload: &[u8],
    corrupt: bool,
    extra: usize,
) -> Result<u8, String> {
    let count = (payload.len() / SECTOR) as u16;
    let mut chunk = payload.to_vec();
    chunk.extend_from_slice(&payload_crc(payload));
    if corrupt {
        chunk[payload.len() / 2] ^= 0x10;
    }
    chunk.extend(std::iter::repeat_n(0x5a, extra));
    link.bulk_out(&Request::write(lba, count, false).encode(), bounds.bulk)
        .map_err(|e| format!("the write request: {e}"))?;
    link.bulk_out(&chunk, bounds.bulk)
        .map_err(|e| format!("the chunk ({} bytes): {e}", chunk.len()))?;
    status(link, bounds)
}

/// Sends a write request with the compared mark as given, and its chunk. Returns the status byte.
fn write_marked(
    link: &mut dyn Link,
    bounds: &Bounds,
    lba: u32,
    payload: &[u8],
    compared: bool,
) -> Result<u8, String> {
    let count = (payload.len() / SECTOR) as u16;
    let mut chunk = payload.to_vec();
    chunk.extend_from_slice(&payload_crc(payload));
    link.bulk_out(&Request::write(lba, count, compared).encode(), bounds.bulk)
        .map_err(|e| format!("the write request: {e}"))?;
    link.bulk_out(&chunk, bounds.bulk)
        .map_err(|e| format!("the chunk ({} bytes): {e}", chunk.len()))?;
    status(link, bounds)
}

fn status(link: &mut dyn Link, bounds: &Bounds) -> Result<u8, String> {
    let reply = link
        .bulk_in(REPLY_MAX, bounds.bulk)
        .map_err(|e| format!("the status: {e}"))?;
    match reply.as_slice() {
        [status] => Ok(*status),
        other => Err(format!("a status of {} bytes", other.len())),
    }
}

/// A read: the payload, with the reply's CRC (device to host) checked. `Err` carries a refusal's status
/// or the fault.
fn read_frames(
    link: &mut dyn Link,
    bounds: &Bounds,
    lba: u32,
    count: u16,
) -> Result<Result<Vec<u8>, u8>, String> {
    link.bulk_out(&Request::read(lba, count).encode(), bounds.bulk)
        .map_err(|e| format!("the read request: {e}"))?;
    let reply = link
        .bulk_in(REPLY_MAX, bounds.bulk)
        .map_err(|e| format!("the read reply: {e}"))?;
    let payload_len = usize::from(count) * SECTOR;
    if reply.len() == 1 {
        return Ok(Err(reply[0]));
    }
    if reply.len() != 3 + payload_len {
        return Err(format!(
            "a read reply of {} bytes, not {}",
            reply.len(),
            3 + payload_len
        ));
    }
    let crc = crc16_ccitt(&reply[..1 + payload_len]).to_be_bytes();
    if crc != reply[1 + payload_len..] {
        return Err(format!(
            "the read reply's CRC {} does not match its bytes ({}): corrupted on the way to the host",
            crate::hex(&reply[1 + payload_len..]),
            crate::hex(&crc)
        ));
    }
    if reply[0] != STATUS_OK {
        return Err(format!("a full read reply with status {}", reply[0]));
    }
    Ok(Ok(reply[1..1 + payload_len].to_vec()))
}

fn end(link: &mut dyn Link, bounds: &Bounds) -> Result<(), String> {
    link.bulk_out(&Request::end(ResetKind::Normal).encode(), bounds.bulk)
        .map_err(|e| format!("the END request: {e}"))?;
    match status(link, bounds)? {
        STATUS_OK => Ok(()),
        other => Err(format!("END answered status {other}")),
    }
}

/// The write sizes, in sectors: one, a few, one past a power of two, half, one short of the most, the most.
pub const SIZES: [u16; 6] = [1, 8, 9, 64, 127, 128];

/// Step 5. Returns whether END was sent and answered.
fn bulk_checks(
    link: &mut dyn Link,
    plan: &Plan,
    seen: &Seen,
    outcome: &mut Outcome,
    failures: &mut Vec<String>,
    log: &mut dyn FnMut(&str),
) -> bool {
    let b = &plan.bounds;
    // Identify first: nothing else is sent to a build that is not the check build.
    let reply = link
        .bulk_out(&Request::identify().encode(), b.bulk)
        .map_err(|e| format!("the identify request: {e}"))
        .and_then(|()| {
            link.bulk_in(REPLY_MAX, b.bulk)
                .map_err(|e| format!("the identity: {e}"))
        });
    match reply {
        Err(text) => {
            log(&format!("identify: FAIL {text}"));
            failures.push(format!("identify: {text}"));
            return false;
        }
        Ok(reply) => match identity::check(&reply) {
            Ok(peer) if peer.build_id == CHECK_BUILD_ID && reply == CHECK_IDENTITY => {
                log(&format!(
                    "identify: the check build {:#018x}, {} bytes, regions as published",
                    peer.build_id,
                    reply.len()
                ))
            }
            Ok(peer) => {
                let text = format!(
                    "identify: build {:#018x}, not the check build {CHECK_BUILD_ID:#018x}: nothing more is sent",
                    peer.build_id
                );
                log(&text);
                failures.push(text);
                return false;
            }
            Err(skew) => {
                let text = format!("identify: {skew:?}: nothing more is sent");
                log(&text);
                failures.push(text);
                return false;
            }
        },
    }
    let fail = |failures: &mut Vec<String>, log: &mut dyn FnMut(&str), text: String| {
        log(&format!("FAIL {text}"));
        failures.push(text);
    };
    // Writes and reads at each size, CRC checked both ways, compared.
    for (i, &count) in SIZES.iter().enumerate() {
        let payload = pattern(usize::from(count), i as u8 + 1);
        let lba = CHECK_LBA + (CHECK_SECTORS - u32::from(count)) * (i as u32 % 2);
        match write_frames(link, b, lba, &payload, false, 0) {
            Ok(STATUS_OK) => {}
            Ok(status) => {
                fail(
                    failures,
                    log,
                    format!("write {count} at {lba}: status {status}"),
                );
                continue;
            }
            Err(text) => {
                fail(failures, log, format!("write {count} at {lba}: {text}"));
                return false;
            }
        }
        match read_frames(link, b, lba, count) {
            Ok(Ok(back)) if back == payload => log(&format!(
                "write and read {count} sectors at {lba}: status 0, CRC held both ways, read back equal"
            )),
            Ok(Ok(_)) => fail(
                failures,
                log,
                format!("read {count} at {lba}: differs from what was written"),
            ),
            Ok(Err(status)) => fail(
                failures,
                log,
                format!("read {count} at {lba}: status {status}"),
            ),
            Err(text) => {
                fail(failures, log, format!("read {count} at {lba}: {text}"));
                return false;
            }
        }
    }
    // A bad CRC: refused with status 1, and nothing stored.
    let before = read_frames(link, b, CHECK_LBA, 8);
    let bad = pattern(8, 0xc3);
    match write_frames(link, b, CHECK_LBA, &bad, true, 0) {
        Ok(STATUS_CRC) => {
            log("a chunk with one bit flipped after its CRC: status 1 (CRC), as it must")
        }
        other => fail(
            failures,
            log,
            format!("the corrupted chunk: {other:?}, not status 1"),
        ),
    }
    match (before, read_frames(link, b, CHECK_LBA, 8)) {
        (Ok(Ok(a)), Ok(Ok(c))) if a == c => log("the window is unchanged after the refused chunk"),
        other => fail(
            failures,
            log,
            format!("the window after the refused chunk: {other:?}"),
        ),
    }
    // The largest chunk and one byte: refused by its length, not cut to fit.
    let most = pattern(usize::from(MAX_SECTORS), 0x77);
    let before = read_frames(link, b, CHECK_LBA, MAX_SECTORS);
    match write_frames(link, b, CHECK_LBA, &most, false, 1) {
        Ok(STATUS_BAD_REQUEST) => log(&format!(
            "a chunk of {} bytes (the most and one): status 5, as it must",
            MAX_CHUNK + 1
        )),
        other => fail(
            failures,
            log,
            format!("the chunk of the most and one byte: {other:?}, not status 5"),
        ),
    }
    match (before, read_frames(link, b, CHECK_LBA, MAX_SECTORS)) {
        (Ok(Ok(a)), Ok(Ok(c))) if a == c => log("the window is unchanged after the oversize chunk"),
        other => fail(
            failures,
            log,
            format!("the window after the oversize chunk: {other:?}"),
        ),
    }
    // 129 sectors: the request itself is refused (no chunk follows).
    let mut over = Request::write(CHECK_LBA, MAX_SECTORS, false).encode();
    over[4..6].copy_from_slice(&(MAX_SECTORS + 1).to_le_bytes());
    let crc = payload_crc(&over[..10]);
    over[10..12].copy_from_slice(&crc);
    match link
        .bulk_out(&over, b.bulk)
        .map_err(|e| e.to_string())
        .and_then(|()| status(link, b))
    {
        Ok(STATUS_BAD_REQUEST) => log("a request for 129 sectors: status 5, as it must"),
        other => fail(
            failures,
            log,
            format!("the request for 129 sectors: {other:?}, not status 5"),
        ),
    }
    // A short request frame.
    match link
        .bulk_out(&Request::identify().encode()[..REQUEST_LEN - 1], b.bulk)
        .map_err(|e| e.to_string())
        .and_then(|()| status(link, b))
    {
        Ok(STATUS_BAD_REQUEST) => log("a request of 11 bytes: status 5, as it must"),
        other => fail(
            failures,
            log,
            format!("the request of 11 bytes: {other:?}, not status 5"),
        ),
    }
    if plan.outside_window {
        for (what, lba) in [
            ("before the window", CHECK_LBA - 1),
            ("the record", RECORD_LBA),
        ] {
            match write_frames(link, b, lba, &pattern(1, 9), false, 0) {
                Ok(STATUS_STORAGE) => log(&format!("a write to {what} ({lba}): status 2, refused")),
                other => fail(
                    failures,
                    log,
                    format!("a write to {what} ({lba}): {other:?}, not status 2"),
                ),
            }
        }
        // Sector 0 is in the boot area: the region gate refuses a write without the compared mark (status
        // 4) before the storage is reached.
        match write_frames(link, b, 0, &pattern(1, 9), false, 0) {
            Ok(STATUS_NOT_COMPARED) => {
                log("a write to sector 0: status 4, refused by the region gate")
            }
            other => fail(
                failures,
                log,
                format!("a write to sector 0: {other:?}, not status 4"),
            ),
        }
        match read_frames(link, b, 0, 1) {
            Ok(Err(STATUS_STORAGE)) => log("a read of sector 0: status 2, refused"),
            other => fail(
                failures,
                log,
                format!("a read of sector 0: {other:?}, not status 2"),
            ),
        }
    }
    // The session still serves after every refusal.
    match link
        .bulk_out(&Request::identify().encode(), b.bulk)
        .map_err(|e| e.to_string())
        .and_then(|()| link.bulk_in(REPLY_MAX, b.bulk).map_err(|e| e.to_string()))
    {
        Ok(reply) if reply == CHECK_IDENTITY => log("identify after the refusals: the same record"),
        other => fail(
            failures,
            log,
            format!("identify after the refusals: {other:?}"),
        ),
    }
    // The record.
    match read_frames(link, b, RECORD_LBA, 1) {
        Ok(Ok(sector)) => match record::parse(&sector) {
            Some(words) => {
                for (name, word) in record::NAMES.iter().zip(words) {
                    log(&format!("record {name} {word:#010x} ({word})"));
                }
                for text in record_checks(&words, seen) {
                    fail(failures, log, text);
                }
                outcome.record = Some(words);
            }
            None => fail(failures, log, "the record sector has no magic".to_string()),
        },
        other => fail(failures, log, format!("the record: {other:?}")),
    }
    match end(link, b) {
        Ok(()) => {
            log("END: status 0; the stage reports and resets");
            true
        }
        Err(text) => {
            fail(failures, log, format!("END: {text}"));
            false
        }
    }
}

/// The bench checks the record must hold, against what the kernel saw. Each failure is a line.
pub fn record_checks(words: &[u32; record::WORDS], seen: &Seen) -> Vec<String> {
    let w = |name: &str| words[record::index(name)];
    let mut out = Vec::new();
    let mut want = |ok: bool, text: String| {
        if !ok {
            out.push(text);
        }
    };
    want(
        w("speed") == 0,
        format!(
            "record speed {} (DSTS ENUMSPD), not 0 (high speed)",
            w("speed")
        ),
    );
    // The bus address is not the kernel's device number under xHCI: the host controller chooses it
    // (its Address Device command) and it is not visible to this check (B2 run 2: address 17, devnums
    // 119-122). What can be checked is the stage's own side: DCFG holds the address SET_ADDRESS gave.
    let _ = seen;
    want(
        w("set-address") != 0,
        "record set-address is 0 (no SET_ADDRESS was taken)".to_string(),
    );
    want(
        (w("dcfg-after-address") >> 4) & 0x7f == w("set-address"),
        format!(
            "DCFG after SET_ADDRESS {:#x} does not hold the address SET_ADDRESS gave, {}",
            w("dcfg-after-address"),
            w("set-address")
        ),
    );
    want(
        w("address-status-in") == 1,
        "the SET_ADDRESS status stage was not seen after the address was written".to_string(),
    );
    want(
        w("configured") == 1,
        "record configured is not 1".to_string(),
    );
    want(w("bus-resets") >= 1, "record bus-resets is 0".to_string());
    want(w("reset-done") != 0, "record reset-done is 0".to_string());
    want(
        w("out-frames") >= 1 && w("out-rearms") == w("out-frames"),
        format!(
            "bulk OUT re-arms {} differ from the frames taken {} (one re-arm per frame)",
            w("out-rearms"),
            w("out-frames")
        ),
    );
    want(
        w("out-longest") as usize == MAX_CHUNK + 1,
        format!(
            "record out-longest {} is not the oversize chunk's {} (taken whole, not cut)",
            w("out-longest"),
            MAX_CHUNK + 1
        ),
    );
    want(
        w("out-oversize") == 0,
        "a frame ran past the bulk buffer".to_string(),
    );
    want(
        w("stalls") >= 2,
        format!("record stalls {} (two asked for)", w("stalls")),
    );
    want(
        w("ep0-status-out") >= 1,
        "no endpoint 0 OUT status stage was counted".to_string(),
    );
    // The timebase cross-check: the watchdog counted its counts and `time` moved over them, at a rate
    // the measured clock agrees with (`tb-cross-us` within a quarter of counts / wdt-hz).
    let cross_ok = w("tb-cross-counts") >= CROSS_COUNTS && w("tb-cross-us") != 0;
    want(
        cross_ok,
        format!(
            "record timebase cross-check {} counts in {} us (the timebase was not shown to move)",
            w("tb-cross-counts"),
            w("tb-cross-us")
        ),
    );
    if cross_ok && w("wdt-hz") != 0 {
        let expect = u64::from(w("tb-cross-counts")) * 1_000_000 / u64::from(w("wdt-hz"));
        let got = u64::from(w("tb-cross-us"));
        want(
            got * 4 >= expect * 3 && got * 4 <= expect * 5,
            format!(
                "record timebase cross-check {got} us for {} counts, the measured clock gives {expect} us",
                w("tb-cross-counts")
            ),
        );
    }
    want(
        w("wdt-top") <= 15,
        format!(
            "record wdt-top {:#x}: no period covered the run",
            w("wdt-top")
        ),
    );
    want(
        w("rounds") >= 1,
        "record rounds is 0 (no verified round)".to_string(),
    );
    out
}

/// Step 6: the boot ROM at the port under a device number not in `old`.
fn wait_back(
    board: &mut dyn Board,
    plan: &Plan,
    old: &[u32],
    from: Instant,
) -> Result<Seen, String> {
    loop {
        if let Some(seen) = board
            .scan()
            .into_iter()
            .find(|s| s.port == plan.port && s.is_rom() && !old.contains(&s.devnum))
        {
            return Ok(seen);
        }
        if from.elapsed() >= plan.bounds.back {
            return Err(format!(
                "the boot ROM did not come back at {} within {:.0} s",
                plan.port,
                plan.bounds.back.as_secs_f64()
            ));
        }
        std::thread::sleep(plan.bounds.poll);
    }
}

/// The record words the console printed (`b2 <name> <hex>`), by name.
pub fn console_words(lines: &[String]) -> Vec<(String, u32)> {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("b2 ")?;
            let (name, hex) = rest.split_once(' ')?;
            let value = u32::from_str_radix(hex.trim(), 16).ok()?;
            record::NAMES
                .contains(&name)
                .then(|| (name.to_string(), value))
        })
        .collect()
}

/// The words that cannot change after the record was read over USB: the console must print the same.
pub const STABLE_WORDS: [&str; 15] = [
    "magic",
    "gsnpsid",
    "ghwcfg3",
    "gahbcfg-found",
    "reset-done",
    "dsts",
    "speed",
    "set-address",
    "dcfg-after-address",
    "configured",
    "wdt-first",
    "wdt-last",
    "wdt-hz",
    "tb-cross-counts",
    "tb-cross-us",
];

/// Where the console's record and the USB record disagree on a stable word, or the console lacks one.
pub fn console_agrees(console: &[(String, u32)], usb: &[u32; record::WORDS]) -> Vec<String> {
    STABLE_WORDS
        .iter()
        .filter_map(|&name| {
            let want = usb[record::index(name)];
            match console.iter().find(|(n, _)| n == name) {
                Some((_, v)) if *v == want => None,
                Some((_, v)) => Some(format!("console {name} {v:#x}, USB record {want:#x}")),
                None => Some(format!("console printed no {name}")),
            }
        })
        .collect()
}
