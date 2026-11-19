//! Simulators. [`ScriptedIo`] answers a fixed script, for the protocol tests. [`SimBoard`] is a
//! simulated K230: its boot ROM, its stage (the `lemnos-k230-usbboot-stage` code, the same code the
//! board runs), its two watchdog models, the SoC software reset, and a medium that can lose power at
//! any operation or byte, and can show the faults a real medium shows. One virtual clock runs all.
//!
//! The ROM's behaviour is modelled, not measured. Where the vendor sources do not say what the ROM
//! does, the model takes the weakest assumption that still makes the proofs hold, and the
//! UNVERIFIED facts are listed in `docs/flashing.md`.
//!
//! Reset causes, all of them a reset with no hands: a watchdog runs out (either one), the entry's
//! arming readback fails (the SoC reset), or the deadline after the last kick passes (the SoC
//! reset). Each is a fault a test can set ([`SimBoard::set_wdt_fault`], [`SimBoard::set_reset_lost`]).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use lemnos_k230_usbboot_stage::format::{self, LOADER_VERSION, STAGE_IDENT, rom_accepts};
use lemnos_k230_usbboot_stage::medium::{Geometry, Medium, MediumError};
use lemnos_k230_usbboot_stage::stage::{NoReply, Reboot, Stage};
use lemnos_k230_usbboot_stage::wdt::{self, Dog, Fault, Model, RegAccess};

use crate::bootrom::{
    DOWNLOAD_PID, DOWNLOAD_VID, GET_CPU_INFO, PROG_START, SET_DATA_ADDRESS, TEXT_BOOT_ROM,
};
use crate::transport::{UsbError, UsbIo};

/// How long the boot ROM takes to come back after a reset, in microseconds (modelled).
pub const BOOT_US: u64 = 200_000;
/// The bulk timeout the simulated device reports, in milliseconds.
const TIMEOUT_MS: u32 = 1_000;
/// The step a waiting host takes while it watches for the device, in microseconds.
const WAIT_STEP_US: u64 = 10_000;

/// One answer of a [`ScriptedIo`] script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// An OUT transfer is accepted.
    Ok,
    /// An IN transfer returns these bytes.
    Data(Vec<u8>),
    /// The device stalls the request.
    Stall,
    /// No answer in time.
    Timeout,
    /// The device leaves the bus (and stays gone).
    Disconnect,
}

/// A transfer a [`ScriptedIo`] completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transfer {
    ControlIn {
        request: u8,
        value: u16,
        index: u16,
        len: u16,
    },
    ControlOut {
        request: u8,
        value: u16,
        index: u16,
        data: Vec<u8>,
    },
    BulkOut(Vec<u8>),
    BulkIn(usize),
    WaitForDevice(u32),
}

/// A USB device that answers a fixed script, one answer per transfer, and records what completed.
/// Its re-enumeration answer is set by [`ScriptedIo::reattach`] (the download IDs by default).
pub struct ScriptedIo {
    answers: VecDeque<Answer>,
    records: Vec<Transfer>,
    gone: bool,
    reattach: Result<(u16, u16), UsbError>,
}

