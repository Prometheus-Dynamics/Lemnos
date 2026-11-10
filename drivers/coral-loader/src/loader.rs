//! The download, as a state machine that does at most one USB transfer per tick and never waits.
//!
//! The order is the one DFU 1.1 defines: bring the device to `dfuIDLE`, then for each block a
//! DNLOAD and GETSTATUS until the device is idle again, then a zero-length DNLOAD and GETSTATUS
//! until the device has manifested, then one USB reset, which is what makes the device start the
//! firmware and come back under its new identity. Block numbers count up from 0 and wrap at
//! 65535, as the specification says.
//!
//! A failure during the download restarts it from block 0 after a backoff. DFU has no way to
//! resend one block, so the block a device saw last cannot be trusted after a fault. A reset
//! that was not carried out is made again after the same backoff, and nothing is downloaded
//! again for it.

use core::time::Duration;

use lemnos_core::ErrorKind;
use lemnos_hal::UsbDevice;
use lemnos_hal::blocks::ControlSetup;

use crate::dfu::{self, DfuInterface, DfuStatus};

/// The USB device, as this module sees it.
pub type Usb = dyn UsbDevice<Error = ErrorKind> + Send;

/// The largest DNLOAD sent. `wTransferSize` is a maximum, so sending less is always allowed, and
/// Linux refuses a control transfer longer than a page. A device can claim up to 65535.
pub const MAX_BLOCK: usize = 4096;

/// How long one control transfer may take. It runs on the driver's lane, so it stays well under
/// the host's deadline for a call.
const TRANSFER_TIMEOUT: Duration = Duration::from_millis(500);

/// The bytes read from the configuration descriptor.
const DESCRIPTOR_BYTES: usize = 1024;

const NS_PER_MS: u64 = 1_000_000;
/// The least time between two polls, so a device that says 0 forever is not polled in a spin.
const MIN_POLL_MS: u32 = 1;
/// The most `bwPollTimeout` is honoured for. A device can ask for hours.
pub const MAX_POLL_MS: u32 = 8_000;
/// How long a device may keep saying it is busy with one step.
pub const BUSY_BUDGET_NS: u64 = 60_000 * NS_PER_MS;

/// The first wait after a failure. Each further failure doubles it.
pub const BACKOFF_FIRST_NS: u64 = 1_000 * NS_PER_MS;
/// The longest wait after a failure.
pub const BACKOFF_CAP_NS: u64 = 8_000 * NS_PER_MS;

pub const IN_PROGRESS: &str = "firmware download in progress: not running yet";
pub const UNCONFIRMED: &str = "firmware download ended and the device stopped answering while it \
                               manifested: it was reset and left the bus, but the load is not \
                               confirmed";
pub const UNCHANGED: &str = "firmware downloaded and the device reset, but it came back as the \
                             device it was: not running the firmware";

pub const GONE: &str = "the device is gone: retrying";
pub const SILENT: &str = "the device stopped responding: retrying";
pub const CLAIM_BUSY: &str = "something else holds the device: retrying";
pub const TRANSFER_FAILED: &str = "a USB transfer failed: retrying";
pub const SHORT_DOWNLOAD: &str = "the device took less of a block than was sent: retrying";
pub const REFUSED: &str = "the device reported a DFU error: retrying";
pub const NOT_DFU_MODE: &str = "the device is not in DFU mode: retrying";
pub const UNKNOWN_STATE: &str = "the device reported an unknown DFU state: retrying";
pub const WRONG_STATE: &str = "the device left the download states: retrying";
pub const NOT_IDLE: &str = "the device would not return to its idle state: retrying";
pub const STAYED_BUSY: &str = "the device stayed busy past its budget: retrying";
pub const RESET_FAILED: &str = "firmware downloaded, but the USB reset was not carried out: \
                                retrying the reset";

/// What the loader knows once the reset has gone out. The device handle is of no more use in
/// any of these, and the driver cannot see what the device comes back as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The device said it had manifested, and the reset took it off the bus under the identity
    /// it had: it comes back running the firmware.
    Loaded,
    /// The device stopped answering while it manifested, which DFU allows, and the reset took
    /// it off the bus.
    Unconfirmed,
    /// The reset was carried out and the device is still the one it was.
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Read the descriptors and claim the interface.
    Probe,
    /// Find out what state the device was left in, and bring it to `dfuIDLE`.
    Sync,
    /// Send the next block, or the zero-length block that ends the download.
    Send,
    /// A block is out: wait for the device to take it.
    Poll,
    /// The download is over: wait for the device to manifest.
    Manifest,
    /// The download is over, confirmed by the device or not: reset the device.
    Reset { confirmed: bool },
    /// A failure: wait, then go back to the start of the download, or to the reset.
    Backoff,
    /// Nothing more this driver can do. Stays here for good.
    Ended(Ended),
}

