//! The serve loop: one request at a time over a [`Link`], until an END.
//!
//! What ends the session, and what does not:
//! - **A refused request** (a bad length, a bad CRC, an unknown op, a reserved bit it does not define, a chunk of the
//!   wrong length) gets a one-byte status reply and the session goes on. Nothing is done for it.
//! - **A failed storage operation** gets its status and the session goes on. The range may be partly written.
//! - **An END** replies its status and returns the reset kind it asked for.
//! - **The idle deadline.** The header wait gets the budget the session computes: the time left before the idle
//!   deadline, counted from the last watchdog feed. If no request arrives in that budget the loop returns
//!   [`Progress::Idle`], and `serve` reports [`ServeError::Idle`]. A reply does not restart it.
//! - **A bus reset or an unconfiguration** ends the session with `IoError::Reset`: the chip enumerates again.
//! - **A link cut** (a transfer that started and never finished) ends the session with its timeout. No reply is
//!   possible then.

use crate::chunk::{Fault, read_round, write_round};
use crate::guard::Watchdog;
use crate::link::{IoError, Link, Storage};
use crate::region::Region;
use lemnos_flash_wire::frame::{
    FrameError, MAX_CHUNK, Op, REQUEST_LEN, Request, ResetKind, STATUS_BAD_REQUEST, STATUS_CRC,
    STATUS_NOT_COMPARED, STATUS_OK, STATUS_STORAGE, STATUS_VERIFY,
};
use lemnos_flash_wire::timing::TRANSFER_BUDGET_US;

/// What one step of the loop did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// A request was served and its reply sent.
    Served,
    /// A request was refused: its status was sent, and nothing was done for it.
    Refused(u8),
    /// No request came before the idle deadline.
    Idle,
    /// An END was served; the reset kind it asked for.
    Ended(ResetKind),
}

/// Why the loop ended before an END.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeError {
    /// The link failed: a cut, a reset, or a stall. See the module docs.
    Link(IoError),
    /// The idle deadline passed with no request. The host did not ask in time; the watchdog has not fired.
    Idle,
}

/// The status a refused frame gets.
fn refusal_status(error: FrameError) -> u8 {
    match error {
        FrameError::Crc => STATUS_CRC,
        _ => STATUS_BAD_REQUEST,
    }
}

/// What a step serves from: the card's regions and the stage's identity record.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub regions: &'a [Region],
    pub identity: &'a [u8],
}