impl ScriptedIo {
    /// A device that answers `answers` in order, and re-enumerates with the download IDs.
    pub fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: answers.into_iter().collect(),
            records: Vec::new(),
            gone: false,
            reattach: Ok((DOWNLOAD_VID, DOWNLOAD_PID)),
        }
    }

    /// Sets what [`UsbIo::wait_for_device`] answers: the IDs it comes back with, or the error.
    pub fn reattach(mut self, answer: Result<(u16, u16), UsbError>) -> Self {
        self.reattach = answer;
        self
    }

    /// The transfers the host attempted, in order (a stalled or refused one included).
    pub fn records(&self) -> &[Transfer] {
        &self.records
    }

    /// Whether the device has left the bus.
    pub fn is_gone(&self) -> bool {
        self.gone
    }

    /// Takes the next answer. `expect_data` says whether the transfer is an IN.
    fn take(&mut self, expect_data: bool) -> Result<Vec<u8>, UsbError> {
        if self.gone {
            return Err(UsbError::Disconnected);
        }
        let answer = self
            .answers
            .pop_front()
            .ok_or_else(|| UsbError::Other("the script ran out of answers".into()))?;
        match answer {
            Answer::Disconnect => {
                self.gone = true;
                Err(UsbError::Disconnected)
            }
            Answer::Stall => Err(UsbError::Stall),
            Answer::Timeout => Err(UsbError::Timeout(TIMEOUT_MS)),
            Answer::Ok if expect_data => Err(UsbError::Protocol(
                "the script had an OK where data was due".into(),
            )),
            Answer::Ok => Ok(Vec::new()),
            Answer::Data(_) if !expect_data => Err(UsbError::Protocol(
                "the script had data where an OK was due".into(),
            )),
            Answer::Data(bytes) => Ok(bytes),
        }
    }
}

impl UsbIo for ScriptedIo {
    fn control_in(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
    ) -> Result<Vec<u8>, UsbError> {
        self.records.push(Transfer::ControlIn {
            request,
            value,
            index,
            len,
        });
        self.take(true)
    }

    fn control_out(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), UsbError> {
        self.records.push(Transfer::ControlOut {
            request,
            value,
            index,
            data: data.to_vec(),
        });
        self.take(false).map(drop)
    }

    fn bulk_out(&mut self, data: &[u8]) -> Result<(), UsbError> {
        self.records.push(Transfer::BulkOut(data.to_vec()));
        self.take(false).map(drop)
    }

    fn bulk_in(&mut self, len: usize) -> Result<Vec<u8>, UsbError> {
        self.records.push(Transfer::BulkIn(len));
        self.take(true)
    }

    fn max_packet_out(&self) -> usize {
        512
    }

    fn wait_for_device(&mut self, timeout_ms: u32) -> Result<(u16, u16), UsbError> {
        self.records.push(Transfer::WaitForDevice(timeout_ms));
        self.gone = false;
        self.reattach.clone()
    }
}

/// A medium operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
    Erase,
}

/// A hook that may change the medium before a read: it gets the read's offset and the bytes.
pub type Hook = Box<dyn FnMut(Op, u64, &mut Vec<u8>)>;

/// What the start request does on the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartReply {
    /// The start resets the board (the normal case): the request sees a disconnect.
    Disconnect,
    /// The start request returns normally, and the board resets just after.
    Ok,
    /// The start request stalls; the board does not reset, and the image does not run.
    Stall,
}

/// The board's clock, its medium, its watchdogs and its power, shared by the handles.
struct Shared {
    /// The virtual time.
    now_us: u64,
    /// The virtual time the device last reset.
    reset_at: u64,
    medium: Vec<u8>,
    geometry: Geometry,
    clock_hz: u64,
    /// `wdt0` then `wdt1`.
    wdt: [Model; 2],
    wdt_faults: [Fault; 2],
    /// The WDT resets do not reach the board (a fault the deadline must catch).
    reset_lost: bool,
    /// The last kick, in virtual time (the deadline counts from it).
    last_kick_us: u64,
    kicks_seen: u64,
    /// True while the stage is armed and running (the deadline applies).
    armed: bool,
    powered: bool,
    ops: u64,
    /// (operation index, bytes applied before the power goes): a cut at that operation.
    cut: Option<(u64, u64)>,
    op_us: u64,
    write_us_per_kib: u64,
    read_stall_us: u64,
    drop_writes: bool,
    hook: Option<Hook>,
}

impl Shared {
    fn kicks(&self) -> u64 {
        self.wdt.iter().map(Model::kicks).sum()
    }
}

/// The board's reset: the power goes off, and the ROM runs again after [`BOOT_US`].
fn reset_board(s: &mut Shared) {
    s.powered = false;
    s.reset_at = s.now_us;
    s.armed = false;
}

