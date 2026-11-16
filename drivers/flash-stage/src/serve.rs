//! The serve loop: one request at a time over a [`Link`], until an END.
//!
//! A bad write is refused with its status and the loop goes on: its chunk was received and discarded,
//! so the link stays in step. A cut, a stalled transfer or a bad request frame ends the loop with an
//! error; the caller ends the session, and the watchdog's reset is the exit. An identify is answered
//! with this build's identity record and feeds nothing. An END replies its status and returns the reset
//! kind it asked for.

use crate::chunk::{Fault, read_round, write_round};
use crate::guard::Watchdog;
use crate::link::{IoError, Link, Storage};
use crate::region::Region;
use lemnos_flash_wire::frame::{
    FrameError, MAX_CHUNK, Op, REQUEST_LEN, Request, ResetKind, STATUS_CRC, STATUS_NOT_COMPARED,
    STATUS_OK, STATUS_STORAGE, STATUS_VERIFY,
};
use lemnos_flash_wire::identity::IDENTITY;

/// The budget of one request's wait, in microseconds (each round's own bounds apply inside it).
pub const REQUEST_BUDGET_US: u64 = 1_000_000;

/// Why the loop ended before an END.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeError {
    /// The link stalled, disconnected or carried the wrong length.
    Link(IoError),
    /// A request frame was refused.
    Frame(FrameError),
    /// A read failed in storage: no reply is sent.
    Read(IoError),
}

/// Serves requests until an END. `buf` and `scratch` each hold at least [`MAX_CHUNK`] bytes.
pub fn serve<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    regions: &[Region],
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<ResetKind, ServeError> {
    loop {
        let mut header = [0u8; REQUEST_LEN];
        let n = link
            .recv(&mut header, REQUEST_BUDGET_US)
            .map_err(ServeError::Link)?;
        if n != header.len() {
            return Err(ServeError::Frame(FrameError::NotRequest));
        }
        let request = Request::decode(&header).map_err(ServeError::Frame)?;
        match request.op {
            Op::Read => {
                read_round(
                    link,
                    storage,
                    wdt,
                    request.lba,
                    request.count,
                    &mut buf[..usize::from(request.count) * 512 + 2],
                )
                .map_err(|fault| match fault {
                    Fault::Link(e) => ServeError::Link(e),
                    Fault::Storage(e) => ServeError::Read(e),
                    Fault::Corrupt | Fault::Verify | Fault::NotCompared => {
                        ServeError::Read(IoError::Failed)
                    }
                })?;
            }
            Op::Write => {
                let len = usize::from(request.count) * 512 + 2;
                let status = match write_round(
                    link,
                    storage,
                    wdt,
                    regions,
                    &request,
                    &mut buf[..len],
                    &mut scratch[..len - 2],
                ) {
                    Ok(()) => STATUS_OK,
                    Err(Fault::Corrupt) => STATUS_CRC,
                    Err(Fault::NotCompared) => STATUS_NOT_COMPARED,
                    Err(Fault::Verify) => STATUS_VERIFY,
                    Err(Fault::Storage(_)) => STATUS_STORAGE,
                    Err(Fault::Link(e)) => return Err(ServeError::Link(e)),
                };
                link.send(&[status], REQUEST_BUDGET_US)
                    .map_err(ServeError::Link)?;
            }
            Op::Identify => {
                link.send(&IDENTITY, REQUEST_BUDGET_US)
                    .map_err(ServeError::Link)?;
            }
            Op::End => {
                link.send(&[STATUS_OK], REQUEST_BUDGET_US)
                    .map_err(ServeError::Link)?;
                return Ok(request.reset_kind());
            }
        }
    }
}

/// The size both buffers of [`serve`] must hold.
pub const BUFFER_LEN: usize = MAX_CHUNK;