/// A request that did not produce a usable reply.
enum Failure {
    Usb(ErrorKind),
    Malformed(&'static str),
}

/// The download of one image to one device.
pub struct Loader {
    /// Released when the last block has gone. Everything before that may need it again.
    image: Vec<u8>,
    phase: Phase,
    /// When the next step is due. A tick can come early.
    next_ns: u64,
    /// When the current wait for the device runs out.
    deadline_ns: u64,
    /// The soonest the device said it can be asked again. A retry waits for this too.
    poll_after_ns: u64,
    dfu: Option<DfuInterface>,
    /// Bytes of the image already sent.
    offset: usize,
    /// The number the next DNLOAD carries.
    block: u16,
    claimed: bool,
    /// A CLRSTATUS or ABORT has gone out since the last `dfuIDLE`, so another is pointless.
    recovering: bool,
    /// Set once the download is over: whether the device confirmed it. From then on only the
    /// reset is left, and a failure comes back to it.
    downloaded: Option<bool>,
    /// Failures in a row.
    attempts: u32,
    fault: &'static str,
}

impl Loader {
    pub fn new(image: Vec<u8>) -> Self {
        Self {
            image,
            phase: Phase::Probe,
            next_ns: 0,
            deadline_ns: 0,
            poll_after_ns: 0,
            dfu: None,
            offset: 0,
            block: 0,
            claimed: false,
            recovering: false,
            downloaded: None,
            attempts: 0,
            fault: IN_PROGRESS,
        }
    }

    /// How the download ended, once it has.
    pub fn ended(&self) -> Option<Ended> {
        match self.phase {
            Phase::Ended(ended) => Some(ended),
            _ => None,
        }
    }

    /// Why the download is waiting to try again, if it is.
    pub fn fault(&self) -> Option<&'static str> {
        (self.phase == Phase::Backoff).then_some(self.fault)
    }

    /// The bytes of image still held in memory.
    #[cfg(test)]
    pub fn image_len(&self) -> usize {
        self.image.len()
    }

    /// Does what is due at `now_ns`, and returns when to be called next, or `None` once the
    /// reset has gone out.
    pub fn tick(&mut self, usb: &mut Usb, now_ns: u64) -> Option<u64> {
        if self.ended().is_some() {
            return None;
        }
        if now_ns < self.next_ns {
            return Some(self.next_ns);
        }
        if self.phase == Phase::Backoff {
            self.restart(now_ns);
        }
        match self.step(usb, now_ns) {
            Ok(Some(next_ns)) => {
                self.next_ns = next_ns;
                Some(next_ns)
            }
            Ok(None) => None,
            Err(fault) => {
                self.next_ns = self.back_off(fault, now_ns);
                Some(self.next_ns)
            }
        }
    }

