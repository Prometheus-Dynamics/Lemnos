//! The stand-in's ROM-side control-transfer face: the vendor requests a host sends to the maskrom entry, taken the way
//! the boot ROM takes them, with the reassembled stages handed to the [`Stand`]'s model.
//!
//! The face answers to a host that drives control transfers, and nothing else. It holds no host code: a caller
//! implements its own transfer trait over [`RomFace::control_out`] and sends what it sends.
//!
//! Wire facts the face relies on. They come from the host project's sender, which cites rkdeveloptool. That source was
//! not re-read on this PC, so each fact is as the sender cites it, not as a ROM was measured:
//! - A stage is requests with bRequest 0x0c, wValue 0 and wIndex 0x0471 (the DDR stage) or 0x0472 (the usbplug stage).
//!   The host project's `maskrom.rs` cites rkdeveloptool `RKComm.cpp` `RKU_DeviceRequest` and `RKDevice.cpp`
//!   `DownloadBoot`.
//! - A request carries at most 4096 bytes. A stage ends at the first request shorter than 4096 bytes.
//! - A stage's last two bytes are CRC-16/CCITT-FALSE, big-endian, over what precedes them. A payload 4095 bytes into
//!   its last chunk carries one pad byte before the CRC (inside the CRC). A payload 4094 bytes into its last chunk
//!   ends with a lone pad byte request, after a full 4096-byte request. The pad is zero (RKComm.cpp:829 and :867).
//!
//! Refusals. A refused request is a stall: the control transfer fails and the stage does not run. The host project's
//! simulated boot ROM refuses the same things the same way, and says of its own framing that it is "dev's framing,
//! not a measured ROM" (`crates/flash/rockchip/src/maskrom.rs`). For each refusal the source is named:
//! - a request that is not a download, or names neither stage: a protocol error in the simulated ROM (`sim.rs`,
//!   `control_out`). The real ROM's answer is not known from any source read here: modelled as a stall.
//! - a request longer than 4096 bytes: the simulated ROM ("maskrom takes at most 4096"). Real ROM: not known, a stall.
//! - a stage that starts before the open one ends, or any request after the usbplug is loaded: the simulated ROM
//!   ("a new maskrom stage before the last one ended", "a maskrom request after the usbplug ran"). Real ROM: not
//!   known, a stall.
//! - a 472 before a 471 has returned: the simulated ROM ("the usbplug stage came before the DDR init") and the
//!   stand-in's own rule. Real ROM: not known, a stall.
//! - a bad CRC: the simulated ROM's `stage_is_intact`. Real ROM: not known; the stall is the strictest plausible
//!   answer, so the stage does not run and nothing is armed.
//! - a lone pad byte that is not zero: the simulated ROM requires the pad to equal its FILL (zero). Real ROM: not known.
//! - a 471 image the model cannot run: the model needs to know what an image does (arm and spin, or arm and return).
//!   A real ROM runs the bytes; the model has to be told. Modelled as a stall.

use lemnos_flash_wire::frame::crc16_ccitt;

use crate::{Refused, Stand, Stub};

/// bRequest of every maskrom download request (RKComm.cpp `RKU_DeviceRequest`, as the host project cites it).
pub const REQUEST: u8 = 0x0c;
/// wIndex of the DDR stage (471).
pub const STAGE_471: u16 = 0x0471;
/// wIndex of the usbplug stage (472).
pub const STAGE_472: u16 = 0x0472;
/// The largest request, and the size a non-final request has.
pub const CHUNK: usize = 4096;
/// The pad byte: zero, in the buffer rkdeveloptool zeroes and in its lone trailing byte.
pub const PAD: u8 = 0;

/// Why the face refused a request. The stage the request belonged to does not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stall {
    /// bRequest or wValue is not a maskrom download request.
    NotADownload,
    /// wIndex is neither 0x0471 nor 0x0472.
    UnknownStage,
    /// A request longer than one chunk.
    Oversize,
    /// A stage began before the open one ended, or a request came after the usbplug was loaded.
    Order,
    /// The completed stage's framing does not hold: the CRC, or a lone pad byte that is not zero.
    Framing,
    /// The 471 image is not one the model can run.
    UnknownImage,
    /// The stand-in refused the stage.
    Rom(Refused),
}

/// Maps a completed 471 image to the entry the model runs. `None` for an image the model does not know.
pub type Classify<'c> = &'c dyn Fn(&[u8]) -> Option<Stub>;

/// The ROM-side face: a stand-in board's maskrom entry as a control-transfer endpoint.
pub struct RomFace<'a> {
    stand: &'a mut Stand,
    classify: Classify<'a>,
    /// The stage being received: its wIndex and its bytes so far.
    open: Option<(u16, Vec<u8>)>,
    /// Set once a 472 is loaded: the ROM is then running the usbplug and takes no more download requests.
    loaded: bool,
}

impl<'a> RomFace<'a> {
    /// A face over `stand`. `classify` says what each 471 image does.
    pub fn new(stand: &'a mut Stand, classify: Classify<'a>) -> Self {
        Self {
            stand,
            classify,
            open: None,
            loaded: false,
        }
    }

    /// The stand-in board behind the face, for the caller's observations of time and reset.
    pub fn stand(&mut self) -> &mut Stand {
        self.stand
    }

    /// A vendor OUT request to the maskrom entry, with `data` as its data stage.
    pub fn control_out(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), Stall> {
        if request != REQUEST || value != 0 {
            return Err(Stall::NotADownload);
        }
        if index != STAGE_471 && index != STAGE_472 {
            return Err(Stall::UnknownStage);
        }
        if self.loaded {
            return Err(Stall::Order);
        }
        if data.len() > CHUNK {
            self.open = None;
            return Err(Stall::Oversize);
        }
        let mut bytes = match self.open.take() {
            Some((open, bytes)) if open == index => bytes,
            Some(_) => return Err(Stall::Order),
            None => Vec::new(),
        };
        bytes.extend_from_slice(data);
        if data.len() == CHUNK {
            self.open = Some((index, bytes));
            return Ok(());
        }
        // A short request ends the stage, and its framing decides whether the stage is whole.
        let tail = framing_tail(&bytes).ok_or(Stall::Framing)?;
        self.complete(index, &bytes[..bytes.len() - tail])
    }

    /// Runs a whole stage's payload: arms the 471 entry in the model, or starts the 472 session.
    fn complete(&mut self, index: u16, payload: &[u8]) -> Result<(), Stall> {
        if index == STAGE_471 {
            let stub = (self.classify)(payload).ok_or(Stall::UnknownImage)?;
            self.stand.send_471(stub).map_err(Stall::Rom)
        } else {
            self.stand.send_472().map_err(Stall::Rom)?;
            self.loaded = true;
            Ok(())
        }
    }
}

/// The framing at the end of a whole stage, as received: the number of bytes it takes after the payload, 2 for a
/// CRC-16 and 3 for a CRC-16 and a lone pad byte. `None` when neither framing holds.
fn framing_tail(bytes: &[u8]) -> Option<usize> {
    let n = bytes.len();
    if n >= 2 && crc16_ccitt(&bytes[..n - 2]).to_be_bytes() == bytes[n - 2..] {
        return Some(2);
    }
    if n >= 3
        && bytes[n - 1] == PAD
        && crc16_ccitt(&bytes[..n - 3]).to_be_bytes() == bytes[n - 3..n - 1]
    {
        return Some(3);
    }
    None
}