/// Charges `us` to the clock, runs both watchdogs and the deadline. Returns true when the board
/// resets: a watchdog runs out (unless its reset is lost), or the deadline passes after the last kick.
fn spend(s: &mut Shared, us: u64) -> bool {
    // A kick since the last charge happened at the start of this one (kicks are noticed at the next
    // charge, so the deadline is counted from the last kick as the board saw it, to within one op).
    if s.kicks() != s.kicks_seen {
        s.kicks_seen = s.kicks();
        s.last_kick_us = s.now_us;
    }
    s.now_us += us;
    let mut fired = false;
    for model in s.wdt.iter_mut() {
        fired |= model.advance_us(us);
    }
    let deadline = s.armed && s.now_us >= s.last_kick_us + wdt::DEADLINE_US;
    let reset = (fired && !s.reset_lost) || deadline;
    if reset {
        reset_board(s);
    }
    reset
}

/// The medium the stage writes, over the board's shared state.
pub struct SimMedium {
    shared: Rc<RefCell<Shared>>,
}

/// Decides how an operation of `len` bytes goes: `None` when it completes, `Some(bytes)` when the
/// power goes after `bytes` of it. Charges its time, and may cut the power.
fn gate(s: &mut Shared, op: Op, len: u64) -> Result<Option<u64>, MediumError> {
    if !s.powered {
        return Err(MediumError("the power is gone"));
    }
    let index = s.ops;
    s.ops += 1;
    let mut cost = s.op_us;
    if op == Op::Write {
        cost += len * s.write_us_per_kib / 1024;
    }
    if op == Op::Read {
        cost += s.read_stall_us;
    }
    let reset = spend(s, cost);
    let mut cut = None;
    if let Some((at, bytes)) = s.cut
        && at == index
    {
        cut = Some(bytes.min(len));
    }
    if reset && cut.is_none() {
        cut = Some(len / 2);
    }
    if cut.is_some() {
        reset_board(s);
    }
    Ok(cut)
}

impl Medium for SimMedium {
    fn geometry(&self) -> Geometry {
        self.shared.borrow().geometry
    }

    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), MediumError> {
        let mut guard = self.shared.borrow_mut();
        let s = &mut *guard;
        let cut = gate(s, Op::Read, out.len() as u64)?;
        if let Some(mut hook) = s.hook.take() {
            hook(Op::Read, offset, &mut s.medium);
            s.hook = Some(hook);
        }
        if cut.is_some() {
            return Err(MediumError("the power was cut during the read"));
        }
        let end = offset as usize + out.len();
        if end > s.medium.len() {
            return Err(MediumError("past the end of the medium"));
        }
        out.copy_from_slice(&s.medium[offset as usize..end]);
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), MediumError> {
        let mut guard = self.shared.borrow_mut();
        let s = &mut *guard;
        let cut = gate(s, Op::Write, data.len() as u64)?;
        let end = offset as usize + data.len();
        if end > s.medium.len() {
            return Err(MediumError("past the end of the medium"));
        }
        match cut {
            Some(bytes) => {
                let bytes = bytes as usize;
                s.medium[offset as usize..offset as usize + bytes].copy_from_slice(&data[..bytes]);
                Err(MediumError("the power was cut during the write"))
            }
            None if s.drop_writes => {
                // The fault: the medium keeps half and reports success.
                let half = data.len() / 2;
                s.medium[offset as usize..offset as usize + half].copy_from_slice(&data[..half]);
                Ok(())
            }
            None => {
                s.medium[offset as usize..end].copy_from_slice(data);
                Ok(())
            }
        }
    }

    fn erase(&mut self, offset: u64, len: u64) -> Result<(), MediumError> {
        let mut guard = self.shared.borrow_mut();
        let s = &mut *guard;
        let cut = gate(s, Op::Erase, len)?;
        let end = offset as usize + len as usize;
        if end > s.medium.len() {
            return Err(MediumError("past the end of the medium"));
        }
        match cut {
            Some(bytes) => {
                let end = offset as usize + bytes as usize;
                s.medium[offset as usize..end].fill(0xff);
                Err(MediumError("the power was cut during the erase"))
            }
            None => {
                s.medium[offset as usize..end].fill(0xff);
                Ok(())
            }
        }
    }

    /// The simulated medium is a flat store for the legacy protocol, not the card: it has only its end. The card's
    /// declared range and protected sectors are `SdMedium`'s, tested over the stage's own SD code.
    fn permits(&self, offset: u64, len: u64) -> Result<(), MediumError> {
        let end = offset
            .checked_add(len)
            .ok_or(MediumError("past the end of the medium"))?;
        if end > self.shared.borrow().medium.len() as u64 {
            return Err(MediumError("past the end of the medium"));
        }
        Ok(())
    }
}

