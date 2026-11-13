//! Two-copy records: a small value the board writes in place, one 512-byte sector at a time, so a
//! power cut at any moment leaves the old value or the new one.
//!
//! A record is a pre-created file (or a fixed raw place) of two sectors. Each sector is a copy:
//!
//! | bytes  | field                                                    |
//! |--------|----------------------------------------------------------|
//! | 0..4   | magic `LMRC`                                             |
//! | 4      | version (1)                                              |
//! | 5      | kind ([`Kind`], one per record type)                     |
//! | 6..8   | zero                                                     |
//! | 8..16  | sequence, little endian: a write uses the highest + 1    |
//! | 16..18 | payload length, little endian (at most [`MAX_PAYLOAD`])  |
//! | 18..   | payload, then zero padding                               |
//! | 508..512 | CRC-32 (IEEE) over bytes 0..508, little endian         |
//!
//! Reading takes the valid copy with the higher sequence. Writing overwrites the copy that does not
//! hold it, with one sector write and nothing else. A torn sector fails its CRC, so the other copy
//! still holds the previous value. Neither copy is ever written by the same update.
//!
//! Adding a record kind: add a [`Kind`] constant here (the next free number, never reused), a
//! file name in [`Kind::file_name`], and nothing else: [`precreate`] builds the files from the list
//! of kinds the image needs. See `docs/slots.md`, "Two-copy records".
//!
//! The device is any [`Sectors`]. [`FileSectors`] is the block node (or a file in tests): every write
//! is followed by `sync_data`, so the sector is on the medium when `write` returns. Reads go through
//! the same descriptor, so they see that write; nothing mounts the partition, so no other cache holds
//! these sectors.

use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

/// One device sector, the size of a copy.
pub const SECTOR: usize = 512;
/// The largest payload a copy carries.
pub const MAX_PAYLOAD: usize = 508 - HEADER;
const MAGIC: &[u8; 4] = b"LMRC";
const VERSION: u8 = 1;
const HEADER: usize = 18;
const CRC_AT: usize = 508;

/// A record's kind: which value it holds. The number is the on-disk byte, so it is never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Kind(pub u8);

impl Kind {
    /// A pending trial: the slot id, its attempts left, and the outcome text.
    pub const TRIAL: Kind = Kind(1);
    /// The reason a trial fell back, when the data partition could not take it.
    pub const REASON: Kind = Kind(2);
    /// The boot history (I6f).
    pub const BOOT_HISTORY: Kind = Kind(3);
    /// The board's security switch and its generation (S2).
    pub const SECURITY: Kind = Kind(4);

    /// The pre-created file's FAT directory name for this kind (eleven bytes: 8.3, space padded,
    /// upper case). `None` for a kind this build does not know.
    pub fn file_name(self) -> Option<&'static [u8; 11]> {
        match self {
            Kind::TRIAL => Some(b"TRIAL   REC"),
            Kind::REASON => Some(b"REASON  REC"),
            Kind::BOOT_HISTORY => Some(b"BOOTHISTREC"),
            Kind::SECURITY => Some(b"SECURE  REC"),
            _ => None,
        }
    }
}

/// One sector, as read or written.
pub type Sector = [u8; SECTOR];

/// Why a record could not be read or written.
#[derive(Debug)]
pub enum Error {
    /// The device failed. Nothing was written past the failure.
    Io(io::Error),
    /// Both copies are valid with the same sequence and different contents: the record was not
    /// written by this code. The caller refuses rather than choosing one.
    Conflict,
    /// The payload does not fit a copy.
    TooLong(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "record I/O failed: {e}"),
            Error::Conflict => {
                write!(f, "record copies disagree at the same sequence: refused")
            }
            Error::TooLong(n) => write!(f, "record payload is {n} bytes; at most {MAX_PAYLOAD}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// CRC-32 (IEEE, reflected, the zlib polynomial), bit by bit: a few hundred bytes per call, and no
/// table to get wrong.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A valid copy, as decoded: the kind, its sequence and the payload (borrowed from the sector).
#[derive(Debug, PartialEq, Eq)]
pub struct Copy<'a> {
    /// The record kind the copy carries.
    pub kind: Kind,
    /// The copy's sequence.
    pub seq: u64,
    /// The payload bytes.
    pub payload: &'a [u8],
}

/// Writes one copy into `out`: header, payload, zero padding, CRC.
pub fn encode(kind: Kind, seq: u64, payload: &[u8], out: &mut Sector) -> Result<(), Error> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Error::TooLong(payload.len()));
    }
    out.fill(0);
    out[0..4].copy_from_slice(MAGIC);
    out[4] = VERSION;
    out[5] = kind.0;
    out[8..16].copy_from_slice(&seq.to_le_bytes());
    let len = payload.len() as u16;
    out[16..18].copy_from_slice(&len.to_le_bytes());
    out[HEADER..HEADER + payload.len()].copy_from_slice(payload);
    let crc = crc32(&out[..CRC_AT]);
    out[CRC_AT..].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Decodes a sector as a copy. `None` when it is not a copy: wrong magic or version, a length past
