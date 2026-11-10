//! The driver's work, as a state machine that does at most one USB transfer per tick.
//!
//! Finding the device out, taking its video interfaces from the host's own driver and claiming
//! them, probe and commit, the alternate setting and each isochronous transfer all happen here.
//! Nothing here keeps anything across a restart of the driver. Any failure backs off, capped at
//! 8 s, and starts again from the last step that still holds. It never gives up and never switches
//! itself off.
//!
//! A device that has gone (unplugged, or come back from a reset as a new device) makes every call
//! on its handle fail with `Unavailable` from then on. The session then forgets everything it
//! knew about the device, reports [`GONE`], and keeps retrying from the start; the contract's
//! methods answer `NO_DEVICE`, which is how the host learns to drop the driver and bind it again,
//! opening the device afresh by its name.

use core::time::Duration;

use lemnos_core::ErrorKind;
use lemnos_hal::blocks::{ControlSetup, IsoPacket, StreamFrame};
use lemnos_hal::{StreamEndpoint, UsbDevice};

use crate::descriptors::{self, Alt, Camera, MAX_CONFIG_BYTES, Mode};
use crate::payload::{Assembler, Frame, Stats};
use crate::probe::{self, CONTROL_MAX, CONTROL_MIN};

/// The USB device, as this module sees it.
pub type Usb = dyn UsbDevice<Error = ErrorKind> + Send;
/// The stream, as this module sees it.
pub type Stream = dyn StreamEndpoint<Error = ErrorKind> + Send;

/// Isochronous packets per transfer: 4 ms at high speed.
pub const PACKETS: usize = 32;

const CONTROL_TIMEOUT: Duration = Duration::from_millis(500);
const ISO_TIMEOUT: Duration = Duration::from_millis(100);

const NS_PER_MS: u64 = 1_000_000;
/// The first wait after a failure. Each further failure doubles it.
pub const BACKOFF_FIRST_NS: u64 = 1_000 * NS_PER_MS;
/// The longest wait after a failure.
pub const BACKOFF_CAP_NS: u64 = 8_000 * NS_PER_MS;

pub const STARTING: &str = "state: reading the device";
pub const IDLE: &str = "state: idle";
pub const NEGOTIATING: &str = "state: negotiating the format";
pub const STREAMING: &str = "state: streaming";
pub const STOPPING: &str = "state: stopping";

pub const CLAIM_BUSY: &str = "a video interface is held by someone else: retrying";
pub const DETACH_FAILED: &str = "the host's own driver could not be taken off the video interfaces: \
                                 retrying";
pub const GONE: &str = "the device is gone: retrying, and waiting to be opened again by name";
pub const SILENT: &str = "the device stopped responding: retrying";
pub const TRANSFER_FAILED: &str = "a USB transfer failed: retrying";
pub const SHORT_TRANSFER: &str = "the device took less of a control block than was sent: retrying";
pub const BAD_PROBE: &str = "the device gave a malformed probe answer: retrying";
pub const CHANGED_MODE: &str = "the device answered the probe for a different format: retrying";
pub const FRAME_TOO_BIG: &str = "frames are larger than the stream buffer and are being dropped";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Discover,
    Claim,
    Idle,
    SetProbe,
    GetProbe,
    Commit,
    SetAlternate,
    Stream,
    Stop,
    Backoff,
}

/// What became of frames once they were whole.
#[derive(Default)]
struct Outlet {
    sequence: u64,
    frames: u64,
    dropped: u64,
    /// A buffer taken from the stream and not yet published.
    held: Option<u32>,
    /// A frame was whole since this was last cleared, so the device is working.
    any_frame: bool,
    /// The last frame written was refused by the stream, which happens when it is longer than a
    /// buffer of the stream.
    refused: bool,
}