/// The watchdogs' registers, on the board's shared models: each address goes to the model whose
/// window it is in.
pub struct SharedWdt(Rc<RefCell<Shared>>);

impl SharedWdt {
    fn slot(addr: usize) -> Option<usize> {
        wdt::BASES
            .iter()
            .position(|&base| (base..base + 0x800).contains(&addr))
    }
}

impl RegAccess for SharedWdt {
    fn read32(&mut self, addr: usize) -> u32 {
        match Self::slot(addr) {
            Some(i) => self.0.borrow_mut().wdt[i].read32(addr),
            None => 0,
        }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        if let Some(i) = Self::slot(addr) {
            self.0.borrow_mut().wdt[i].write32(addr, value);
        }
    }
}

/// The board's configuration.
#[derive(Clone, Debug)]
pub struct SimConfig {
    /// The medium's size in bytes.
    pub medium_bytes: usize,
    /// The medium's block size.
    pub block: u64,
    /// The medium's erase size.
    pub erase: u64,
    /// The WDT clock, in hertz (UNVERIFIED on the board; see `wdt`).
    pub clock_hz: u64,
    /// The time an operation takes, in microseconds, whatever its size.
    pub op_us: u64,
    /// The time a write takes per KiB, in microseconds.
    pub write_us_per_kib: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            medium_bytes: 1 << 20,
            block: 512,
            erase: 4096,
            clock_hz: 24_000_000,
            op_us: 50,
            write_us_per_kib: 100,
        }
    }
}

/// The board's state, as the host would find it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardState {
    /// The boot ROM's download mode.
    Rom,
    /// Our stage, armed.
    Stage,
    /// The ROM booted the medium's image: the device has left USB.
    Booted,
    /// The board is resetting and the ROM is not back yet.
    Absent,
}

enum Mode {
    Rom,
    Stage(Box<Stage<SimMedium, Dog<SharedWdt>>>),
    /// The stage image in report mode: it answers the queued records and feeds nothing; wdt0 returns the board.
    Report {
        next: usize,
    },
    Absent,
}

/// The tag at the start of the sim's report image: the sim's ROM runs an image that begins with it in report
/// mode. The image carries the mode block (the host patches it), so it is a real patch target.
pub const SIM_REPORT_TAG: &[u8] = b"lemnos-sim-report";

/// The report image, as the sim's ROM loads it: the tag, the mode block's magic and zeroed fields, and padding.
pub fn sim_report_image() -> Vec<u8> {
    let mut image = vec![0u8; 2500];
    image[..SIM_REPORT_TAG.len()].copy_from_slice(SIM_REPORT_TAG);
    let at = 64;
    image[at..at + 4].copy_from_slice(&lemnos_k230_usbboot_stage::image::BLOCK_MAGIC.to_le_bytes());
    image
}

/// A simulated K230: the boot ROM, the stage, the watchdogs and the medium, on one clock.
pub struct SimBoard {
    shared: Rc<RefCell<Shared>>,
    mode: Mode,
    ram_addr: Option<u32>,
    ram: Vec<u8>,
    stage_image: Vec<u8>,
    /// The RAM image the next reset starts (set by the start request).
    pending: Option<Vec<u8>>,
    booted: bool,
    identity: (u16, u16),
    stage_ident: String,
    start_reply: StartReply,
    /// The stage does not come back after the start (the device stays off the bus).
    stage_never_returns: bool,
    deaf_after_us: Option<u64>,
    report_records: Vec<[u8; 60]>,
}

