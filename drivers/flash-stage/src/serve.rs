//! The serve loop: one request at a time over a [`Link`], until an END.
//!
//! What ends the session, and what does not:
//! - **A refused request** (a bad length, a bad CRC, an unknown op, a reserved bit it does not define, a
//!   short or long chunk) gets a one-byte status reply and the session goes on. Nothing is done for it.
//! - **A failed storage operation** gets its status and the session goes on. The range may be partly written.
//! - **An END** replies its status and returns the reset kind it asked for.
//! - **The idle limit** (no request within [`IDLE_LIMIT_US`]) ends the session with [`Progress::Idle`].
//! - **A bus reset or an unconfiguration** ends the session with `IoError::Reset`: the chip enumerates again.
//! - **A link cut** (a transfer that started and never finished) ends the session with its timeout. No reply
//!   is possible then.

use crate::chunk::{Fault, read_round, write_round};
use crate::guard::Watchdog;
use crate::link::{IoError, Link, Storage};
use crate::region::Region;
use lemnos_flash_wire::frame::{
    FrameError, MAX_CHUNK, Op, REQUEST_LEN, Request, ResetKind, STATUS_BAD_REQUEST, STATUS_CRC,
    STATUS_NOT_COMPARED, STATUS_OK, STATUS_STORAGE, STATUS_VERIFY,
};
use lemnos_flash_wire::timing::{IDLE_LIMIT_US, TRANSFER_BUDGET_US};

/// What one step of the loop did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// A request was served and its reply sent.
    Served,
    /// A request was refused: its status was sent, and nothing was done for it.
    Refused(u8),
    /// No request came within the idle limit.
    Idle,
    /// An END was served; the reset kind it asked for.
    Ended(ResetKind),
}

/// Why the loop ended before an END.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeError {
    /// The link failed: a cut, a reset, or a stall. See the module docs.
    Link(IoError),
}

/// The status a refused frame gets.
fn refusal_status(error: FrameError) -> u8 {
    match error {
        FrameError::Crc => STATUS_CRC,
        _ => STATUS_BAD_REQUEST,
    }
}

/// Serves one step: one request, or one idle wait. `buf` and `scratch` each hold at least [`MAX_CHUNK`] bytes.
/// `identity` is the stage's identity record, sent in reply to an identify.
pub fn step<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    regions: &[Region],
    identity: &[u8],
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<Progress, ServeError> {
    let mut header = [0u8; REQUEST_LEN];
    let n = match link.recv(&mut header, IDLE_LIMIT_US) {
        Ok(n) => n,
        Err(IoError::Timeout) => return Ok(Progress::Idle),
        Err(e) => return Err(ServeError::Link(e)),
    };
    let request = if n != REQUEST_LEN {
        return refuse(link, STATUS_BAD_REQUEST);
    } else {
        match Request::decode(&header) {
            Ok(request) => request,
            Err(error) => return refuse(link, refusal_status(error)),
        }
    };
    match request.op {
        Op::Read => {
            let len = 1 + usize::from(request.count) * 512 + 2;
            match read_round(
                link,
                storage,
                wdt,
                request.lba,
                request.count,
                &mut buf[..len],
            ) {
                Ok(()) => Ok(Progress::Served),
                Err(Fault::Storage(_)) => send_status(link, STATUS_STORAGE),
                Err(Fault::Link(e)) => Err(ServeError::Link(e)),
                Err(_) => send_status(link, STATUS_BAD_REQUEST),
            }
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
                Err(Fault::Length) => STATUS_BAD_REQUEST,
                Err(Fault::Link(e)) => return Err(ServeError::Link(e)),
            };
            send_status(link, status)
        }
        Op::Identify => {
            link.send(identity, TRANSFER_BUDGET_US)
                .map_err(ServeError::Link)?;
            Ok(Progress::Served)
        }
        Op::End => {
            link.send(&[STATUS_OK], TRANSFER_BUDGET_US)
                .map_err(ServeError::Link)?;
            Ok(Progress::Ended(request.reset_kind()))
        }
    }
}

fn send_status<L: Link>(link: &mut L, status: u8) -> Result<Progress, ServeError> {
    link.send(&[status], TRANSFER_BUDGET_US)
        .map_err(ServeError::Link)?;
    Ok(if status == STATUS_OK {
        Progress::Served
    } else {
        Progress::Refused(status)
    })
}

fn refuse<L: Link>(link: &mut L, status: u8) -> Result<Progress, ServeError> {
    send_status(link, status)
}

