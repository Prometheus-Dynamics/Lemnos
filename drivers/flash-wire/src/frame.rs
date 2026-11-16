//! The request and reply frames of the flash link, and the CRC that checks them.
//!
//! A request is 12 bytes: `LM`, the op (u8), the reserved byte (u8), the sector count (u16, little
//! endian, 1 to [`MAX_SECTORS`]), the first sector (u32, little endian), then the CRC-16/CCITT-FALSE
//! of the first 10 bytes (big-endian). The reserved byte's meaning is per op, see [`Request`].
//!
//! Replies are sent in one transfer each, and a reply's length is what the status says it is (see the
//! README): a request that was refused gets a one-byte status reply, and so does a failed read.
//!
//! A chunk, which the host sends after a write request, and a read reply are a payload then its
//! CRC-16 (big-endian) over the payload. A write is answered by one status byte, an END by one
//! status byte, and an identify by the identity record (see [`crate::identity`]).

/// The magic at the start of a request.
pub const MAGIC_REQUEST: [u8; 2] = *b"LM";
/// The size of a sector on the card.
pub const SECTOR: usize = 512;
/// The most sectors one request moves: 128 * 512 = 64 KiB.
pub const MAX_SECTORS: u16 = 128;
/// The length of a request frame.
pub const REQUEST_LEN: usize = 12;
/// The byte a host pads a partial last sector with. The stage cannot tell a pad from image data: it compares and writes the
/// pad bytes as image bytes, so a fixed pad byte makes the same image compare equal whichever host wrote it. The stage
/// does not enforce it; the host must use this constant (the wire README, "Padding").
pub const PAD_BYTE: u8 = 0x00;

/// The most bytes a chunk or a read reply holds: the payload and its CRC.
pub const MAX_CHUNK: usize = MAX_SECTORS as usize * SECTOR + 2;

/// The CRC polynomial, CRC-16/CCITT-FALSE: 0x1021, initial value 0xFFFF, no reflection, no final XOR.
pub const CRC_POLY: u16 = 0x1021;
/// The CRC's initial value.
pub const CRC_INIT: u16 = 0xFFFF;

/// The status byte of a write or an END.
pub const STATUS_OK: u8 = 0;
/// The chunk's CRC did not match its payload. Nothing was written.
pub const STATUS_CRC: u8 = 1;
/// The storage refused the write.
pub const STATUS_STORAGE: u8 = 2;
/// The read-back did not equal the payload.
pub const STATUS_VERIFY: u8 = 3;
/// The write went to a region that needs the compared mark, and the request did not carry it. Nothing
/// was written.
pub const STATUS_NOT_COMPARED: u8 = 4;

/// The request frame was not a request this build understands: a wrong length, an unknown op, a count
/// outside 1 to 128, or a reserved bit the op does not define. Nothing was done, and the session goes on.
pub const STATUS_BAD_REQUEST: u8 = 5;

/// The reserved bit of a write that says the host compared the region with its baseline before the
/// write. A write to a boot area is refused without it.
pub const RESERVED_COMPARED: u8 = 1 << 0;

/// The reset kind an END asks for, as its reserved byte.
pub const RESET_NORMAL: u8 = 0;
/// The reset kind that asks the boot ROM for its maskrom path.
pub const RESET_MASKROM: u8 = 3;

/// CRC-16/CCITT-FALSE over `bytes`.
pub fn crc16_ccitt(bytes: &[u8]) -> u16 {
    crc16_ccitt_const(bytes)
}

/// The same CRC as a `const fn`, so the layout hash can use it. Its check value is `0x29B1` for
/// `"123456789"`.
pub const fn crc16_ccitt_const(bytes: &[u8]) -> u16 {
    let mut crc = CRC_INIT;
    let mut i = 0;
    while i < bytes.len() {
        crc ^= (bytes[i] as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ CRC_POLY
            } else {
                crc << 1
            };
            bit += 1;
        }
        i += 1;
    }
    crc
}

/// The CRC of a payload as a chunk or a read reply carries it: two bytes, big-endian.
pub fn payload_crc(payload: &[u8]) -> [u8; 2] {
    crc16_ccitt(payload).to_be_bytes()
}

/// The ops a request names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Read `count` sectors at `lba`. The reply is the sectors, then their CRC.
    Read,
    /// Write `count` sectors at `lba`. A chunk follows the request; the reply is a status byte.
    Write,
    /// End the session. The reserved byte is the reset kind. The reply is a status byte.
    End,
    /// Ask for the identity record. No payload; the reply is the record.
    Identify,
}

impl Op {
    /// The op's number on the wire.
    pub const fn to_wire(self) -> u8 {
        match self {
            Op::Read => 1,
            Op::Write => 2,
            Op::End => 3,
            Op::Identify => 4,
        }
    }

    /// The op a number names, if it names one.
    pub const fn from_wire(byte: u8) -> Option<Op> {
        match byte {
            1 => Some(Op::Read),
            2 => Some(Op::Write),
            3 => Some(Op::End),
            4 => Some(Op::Identify),
            _ => None,
        }
    }
}

