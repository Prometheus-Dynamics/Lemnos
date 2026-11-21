//! The stage: the K230 USB-boot stage's state machine. The host sends command packets and data;
//! the stage answers them, writes the medium, and feeds the watchdog.
//!
//! The rules the stage enforces, whatever the host sends:
//! - **Feed after a verified chunk only.** A write piece is fed only after it is read back equal
//!   to what was sent. A VERIFY feeds after each chunk it has read and hashed. Nothing else feeds.
//! - **No write under a valid header.** While the medium holds a header the ROM would boot, a
//!   write or erase may touch only the header block. So the host must erase the header first.
//! - **Header last, and only for a verified payload.** A write to the header block must carry a
//!   header that names the payload the last VERIFY covered, with the same size and digest.
//! - **Reboot needs its mark,** and then the stage asks for a fast reset.
//!
//! The stage does no waiting of its own: it is driven one message at a time. A host that stops
//! sending leaves the watchdog unfed, and the watchdog resets the board.

use sha2::{Digest, Sha256};

use crate::format::{
    self, BOOT_HEADER_LEN, BootHeader, CHUNK, HEADER_BLOCK, PACKET, RESULT_MESSAGE, RESULT_OK,
    STAGE_MEDIUM_TYPE, cmd,
};
use crate::medium::Medium;
use crate::wdt::Feed;

/// The stage has nothing to send: the host's read times out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoReply;

/// The host asked for a reboot. The caller makes the fast reset, and nothing is fed after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reboot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Session {
    Idle,
    /// Data pieces are arriving for `[cursor, end)`.
    Writing(Write),
    /// The host is reading `[cursor, end)` in chunks.
    Reading {
        cursor: u64,
        end: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Write {
    cursor: u64,
    end: u64,
    /// The write covers the header block, so its first piece must carry the verified header.
    header: bool,
    /// The first reason the write failed; later pieces are dropped and the host is told.
    error: Option<&'static str>,
}

/// The last VERIFY: the range it covered and its digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Verified {
    offset: u64,
    size: u64,
    digest: [u8; 32],
}

/// The stage, over a medium and a watchdog it has armed.
pub struct Stage<M: Medium, F: Feed> {
    medium: M,
    feed: F,
    geometry: crate::medium::Geometry,
    /// The medium's geometry fits the header block; otherwise every write and erase is refused.
    usable: bool,
    /// The one command reply waiting for the host.
    reply: Option<[u8; PACKET]>,
    session: Session,
    verified: Option<Verified>,
    buf: [u8; CHUNK],
}

impl<M: Medium, F: Feed> Stage<M, F> {
    /// A stage over `medium`, feeding `feed`. The watchdog must be armed first; `feed` can only be
    /// made from an armed watchdog (see [`crate::wdt::Dog`]).
    pub fn new(medium: M, feed: F) -> Self {
        let geometry = medium.geometry();
        let usable = geometry.block > 0
            && geometry.erase > 0
            && geometry.erase.is_multiple_of(geometry.block)
            && HEADER_BLOCK.is_multiple_of(geometry.erase)
            && geometry.capacity >= HEADER_BLOCK;
        Self {
            medium,
            feed,
            geometry,
            usable,
            reply: None,
            session: Session::Idle,
            verified: None,
            buf: [0; CHUNK],
        }
    }

    /// The number of watchdog feeds the stage has made (test support: the feed is behind `F`, and a
    /// test's feed counts through this).
    pub fn feed_count_for_test(&self) -> u32 {
        self.feed.count_for_test()
    }

    /// Abandons whatever the stage was in the middle of: a bus reset ends the host's session. The
    /// medium keeps what it holds (a half-written piece is not a verified one, and a header is only
    /// written after its payload is verified again).
    pub fn abandon(&mut self) {
        self.session = Session::Idle;
        self.reply = None;
        self.verified = None;
    }

    /// The medium, for a test or the simulator to look at.
    pub fn medium(&self) -> &M {
        &self.medium
    }