/// Serves until an END. The idle limit and every failure end the loop as an error or as [`Progress::Idle`]
/// (returned as an error here, `IoError::Timeout`, so the caller ends the session).
pub fn serve<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    regions: &[Region],
    identity: &[u8],
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<ResetKind, ServeError> {
    loop {
        match step(link, storage, wdt, regions, identity, buf, scratch)? {
            Progress::Ended(kind) => return Ok(kind),
            Progress::Idle => return Err(ServeError::Link(IoError::Timeout)),
            Progress::Served | Progress::Refused(_) => {}
        }
    }
}

/// The size both buffers of [`serve`] must hold.
pub const BUFFER_LEN: usize = MAX_CHUNK + 1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::Watchdog;
    use crate::link::{IoError, Link, Storage};
    use crate::region::Class;
    use lemnos_flash_wire::frame::{SECTOR, payload_crc};
    use std::cell::RefCell;

    /// A link that delivers the host's frames in order and records each send, and a watchdog and a card that
    /// record the same order into one log.
    struct Wire<'a> {
        inbound: Vec<Vec<u8>>,
        log: &'a RefCell<Vec<&'static str>>,
        fail_send: bool,
    }

    impl Link for Wire<'_> {
        fn recv(&mut self, buf: &mut [u8], _budget_us: u64) -> Result<usize, IoError> {
            if self.inbound.is_empty() {
                return Err(IoError::Timeout);
            }
            let frame = self.inbound.remove(0);
            buf[..frame.len()].copy_from_slice(&frame);
            Ok(frame.len())
        }

        fn send(&mut self, _data: &[u8], _budget_us: u64) -> Result<(), IoError> {
            self.log.borrow_mut().push("send");
            if self.fail_send {
                Err(IoError::Failed)
            } else {
                Ok(())
            }
        }
    }

    struct Card<'a>(&'a RefCell<Vec<&'static str>>);

    impl Storage for Card<'_> {
        fn write(&mut self, _lba: u32, _data: &[u8], _budget_us: u64) -> Result<(), IoError> {
            self.0.borrow_mut().push("write");
            Ok(())
        }
        fn read(&mut self, _lba: u32, out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
            // The read-back returns what the tests write: the payload's fill.
            out.fill(0x3C);
            Ok(())
        }
    }

    struct Dog<'a>(&'a RefCell<Vec<&'static str>>);

    impl Watchdog for Dog<'_> {
        fn feed(&mut self) {
            self.0.borrow_mut().push("feed");
        }
    }

    const REGIONS: [Region; 1] = [Region {
        first: 64,
        end: 100,
        class: Class::BootArea,
    }];

    #[test]
    fn a_verified_write_is_fed_before_its_status_is_sent() {
        let log = RefCell::new(Vec::new());
        let payload = vec![0x3C; SECTOR];
        let mut chunk = payload.clone();
        chunk.extend_from_slice(&payload_crc(&payload));
        let mut link = Wire {
            inbound: vec![Request::write(500, 1, false).encode().to_vec(), chunk],
            log: &log,
            fail_send: false,
        };
        let mut card = Card(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let progress = step(
            &mut link,
            &mut card,
            &mut dog,
            &REGIONS,
            &[],
            &mut buf,
            &mut scratch,
        )
        .unwrap();
        assert_eq!(progress, Progress::Served);
        assert_eq!(
            *log.borrow(),
            vec!["write", "feed", "send"],
            "stored, verified and fed, then the status"
        );
    }

    #[test]
    fn a_status_that_cannot_be_sent_after_a_feed_still_leaves_the_feed_counted_and_ends_the_session()
     {
        let log = RefCell::new(Vec::new());
        let payload = vec![0x3C; SECTOR];
        let mut chunk = payload.clone();
        chunk.extend_from_slice(&payload_crc(&payload));
        let mut link = Wire {
            inbound: vec![Request::write(500, 1, false).encode().to_vec(), chunk],
            log: &log,
            fail_send: true,
        };
        let mut card = Card(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let result = step(
            &mut link,
            &mut card,
            &mut dog,
            &REGIONS,
            &[],
            &mut buf,
            &mut scratch,
        );
        assert_eq!(result, Err(ServeError::Link(IoError::Failed)));
        assert_eq!(
            *log.borrow(),
            vec!["write", "feed", "send"],
            "the fed round is not undone; the session parks after it"
        );
    }

    #[test]
    fn a_read_is_fed_only_after_its_reply_is_taken() {
        let log = RefCell::new(Vec::new());
        let mut link = Wire {
            inbound: vec![Request::read(1, 1).encode().to_vec()],
            log: &log,
            fail_send: false,
        };
        let mut card = Card(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        step(
            &mut link,
            &mut card,
            &mut dog,
            &REGIONS,
            &[],
            &mut buf,
            &mut scratch,
        )
        .unwrap();
        assert_eq!(*log.borrow(), vec!["send", "feed"]);
    }
}