/// Serves one step: one request, or one idle wait of at most `header_budget_us`. `buf` and `scratch` each hold at
/// least [`BUFFER_LEN`] bytes. `identity` is the stage's identity record, sent in reply to an identify.
pub fn step<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    ctx: &Context<'_>,
    header_budget_us: u64,
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<Progress, ServeError> {
    let mut header = [0u8; REQUEST_LEN];
    let n = match link.recv(&mut header, header_budget_us) {
        Ok(n) => n,
        Err(IoError::Timeout) => return Ok(Progress::Idle),
        Err(e) => return Err(ServeError::Link(e)),
    };
    if n != REQUEST_LEN {
        return send_status(link, STATUS_BAD_REQUEST);
    }
    let request = match Request::decode(&header) {
        Ok(request) => request,
        Err(error) => return send_status(link, refusal_status(error)),
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
                ctx.regions,
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
            link.send(ctx.identity, TRANSFER_BUDGET_US)
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

/// Sends a one-byte status and reports what it was.
fn send_status<L: Link>(link: &mut L, status: u8) -> Result<Progress, ServeError> {
    link.send(&[status], TRANSFER_BUDGET_US)
        .map_err(ServeError::Link)?;
    Ok(if status == STATUS_OK {
        Progress::Served
    } else {
        Progress::Refused(status)
    })
}

/// Serves until an END. The idle deadline ends it with [`ServeError::Idle`]; every link failure with its error.
pub fn serve<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    ctx: &Context<'_>,
    header_budget_us: u64,
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<ResetKind, ServeError> {
    loop {
        match step(link, storage, wdt, ctx, header_budget_us, buf, scratch)? {
            Progress::Ended(kind) => return Ok(kind),
            Progress::Idle => return Err(ServeError::Idle),
            Progress::Served | Progress::Refused(_) => {}
        }
    }
}

/// The size both buffers of [`serve`] must hold: a read reply, which carries its status first.
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

        fn now_us(&mut self) -> u64 {
            0
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

    const IDLE_BUDGET: u64 = 30_000_000;

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
            &Context {
                regions: &REGIONS,
                identity: &[],
            },
            IDLE_BUDGET,
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
            &Context {
                regions: &REGIONS,
                identity: &[],
            },
            IDLE_BUDGET,
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
            &Context {
                regions: &REGIONS,
                identity: &[],
            },
            IDLE_BUDGET,
            &mut buf,
            &mut scratch,
        )
        .unwrap();
        assert_eq!(*log.borrow(), vec!["send", "feed"]);
    }

    /// A card whose read-back fails: the write lands, the read does not.
    struct FailingReadBack<'a>(&'a RefCell<Vec<&'static str>>);

    impl Storage for FailingReadBack<'_> {
        fn write(&mut self, _lba: u32, _data: &[u8], _budget_us: u64) -> Result<(), IoError> {
            self.0.borrow_mut().push("write");
            Ok(())
        }
        fn read(&mut self, _lba: u32, _out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
            Err(IoError::Failed)
        }
    }

    const BOOT: [Region; 1] = [Region {
        first: 64,
        end: 32_768,
        class: Class::BootArea,
    }];

    /// One write request, with the chunk the host sends after it. Returns what the step did and the log of its effects.
    fn one_write(
        lba: u32,
        count: u16,
        chunk: Vec<u8>,
    ) -> (Result<Progress, ServeError>, Vec<&'static str>) {
        let log = RefCell::new(Vec::new());
        let mut link = Wire {
            inbound: vec![Request::write(lba, count, true).encode().to_vec(), chunk],
            log: &log,
            fail_send: false,
        };
        let mut card = Card(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let ctx = Context {
            regions: &BOOT,
            identity: &[],
        };
        let progress = step(
            &mut link,
            &mut card,
            &mut dog,
            &ctx,
            IDLE_BUDGET,
            &mut buf,
            &mut scratch,
        );
        let effects = log.borrow().clone();
        (progress, effects)
    }

    fn chunk_of(sectors: usize) -> Vec<u8> {
        let payload = vec![0x3C; sectors * SECTOR];
        let mut out = payload.clone();
        out.extend_from_slice(&payload_crc(&payload));
        out
    }

    #[test]
    fn a_chunk_crossing_the_boot_area_end_is_refused_as_bad_request_and_nothing_is_written_or_fed()
    {
        // Sectors 32767 (boot area) and 32768 (plain): the boundary at 32768 is strictly inside the write.
        let (progress, effects) = one_write(32_767, 2, chunk_of(2));
        assert_eq!(progress, Ok(Progress::Refused(5)));
        assert_eq!(effects, vec!["send"], "the status only: no write, no feed");
    }

    #[test]
    fn a_chunk_crossing_the_boot_area_start_is_refused_as_bad_request_and_nothing_is_written_or_fed()
     {
        // Sectors 63 (plain) and 64 (boot area): the boundary at 64 is strictly inside the write.
        let (progress, effects) = one_write(63, 2, chunk_of(2));
        assert_eq!(progress, Ok(Progress::Refused(5)));
        assert_eq!(effects, vec!["send"]);
    }

    #[test]
    fn a_chunk_that_is_not_whole_sectors_is_refused_as_bad_request_and_nothing_is_written() {
        // One sector's request with one byte short: the chunk is not count * 512 + 2, and the stage never pads it.
        let mut short = chunk_of(1);
        short.pop();
        let (progress, effects) = one_write(500, 1, short);
        assert_eq!(progress, Ok(Progress::Refused(5)));
        assert_eq!(effects, vec!["send"]);
    }

    #[test]
    fn a_failed_read_back_is_storage_status_two_and_nothing_is_fed() {
        let log = RefCell::new(Vec::new());
        let mut link = Wire {
            inbound: vec![Request::write(500, 1, true).encode().to_vec(), chunk_of(1)],
            log: &log,
            fail_send: false,
        };
        let mut card = FailingReadBack(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let ctx = Context {
            regions: &BOOT,
            identity: &[],
        };
        let progress = step(
            &mut link,
            &mut card,
            &mut dog,
            &ctx,
            IDLE_BUDGET,
            &mut buf,
            &mut scratch,
        );
        assert_eq!(progress, Ok(Progress::Refused(2)), "the storage's status");
        assert_eq!(
            *log.borrow(),
            vec!["write", "send"],
            "the write was attempted and the status sent; no feed"
        );
    }

    /// A card whose read-back returns other bytes than the write stored: the write lands, the read differs.
    struct MismatchedReadBack<'a>(&'a RefCell<Vec<&'static str>>);

    impl Storage for MismatchedReadBack<'_> {
        fn write(&mut self, _lba: u32, _data: &[u8], _budget_us: u64) -> Result<(), IoError> {
            self.0.borrow_mut().push("write");
            Ok(())
        }
        fn read(&mut self, _lba: u32, out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
            out.fill(0x00);
            Ok(())
        }
    }

    #[test]
    fn a_read_back_that_differs_from_the_write_is_verify_status_three_and_nothing_is_fed() {
        let log = RefCell::new(Vec::new());
        let mut link = Wire {
            inbound: vec![
                Request::write(40_000, 1, false).encode().to_vec(),
                chunk_of(1),
            ],
            log: &log,
            fail_send: false,
        };
        let mut card = MismatchedReadBack(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let ctx = Context {
            regions: &BOOT,
            identity: &[],
        };
        let progress = step(
            &mut link,
            &mut card,
            &mut dog,
            &ctx,
            IDLE_BUDGET,
            &mut buf,
            &mut scratch,
        );
        assert_eq!(
            progress,
            Ok(Progress::Refused(3)),
            "the read-back mismatch is verify (3), not storage (2)"
        );
        assert_eq!(
            *log.borrow(),
            vec!["write", "send"],
            "written, reported, not fed"
        );
    }

    /// A plain-data write of `count` sectors at `lba`, served through one step, with its effects.
    fn plain_write(lba: u32, count: u16) -> (Result<Progress, ServeError>, Vec<&'static str>) {
        let log = RefCell::new(Vec::new());
        let mut link = Wire {
            inbound: vec![
                Request::write(lba, count, false).encode().to_vec(),
                chunk_of(usize::from(count)),
            ],
            log: &log,
            fail_send: false,
        };
        let mut card = Card(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let ctx = Context {
            regions: &BOOT,
            identity: &[],
        };
        let progress = step(
            &mut link,
            &mut card,
            &mut dog,
            &ctx,
            IDLE_BUDGET,
            &mut buf,
            &mut scratch,
        );
        let effects = log.borrow().clone();
        (progress, effects)
    }

    #[test]
    fn a_write_of_128_sectors_is_served_and_fed() {
        let (progress, effects) = plain_write(40_000, 128);
        assert_eq!(progress, Ok(Progress::Served));
        assert_eq!(
            effects,
            vec!["write", "feed", "send"],
            "the largest chunk: written, fed, then the status"
        );
    }

    #[test]
    fn a_write_of_129_sectors_is_refused_as_bad_request_and_nothing_is_written() {
        // The count is 1..=128. A count of 129 is refused by the frame, before any chunk is read from the link.
        let log = RefCell::new(Vec::new());
        let mut link = Wire {
            inbound: vec![Request::write(40_000, 129, false).encode().to_vec()],
            log: &log,
            fail_send: false,
        };
        let mut card = Card(&log);
        let mut dog = Dog(&log);
        let mut buf = vec![0u8; MAX_CHUNK + 1];
        let mut scratch = vec![0u8; MAX_CHUNK];
        let ctx = Context {
            regions: &BOOT,
            identity: &[],
        };
        let progress = step(
            &mut link,
            &mut card,
            &mut dog,
            &ctx,
            IDLE_BUDGET,
            &mut buf,
            &mut scratch,
        );
        assert_eq!(progress, Ok(Progress::Refused(5)));
        assert_eq!(
            *log.borrow(),
            vec!["send"],
            "the status only: no write, no feed"
        );
    }

    #[test]
    fn a_chunk_with_a_bad_crc_is_status_one_and_nothing_is_written_or_fed() {
        let mut bad = chunk_of(1);
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        let (progress, effects) = one_write(500, 1, bad);
        assert_eq!(
            progress,
            Ok(Progress::Refused(1)),
            "a CRC mismatch is crc (1)"
        );
        assert_eq!(effects, vec!["send"], "the status only: no write, no feed");
    }
}