    /// Takes one message the host sent: a command packet, or a data piece of a write.
    pub fn on_out(&mut self, message: &[u8]) -> Result<(), Reboot> {
        if let Session::Writing(write) = self.session {
            self.piece(write, message);
            return Ok(());
        }
        if let Session::Reading { .. } = self.session {
            self.session = Session::Idle;
            self.fail(cmd::READ, "the host sent while the stage was reading");
            return Ok(());
        }
        if message.len() != PACKET {
            self.fail(0, "a command is one 60-byte packet");
            return Ok(());
        }
        let command = format::le16(message, 0);
        let size = usize::from(format::le16(message, 4));
        if size > format::PACKET_DATA {
            self.fail(command, "a data size over 54");
            return Ok(());
        }
        let data = &message[format::PACKET_HEAD..format::PACKET_HEAD + size];
        match command {
            cmd::NOP => self.answer(command, RESULT_OK, &[]),
            cmd::REBOOT => {
                if data.len() == 8 && format::le64(data, 0) == format::REBOOT_MARK {
                    return Err(Reboot);
                }
                self.fail(command, "the reboot mark is wrong");
            }
            cmd::PROBE => {
                if data.len() != 2 {
                    self.fail(command, "a probe names one medium and nothing else");
                } else {
                    let mut out = [0u8; 16];
                    format::put64(&mut out, 0, CHUNK as u64);
                    format::put64(&mut out, 8, CHUNK as u64);
                    self.answer(command, RESULT_OK, &out);
                }
            }
            cmd::GET_INFO => {
                let mut out = [0u8; 32];
                format::put64(&mut out, 0, self.geometry.capacity);
                format::put64(&mut out, 8, self.geometry.block);
                format::put64(&mut out, 16, self.geometry.erase);
                // timeout_ms:32, wp:8, type:7, valid:1, in the last word.
                let bits = (u64::from(STAGE_MEDIUM_TYPE) << 40) | (1 << 47);
                format::put64(&mut out, 24, bits);
                self.answer(command, RESULT_OK, &out);
            }
            cmd::ERASE => self.erase(data),
            cmd::WRITE => self.write_start(data),
            cmd::READ => self.read_start(data),
            cmd::VERIFY => self.verify(data),
            _ => self.fail(command, "an unknown command"),
        }
        Ok(())
    }

    /// Gives the host the next message from the stage: a command reply, or a read chunk.
    /// `out` is the host's request; a reply longer than it is not sent.
    pub fn on_in(&mut self, out: &mut [u8]) -> Result<usize, NoReply> {
        if let Some(reply) = self.reply {
            if out.len() < PACKET {
                return Err(NoReply);
            }
            out[..PACKET].copy_from_slice(&reply);
            self.reply = None;
            return Ok(PACKET);
        }
        match self.session {
            Session::Reading { cursor, end } if cursor < end => {
                let take = (end - cursor).min(CHUNK as u64) as usize;
                if out.len() < format::PACKET_HEAD + take {
                    return Err(NoReply);
                }
                if self.medium.read(cursor, &mut self.buf[..take]).is_err() {
                    self.session = Session::Idle;
                    let reply = format::reply(
                        cmd::READ_CHUNK,
                        RESULT_MESSAGE,
                        b"the medium refused the read",
                    );
                    out[..PACKET].copy_from_slice(&reply);
                    return Ok(PACKET);
                }
                out[0..2].copy_from_slice(&(cmd::READ_CHUNK | format::REPLY_FLAG).to_le_bytes());
                out[2..4].copy_from_slice(&RESULT_OK.to_le_bytes());
                out[4..6].copy_from_slice(&(take as u16).to_le_bytes());
                out[6..6 + take].copy_from_slice(&self.buf[..take]);
                self.session = Session::Reading {
                    cursor: cursor + take as u64,
                    end,
                };
                Ok(format::PACKET_HEAD + take)
            }
            Session::Reading { .. } => {
                self.session = Session::Idle;
                if out.len() < PACKET {
                    return Err(NoReply);
                }
                out[..PACKET].copy_from_slice(&format::reply(cmd::READ_CHUNK, RESULT_OK, &[]));
                Ok(PACKET)
            }
            _ => Err(NoReply),
        }
    }