/// The stage's image, as the simulated ROM loads it. Any bytes will do; the simulator checks that
/// the host loads exactly these.
pub fn sim_stage_image() -> Vec<u8> {
    b"lemnos-sim-stage "
        .iter()
        .cycle()
        .take(2500)
        .copied()
        .collect()
}

/// A handle on the board's medium, for a test to change a byte the way a medium can decay.
#[derive(Clone)]
pub struct MediumHandle(Rc<RefCell<Shared>>);

impl MediumHandle {
    /// Sets the byte at `offset` (no operation, no time: the medium changed underneath).
    pub fn poke(&self, offset: u64, value: u8) {
        self.0.borrow_mut().medium[offset as usize] = value;
    }
}

/// A handle on the board's clock: the host's waits advance it.
#[derive(Clone)]
pub struct SimClock(Rc<RefCell<Shared>>);

impl SimClock {
    /// The virtual time, in microseconds.
    pub fn now_us(&self) -> u64 {
        self.0.borrow().now_us
    }

    /// Lets `us` microseconds pass on the board. A reset (a watchdog, the deadline) takes the power.
    pub fn advance_us(&self, us: u64) {
        let mut s = self.0.borrow_mut();
        spend(&mut s, us);
    }

    /// The number of medium operations so far (usable while a burner holds the board).
    pub fn ops(&self) -> u64 {
        self.0.borrow().ops
    }

    /// Adds `us` to every medium read from now on (a stalled medium). Usable while a burner holds
    /// the board.
    pub fn stall_reads(&self, us: u64) {
        self.0.borrow_mut().read_stall_us = us;
    }
}

impl SimBoard {
    /// A board in the ROM's download mode, with a blank (erased) medium.
    pub fn new(config: &SimConfig) -> Self {
        let geometry = Geometry {
            capacity: config.medium_bytes as u64,
            block: config.block,
            erase: config.erase,
        };
        let shared = Shared {
            now_us: 0,
            reset_at: 0,
            medium: vec![0xff; config.medium_bytes],
            geometry,
            clock_hz: config.clock_hz,
            wdt: [
                Model::new(wdt::BASE0, config.clock_hz, Fault::None),
                Model::new(wdt::BASE1, config.clock_hz, Fault::None),
            ],
            wdt_faults: [Fault::None; 2],
            reset_lost: false,
            last_kick_us: 0,
            kicks_seen: 0,
            armed: false,
            powered: true,
            ops: 0,
            cut: None,
            op_us: config.op_us,
            write_us_per_kib: config.write_us_per_kib,
            read_stall_us: 0,
            drop_writes: false,
            hook: None,
        };
        Self {
            shared: Rc::new(RefCell::new(shared)),
            mode: Mode::Rom,
            ram_addr: None,
            ram: Vec::new(),
            stage_image: sim_stage_image(),
            pending: None,
            booted: false,
            identity: (DOWNLOAD_VID, DOWNLOAD_PID),
            stage_ident: STAGE_IDENT.to_string(),
            start_reply: StartReply::Disconnect,
            stage_never_returns: false,
            deaf_after_us: None,
            report_records: Vec::new(),
        }
    }

    /// A handle on the board's clock.
    pub fn clock(&self) -> SimClock {
        SimClock(Rc::clone(&self.shared))
    }

    /// A handle on the medium, to change it outside any operation.
    pub fn medium_handle(&self) -> MediumHandle {
        MediumHandle(Rc::clone(&self.shared))
    }

    /// The medium's bytes.
    pub fn medium(&self) -> Vec<u8> {
        self.shared.borrow().medium.clone()
    }

    /// The number of medium operations so far.
    pub fn ops(&self) -> u64 {
        self.shared.borrow().ops
    }