impl Outlet {
    /// Writes a whole frame into a buffer of the stream and publishes it. With no consumer
    /// attached, or no buffer free, the frame is dropped and counted and nothing waits.
    fn deliver(&mut self, stream: &mut Stream, frame: &Frame<'_>, now_ns: u64) {
        self.any_frame = true;
        let sequence = self.sequence;
        self.sequence += 1;
        let buffer = match self.held.take() {
            Some(buffer) => buffer,
            None => match stream.next_buffer() {
                Ok(Some(buffer)) => buffer,
                _ => {
                    self.dropped += 1;
                    return;
                }
            },
        };
        // The buffer stays ours whatever `write` says, so it is kept for the next frame.
        match stream.write(buffer, 0, frame.bytes) {
            Ok(true) => self.refused = false,
            Ok(false) => {
                self.held = Some(buffer);
                self.dropped += 1;
                return;
            }
            Err(_) => {
                self.held = Some(buffer);
                self.dropped += 1;
                self.refused = true;
                return;
            }
        }
        let published = stream.publish(StreamFrame {
            buffer,
            bytes: frame.bytes.len() as u32,
            sequence,
            identity: identity(frame.pts, frame.stc),
            timestamp_ns: now_ns,
        });
        match published {
            Ok(()) => self.frames += 1,
            Err(_) => {
                self.held = Some(buffer);
                self.dropped += 1;
            }
        }
    }
}

/// A frame's identity: the device's presentation time in the high half, the source clock
/// reference in the low half, each 0 when the frame did not carry one.
pub fn identity(pts: Option<u32>, stc: Option<u32>) -> u64 {
    (u64::from(pts.unwrap_or(0)) << 32) | u64::from(stc.unwrap_or(0))
}

/// One camera and everything going on with it.
pub struct Session {
    phase: Phase,
    next_ns: u64,
    attempts: u32,
    fault: &'static str,
    camera: Option<Camera>,
    /// How many of [`Session::interfaces`] are claimed, in order.
    claimed: usize,
    /// The handle answered `Unavailable`: the device it opened has gone.
    gone: bool,
    selected: Option<u16>,
    want: bool,
    /// The streaming interface is on an alternate setting other than 0.
    alternate_set: bool,
    block: [u8; CONTROL_MAX],
    block_len: usize,
    alt: Option<Alt>,
    packet_bytes: Vec<u8>,
    packets: [IsoPacket; PACKETS],
    assembler: Assembler,
    outlet: Outlet,
    /// Overruns seen by the last frame delivered.
    overruns_seen: u64,
    malformed_descriptors: u32,
}

impl Session {
    pub fn new(capacity: usize) -> Self {
        Self {
            phase: Phase::Discover,
            next_ns: 0,
            attempts: 0,
            fault: STARTING,
            camera: None,
            claimed: 0,
            gone: false,
            selected: None,
            want: false,
            alternate_set: false,
            block: [0; CONTROL_MAX],
            block_len: CONTROL_MIN,
            alt: None,
            packet_bytes: Vec::new(),
            packets: [IsoPacket::default(); PACKETS],
            assembler: Assembler::new(capacity),
            outlet: Outlet::default(),
            overruns_seen: 0,
            malformed_descriptors: 0,
        }
    }

    /// The modes, once the descriptors have been read.
    pub fn modes(&self) -> Option<&[Mode]> {
        self.camera.as_ref().map(|camera| camera.modes.as_slice())
    }

    pub fn selected(&self) -> Option<u16> {
        self.selected
    }

    pub fn is_started(&self) -> bool {
        self.want
    }

    /// Picks the mode the next `start` uses. The caller has checked it exists.
    pub fn select(&mut self, index: u16) {
        self.selected = Some(index);
    }

    /// Starts streaming the selected mode, counting from nothing.
    pub fn start(&mut self) {
        self.want = true;
        self.assembler.clear_stats();
        self.outlet.frames = 0;
        self.outlet.dropped = 0;
        self.overruns_seen = 0;
    }

    pub fn stop(&mut self) {
        self.want = false;
    }

    /// Frames delivered, frames dropped, frames damaged.
    pub fn counters(&self) -> (u64, u64, u64) {
        let stats = self.assembler.stats();
        (
            self.outlet.frames,
            self.outlet.dropped + stats.overruns,
            stats.damaged,
        )
    }

    pub fn stats(&self) -> Stats {
        self.assembler.stats()
    }

    /// The device this session's handle opened has gone, and the handle can't reach it again.
    pub fn is_gone(&self) -> bool {
        self.gone
    }

