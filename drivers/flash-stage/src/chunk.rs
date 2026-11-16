//! The two rounds of the protocol: a write round and a read round.
//!
//! **Feed rule.** The watchdog is fed once per verified round, and only then. A write round is verified when
//! the chunk's CRC matches, the region admits it, and the storage read-back equals the payload. The feed
//! comes after that. The status reply goes out after the feed, so a fault in the reply cannot hide one, and
//! a lost reply cannot leave a fed round unrecorded: the round was verified and stored, and a session that
//! then ends without a reply parks, and the watchdog's reset is the exit.
//!
//! **Storage state.** A write that fails in storage (or fails its read-back) may leave the range partly
//! written. The status says so (see the wire README): the host must read the range back and verify it
//! before it relies on it.

use crate::guard::Watchdog;
use crate::link::{IoError, Link, Storage};
use crate::region::{Region, classify, gate, verify, wholly_inside_one_class};
use lemnos_flash_wire::frame::{Request, SECTOR, STATUS_OK, payload_crc};
use lemnos_flash_wire::timing::TRANSFER_BUDGET_US;

/// Why a round stopped. Every variant ends the round without a feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The link failed (a cut, a reset, a stall). No status can be sent: the session ends.
    Link(IoError),
    /// The chunk did not have the length its request makes it, or it crosses a region boundary (a chunk lies wholly inside
    /// one class). Nothing was written; the status is bad request.
    Length,
    /// The chunk's CRC did not match its payload. Nothing was written.
    Corrupt,
    /// The region needs the compared mark and the request lacked it. Nothing was written.
    NotCompared,
    /// The storage failed. The range may be partly written.
    Storage(IoError),
    /// The read-back did not equal the payload. The range may be partly written.
    Verify,
}

/// A write round for `request`. `buf` is the chunk's length exactly (payload and CRC), and `scratch` at least
/// the payload. On success the storage holds the payload at `lba`, the read-back equals it, and the watchdog
/// was fed once. The caller sends the status after this returns.
pub fn write_round<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    regions: &[Region],
    request: &Request,
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<(), Fault> {
    let (lba, count) = (request.lba, request.count);
    let expected = usize::from(count) * SECTOR + 2;
    if buf.len() != expected {
        return Err(Fault::Length);
    }
    let len = link.recv(buf, TRANSFER_BUDGET_US).map_err(Fault::Link)?;
    if len != expected {
        return Err(Fault::Length);
    }
    if !wholly_inside_one_class(regions, lba, count) {
        return Err(Fault::Length);
    }
    let verified = verify(buf).ok_or(Fault::Corrupt)?;
    let class = classify(regions, lba, count);
    let admitted = gate(class, request.compared(), verified).map_err(|_| Fault::NotCompared)?;
    let payload = admitted.payload();
    storage
        .write(lba, payload, TRANSFER_BUDGET_US)
        .map_err(Fault::Storage)?;
    let back = &mut scratch[..payload.len()];
    storage
        .read(lba, back, TRANSFER_BUDGET_US)
        .map_err(Fault::Storage)?;
    if back != payload {
        return Err(Fault::Verify);
    }
    wdt.feed_at(link.now_us());
    Ok(())
}

/// A read round for `count` sectors at `lba`. `buf` holds `1 + count * 512 + 2` bytes: the status, the sectors and
/// their CRC. On a storage failure nothing is sent here: the caller sends the one-byte failure status. On success
/// the reply is sent and the watchdog is fed after the send is taken.
pub fn read_round<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    lba: u32,
    count: u16,
    buf: &mut [u8],
) -> Result<(), Fault> {
    let payload_len = usize::from(count) * SECTOR;
    if buf.len() < 1 + payload_len + 2 {
        return Err(Fault::Length);
    }
    storage
        .read(lba, &mut buf[1..1 + payload_len], TRANSFER_BUDGET_US)
        .map_err(Fault::Storage)?;
    buf[0] = STATUS_OK;
    let crc = payload_crc(&buf[..1 + payload_len]);
    buf[1 + payload_len..3 + payload_len].copy_from_slice(&crc);
    link.send(&buf[..3 + payload_len], TRANSFER_BUDGET_US)
        .map_err(Fault::Link)?;
    wdt.feed_at(link.now_us());
    Ok(())
}