    /// Cuts the power at medium operation `op`, after `bytes` of it.
    pub fn cut_at(&self, op: u64, bytes: u64) {
        self.shared.borrow_mut().cut = Some((op, bytes));
    }

    /// Makes writes silently keep only half their bytes (a fault a read-back must catch).
    pub fn drop_writes(&self, on: bool) {
        self.shared.borrow_mut().drop_writes = on;
    }

    /// Adds `us` to every medium read (a stalled medium, which a VERIFY must not outlast).
    pub fn stall_reads(&self, us: u64) {
        self.shared.borrow_mut().read_stall_us = us;
    }

    /// Sets watchdog `index` (0 for `wdt0`, 1 for `wdt1`) to `fault`, from now on.
    pub fn set_wdt_fault(&self, index: usize, fault: Fault) {
        let mut s = self.shared.borrow_mut();
        s.wdt_faults[index] = fault;
        let base = wdt::BASES[index];
        let hz = s.clock_hz;
        s.wdt[index] = Model::new(base, hz, fault);
    }

    /// Makes the watchdogs' resets not reach the board: the counters run and fire into nothing.
    pub fn set_reset_lost(&self, lost: bool) {
        self.shared.borrow_mut().reset_lost = lost;
    }

    /// The records the report image answers, in order (set before the start that runs the report).
    pub fn set_report_records(&mut self, records: Vec<[u8; 60]>) {
        self.report_records = records;
    }

    /// Sets what the start request does.
    pub fn set_start_reply(&mut self, reply: StartReply) {
        self.start_reply = reply;
    }

    /// Makes the stage fail to come back after its start: the device stays off the bus.
    pub fn set_stage_never_returns(&mut self, on: bool) {
        self.stage_never_returns = on;
    }

    /// The ROM goes deaf this long after its reset: its control and bulk transfers fail (EPROTO, as the
    /// bench saw), and the device stays enumerated. `None` (the default) never goes deaf.
    pub fn set_deaf_after(&mut self, us: Option<u64>) {
        self.deaf_after_us = us;
    }

    fn deaf(&self) -> bool {
        match (self.deaf_after_us, &self.mode) {
            (Some(after), Mode::Rom) => {
                let s = self.shared.borrow();
                s.now_us >= s.reset_at + after
            }
            _ => false,
        }
    }

    /// Sets the IDs the device re-enumerates with (the download IDs by default).
    pub fn set_identity(&mut self, vid: u16, pid: u16) {
        self.identity = (vid, pid);
    }

    /// Sets the text the stage answers its CPU info with (the stage's identity by default).
    pub fn set_stage_ident(&mut self, text: &str) {
        self.stage_ident = text.to_string();
    }

    /// Installs a hook that may change the medium before each read.
    pub fn set_hook(&self, hook: Hook) {
        self.shared.borrow_mut().hook = Some(hook);
    }

    /// The board's state now.
    pub fn state(&mut self) -> BoardState {
        let _ = self.power();
        match self.mode {
            Mode::Rom => BoardState::Rom,
            Mode::Stage(_) | Mode::Report { .. } => BoardState::Stage,
            Mode::Absent if self.booted => BoardState::Booted,
            Mode::Absent => BoardState::Absent,
        }
    }