    fn step(&mut self, usb: &mut Usb, now_ns: u64) -> Result<Option<u64>, &'static str> {
        match self.phase {
            Phase::Probe => self.probe(usb, now_ns).map(Some),
            Phase::Sync => self.sync(usb, now_ns),
            Phase::Send => self.send(usb, now_ns).map(Some),
            Phase::Poll => self.poll(usb, now_ns).map(Some),
            Phase::Manifest => self.manifest(usb, now_ns).map(Some),
            Phase::Reset { confirmed } => self.reset(usb, confirmed),
            // `tick` has already moved on from these.
            Phase::Backoff | Phase::Ended(_) => Ok(Some(now_ns)),
        }
    }

    /// Picks up from the top after a failure: the device is whatever it now says it is. Once
    /// the download is over, only the reset is picked up again.
    fn restart(&mut self, now_ns: u64) {
        self.recovering = false;
        self.phase = match (self.downloaded, self.dfu) {
            (Some(confirmed), _) => Phase::Reset { confirmed },
            (None, Some(_)) => Phase::Sync,
            (None, None) => Phase::Probe,
        };
        self.deadline_ns = now_ns.saturating_add(BUSY_BUDGET_NS);
    }

    /// Records a failure and returns when to try again.
    fn back_off(&mut self, fault: &'static str, now_ns: u64) -> u64 {
        self.fault = fault;
        self.phase = Phase::Backoff;
        let wait = BACKOFF_FIRST_NS
            .checked_shl(self.attempts)
            .unwrap_or(BACKOFF_CAP_NS)
            .min(BACKOFF_CAP_NS);
        self.attempts = self.attempts.saturating_add(1);
        now_ns.saturating_add(wait).max(self.poll_after_ns)
    }

    /// The download is over. Nothing more will be sent, so the image can go, and what is left
    /// is the reset. A failure that came before this has been got past.
    fn downloaded(&mut self, confirmed: bool, now_ns: u64) -> u64 {
        self.image = Vec::new();
        self.downloaded = Some(confirmed);
        self.attempts = 0;
        self.phase = Phase::Reset { confirmed };
        now_ns
    }

    /// Resets the device, which starts the firmware. A device that leaves the bus under this
    /// identity is the reset working: the handle is dead from then on, so there is no more to
    /// ask. A reset that was not carried out is made again after the backoff.
    fn reset(&mut self, usb: &mut Usb, confirmed: bool) -> Result<Option<u64>, &'static str> {
        let ended = match usb.reset() {
            Ok(()) => Ended::Unchanged,
            Err(ErrorKind::Unavailable) if confirmed => Ended::Loaded,
            Err(ErrorKind::Unavailable) => Ended::Unconfirmed,
            Err(_) => return Err(RESET_FAILED),
        };
        self.phase = Phase::Ended(ended);
        Ok(None)
    }

    fn interface(&self) -> u8 {
        self.dfu.map_or(0, |dfu| dfu.interface)
    }

    fn probe(&mut self, usb: &mut Usb, now_ns: u64) -> Result<u64, &'static str> {
        let mut descriptors = [0u8; DESCRIPTOR_BYTES];
        let length = usb
            .config_descriptor(0, &mut descriptors)
            .map_err(usb_fault)?;
        let dfu = dfu::find_interface(&descriptors[..length.min(DESCRIPTOR_BYTES)])?;
        if !self.claimed {
            usb.claim_interface(dfu.interface).map_err(usb_fault)?;
            self.claimed = true;
        }
        if dfu.alternate != 0 {
            usb.set_alternate(dfu.interface, dfu.alternate)
                .map_err(usb_fault)?;
        }
        self.dfu = Some(dfu);
        self.phase = Phase::Sync;
        self.deadline_ns = now_ns.saturating_add(BUSY_BUDGET_NS);
        Ok(now_ns)
    }

    /// Reads the state the device is in and takes one step towards `dfuIDLE`. The driver keeps
    /// nothing across restarts, so this is also how it finds a device left mid-download.
    fn sync(&mut self, usb: &mut Usb, now_ns: u64) -> Result<Option<u64>, &'static str> {
        let status = self.status(usb).map_err(failure_fault)?;
        if status.status != dfu::STATUS_OK || status.state == dfu::ERROR {
            return self.recover(usb, dfu::CLRSTATUS, now_ns).map(Some);
        }
        match status.state {
            dfu::IDLE => {
                self.recovering = false;
                self.offset = 0;
                self.block = 0;
                self.phase = Phase::Send;
                Ok(Some(now_ns))
            }
            dfu::DNLOAD_IDLE | dfu::UPLOAD_IDLE | dfu::MANIFEST_SYNC => {
                self.recover(usb, dfu::ABORT, now_ns).map(Some)
            }
            dfu::DNLOAD_SYNC | dfu::DNBUSY | dfu::MANIFEST => {
                self.wait(status.poll_ms, now_ns).map(Some)
            }
            // Left there by a download before a restart: all it needs is the reset.
            dfu::MANIFEST_WAIT_RESET => Ok(Some(self.downloaded(true, now_ns))),
            dfu::APP_IDLE | dfu::APP_DETACH => Err(NOT_DFU_MODE),
            _ => Err(UNKNOWN_STATE),
        }
    }

    /// Sends CLRSTATUS or ABORT, once.
    fn recover(&mut self, usb: &mut Usb, request: u8, now_ns: u64) -> Result<u64, &'static str> {
        if self.recovering {
            return Err(NOT_IDLE);
        }
        self.recovering = true;
        self.control_out(usb, request, 0, &[]).map_err(usb_fault)?;
        Ok(now_ns)
    }

    fn send(&mut self, usb: &mut Usb, now_ns: u64) -> Result<u64, &'static str> {
        let transfer = self
            .dfu
            .map_or(MAX_BLOCK, |dfu| usize::from(dfu.transfer_size));
        let end = self
            .offset
            .saturating_add(transfer.min(MAX_BLOCK))
            .min(self.image.len());
        let block = self.block;
        let data = &self.image[self.offset..end];
        let finished = data.is_empty();
        let length = data.len();
        let setup = self.setup(dfu::REQUEST_OUT, dfu::DNLOAD, block);
        let sent = usb
            .control_out(setup, data, TRANSFER_TIMEOUT)
            .map_err(usb_fault)?;
        if sent != length {
            return Err(SHORT_DOWNLOAD);
        }
        self.offset = end;
        self.block = block.wrapping_add(1);
        self.phase = if finished {
            Phase::Manifest
        } else {
            Phase::Poll
        };
        self.deadline_ns = now_ns.saturating_add(BUSY_BUDGET_NS);
        Ok(now_ns)
    }

    fn poll(&mut self, usb: &mut Usb, now_ns: u64) -> Result<u64, &'static str> {
        let status = self.status(usb).map_err(failure_fault)?;
        if status.status != dfu::STATUS_OK || status.state == dfu::ERROR {
            return Err(REFUSED);
        }
        match status.state {
            dfu::DNLOAD_IDLE => {
                // The device took a block, so it is alive and the backoff is over.
                self.attempts = 0;
                self.phase = Phase::Send;
                Ok(now_ns)
            }
            dfu::DNLOAD_SYNC | dfu::DNBUSY => self.wait(status.poll_ms, now_ns),
            _ => Err(WRONG_STATE),
        }
    }

    /// Waits for the end of manifestation. A device that isn't manifestation tolerant may stop
    /// answering here, and one may reset itself: either way there is no more to send.
    fn manifest(&mut self, usb: &mut Usb, now_ns: u64) -> Result<u64, &'static str> {
        let tolerant = self.dfu.is_some_and(|dfu| dfu.manifestation_tolerant);
        let status = match self.status(usb) {
            Ok(status) => status,
            Err(Failure::Usb(kind)) if !tolerant || is_gone(kind) => {
                return Ok(self.downloaded(false, now_ns));
            }
            Err(failure) => return Err(failure_fault(failure)),
        };
        if status.status != dfu::STATUS_OK || status.state == dfu::ERROR {
            return Err(REFUSED);
        }
        match status.state {
            dfu::MANIFEST_WAIT_RESET | dfu::IDLE => Ok(self.downloaded(true, now_ns)),
            dfu::MANIFEST_SYNC | dfu::MANIFEST => self.wait(status.poll_ms, now_ns),
            _ => Err(WRONG_STATE),
        }
    }

    /// When to ask again, as the device requested but within reason.
    fn wait(&mut self, poll_ms: u32, now_ns: u64) -> Result<u64, &'static str> {
        let ms = u64::from(poll_ms.clamp(MIN_POLL_MS, MAX_POLL_MS));
        self.poll_after_ns = now_ns.saturating_add(ms * NS_PER_MS);
        if now_ns >= self.deadline_ns {
            return Err(STAYED_BUSY);
        }
        Ok(self.poll_after_ns)
    }

    fn setup(&self, request_type: u8, request: u8, value: u16) -> ControlSetup {
        ControlSetup {
            request_type,
            request,
            value,
            index: u16::from(self.interface()),
        }
    }

    fn control_out(
        &self,
        usb: &mut Usb,
        request: u8,
        value: u16,
        data: &[u8],
    ) -> Result<usize, ErrorKind> {
        let setup = self.setup(dfu::REQUEST_OUT, request, value);
        usb.control_out(setup, data, TRANSFER_TIMEOUT)
    }

    fn status(&self, usb: &mut Usb) -> Result<DfuStatus, Failure> {
        let setup = self.setup(dfu::REQUEST_IN, dfu::GETSTATUS, 0);
        let mut reply = [0u8; dfu::STATUS_LEN];
        let length = usb
            .control_in(setup, &mut reply, TRANSFER_TIMEOUT)
            .map_err(Failure::Usb)?;
        DfuStatus::parse(&reply[..length.min(dfu::STATUS_LEN)]).map_err(Failure::Malformed)
    }
}

fn is_gone(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::Unavailable | ErrorKind::NotFound)
}

fn usb_fault(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Timeout => SILENT,
        ErrorKind::Busy => CLAIM_BUSY,
        kind if is_gone(kind) => GONE,
        _ => TRANSFER_FAILED,
    }
}

fn failure_fault(failure: Failure) -> &'static str {
    match failure {
        Failure::Usb(kind) => usb_fault(kind),
        Failure::Malformed(fault) => fault,
    }
}