/// the payload's room, non-zero reserved bytes, or a CRC that does not match.
pub fn decode(sector: &Sector) -> Option<Copy<'_>> {
    if &sector[0..4] != MAGIC || sector[4] != VERSION || sector[6] != 0 || sector[7] != 0 {
        return None;
    }
    let len = usize::from(u16::from_le_bytes([sector[16], sector[17]]));
    if len > MAX_PAYLOAD {
        return None;
    }
    let crc = u32::from_le_bytes([
        sector[CRC_AT],
        sector[CRC_AT + 1],
        sector[CRC_AT + 2],
        sector[CRC_AT + 3],
    ]);
    if crc32(&sector[..CRC_AT]) != crc {
        return None;
    }
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&sector[8..16]);
    Some(Copy {
        kind: Kind(sector[5]),
        seq: u64::from_le_bytes(seq),
        payload: &sector[HEADER..HEADER + len],
    })
}

/// The device a record is written on. `write` returns when the sector is on the medium.
pub trait Sectors {
    /// Reads sector `lba` (relative to the record's partition or place).
    fn read(&mut self, lba: u64, buf: &mut Sector) -> io::Result<()>;
    /// Writes sector `lba`, durably, before it returns.
    fn write(&mut self, lba: u64, buf: &Sector) -> io::Result<()>;
}

/// The current value of a record.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Current {
    /// The copy's sequence.
    pub seq: u64,
    /// Which copy holds it: `0` is the first sector, `1` the second.
    pub copy: u8,
    /// The payload.
    pub payload: Vec<u8>,
}

/// A record's two copies, at `lba` and `lba + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    lba: u64,
}

impl Pair {
    /// The record whose first copy is sector `lba`.
    pub const fn at(lba: u64) -> Self {
        Self { lba }
    }

    /// The first copy's sector.
    pub const fn lba(&self) -> u64 {
        self.lba
    }

    /// The value of kind `kind`. `Ok(None)` when neither copy is a valid copy of that kind: the
    /// record is absent, which is not a failure. `Err(Conflict)` when both are valid with the same
    /// sequence and different payloads.
    pub fn read<D: Sectors>(&self, dev: &mut D, kind: Kind) -> Result<Option<Current>, Error> {
        let copies = self.read_both(dev)?;
        let mut best: Option<Current> = None;
        for (i, copy) in copies.iter().enumerate() {
            let Some(c) = copy else { continue };
            if c.kind != kind {
                continue;
            }
            match &best {
                None => {
                    best = Some(Current {
                        seq: c.seq,
                        copy: i as u8,
                        payload: c.payload.to_vec(),
                    })
                }
                Some(b) if c.seq > b.seq => {
                    best = Some(Current {
                        seq: c.seq,
                        copy: i as u8,
                        payload: c.payload.to_vec(),
                    })
                }
                Some(b) if c.seq == b.seq && b.payload != c.payload => return Err(Error::Conflict),
                Some(_) => {}
            }
        }
        Ok(best)
    }