    /// Applies a reset that happened since the last call. The boot ROM then decides: a RAM image
    /// the start request loaded runs (the stage, or another image, which the ROM answers again to);
    /// otherwise the medium's header decides whether the ROM boots it or waits in download mode.
    /// Errors while the device is absent (being reset, or booted from the medium).
    fn power(&mut self) -> Result<(), UsbError> {
        let powered = self.shared.borrow().powered;
        if !powered {
            self.mode = Mode::Absent;
            self.ram_addr = None;
            self.ram.clear();
            let mut s = self.shared.borrow_mut();
            let header = rom_accepts(&s.medium[..format::BOOT_HEADER_LEN]);
            self.booted = self.pending.is_none() && header;
            let hz = s.clock_hz;
            for i in 0..2 {
                let fault = s.wdt_faults[i];
                s.wdt[i] = Model::new(wdt::BASES[i], hz, fault);
            }
            s.kicks_seen = 0;
            s.powered = true;
        }
        if let Mode::Absent = self.mode {
            let (now, reset_at) = {
                let s = self.shared.borrow();
                (s.now_us, s.reset_at)
            };
            if self.booted || now < reset_at + BOOT_US {
                return Err(UsbError::Disconnected);
            }
            self.mode = Mode::Rom;
            if let Some(image) = self.pending.take() {
                self.run_image(image);
                if let Mode::Absent = self.mode {
                    return Err(UsbError::Disconnected);
                }
            }
        }
        Ok(())
    }

    /// The boot ROM runs a RAM image it was told to start. Our stage arms the watchdogs and reads
    /// them back; a failed readback takes the SoC reset. Any other image is not ours: the ROM answers
    /// again (the board resets into download mode with the image gone).
    fn run_image(&mut self, image: Vec<u8>) {
        if image.starts_with(SIM_REPORT_TAG) {
            // Report mode: wdt0 armed at TOP 11 (as the image's mode arms it), no spin, no feed.
            let mut regs = SharedWdt(Rc::clone(&self.shared));
            regs.write32(wdt::BASE0 + wdt::TORR, wdt::torr(wdt::TOP));
            regs.write32(wdt::BASE0 + wdt::CRR, wdt::KICK);
            regs.write32(wdt::BASE0 + wdt::CR, wdt::CR_ENABLE);
            self.mode = Mode::Report { next: 0 };
            return;
        }
        if image != self.stage_image {
            reset_board(&mut self.shared.borrow_mut());
            return;
        }
        if self.stage_never_returns {
            // The stage stops answering and the board does not come back: the test's no-return case.
            self.booted = true;
            self.mode = Mode::Absent;
            return;
        }
        let mut regs = SharedWdt(Rc::clone(&self.shared));
        match wdt::arm(&mut regs) {
            Ok(armed) => {
                let medium = SimMedium {
                    shared: Rc::clone(&self.shared),
                };
                let mut s = self.shared.borrow_mut();
                s.armed = true;
                s.last_kick_us = s.now_us;
                drop(s);
                self.mode = Mode::Stage(Box::new(Stage::new(medium, Dog::new(regs, armed))));
            }
            Err(_) => {
                // The readback did not take: the second path, the SoC reset, with no spin.
                self.soc_reset();
            }
        }
    }

    /// The SoC software reset (`soc_glb_rst`, `lemnos_k230_usbboot_stage::sysctl`). It does not use
    /// the watchdogs, so it is taken whatever they did.
    fn soc_reset(&mut self) {
        reset_board(&mut self.shared.borrow_mut());
        self.mode = Mode::Absent;
    }

    /// The power off after a reboot command: the medium's header decides what the ROM does.
    fn power_off(&mut self) {
        reset_board(&mut self.shared.borrow_mut());
    }
}

fn cpu_info(text: &str, len: usize) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.resize(len, 0);
    bytes
}

impl UsbIo for SimBoard {
    fn control_in(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
    ) -> Result<Vec<u8>, UsbError> {
        let _ = index;
        self.power()?;
        if self.deaf() {
            return Err(UsbError::Deaf);
        }
        let len = len as usize;
        match &self.mode {
            // The real ROM answers the four bytes of its text, not a padded 32 (bench: "K230", 4b323330).
            Mode::Rom if request == GET_CPU_INFO && value == 0 => {
                Ok(TEXT_BOOT_ROM.as_bytes()[..len.min(TEXT_BOOT_ROM.len())].to_vec())
            }
            Mode::Report { .. } if request == GET_CPU_INFO && value == 0 => {
                Ok(cpu_info(STAGE_IDENT, len))
            }
            Mode::Stage(_) if request == GET_CPU_INFO && value == 0 => {
                Ok(cpu_info(&self.stage_ident, len))
            }
            Mode::Stage(_) if request == GET_CPU_INFO && value == 1 => {
                Ok(LOADER_VERSION.to_le_bytes().to_vec())
            }
            _ => Err(UsbError::Stall),
        }
    }