    pub fn malformed_descriptors(&self) -> u32 {
        self.malformed_descriptors
    }

    pub fn fault(&self) -> Option<&'static str> {
        (self.phase == Phase::Backoff).then_some(self.fault)
    }

    /// What's happening now, in words, for the device's description.
    pub fn state(&self) -> &'static str {
        match self.phase {
            Phase::Backoff => self.fault,
            Phase::Discover | Phase::Claim => STARTING,
            Phase::Idle => IDLE,
            Phase::SetProbe | Phase::GetProbe | Phase::Commit | Phase::SetAlternate => NEGOTIATING,
            Phase::Stream => STREAMING,
            Phase::Stop => STOPPING,
        }
    }

    /// Something short of a failure that a caller should know about.
    pub fn warning(&self) -> Option<&'static str> {
        if matches!(self.phase, Phase::Backoff | Phase::Discover | Phase::Claim) {
            return Some(self.state());
        }
        (self.outlet.refused || self.assembler.stats().overruns > self.overruns_seen)
            .then_some(FRAME_TOO_BIG)
    }

    /// Does what is due at `now_ns`, and returns when to be called next.
    pub fn tick(&mut self, usb: &mut Usb, stream: &mut Stream, now_ns: u64) -> Option<u64> {
        if now_ns < self.next_ns {
            return Some(self.next_ns);
        }
        if self.phase == Phase::Backoff {
            self.phase = self.after_backoff();
        }
        match self.step(usb, stream, now_ns) {
            Ok(next) => {
                self.next_ns = next.unwrap_or(0);
                next
            }
            Err(fault) => {
                self.next_ns = self.back_off(fault, now_ns);
                Some(self.next_ns)
            }
        }
    }

    fn after_backoff(&self) -> Phase {
        if self.camera.is_none() {
            Phase::Discover
        } else if self.claimed < self.interfaces().1 {
            Phase::Claim
        } else {
            Phase::Idle
        }
    }

    fn back_off(&mut self, fault: &'static str, now_ns: u64) -> u64 {
        if fault == GONE {
            // Nothing the device was told holds any more: start again from its descriptors.
            self.gone = true;
            self.camera = None;
            self.claimed = 0;
            self.alternate_set = false;
            self.alt = None;
        }
        self.fault = fault;
        self.phase = Phase::Backoff;
        let wait = BACKOFF_FIRST_NS
            .checked_shl(self.attempts)
            .unwrap_or(BACKOFF_CAP_NS)
            .min(BACKOFF_CAP_NS);
        self.attempts = self.attempts.saturating_add(1);
        now_ns.saturating_add(wait)
    }

    fn step(
        &mut self,
        usb: &mut Usb,
        stream: &mut Stream,
        now_ns: u64,
    ) -> Result<Option<u64>, &'static str> {
        match self.phase {
            Phase::Discover => self.discover(usb).map(|()| Some(now_ns)),
            Phase::Claim => self.claim(usb).map(|()| Some(now_ns)),
            Phase::Idle => Ok(self.idle(now_ns)),
            Phase::SetProbe => self.set_probe(usb).map(|()| Some(now_ns)),
            Phase::GetProbe => self.get_probe(usb).map(|()| Some(now_ns)),
            Phase::Commit => self.commit(usb).map(|()| Some(now_ns)),
            Phase::SetAlternate => self.set_alternate(usb).map(|()| Some(now_ns)),
            Phase::Stream => self.transfer(usb, stream, now_ns).map(Some),
            Phase::Stop => self.halt(usb).map(|()| Some(now_ns)),
            // `tick` has already moved on from this.
            Phase::Backoff => Ok(Some(now_ns)),
        }
    }

    fn interface(&self) -> u8 {
        self.camera
            .as_ref()
            .map_or(0, |camera| camera.stream_interface)
    }

    /// The interfaces to take, control first, and how many there are: one when a device names
    /// the same interface for both.
    fn interfaces(&self) -> ([u8; 2], usize) {
        match &self.camera {
            None => ([0; 2], 0),
            Some(camera) if camera.control_interface == camera.stream_interface => {
                ([camera.stream_interface; 2], 1)
            }
            Some(camera) => ([camera.control_interface, camera.stream_interface], 2),
        }
    }

    fn discover(&mut self, usb: &mut Usb) -> Result<(), &'static str> {
        let mut bytes = vec![0u8; MAX_CONFIG_BYTES];
        let length = usb.config_descriptor(0, &mut bytes).map_err(usb_fault)?;
        let camera =
            descriptors::parse(&bytes[..length.min(MAX_CONFIG_BYTES)]).inspect_err(|_| {
                self.malformed_descriptors = self.malformed_descriptors.saturating_add(1);
            })?;
        self.malformed_descriptors = camera.malformed;
        self.camera = Some(camera);
        self.gone = false;
        self.claimed = 0;
        self.phase = Phase::Claim;
        Ok(())
    }

    /// Takes the interfaces from the host's own driver, then claims them. The host's driver can
    /// come back after a reset or replug, and nothing detaches it again, so every attempt does.
    fn claim(&mut self, usb: &mut Usb) -> Result<(), &'static str> {
        let (interfaces, count) = self.interfaces();
        let unclaimed = interfaces[..count].get(self.claimed..).unwrap_or(&[]);
        for interface in unclaimed {
            usb.detach_kernel_driver(*interface).map_err(detach_fault)?;
        }
        for interface in unclaimed {
            usb.claim_interface(*interface).map_err(claim_fault)?;
            self.claimed += 1;
        }
        self.attempts = 0;
        self.phase = Phase::Idle;
        Ok(())
    }

    fn idle(&mut self, now_ns: u64) -> Option<u64> {
        if self.want {
            self.phase = Phase::SetProbe;
            return Some(now_ns);
        }
        if self.alternate_set {
            // A stop that failed: the camera is still on.
            self.phase = Phase::Stop;
            return Some(now_ns);
        }
        None
    }

    /// The mode to negotiate. `None` when `stop` got in first.
    fn mode(&self) -> Option<Mode> {
        let index = usize::from(self.selected?);
        self.camera.as_ref()?.modes.get(index).copied()
    }

    fn setup(&self, request_type: u8, request: u8, control: u8) -> ControlSetup {
        ControlSetup {
            request_type,
            request,
            value: u16::from(control) << 8,
            index: u16::from(self.interface()),
        }
    }

    /// `true` if the stream should carry on. Otherwise on to `Stop`.
    fn still_wanted(&mut self) -> bool {
        if !self.want {
            self.phase = Phase::Stop;
        }
        self.want
    }

    fn set_probe(&mut self, usb: &mut Usb) -> Result<(), &'static str> {
        if !self.still_wanted() {
            return Ok(());
        }
        if self.alternate_set {
            // Negotiating happens with the camera off, and after a failure it may still be on.
            let _ = usb.set_alternate(self.interface(), 0);
            self.alternate_set = false;
        }
        let (Some(mode), Some(version)) = (
            self.mode(),
            self.camera.as_ref().map(|camera| camera.version),
        ) else {
            self.want = false;
            self.phase = Phase::Stop;
            return Ok(());
        };
        self.block = probe::request(&mode);
        self.block_len = probe::control_len(version);
        let setup = self.setup(probe::REQUEST_OUT, probe::SET_CUR, probe::VS_PROBE_CONTROL);
        let sent = usb
            .control_out(setup, &self.block[..self.block_len], CONTROL_TIMEOUT)
            .map_err(usb_fault)?;
        if sent != self.block_len {
            return Err(SHORT_TRANSFER);
        }
        self.phase = Phase::GetProbe;
        Ok(())
    }

    fn get_probe(&mut self, usb: &mut Usb) -> Result<(), &'static str> {
        if !self.still_wanted() {
            return Ok(());
        }
        let setup = self.setup(probe::REQUEST_IN, probe::GET_CUR, probe::VS_PROBE_CONTROL);
        let got = usb
            .control_in(setup, &mut self.block[..self.block_len], CONTROL_TIMEOUT)
            .map_err(usb_fault)?;
        let reply = probe::decode(&self.block[..got.min(self.block_len)]).ok_or(BAD_PROBE)?;
        let mode = self.mode().ok_or(BAD_PROBE)?;
        if reply.format_index != mode.format_index || reply.frame_index != mode.frame_index {
            return Err(CHANGED_MODE);
        }
        let camera = self.camera.as_ref().ok_or(BAD_PROBE)?;
        // A device can honestly report no bandwidth. It still needs a setting with a packet in it.
        let alt = camera.alt_for(reply.max_payload.max(1)).ok_or(BAD_PROBE)?;
        self.alt = Some(alt);
        self.phase = Phase::Commit;
        Ok(())
    }

    fn commit(&mut self, usb: &mut Usb) -> Result<(), &'static str> {
        if !self.still_wanted() {
            return Ok(());
        }
        let setup = self.setup(probe::REQUEST_OUT, probe::SET_CUR, probe::VS_COMMIT_CONTROL);
        let sent = usb
            .control_out(setup, &self.block[..self.block_len], CONTROL_TIMEOUT)
            .map_err(usb_fault)?;
        if sent != self.block_len {
            return Err(SHORT_TRANSFER);
        }
        self.phase = Phase::SetAlternate;
        Ok(())
    }

    fn set_alternate(&mut self, usb: &mut Usb) -> Result<(), &'static str> {
        if !self.still_wanted() {
            return Ok(());
        }
        let alt = self.alt.ok_or(BAD_PROBE)?;
        let mode = self.mode().ok_or(BAD_PROBE)?;
        let largest = self.camera.as_ref().map_or(0, Camera::largest_packet) as usize;
        if self.packet_bytes.len() < largest * PACKETS {
            self.packet_bytes.resize(largest * PACKETS, 0);
        }
        usb.set_alternate(self.interface(), alt.alternate)
            .map_err(usb_fault)?;
        self.alternate_set = true;
        self.assembler
            .reset(mode.expected_bytes.map(|bytes| bytes as usize));
        self.phase = Phase::Stream;
        Ok(())
    }

    fn transfer(
        &mut self,
        usb: &mut Usb,
        stream: &mut Stream,
        now_ns: u64,
    ) -> Result<u64, &'static str> {
        if !self.still_wanted() {
            return Ok(now_ns);
        }
        let alt = self.alt.ok_or(BAD_PROBE)?;
        let capacity = alt.bytes as usize;
        self.packets.fill(IsoPacket {
            capacity: alt.bytes,
            received: 0,
            ok: false,
        });
        let data = self
            .packet_bytes
            .get_mut(..capacity * PACKETS)
            .ok_or(TRANSFER_FAILED)?;
        usb.iso_in(alt.endpoint, data, &mut self.packets, ISO_TIMEOUT)
            .map_err(usb_fault)?;

        let Self {
            assembler,
            packet_bytes,
            packets,
            outlet,
            ..
        } = self;
        for (index, packet) in packets.iter().enumerate() {
            let received = packet.received as usize;
            let payload = (received <= capacity)
                .then(|| packet_bytes.get(index * capacity..index * capacity + received))
                .flatten();
            let Some(payload) = payload else {
                assembler.bad_packet();
                continue;
            };
            assembler.push(payload, packet.ok, &mut |frame| {
                outlet.deliver(stream, &frame, now_ns);
            });
        }
        if core::mem::take(&mut self.outlet.any_frame) {
            self.attempts = 0;
            self.overruns_seen = self.assembler.stats().overruns;
        }
        Ok(now_ns)
    }

    fn halt(&mut self, usb: &mut Usb) -> Result<(), &'static str> {
        if self.alternate_set {
            usb.set_alternate(self.interface(), 0).map_err(usb_fault)?;
            self.alternate_set = false;
        }
        self.phase = Phase::Idle;
        Ok(())
    }
}

/// `Unavailable` is the one error that says the handle is dead for good.
fn usb_fault(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Timeout => SILENT,
        ErrorKind::Unavailable => GONE,
        _ => TRANSFER_FAILED,
    }
}

fn detach_fault(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Unavailable => GONE,
        _ => DETACH_FAILED,
    }
}

fn claim_fault(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Busy => CLAIM_BUSY,
        kind => usb_fault(kind),
    }
}
