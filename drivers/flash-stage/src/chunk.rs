//! One round of the protocol, as the feed rule and the order rules shape it.
//!
//! A write round: receive the chunk (bounded), verify its CRC, gate it by its region (see
//! [`crate::region`]), write it, read it back and compare, then feed the watchdog and acknowledge. Each
//! step that fails ends the round before the next one, and nothing after a failure is fed.
//!
//! A read round: read the sectors, send them with their CRC, then feed. A failed read or a failed send
//! does not feed.

use crate::guard::Watchdog;
use crate::link::{IoError, Link, Storage};
use crate::region::{Region, classify, gate, verify};
use lemnos_flash_wire::frame::{Request, SECTOR, payload_crc};

/// The longest a single round may take, in microseconds. Each of its three transfers gets a third.
pub const ROUND_BUDGET_US: u64 = 11_000_000;

/// Why a round stopped. Every variant ends the round without a feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The link failed, or the chunk was not the length its request makes it.
    Link(IoError),
    /// The storage failed.
    Storage(IoError),
    /// The chunk's CRC did not match its payload. Nothing was written.
    Corrupt,
    /// The region needs the compared mark and the request lacked it. Nothing was written.
    NotCompared,
    /// The read-back did not equal the payload that was written.
    Verify,
}

/// A write round for `count` sectors at `lba`. `buf` holds exactly the chunk (payload and CRC), and
/// `scratch` at least the payload. On success the storage holds the payload at `lba`, the read-back equals
/// it, the watchdog was fed once and the link acknowledged.
pub fn write_round<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    regions: &[Region],
    request: &Request,
    buf: &mut [u8],
    scratch: &mut [u8],
) -> Result<(), Fault> {
    let third = ROUND_BUDGET_US / 3;
    let (lba, count) = (request.lba, request.count);
    let expected = usize::from(count) * SECTOR + 2;
    if buf.len() != expected {
        return Err(Fault::Link(IoError::Failed));
    }
    let len = link.recv(buf, third).map_err(Fault::Link)?;
    if len != expected {
        // A short or long chunk is a cut frame: the request said how long it is.
        return Err(Fault::Link(IoError::Failed));
    }
    let verified = verify(buf).ok_or(Fault::Corrupt)?;
    let class = classify(regions, lba, count);
    let admitted = gate(class, request.compared(), verified).map_err(|_| Fault::NotCompared)?;
    let payload = admitted.payload();
    storage.write(lba, payload, third).map_err(Fault::Storage)?;
    let back = &mut scratch[..payload.len()];
    storage.read(lba, back, third).map_err(Fault::Storage)?;
    if back != payload {
        return Err(Fault::Verify);
    }
    wdt.feed();
    link.ack().map_err(Fault::Link)
}

/// A read round: reads `count` sectors at `lba` into `buf`, appends their CRC, and sends them. `buf`
/// holds `count * 512 + 2` bytes. A failed read or a failed send never feeds.
pub fn read_round<L: Link, S: Storage, W: Watchdog>(
    link: &mut L,
    storage: &mut S,
    wdt: &mut W,
    lba: u32,
    count: u16,
    buf: &mut [u8],
) -> Result<(), Fault> {
    let third = ROUND_BUDGET_US / 3;
    let payload_len = usize::from(count) * SECTOR;
    if buf.len() < payload_len + 2 {
        return Err(Fault::Link(IoError::Failed));
    }
    storage
        .read(lba, &mut buf[..payload_len], third)
        .map_err(Fault::Storage)?;
    let crc = payload_crc(&buf[..payload_len]);
    buf[payload_len..payload_len + 2].copy_from_slice(&crc);
    link.send(&buf[..payload_len + 2], third)
        .map_err(Fault::Link)?;
    wdt.feed();
    Ok(())
}