    fn control_out(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), UsbError> {
        self.power()?;
        if self.deaf() {
            return Err(UsbError::Deaf);
        }
        let _ = data;
        if !matches!(self.mode, Mode::Rom) {
            return Err(UsbError::Stall);
        }
        let address = (u32::from(value) << 16) | u32::from(index);
        match request {
            SET_DATA_ADDRESS => {
                self.ram_addr = Some(address);
                self.ram.clear();
                Ok(())
            }
            PROG_START => {
                if self.ram_addr != Some(address) {
                    return Err(UsbError::Stall);
                }
                match self.start_reply {
                    StartReply::Stall => Err(UsbError::Stall),
                    reply => {
                        self.pending = Some(self.ram.clone());
                        self.power_off();
                        if reply == StartReply::Ok {
                            Ok(())
                        } else {
                            Err(UsbError::Disconnected)
                        }
                    }
                }
            }
            _ => Err(UsbError::Stall),
        }
    }

    fn bulk_out(&mut self, data: &[u8]) -> Result<(), UsbError> {
        self.power()?;
        if self.deaf() {
            return Err(UsbError::Deaf);
        }
        match &mut self.mode {
            Mode::Rom => {
                if self.ram_addr.is_none() {
                    return Err(UsbError::Stall);
                }
                self.ram.extend_from_slice(data);
                Ok(())
            }
            Mode::Stage(stage) => {
                let outcome = stage.on_out(data);
                if let Err(Reboot) = outcome {
                    self.power_off();
                    return Err(UsbError::Disconnected);
                }
                if !self.shared.borrow().powered {
                    return Err(UsbError::Disconnected);
                }
                Ok(())
            }
            // The report image reads its records; a host write to it is ignored.
            Mode::Report { .. } => Ok(()),
            Mode::Absent => Err(UsbError::Disconnected),
        }
    }

    fn bulk_in(&mut self, len: usize) -> Result<Vec<u8>, UsbError> {
        self.power()?;
        if self.deaf() {
            return Err(UsbError::Deaf);
        }
        match &mut self.mode {
            Mode::Report { next } => {
                // The next record; a host that does not read leaves the queue where it is.
                let Some(packet) = self.report_records.get(*next) else {
                    return Err(UsbError::Timeout(TIMEOUT_MS));
                };
                let packet = packet.to_vec();
                *next += 1;
                Ok(packet)
            }
            Mode::Stage(stage) => {
                let mut buf = vec![0u8; len];
                match stage.on_in(&mut buf) {
                    Ok(n) => {
                        if !self.shared.borrow().powered {
                            return Err(UsbError::Disconnected);
                        }
                        buf.truncate(n);
                        Ok(buf)
                    }
                    Err(NoReply) => Err(UsbError::Timeout(TIMEOUT_MS)),
                }
            }
            _ => Err(UsbError::Stall),
        }
    }

    fn max_packet_out(&self) -> usize {
        512
    }

    fn wait_for_device(&mut self, timeout_ms: u32) -> Result<(u16, u16), UsbError> {
        let budget_us = u64::from(timeout_ms) * 1000;
        let start = self.shared.borrow().now_us;
        loop {
            if self.power().is_ok() {
                return Ok(self.identity);
            }
            let waited = self.shared.borrow().now_us - start;
            if waited >= budget_us {
                return Err(UsbError::Timeout(timeout_ms));
            }
            let step = WAIT_STEP_US.min(budget_us - waited);
            spend(&mut self.shared.borrow_mut(), step);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripted_device_answers_its_reattach_ids() {
        let mut io = ScriptedIo::new([]);
        assert_eq!(io.wait_for_device(1), Ok((DOWNLOAD_VID, DOWNLOAD_PID)));
    }
}