    /// Replaces the value with `payload` as kind `kind`, in one sector write. Returns the sequence
    /// written. The write goes to the copy that does not hold the highest valid sequence, so the
    /// other copy is the value a cut leaves.
    pub fn write<D: Sectors>(&self, dev: &mut D, kind: Kind, payload: &[u8]) -> Result<u64, Error> {
        let copies = self.read_both(dev)?;
        let mut target = 0usize;
        let mut max_seq = None::<u64>;
        for (i, copy) in copies.iter().enumerate() {
            let Some(c) = copy else { continue };
            if c.kind != kind {
                continue;
            }
            match max_seq {
                None => {
                    max_seq = Some(c.seq);
                    target = 1 - i;
                }
                Some(m) if c.seq > m => {
                    max_seq = Some(c.seq);
                    target = 1 - i;
                }
                Some(m) if c.seq == m => {
                    // Two valid copies at the same sequence: refuse unless they agree, in which
                    // case either may be overwritten. They agree when the payloads match.
                    let other = copies[1 - i].as_ref().map(|o| o.payload.as_slice());
                    if other != Some(c.payload.as_slice()) {
                        return Err(Error::Conflict);
                    }
                }
                Some(_) => {}
            }
        }
        let seq = max_seq.map_or(1, |m| m + 1);
        let mut sector = [0u8; SECTOR];
        encode(kind, seq, payload, &mut sector)?;
        dev.write(self.lba + target as u64, &sector)?;
        Ok(seq)
    }

    /// Both copies decoded: `None` for a copy that is not valid. Owned payloads so the caller may
    /// keep reading.
    fn read_both<D: Sectors>(&self, dev: &mut D) -> Result<[Option<OwnedCopy>; 2], Error> {
        let mut out = [None, None];
        for (i, slot) in out.iter_mut().enumerate() {
            let mut sector = [0u8; SECTOR];
            dev.read(self.lba + i as u64, &mut sector)?;
            *slot = decode(&sector).map(|c| OwnedCopy {
                kind: c.kind,
                seq: c.seq,
                payload: c.payload.to_vec(),
            });
        }
        Ok(out)
    }
}

/// A decoded copy with its payload owned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedCopy {
    kind: Kind,
    seq: u64,
    payload: Vec<u8>,
}

/// A block node or a plain file as the record device. Writes are followed by `sync_data`.
#[derive(Debug)]
pub struct FileSectors {
    file: File,
    base: u64,
}

impl FileSectors {
    /// The device whose sector 0 is byte `base` of `file` (the partition's first byte, for a
    /// whole-disk file; 0 for a partition's own node).
    pub fn new(file: File, base: u64) -> Self {
        Self { file, base }
    }
}

impl Sectors for FileSectors {
    fn read(&mut self, lba: u64, buf: &mut Sector) -> io::Result<()> {
        self.file
            .read_exact_at(buf, self.base + lba * SECTOR as u64)
    }

    fn write(&mut self, lba: u64, buf: &Sector) -> io::Result<()> {
        self.file
            .write_all_at(buf, self.base + lba * SECTOR as u64)?;
        self.file.sync_data()
    }
}

/// The files a boot partition needs for `kinds`: each kind's file, two sectors, copy A at sequence
/// 1 holding `initial` and copy B at sequence 0 holding the same bytes, so both are valid and A
/// wins. Returned as `(file name, sector bytes)` in the order given. Run by the image builder, which
/// places each file with [`crate::bootfat`] and writes the bytes into the image.
/// One pre-created file: its FAT directory name and its bytes.
pub type Precreated = (&'static [u8; 11], Vec<u8>);

pub fn precreate(kinds: &[(Kind, &[u8])]) -> Result<Vec<Precreated>, Error> {
    let mut out = Vec::with_capacity(kinds.len());
    for &(kind, initial) in kinds {
        let name = kind.file_name().ok_or_else(|| {
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("record kind {} has no file name in this build", kind.0),
            ))
        })?;
        let mut bytes = vec![0u8; 2 * SECTOR];
        let mut sector = [0u8; SECTOR];
        encode(kind, 0, initial, &mut sector)?;
        bytes[SECTOR..].copy_from_slice(&sector);
        encode(kind, 1, initial, &mut sector)?;
        bytes[..SECTOR].copy_from_slice(&sector);
        out.push((name, bytes));
    }
    Ok(out)
}

#[cfg(test)]
#[path = "twocopy_tests.rs"]
mod tests;