/// The reset kind an END asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetKind {
    /// The normal reset.
    Normal,
    /// The reset that asks the boot ROM for its maskrom path.
    Maskrom,
}

impl ResetKind {
    /// The reserved byte of an END that asks for this kind.
    pub const fn to_wire(self) -> u8 {
        match self {
            ResetKind::Normal => RESET_NORMAL,
            ResetKind::Maskrom => RESET_MASKROM,
        }
    }

    /// The kind an END's reserved byte asks for. Any value other than [`RESET_MASKROM`] is a normal
    /// reset, as the first stage has always read it (see `docs/flashing.md`, "Reset kinds").
    pub const fn from_wire(byte: u8) -> ResetKind {
        if byte == RESET_MASKROM {
            ResetKind::Maskrom
        } else {
            ResetKind::Normal
        }
    }
}

/// A request.
///
/// The reserved byte, by op:
/// - `Read` and `Identify`: 0. Any other value is refused.
/// - `Write`: [`RESERVED_COMPARED`] or 0. Any other bit is refused.
/// - `End`: the reset kind (see [`ResetKind::from_wire`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// The op.
    pub op: Op,
    /// The reserved byte, meaning per op.
    pub reserved: u8,
    /// The sector count, 1 to [`MAX_SECTORS`].
    pub count: u16,
    /// The first sector.
    pub lba: u32,
}

impl Request {
    /// A read of `count` sectors at `lba`.
    pub const fn read(lba: u32, count: u16) -> Self {
        Self {
            op: Op::Read,
            reserved: 0,
            count,
            lba,
        }
    }

    /// A write of `count` sectors at `lba`. `compared` says the host compared the region first.
    pub const fn write(lba: u32, count: u16, compared: bool) -> Self {
        Self {
            op: Op::Write,
            reserved: if compared { RESERVED_COMPARED } else { 0 },
            count,
            lba,
        }
    }

    /// An END that asks for `kind`.
    pub const fn end(kind: ResetKind) -> Self {
        Self {
            op: Op::End,
            reserved: kind.to_wire(),
            count: 1,
            lba: 0,
        }
    }

    /// An identify request.
    pub const fn identify() -> Self {
        Self {
            op: Op::Identify,
            reserved: 0,
            count: 1,
            lba: 0,
        }
    }

    /// Whether this is a write that carries the compared mark.
    pub const fn compared(&self) -> bool {
        matches!(self.op, Op::Write) && self.reserved & RESERVED_COMPARED != 0
    }

    /// The reset kind of an END. Meaningless for other ops.
    pub const fn reset_kind(&self) -> ResetKind {
        ResetKind::from_wire(self.reserved)
    }

    /// The 12 bytes on the wire.
    pub fn encode(&self) -> [u8; REQUEST_LEN] {
        let mut out = [0u8; REQUEST_LEN];
        out[0..2].copy_from_slice(&MAGIC_REQUEST);
        out[2] = self.op.to_wire();
        out[3] = self.reserved;
        out[4..6].copy_from_slice(&self.count.to_le_bytes());
        out[6..10].copy_from_slice(&self.lba.to_le_bytes());
        let crc = payload_crc(&out[..10]);
        out[10..12].copy_from_slice(&crc);
        out
    }

    /// Parses and checks a request frame. Every refusal is typed, and nothing after a refusal is
    /// trusted: the caller ends the link.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() != REQUEST_LEN || bytes[0..2] != MAGIC_REQUEST {
            return Err(FrameError::NotRequest);
        }
        if payload_crc(&bytes[..10]) != [bytes[10], bytes[11]] {
            return Err(FrameError::Crc);
        }
        let op = Op::from_wire(bytes[2]).ok_or(FrameError::Op(bytes[2]))?;
        let reserved = bytes[3];
        let count = u16::from_le_bytes([bytes[4], bytes[5]]);
        if count == 0 || count > MAX_SECTORS {
            return Err(FrameError::Count(count));
        }
        let allowed = match op {
            Op::Read | Op::Identify => 0,
            Op::Write => RESERVED_COMPARED,
            Op::End => 0xFF,
        };
        if reserved & !allowed != 0 {
            return Err(FrameError::Reserved { op, reserved });
        }
        Ok(Self {
            op,
            reserved,
            count,
            lba: u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
        })
    }
}

/// Why a request frame was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Not 12 bytes, or the magic is wrong.
    NotRequest,
    /// The request CRC does not match.
    Crc,
    /// The op number names no op.
    Op(u8),
    /// The count is outside 1..=[`MAX_SECTORS`].
    Count(u16),
    /// The reserved byte has a bit the op does not define.
    Reserved {
        /// The op that does not define the bit.
        op: Op,
        /// The reserved byte.
        reserved: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crc_check_value_is_the_standard_one() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
        assert_eq!(crc16_ccitt_const(b"123456789"), 0x29B1);
        assert_eq!(
            crc16_ccitt(b""),
            0xFFFF,
            "an empty payload is the initial value"
        );
    }

    #[test]
    fn the_crc_of_a_payload_is_big_endian() {
        assert_eq!(payload_crc(b"123456789"), [0x29, 0xB1]);
    }
}