    fn answer(&mut self, command: u16, result: u16, data: &[u8]) {
        self.reply = Some(format::reply(command, result, data));
    }

    fn fail(&mut self, command: u16, message: &'static str) {
        let bytes = message.as_bytes();
        let bytes = &bytes[..bytes.len().min(format::PACKET_DATA)];
        self.reply = Some(format::reply(command, RESULT_MESSAGE, bytes));
    }

    /// Whether the medium holds a header the ROM would boot right now.
    fn header_on_medium(&mut self) -> bool {
        match self.medium.read(0, &mut self.buf[..BOOT_HEADER_LEN]) {
            Ok(()) => format::rom_accepts(&self.buf[..BOOT_HEADER_LEN]),
            // Unreadable: assume the header is there, so nothing is written over it.
            Err(_) => true,
        }
    }

    /// The checks every erase and write share: the range is on the medium and aligned, and a
    /// valid header is untouched unless the range is the header block.
    fn check_range(
        &mut self,
        offset: u64,
        len: u64,
        whole_blocks: u64,
    ) -> Result<(), &'static str> {
        if !self.usable {
            return Err("the medium's block sizes do not fit the boot header");
        }
        let g = self.geometry;
        if len == 0 || offset > g.capacity || len > g.capacity - offset {
            return Err("the range runs past the medium");
        }
        if !offset.is_multiple_of(g.erase) || !len.is_multiple_of(whole_blocks) {
            return Err("the range is not aligned to the medium");
        }
        if self.header_on_medium() && !(offset == 0 && len <= HEADER_BLOCK) {
            return Err("a valid boot header is on the medium: erase it first");
        }
        self.medium.permits(offset, len).map_err(|e| e.0)?;
        Ok(())
    }

    fn erase(&mut self, data: &[u8]) {
        if data.len() != 16 {
            return self.fail(cmd::ERASE, "an erase names an offset and a size");
        }
        let (offset, size) = (format::le64(data, 0), format::le64(data, 8));
        let erase = self.geometry.erase;
        if let Err(message) = self.check_range(offset, size, erase) {
            return self.fail(cmd::ERASE, message);
        }
        self.verified = None;
        match self.medium.erase(offset, size) {
            Ok(()) => self.answer(cmd::ERASE, RESULT_OK, &[]),
            Err(_) => self.fail(cmd::ERASE, "the medium refused the erase"),
        }
    }

    fn write_start(&mut self, data: &[u8]) {
        if data.len() != 32 {
            return self.fail(
                cmd::WRITE,
                "a write names an offset, a size, a region and a flag",
            );
        }
        let (offset, total, region) = (
            format::le64(data, 0),
            format::le64(data, 8),
            format::le64(data, 16),
        );
        let block = self.geometry.block;
        if let Err(message) = self.check_range(offset, total, block) {
            return self.fail(cmd::WRITE, message);
        }
        let capacity = self.geometry.capacity;
        if region != 0 && (total > region || region > capacity - offset) {
            return self.fail(cmd::WRITE, "the write exceeds its region");
        }
        if offset == 0 && total != HEADER_BLOCK {
            return self.fail(cmd::WRITE, "the boot header is written as one whole block");
        }
        // A payload write may change what a VERIFY covered; the header write must not clear it.
        if offset != 0 {
            self.verified = None;
        }
        self.session = Session::Writing(Write {
            cursor: offset,
            end: offset + total,
            header: offset == 0,
            error: None,
        });
        self.answer(cmd::WRITE, RESULT_OK, &[]);
    }

    /// One data piece of a write. Stores it, reads it back, feeds only if it matches; when the
    /// write's last byte has come, the host gets the write's result.
    fn piece(&mut self, mut write: Write, data: &[u8]) {
        if write.error.is_none() && !data.is_empty() {
            if data.len() as u64 > write.end - write.cursor {
                write.error = Some("more data than the write asked for");
            } else if write.header && write.cursor == 0 && !self.header_commits(data) {
                write.error = Some("the boot header does not name the verified payload");
            } else if let Err(message) = self.store(write.cursor, data) {
                write.error = Some(message);
            }
            write.cursor += data.len() as u64;
        }
        if write.cursor >= write.end {
            self.session = Session::Idle;
            if write.header {
                // The header is written: the verified payload is now the booted one, and must be
                // verified again before another header names it.
                self.verified = None;
            }
            match write.error {
                None => self.answer(cmd::WRITE, RESULT_OK, &[]),
                Some(message) => self.fail(cmd::WRITE, message),
            }
        } else {
            self.session = Session::Writing(write);
        }
    }

    /// Whether the first piece of the header block carries a header that names the verified
    /// payload exactly.
    fn header_commits(&self, data: &[u8]) -> bool {
        if data.len() < BOOT_HEADER_LEN {
            return false;
        }
        let Some(header) = BootHeader::decode(&data[..BOOT_HEADER_LEN]) else {
            return false;
        };
        match self.verified {
            Some(v) => {
                v.offset == header.payload_offset
                    && v.size == header.payload_size
                    && v.digest == header.digest
            }
            None => false,
        }
    }

    /// Writes `data` at `offset`, reads it back, and feeds the watchdog only if it is equal.
    fn store(&mut self, offset: u64, data: &[u8]) -> Result<(), &'static str> {
        if data.len() > CHUNK {
            return Err("a piece is bigger than the stage's buffer");
        }
        self.medium
            .write(offset, data)
            .map_err(|_| "the medium refused the write")?;
        let back = &mut self.buf[..data.len()];
        self.medium
            .read(offset, back)
            .map_err(|_| "the medium refused the read-back")?;
        if &*back != data {
            return Err("the read-back differs from what was written");
        }
        self.feed.feed();
        Ok(())
    }

    fn read_start(&mut self, data: &[u8]) {
        if data.len() != 16 {
            return self.fail(cmd::READ, "a read names an offset and a size");
        }
        let (offset, size) = (format::le64(data, 0), format::le64(data, 8));
        let g = self.geometry;
        if !self.usable || size == 0 || offset > g.capacity || size > g.capacity - offset {
            return self.fail(cmd::READ, "the read runs past the medium");
        }
        self.session = Session::Reading {
            cursor: offset,
            end: offset + size,
        };
        self.answer(cmd::READ, RESULT_OK, &[]);
    }

    /// Reads and hashes `[offset, offset + size)`, feeding after each chunk, and keeps the
    /// digest for the header commit.
    fn verify(&mut self, data: &[u8]) {
        if data.len() != 16 {
            return self.fail(cmd::VERIFY, "a verify names an offset and a size");
        }
        let (offset, size) = (format::le64(data, 0), format::le64(data, 8));
        let g = self.geometry;
        if !self.usable || size == 0 || offset > g.capacity || size > g.capacity - offset {
            return self.fail(cmd::VERIFY, "the verify runs past the medium");
        }
        let mut hasher = Sha256::new();
        let end = offset + size;
        let mut at = offset;
        while at < end {
            let take = (end - at).min(CHUNK as u64) as usize;
            if self.medium.read(at, &mut self.buf[..take]).is_err() {
                return self.fail(cmd::VERIFY, "the medium refused the verify read");
            }
            hasher.update(&self.buf[..take]);
            self.feed.feed();
            at += take as u64;
        }
        let digest: [u8; 32] = hasher.finalize().into();
        self.verified = Some(Verified {
            offset,
            size,
            digest,
        });
        let mut out = [0u8; 48];
        out[..32].copy_from_slice(&digest);
        format::put64(&mut out, 32, size);
        format::put64(&mut out, 40, 0);
        self.answer(cmd::VERIFY, RESULT_OK, &out);
    }
}
