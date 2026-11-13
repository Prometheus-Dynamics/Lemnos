//! Raw NAND first flash (`kind = "mtd"`): one MTD partition, written in erase blocks.
//!
//! The board names one partition (`/dev/mtdN`) and the kind of its image: `image = "ubi"` (a UBI
//! image of whole erase blocks, each carrying its erase-counter header) or `image = "raw"` (a linear
//! partition image). The driver takes the image as a stream and writes it one erase block at a time:
//!
//! - Image erase block `i` goes to the `i`-th good erase block of the partition; bad blocks (the
//!   chip's bad block table, through `MEMGETBADBLOCK`) are skipped. An image that needs more good
//!   blocks than the chip has is refused before any erase.
//! - Each good block is erased right before its bytes are written. Whole pages are written; the
//!   trailing erased (0xFF) pages of a block are not, so UBI sees them as free. Every page is read
//!   back: ECC-corrected bit flips are counted, an uncorrectable read or a page that differs is a
//!   failure. A failed erase or write marks the block bad when the board allows it (`mark_bad`,
//!   off by default), and fails the job either way.
//! - UBI: each image block's erase-counter header is checked (magic, version, CRC, and the VID and
//!   data offsets against the target's sub-page and page sizes) before its block is erased; the
//!   first block is checked before anything is erased. A target block that already holds a valid
//!   header carries its erase count forward (+1, the header CRC recomputed). A block with no valid
//!   header takes the mean of the counts seen on the chip, or the image's own count on a blank chip.
//!   Good blocks the image does not cover are erased at the end, so no stale UBI volume survives.
//!
//! The UBI on-flash layout is the kernel's documented one (`drivers/mtd/ubi/ubi-media.h`, the
//! `ubi_ec_hdr` fields, big-endian; the CRC is `crc32_le` seeded with `UBI_CRC32_INIT`, 0xFFFFFFFF,
//! over the first 60 bytes, with no final inversion). No vendor or GPL code is used.
//!
//! The chip is behind [`Chip`], so the tests can give it bad blocks and failures; on a board it is
//! [`Device`] (the character device and its ioctls) and in a test target [`Fake`] (a temp file).

use std::fs::{self, File};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::path::Path;

use lemnos_core::Status;
use sha2::{Digest, Sha256};

/// A refusal or a failure: the status, and the sentence that says why.
pub type Failure = (Status, String);

/// The UBI erase-counter header's length, and what its fields hold (`ubi-media.h`).
pub const EC_LEN: usize = 64;
const MAGIC: &[u8; 4] = b"UBI#";
const VERSION: u8 = 1;
/// `UBI_CRC32_INIT`: the seed the header CRC starts from.
const CRC_INIT: u32 = 0xFFFF_FFFF;
/// `UBI_VID_HDR_SIZE`: the VID header's length, which fixes where the data starts.
const VID_LEN: u64 = 64;
/// `EBADMSG`: a read the ECC could not correct.
const EBADMSG: i32 = 74;
/// `EINVAL`, for an offset a kernel request can't carry.
const EINVAL: i32 = 22;
const ERASED: u8 = 0xFF;
/// `ENOSPC`: the image needs more of the target than it has.
const NO_SPACE: Status = Status::from_errno(28);

/// The largest erase block the one-block buffer is sized for: 8 MiB (no NAND in use is larger).
pub const MAX_ERASE: u64 = 8 << 20;
/// The largest write unit (a page): 64 KiB.
pub const MAX_WRITE: u64 = 64 << 10;

/// What the board's image is: a UBI image or a linear partition image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    Ubi,
    Raw,
}

impl Image {
    /// The board file's `image` setting.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ubi" => Some(Self::Ubi),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }

    /// The format string the signed message is built from for this image (`mtd-ubi`, `mtd-raw`).
    pub fn format(self) -> &'static str {
        match self {
            Self::Ubi => "mtd-ubi",
            Self::Raw => "mtd-raw",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Ubi => "ubi",
            Self::Raw => "raw",
        }
    }
}

/// The partition's geometry: erase block, write unit (page), sub-page, and size, in bytes. `oob`
/// is reported in `info` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub erase: u64,
    pub write: u64,
    pub subpage: u64,
    pub size: u64,
    pub oob: u64,
}

impl Geometry {
    /// Whether the geometry is one this driver writes.
    ///
    /// # Errors
    ///
    /// A sentence naming the field that is out of shape.
    pub fn check(&self) -> Result<(), String> {
        if self.erase == 0 || !self.erase.is_power_of_two() || self.erase > MAX_ERASE {
            return Err(format!(
                "the erase block is {} bytes: a power of two up to {MAX_ERASE} is needed",
                self.erase
            ));
        }
        if self.write == 0 || self.write > MAX_WRITE || !self.erase.is_multiple_of(self.write) {
            return Err(format!(
                "the write unit is {} bytes: it must divide the erase block of {} bytes",
                self.write, self.erase
            ));
        }
        if self.subpage == 0 || !self.write.is_multiple_of(self.subpage) {
            return Err(format!(
                "the sub-page is {} bytes: it must divide the write unit of {} bytes",
                self.subpage, self.write
            ));
        }
        if !self.size.is_multiple_of(self.erase) {
            return Err(format!(
                "the partition is {} bytes: not a whole number of {}-byte erase blocks",
                self.size, self.erase
            ));
        }
        Ok(())
    }

    /// The erase blocks the partition has.
    pub fn blocks(&self) -> u64 {
        self.size / self.erase
    }
}

/// The partition's geometry from its sysfs node (`<sysroot>/class/mtd/mtdN`).
///
/// # Errors
///
/// A sentence when a field the writer needs is missing or not a number.
pub fn sysfs_geometry(sysroot: &Path, index: u32) -> Result<Geometry, String> {
    let dir = sysroot
        .join("class")
        .join("mtd")
        .join(format!("mtd{index}"));
    let number = |name: &str| -> Result<u64, String> {
        let text = fs::read_to_string(dir.join(name))
            .map_err(|e| format!("the partition's {name} can't be read: {e}"))?;
        text.trim()
            .parse()
            .map_err(|_| format!("the partition's {name} is not a number"))
    };
    let write = number("writesize")?;
    let subpage = fs::read_to_string(dir.join("subpagesize"))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(write);
    Ok(Geometry {
        erase: number("erasesize")?,
        write,
        subpage,
        size: number("size")?,
        oob: number("oobsize").unwrap_or(0),
    })
}

/// The MTD index a `/dev/mtdN` path names.
///
/// # Errors
///
/// A sentence when the path is not `/dev/mtdN` (a `mtdblock` or any other name is refused).
pub fn index_of(path: &Path) -> Result<u32, String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("mtd"))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            format!(
                "{} is not an MTD character device (/dev/mtdN)",
                path.display()
            )
        })
}

/// CRC-32 as the UBI headers use it: the reflected polynomial 0xEDB88320, seeded by `seed`, with no
/// final inversion (the kernel's `crc32_le` with `UBI_CRC32_INIT`).
pub fn crc32_le(seed: u32, data: &[u8]) -> u32 {
    let mut crc = seed;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// A UBI erase-counter header, checked: its erase count and where its VID and data start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcHeader {
    pub ec: u64,
    pub vid_offset: u64,
    pub data_offset: u64,
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The header at the start of `bytes`, when its magic, version and CRC are right.
pub fn parse_ec(bytes: &[u8]) -> Option<EcHeader> {
    if bytes.len() < EC_LEN || &bytes[..4] != MAGIC || bytes[4] != VERSION {
        return None;
    }
    if crc32_le(CRC_INIT, &bytes[..60]) != be32(bytes, 60) {
        return None;
    }
    let mut ec = [0u8; 8];
    ec.copy_from_slice(&bytes[8..16]);
    Some(EcHeader {
        ec: u64::from_be_bytes(ec),
        vid_offset: u64::from(be32(bytes, 16)),
        data_offset: u64::from(be32(bytes, 20)),
    })
}

/// Whether a header's VID and data offsets are the ones this target lays out: the VID header
/// starts at the first sub-page boundary after the EC header, and the data at the first page
/// boundary after the VID header (as the kernel's attach expects for this geometry).
///
/// # Errors
///
/// A sentence naming both offsets, so the board's log says which page or sub-page size the image
/// was built for.
pub fn check_layout(header: EcHeader, geometry: &Geometry) -> Result<(), String> {
    let align = |n: u64, to: u64| n.div_ceil(to) * to;
    let vid = align(EC_LEN as u64, geometry.subpage);
    let data = align(vid + VID_LEN, geometry.write);
    if header.vid_offset == vid && header.data_offset == data {
        return Ok(());
    }
    Err(format!(
        "this UBI image's VID header is at {} and its data at {}; this target's are at {vid} and {data} (sub-page {}, page {}): the image was built for another page or sub-page size",
        header.vid_offset, header.data_offset, geometry.subpage, geometry.write
    ))
}

/// The chip under the driver: what a write, an erase, a read and the bad-block table do, in bytes
/// from the partition's start. A block is named by its start.
pub trait Chip: Send {
    /// Whether the erase block at `offset` is marked bad.
    fn is_bad(&mut self, offset: u64) -> io::Result<bool>;
    /// Erases the `len` bytes at `offset`.
    fn erase(&mut self, offset: u64, len: u64) -> io::Result<()>;
    /// Marks the erase block at `offset` bad.
    fn mark_bad(&mut self, offset: u64) -> io::Result<()>;
    /// Writes `bytes` at `offset`.
    fn write(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;
    /// Reads `buf.len()` bytes at `offset`, and returns the bit flips the ECC corrected. An
    /// uncorrectable read is an error with `EBADMSG`.
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<u32>;
}

/// Whether a chip error is an uncorrectable read.
pub fn uncorrectable(error: &io::Error) -> bool {
    error.raw_os_error() == Some(EBADMSG)
}

/// A board's MTD character device: its ioctls for the bad block table, erase and ECC counts.
pub struct Device {
    file: File,
}

impl Device {
    pub fn new(file: File) -> Self {
        Self { file }
    }
}

impl Chip for Device {
    fn is_bad(&mut self, offset: u64) -> io::Result<bool> {
        lemnos_driver_system_flash_sys::is_bad_block(self.file.as_fd(), offset)
    }

    fn erase(&mut self, offset: u64, len: u64) -> io::Result<()> {
        let start = u32::try_from(offset).map_err(|_| io::Error::from_raw_os_error(EINVAL))?;
        let length = u32::try_from(len).map_err(|_| io::Error::from_raw_os_error(EINVAL))?;
        lemnos_driver_system_flash_sys::erase(self.file.as_fd(), start, length)
    }

    fn mark_bad(&mut self, offset: u64) -> io::Result<()> {
        lemnos_driver_system_flash_sys::mark_bad_block(self.file.as_fd(), offset)
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all_at(bytes, offset)
    }

    fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<u32> {
        let before = lemnos_driver_system_flash_sys::ecc_stats(self.file.as_fd())?;
        self.file.read_exact_at(buf, offset)?;
        let after = lemnos_driver_system_flash_sys::ecc_stats(self.file.as_fd())?;
        Ok(after.corrected.wrapping_sub(before.corrected))
    }
}

/// A test target: a temp file standing in for the partition, with the bad blocks the test gives
/// (erase-block indices). An erase fills the block with 0xFF; nothing corrects a read.
pub struct Fake {
    file: File,
    erase: u64,
    bad: Vec<u64>,
}

impl Fake {
    pub fn new(file: File, erase: u64, bad: Vec<u64>) -> Self {
        Self { file, erase, bad }
    }
}

impl Chip for Fake {
    fn is_bad(&mut self, offset: u64) -> io::Result<bool> {
        Ok(self.bad.contains(&(offset / self.erase)))
    }

    fn erase(&mut self, offset: u64, len: u64) -> io::Result<()> {
        let len = usize::try_from(len).map_err(|_| io::Error::from_raw_os_error(EINVAL))?;
        self.file.write_all_at(&vec![ERASED; len], offset)
    }

    fn mark_bad(&mut self, offset: u64) -> io::Result<()> {
        self.bad.push(offset / self.erase);
        Ok(())
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all_at(bytes, offset)
    }

    fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<u32> {
        self.file.read_exact_at(buf, offset)?;
        Ok(0)
    }
}

/// The bad-block scan of a partition: the offsets of its good erase blocks, in order.
pub fn good_blocks(chip: &mut dyn Chip, geometry: &Geometry) -> Result<Vec<u64>, Failure> {
    let mut good = Vec::new();
    for block in 0..geometry.blocks() {
        let offset = block * geometry.erase;
        let bad = chip.is_bad(offset).map_err(|e| {
            (
                Status::IO,
                format!("the bad block table can't be read: {e}"),
            )
        })?;
        if !bad {
            good.push(offset);
        }
    }
    Ok(good)
}

/// What a write decided before its first erase: the good blocks, the erase counts the target's
/// headers hold, and the mean of those counts (`None` on a chip with no valid header).
#[derive(Debug, Clone)]
pub struct Plan {
    geometry: Geometry,
    image: Image,
    image_size: u64,
    mark_bad: bool,
    good: Vec<u64>,
    /// Per good block: the erase count its valid header holds, if it has one.
    olds: Vec<Option<u64>>,
    mean: Option<u64>,
}

impl Plan {
    /// Plans a write of `image_size` bytes of `image` onto the partition behind `chip`. Reads the
    /// bad-block table and, for UBI, every good block's header. Nothing is erased or written.
    ///
    /// # Errors
    ///
    /// A refusal, by name, before any erase: a geometry this driver does not write, a UBI image that
    /// is not whole erase blocks, or an image that needs more good blocks than the chip has.
    pub fn new(
        chip: &mut dyn Chip,
        geometry: Geometry,
        image: Image,
        image_size: u64,
        mark_bad: bool,
    ) -> Result<Self, Failure> {
        geometry
            .check()
            .map_err(|why| (Status::INVALID_ARGUMENT, why))?;
        if image == Image::Ubi && !image_size.is_multiple_of(geometry.erase) {
            return Err((
                Status::INVALID_ARGUMENT,
                format!(
                    "a UBI image is whole erase blocks: {image_size} bytes is not a multiple of the target's {}-byte erase block",
                    geometry.erase
                ),
            ));
        }
        let good = good_blocks(chip, &geometry)?;
        let needed = image_size.div_ceil(geometry.erase);
        if needed > good.len() as u64 {
            return Err((
                NO_SPACE,
                format!(
                    "the image needs {needed} good erase blocks and the target has {} ({} of its {} blocks are bad)",
                    good.len(),
                    geometry.blocks() - good.len() as u64,
                    geometry.blocks()
                ),
            ));
        }
        let (olds, mean) = if image == Image::Ubi {
            read_counts(chip, &geometry, &good)?
        } else {
            (Vec::new(), None)
        };
        Ok(Self {
            geometry,
            image,
            image_size,
            mark_bad,
            good,
            olds,
            mean,
        })
    }

    /// The good erase blocks the image needs.
    pub fn needed(&self) -> u64 {
        self.image_size.div_ceil(self.geometry.erase)
    }

    /// The good erase blocks the target has.
    pub fn available(&self) -> u64 {
        self.good.len() as u64
    }
}

/// The erase counts the target's headers hold (per good block) and their mean.
fn read_counts(
    chip: &mut dyn Chip,
    geometry: &Geometry,
    good: &[u64],
) -> Result<(Vec<Option<u64>>, Option<u64>), Failure> {
    let mut page = vec![0u8; geometry.write as usize];
    let mut olds = Vec::with_capacity(good.len());
    let (mut sum, mut seen) = (0u64, 0u64);
    for &offset in good {
        let old = match chip.read(offset, &mut page) {
            Ok(_) => parse_ec(&page).map(|h| h.ec),
            Err(error) if uncorrectable(&error) => None,
            Err(error) => {
                return Err((
                    Status::IO,
                    format!("a block's header can't be read: {error}"),
                ));
            }
        };
        if let Some(ec) = old {
            sum = sum.saturating_add(ec);
            seen += 1;
        }
        olds.push(old);
    }
    let mean = (seen > 0).then(|| sum / seen);
    Ok((olds, mean))
}

/// The writer of one image: takes the stream in order and writes each erase block as it fills.
pub struct Sink {
    chip: Box<dyn Chip>,
    plan: Plan,
    /// The image block being collected, and then patched in place into what is written.
    buf: Vec<u8>,
    /// One page read back at a time.
    page: Vec<u8>,
    /// The index of the image block being collected, and the stream's bytes taken so far.
    block: u64,
    taken: u64,
    /// The hash of what is written (the image, with the UBI counts as written).
    hasher: Sha256,
    /// The ECC corrections counted across the read-backs.
    pub corrected: u64,
}

impl Sink {
    pub fn new(chip: Box<dyn Chip>, plan: Plan) -> Self {
        let erase = plan.geometry.erase as usize;
        let write = plan.geometry.write as usize;
        Self {
            chip,
            buf: Vec::with_capacity(erase),
            page: vec![0; write],
            block: 0,
            taken: 0,
            hasher: Sha256::new(),
            corrected: 0,
            plan,
        }
    }

    /// The image bytes the block being collected holds in all.
    fn block_len(&self) -> usize {
        let erase = self.plan.geometry.erase;
        let start = self.block * erase;
        (self.plan.image_size - start).min(erase) as usize
    }

    /// Takes the next bytes of the image's stream, writing every erase block that fills.
    ///
    /// # Errors
    ///
    /// The first refusal or failure; the target is then partial.
    pub fn push(&mut self, mut bytes: &[u8]) -> Result<(), Failure> {
        while !bytes.is_empty() {
            let want = self.block_len();
            if want == 0 {
                return Err((
                    Status::INVALID_ARGUMENT,
                    "the stream holds more than the image's size".into(),
                ));
            }
            let take = (want - self.buf.len()).min(bytes.len());
            self.buf.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            self.taken += take as u64;
            if self.buf.len() == want {
                self.write_block()?;
            }
        }
        Ok(())
    }

    /// Writes the collected image block to the next good erase block.
    fn write_block(&mut self) -> Result<(), Failure> {
        let index = self.block as usize;
        let offset = self.plan.good[index];
        let image_bytes = self.buf.len();
        let geometry = self.plan.geometry;
        self.buf.resize(geometry.erase as usize, ERASED);
        if self.plan.image == Image::Ubi {
            let header = parse_ec(&self.buf[..EC_LEN]).ok_or_else(|| {
                (
                    Status::INVALID_ARGUMENT,
                    format!(
                        "image block {} has no valid UBI erase-counter header (magic, version or CRC)",
                        self.block
                    ),
                )
            })?;
            check_layout(header, &geometry).map_err(|why| (Status::INVALID_ARGUMENT, why))?;
            let ec = match self.plan.olds[index] {
                Some(old) => old.saturating_add(1),
                None => self.plan.mean.unwrap_or(header.ec),
            };
            self.buf[8..16].copy_from_slice(&ec.to_be_bytes());
            let crc = crc32_le(CRC_INIT, &self.buf[..60]);
            self.buf[60..64].copy_from_slice(&crc.to_be_bytes());
        }
        self.erase_at(offset)?;
        self.write_pages(offset)?;
        self.read_back(offset)?;
        self.hasher.update(&self.buf[..image_bytes]);
        self.block += 1;
        self.buf.clear();
        Ok(())
    }

    /// Erases the block at `offset`; a failure marks it bad when the board allows, and fails.
    fn erase_at(&mut self, offset: u64) -> Result<(), Failure> {
        let erase = self.plan.geometry.erase;
        match self.chip.erase(offset, erase) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.bad(offset, "erase", &error)),
        }
    }

    /// Writes the pages of the collected block up to its last one that is not all erased.
    fn write_pages(&mut self, offset: u64) -> Result<(), Failure> {
        let write = self.plan.geometry.write as usize;
        let last = self
            .buf
            .chunks(write)
            .rposition(|page| page.iter().any(|&b| b != ERASED));
        let Some(last) = last else {
            return Ok(());
        };
        for page in 0..=last {
            let at = page * write;
            let result = self
                .chip
                .write(offset + at as u64, &self.buf[at..at + write]);
            if let Err(error) = result {
                return Err(self.bad(offset, "write", &error));
            }
        }
        Ok(())
    }

    /// Reads every page of the block back and compares it with what was meant to be written.
    fn read_back(&mut self, offset: u64) -> Result<(), Failure> {
        let write = self.plan.geometry.write as usize;
        for page in 0..self.buf.len() / write {
            let at = page * write;
            let corrected = match self.chip.read(offset + at as u64, &mut self.page) {
                Ok(flips) => flips,
                Err(error) => return Err(self.bad(offset, "read back", &error)),
            };
            self.corrected += u64::from(corrected);
            if self.page[..] != self.buf[at..at + write] {
                let error = io::Error::other("the page does not read back as written");
                return Err(self.bad(offset, "read back", &error));
            }
        }
        Ok(())
    }

    /// Ends the block's failure: marks the block bad when the board allows it, and names the failure.
    fn bad(&mut self, offset: u64, what: &str, error: &io::Error) -> Failure {
        let note = if self.plan.mark_bad {
            match self.chip.mark_bad(offset) {
                Ok(()) => "; the block is marked bad".to_string(),
                Err(e) => format!("; marking it bad failed too: {e}"),
            }
        } else {
            String::new()
        };
        let why = if uncorrectable(error) {
            "an uncorrectable ECC error".to_string()
        } else {
            error.to_string()
        };
        (
            Status::IO,
            format!("the erase block at {offset:#x} failed to {what}: {why}{note}"),
        )
    }

    /// The stream is all taken: the image's blocks are written. A UBI target's good blocks the image
    /// does not cover are erased, so no stale volume survives on them. Returns the hash of what
    /// was written and the ECC corrections counted.
    ///
    /// # Errors
    ///
    /// A failure erasing a covered-nothing block, or a stream that ended early.
    pub fn finish(&mut self) -> Result<([u8; 32], u64), Failure> {
        if self.taken != self.plan.image_size || !self.buf.is_empty() {
            return Err((
                Status::INVALID_ARGUMENT,
                "the stream ended before the image did".into(),
            ));
        }
        if self.plan.image == Image::Ubi {
            let covered = self.plan.needed() as usize;
            for index in covered..self.plan.good.len() {
                let offset = self.plan.good[index];
                self.erase_at(offset)?;
            }
        }
        Ok((self.hasher.clone().finalize().into(), self.corrected))
    }
}

/// The verifier of a written partition: reads the image's blocks back, one erase block per call,
/// and hashes what it reads against the hash the write recorded.
pub struct Verifier {
    chip: Box<dyn Chip>,
    geometry: Geometry,
    good: Vec<u64>,
    image_size: u64,
    expect: [u8; 32],
    hasher: Sha256,
    buf: Vec<u8>,
    read: u64,
    /// The ECC corrections counted so far.
    pub corrected: u64,
}

impl Verifier {
    /// # Errors
    ///
    /// A refusal when the geometry is not one this driver reads, or the image no longer fits.
    pub fn new(
        mut chip: Box<dyn Chip>,
        geometry: Geometry,
        image_size: u64,
        expect: [u8; 32],
    ) -> Result<Self, Failure> {
        geometry
            .check()
            .map_err(|why| (Status::INVALID_ARGUMENT, why))?;
        let good = good_blocks(chip.as_mut(), &geometry)?;
        if image_size.div_ceil(geometry.erase) > good.len() as u64 {
            return Err((
                NO_SPACE,
                "the target no longer has the good erase blocks the image needs".into(),
            ));
        }
        Ok(Self {
            chip,
            geometry,
            good,
            image_size,
            expect,
            hasher: Sha256::new(),
            buf: vec![0; geometry.erase as usize],
            read: 0,
            corrected: 0,
        })
    }

    /// The image's bytes read so far, and its size.
    pub fn progress(&self) -> (u64, u64) {
        (self.read, self.image_size)
    }

    /// Reads the next erase block. True while more of the image is to be read; at the last block the
    /// hash is compared.
    ///
    /// # Errors
    ///
    /// An unreadable block (an uncorrectable ECC error included), or a hash that is not the one
    /// written.
    pub fn step(&mut self) -> Result<bool, Failure> {
        let erase = self.geometry.erase;
        let block = (self.read / erase) as usize;
        let n = (self.image_size - self.read).min(erase) as usize;
        let offset = self.good[block];
        match self.chip.read(offset, &mut self.buf) {
            Ok(flips) => self.corrected += u64::from(flips),
            Err(error) if uncorrectable(&error) => {
                return Err((
                    Status::IO,
                    format!(
                        "the erase block at {offset:#x} can't be read: an uncorrectable ECC error"
                    ),
                ));
            }
            Err(error) => {
                return Err((
                    Status::IO,
                    format!("the erase block at {offset:#x} can't be read: {error}"),
                ));
            }
        }
        self.hasher.update(&self.buf[..n]);
        self.read += n as u64;
        if self.read < self.image_size {
            return Ok(true);
        }
        let digest: [u8; 32] = self.hasher.clone().finalize().into();
        if digest == self.expect {
            Ok(false)
        } else {
            Err((
                Status::INVALID_ARGUMENT,
                "the target does not hold the image: its SHA-256 is not the manifest's".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Small blocks, so the tests stay quick: 8 erase blocks of 4 KiB, 512-byte pages.
    const ERASE: u64 = 4096;
    const WRITE: u64 = 512;
    const BLOCKS: u64 = 8;
    const SIZE: u64 = ERASE * BLOCKS;

    fn geometry() -> Geometry {
        Geometry {
            erase: ERASE,
            write: WRITE,
            subpage: WRITE,
            size: SIZE,
            oob: 0,
        }
    }

    /// An in-memory chip with the faults the tests inject. Every operation counts; a power cut
    /// fails the operation at `cut` and every one after it.
    #[derive(Default)]
    struct Mem {
        data: Vec<u8>,
        bad: Vec<bool>,
        ops: u64,
        cut: Option<u64>,
        erases: Vec<u64>,
        writes: Vec<u64>,
        marked: Vec<u64>,
        fail_erase: Option<u64>,
        fail_write: Option<u64>,
        corrupt_write: Option<u64>,
        flips: u32,
        uncorrectable: Option<u64>,
    }

    impl Mem {
        fn new(bad: &[u64]) -> Arc<Mutex<Self>> {
            Arc::new(Mutex::new(Self {
                data: vec![0xFF; SIZE as usize],
                bad: (0..BLOCKS).map(|b| bad.contains(&b)).collect(),
                ..Self::default()
            }))
        }

        fn tick(&mut self) -> io::Result<()> {
            let n = self.ops;
            self.ops += 1;
            if self.cut.is_some_and(|cut| n >= cut) {
                return Err(io::Error::other("power cut"));
            }
            Ok(())
        }
    }

    /// The chip the driver holds: a handle on the shared memory.
    struct Shared(Arc<Mutex<Mem>>);

    impl Chip for Shared {
        fn is_bad(&mut self, offset: u64) -> io::Result<bool> {
            let mut m = self.0.lock().unwrap();
            m.tick()?;
            Ok(m.bad[(offset / ERASE) as usize])
        }

        fn erase(&mut self, offset: u64, len: u64) -> io::Result<()> {
            let mut m = self.0.lock().unwrap();
            m.tick()?;
            if m.fail_erase == Some(offset / ERASE) {
                return Err(io::Error::other("erase refused"));
            }
            m.erases.push(offset);
            m.data[offset as usize..(offset + len) as usize].fill(0xFF);
            Ok(())
        }

        fn mark_bad(&mut self, offset: u64) -> io::Result<()> {
            let mut m = self.0.lock().unwrap();
            m.tick()?;
            m.marked.push(offset);
            m.bad[(offset / ERASE) as usize] = true;
            Ok(())
        }

        fn write(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
            let mut m = self.0.lock().unwrap();
            m.tick()?;
            if m.fail_write == Some(offset) {
                return Err(io::Error::other("write refused"));
            }
            m.writes.push(offset);
            let at = offset as usize;
            m.data[at..at + bytes.len()].copy_from_slice(bytes);
            if m.corrupt_write == Some(offset) {
                m.data[at] ^= 1;
            }
            Ok(())
        }

        fn read(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<u32> {
            let mut m = self.0.lock().unwrap();
            m.tick()?;
            if m.uncorrectable == Some(offset) {
                return Err(io::Error::from_raw_os_error(EBADMSG));
            }
            let at = offset as usize;
            buf.copy_from_slice(&m.data[at..at + buf.len()]);
            Ok(m.flips)
        }
    }

    fn set_header(block: &mut [u8], ec: u64, vid: u32, data: u32) {
        block[..4].copy_from_slice(MAGIC);
        block[4] = VERSION;
        block[5..8].fill(0);
        block[8..16].copy_from_slice(&ec.to_be_bytes());
        block[16..20].copy_from_slice(&vid.to_be_bytes());
        block[20..24].copy_from_slice(&data.to_be_bytes());
        block[24..60].fill(0);
        let crc = crc32_le(CRC_INIT, &block[..60]);
        block[60..64].copy_from_slice(&crc.to_be_bytes());
    }

    /// A UBI erase block with the header for this geometry, a pattern after it, and erased pages
    /// from byte `tail` on.
    fn ubi_block(ec: u64, fill: u8, tail: usize) -> Vec<u8> {
        let mut block: Vec<u8> = (0..ERASE as usize)
            .map(|i| {
                if i >= tail {
                    0xFF
                } else {
                    fill.wrapping_add(i as u8)
                }
            })
            .collect();
        set_header(&mut block, ec, 512, 1024);
        block
    }

    fn image_of(blocks: &[Vec<u8>]) -> Vec<u8> {
        blocks.concat()
    }

    /// The bytes of erase block `index` on the chip.
    fn block_of(mem: &Arc<Mutex<Mem>>, index: u64) -> Vec<u8> {
        let m = mem.lock().unwrap();
        let at = (index * ERASE) as usize;
        m.data[at..at + ERASE as usize].to_vec()
    }

    /// One write: plan, stream the image in pieces of `chunk` bytes, and finish. Returns the hash of
    /// what was written.
    fn write_image(
        mem: &Arc<Mutex<Mem>>,
        image: &[u8],
        kind: Image,
        mark_bad: bool,
        chunk: usize,
    ) -> Result<[u8; 32], Failure> {
        let mut plan_chip = Shared(mem.clone());
        let plan = Plan::new(
            &mut plan_chip,
            geometry(),
            kind,
            image.len() as u64,
            mark_bad,
        )?;
        let mut sink = Sink::new(Box::new(Shared(mem.clone())), plan);
        for piece in image.chunks(chunk) {
            sink.push(piece)?;
        }
        Ok(sink.finish()?.0)
    }

    /// Reads the image's bytes back from the chip and checks them against `expect`.
    fn verify(mem: &Arc<Mutex<Mem>>, size: u64, expect: [u8; 32]) -> Result<(), Failure> {
        let mut verifier = Verifier::new(Box::new(Shared(mem.clone())), geometry(), size, expect)?;
        while verifier.step()? {}
        Ok(())
    }

    fn ec_of(block: &[u8]) -> u64 {
        parse_ec(block).expect("a valid header").ec
    }

    #[test]
    fn the_crc_is_the_kernels_crc32_le_with_no_final_inversion() {
        // The reflected CRC-32 of "123456789" with the standard 0xFFFFFFFF seed and final inversion
        // is 0xCBF43926; without the inversion it is its complement.
        assert_eq!(crc32_le(0xFFFF_FFFF, b"123456789"), !0xCBF4_3926);
    }

    #[test]
    fn a_checked_header_reads_back_its_fields_and_a_damaged_one_is_not_read() {
        let block = ubi_block(7, 1, ERASE as usize);
        assert_eq!(
            parse_ec(&block),
            Some(EcHeader {
                ec: 7,
                vid_offset: 512,
                data_offset: 1024
            })
        );
        let mut bad = block.clone();
        bad[9] ^= 1;
        assert_eq!(parse_ec(&bad), None, "the CRC covers the erase count");
        let mut magic = block;
        magic[0] = b'X';
        assert_eq!(parse_ec(&magic), None);
    }

    #[test]
    fn bad_blocks_are_skipped_and_image_blocks_go_to_good_ones_in_order() {
        let mem = Mem::new(&[1, 4]);
        let image = image_of(&[
            ubi_block(3, 10, ERASE as usize),
            ubi_block(4, 20, ERASE as usize),
            ubi_block(5, 30, ERASE as usize),
        ]);
        write_image(&mem, &image, Image::Ubi, false, 1000).unwrap();
        assert_eq!(block_of(&mem, 0)[64..], image[64..ERASE as usize]);
        assert_eq!(
            block_of(&mem, 2)[64..],
            image[ERASE as usize + 64..2 * ERASE as usize]
        );
        assert_eq!(block_of(&mem, 3)[64..], image[2 * ERASE as usize + 64..]);
        assert!(
            block_of(&mem, 1).iter().all(|&b| b == 0xFF)
                && mem.lock().unwrap().erases.iter().all(|&o| o != ERASE),
            "a bad block is never touched"
        );
        assert_eq!(
            mem.lock().unwrap().erases.len(),
            3 + 3,
            "three covered and three uncovered good blocks"
        );
    }

    #[test]
    fn a_blank_chip_takes_the_images_own_erase_counts() {
        let mem = Mem::new(&[]);
        let image = image_of(&[
            ubi_block(3, 1, ERASE as usize),
            ubi_block(9, 2, ERASE as usize),
        ]);
        write_image(&mem, &image, Image::Ubi, false, 4096).unwrap();
        assert_eq!(ec_of(&block_of(&mem, 0)), 3);
        assert_eq!(ec_of(&block_of(&mem, 1)), 9);
    }

    #[test]
    fn a_valid_header_on_the_target_carries_its_count_forward_and_a_missing_one_takes_the_mean() {
        let mem = Mem::new(&[]);
        {
            let mut m = mem.lock().unwrap();
            m.data[..ERASE as usize].copy_from_slice(&ubi_block(10, 0, ERASE as usize));
            let second = ERASE as usize;
            m.data[second..2 * second].copy_from_slice(&ubi_block(20, 0, ERASE as usize));
        }
        let image = image_of(&[
            ubi_block(3, 1, ERASE as usize),
            ubi_block(4, 2, ERASE as usize),
            ubi_block(5, 3, ERASE as usize),
        ]);
        let digest = write_image(&mem, &image, Image::Ubi, false, 4096).unwrap();
        assert_eq!(ec_of(&block_of(&mem, 0)), 11, "10 carried forward");
        assert_eq!(ec_of(&block_of(&mem, 1)), 21, "20 carried forward");
        assert_eq!(
            ec_of(&block_of(&mem, 2)),
            15,
            "no header: the mean of 10 and 20"
        );
        verify(&mem, image.len() as u64, digest).expect("the image as written verifies");
    }

    #[test]
    fn a_damaged_image_header_is_refused_before_any_erase() {
        let mem = Mem::new(&[]);
        let mut block = ubi_block(3, 1, ERASE as usize);
        block[60] ^= 1;
        let image = image_of(&[block, ubi_block(4, 2, ERASE as usize)]);
        let error = write_image(&mem, &image, Image::Ubi, false, 4096).unwrap_err();
        assert_eq!(error.0, Status::INVALID_ARGUMENT);
        assert!(
            error.1.contains("no valid UBI erase-counter header"),
            "{}",
            error.1
        );
        assert!(mem.lock().unwrap().erases.is_empty(), "nothing erased");
    }

    #[test]
    fn an_image_built_for_another_page_size_is_refused_by_name_before_any_erase() {
        let mem = Mem::new(&[]);
        let mut block = ubi_block(3, 1, ERASE as usize);
        set_header(&mut block, 3, 2048, 4096);
        let error = write_image(&mem, &image_of(&[block]), Image::Ubi, false, 4096).unwrap_err();
        assert_eq!(error.0, Status::INVALID_ARGUMENT);
        assert!(
            error.1.contains("another page or sub-page size"),
            "{}",
            error.1
        );
        assert!(mem.lock().unwrap().erases.is_empty());
    }

    #[test]
    fn an_image_too_large_for_the_good_blocks_is_refused_before_any_erase() {
        let mem = Mem::new(&[0, 1, 2, 3, 4, 5]);
        let image = image_of(&[
            ubi_block(1, 1, ERASE as usize),
            ubi_block(1, 2, ERASE as usize),
            ubi_block(1, 3, ERASE as usize),
        ]);
        let error = write_image(&mem, &image, Image::Ubi, false, 4096).unwrap_err();
        assert_eq!(error.0, NO_SPACE);
        assert!(
            error
                .1
                .contains("needs 3 good erase blocks and the target has 2"),
            "{}",
            error.1
        );
        assert!(mem.lock().unwrap().erases.is_empty() && mem.lock().unwrap().writes.is_empty());
    }

    #[test]
    fn a_ubi_image_that_is_not_whole_erase_blocks_is_refused() {
        let mem = Mem::new(&[]);
        let error = write_image(&mem, &[0xFF; 100], Image::Ubi, false, 100).unwrap_err();
        assert_eq!(error.0, Status::INVALID_ARGUMENT);
        assert!(error.1.contains("whole erase blocks"));
    }

    #[test]
    fn a_geometry_the_driver_does_not_write_is_refused() {
        let mut mem = Mem::new(&[]);
        let bad = Geometry {
            erase: 3000,
            ..geometry()
        };
        let plan = Plan::new(&mut Shared(mem.clone()), bad, Image::Raw, ERASE, false);
        assert!(plan.unwrap_err().1.contains("power of two"));
        let _ = &mut mem;
    }

    #[test]
    fn trailing_erased_pages_are_not_written_so_ubi_sees_them_as_free() {
        let mem = Mem::new(&[]);
        let block = ubi_block(3, 1, 3 * WRITE as usize);
        write_image(&mem, &image_of(&[block]), Image::Ubi, false, 4096).unwrap();
        let writes = mem.lock().unwrap().writes.clone();
        assert!(
            writes.iter().all(|&offset| offset < 3 * WRITE),
            "pages 3 onward are erased and not written: {writes:?}"
        );
        assert_eq!(writes.len(), 3);
    }

    #[test]
    fn a_failed_erase_fails_the_job_and_marks_the_block_bad_only_when_allowed() {
        for mark in [false, true] {
            let mem = Mem::new(&[]);
            mem.lock().unwrap().fail_erase = Some(0);
            let image = image_of(&[ubi_block(3, 1, ERASE as usize)]);
            let error = write_image(&mem, &image, Image::Ubi, mark, 4096).unwrap_err();
            assert_eq!(error.0, Status::IO);
            assert!(error.1.contains("failed to erase"), "{}", error.1);
            assert_eq!(
                mem.lock().unwrap().marked.contains(&0),
                mark,
                "marked only when allowed"
            );
        }
    }

    #[test]
    fn a_failed_write_fails_the_job_and_marks_the_block_bad_only_when_allowed() {
        let mem = Mem::new(&[]);
        mem.lock().unwrap().fail_write = Some(WRITE);
        let image = image_of(&[ubi_block(3, 1, ERASE as usize)]);
        let error = write_image(&mem, &image, Image::Ubi, false, 4096).unwrap_err();
        assert_eq!(error.0, Status::IO);
        assert!(error.1.contains("failed to write"), "{}", error.1);
        assert!(
            mem.lock().unwrap().marked.is_empty(),
            "not allowed: not marked"
        );
    }

    #[test]
    fn a_page_that_does_not_read_back_as_written_fails_the_job() {
        let mem = Mem::new(&[]);
        mem.lock().unwrap().corrupt_write = Some(2 * WRITE);
        let image = image_of(&[ubi_block(3, 1, ERASE as usize)]);
        let error = write_image(&mem, &image, Image::Ubi, false, 4096).unwrap_err();
        assert_eq!(error.0, Status::IO);
        assert!(
            error.1.contains("does not read back as written"),
            "{}",
            error.1
        );
    }

    #[test]
    fn ecc_corrections_are_counted_on_write_and_verify_and_an_uncorrectable_read_fails_the_verify()
    {
        let mem = Mem::new(&[]);
        mem.lock().unwrap().flips = 2;
        let image = image_of(&[
            ubi_block(3, 1, ERASE as usize),
            ubi_block(4, 2, ERASE as usize),
        ]);
        let mut plan_chip = Shared(mem.clone());
        let plan = Plan::new(
            &mut plan_chip,
            geometry(),
            Image::Ubi,
            image.len() as u64,
            false,
        )
        .unwrap();
        let mut sink = Sink::new(Box::new(Shared(mem.clone())), plan);
        sink.push(&image).unwrap();
        let (digest, corrected) = sink.finish().unwrap();
        // Each block reads back every page (8 pages), two flips each.
        assert_eq!(corrected, 2 * 8 * 2);
        let mut verifier = Verifier::new(
            Box::new(Shared(mem.clone())),
            geometry(),
            image.len() as u64,
            digest,
        )
        .unwrap();
        while verifier.step().unwrap() {}
        assert_eq!(
            verifier.corrected,
            2 * 2,
            "one read per block, two flips each"
        );

        mem.lock().unwrap().uncorrectable = Some(ERASE);
        let error = verify(&mem, image.len() as u64, digest).unwrap_err();
        assert_eq!(error.0, Status::IO);
        assert!(error.1.contains("uncorrectable ECC error"), "{}", error.1);
    }

    #[test]
    fn a_marked_block_is_not_used_by_the_next_write() {
        let mem = Mem::new(&[]);
        mem.lock().unwrap().fail_erase = Some(0);
        let image = image_of(&[ubi_block(3, 1, ERASE as usize)]);
        assert!(write_image(&mem, &image, Image::Ubi, true, 4096).is_err());
        mem.lock().unwrap().fail_erase = None;
        let digest = write_image(&mem, &image, Image::Ubi, true, 4096).unwrap();
        verify(&mem, image.len() as u64, digest).expect("the next write lands on block 1");
        assert!(mem.lock().unwrap().marked.contains(&0));
        assert_eq!(block_of(&mem, 1)[64..], image[64..]);
    }

    #[test]
    fn a_power_cut_at_every_operation_is_recovered_by_a_rerun_from_the_first_good_block() {
        let bad = [2];
        let image = image_of(&[
            ubi_block(3, 1, ERASE as usize),
            ubi_block(4, 2, 2 * WRITE as usize),
            ubi_block(5, 3, ERASE as usize),
        ]);
        // How many operations a whole write takes, so the sweep covers each one.
        let clean = Mem::new(&bad);
        write_image(&clean, &image, Image::Ubi, false, 1000).unwrap();
        let total = clean.lock().unwrap().ops;
        for cut in 0..=total {
            let mem = Mem::new(&bad);
            mem.lock().unwrap().cut = Some(cut);
            let first = write_image(&mem, &image, Image::Ubi, false, 1000);
            if cut < total {
                assert!(first.is_err(), "a cut at operation {cut} stops the write");
            }
            mem.lock().unwrap().cut = None;
            let digest = write_image(&mem, &image, Image::Ubi, false, 1000)
                .unwrap_or_else(|e| panic!("the rerun after a cut at {cut} failed: {e:?}"));
            verify(&mem, image.len() as u64, digest)
                .unwrap_or_else(|e| panic!("the rerun after a cut at {cut} doesn't verify: {e:?}"));
        }
    }

    #[test]
    fn the_stream_may_come_in_pieces_of_any_size() {
        let image = image_of(&[ubi_block(3, 1, ERASE as usize), ubi_block(4, 2, 1000)]);
        let whole = write_image(&Mem::new(&[]), &image, Image::Ubi, false, image.len()).unwrap();
        let odd = write_image(&Mem::new(&[]), &image, Image::Ubi, false, 37).unwrap();
        assert_eq!(whole, odd);
    }

    #[test]
    fn a_raw_image_is_written_linearly_with_its_last_page_padded_and_uncovered_blocks_untouched() {
        let mem = Mem::new(&[]);
        mem.lock().unwrap().data[(4 * ERASE) as usize] = 0x42;
        let image: Vec<u8> = (0..(2 * ERASE + 700)).map(|i| (i % 251) as u8).collect();
        let digest = write_image(&mem, &image, Image::Raw, false, 1000).unwrap();
        assert_eq!(
            digest.as_slice(),
            Sha256::digest(&image).as_slice(),
            "raw: the image's own hash"
        );
        assert_eq!(&block_of(&mem, 2)[..700], &image[2 * ERASE as usize..]);
        assert!(
            block_of(&mem, 2)[700..].iter().all(|&b| b == 0xFF),
            "the padding is erased"
        );
        assert_eq!(
            mem.lock().unwrap().data[(4 * ERASE) as usize],
            0x42,
            "a raw image does not erase the rest"
        );
        verify(&mem, image.len() as u64, digest).unwrap();
    }

    #[test]
    fn the_sinks_buffers_are_one_erase_block_and_one_page() {
        let mut chip = Shared(Mem::new(&[]));
        let plan = Plan::new(&mut chip, geometry(), Image::Ubi, ERASE, false).unwrap();
        let sink = Sink::new(Box::new(Shared(Mem::new(&[]))), plan);
        assert_eq!(sink.buf.capacity(), ERASE as usize);
        assert_eq!(sink.page.len(), WRITE as usize);
    }

    #[test]
    fn the_sysfs_geometry_is_read_from_the_partitions_node() {
        let root = std::env::temp_dir().join(format!("lemnos-nand-sysfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("class/mtd/mtd3");
        fs::create_dir_all(&dir).unwrap();
        for (name, value) in [
            ("erasesize", "131072"),
            ("writesize", "2048"),
            ("subpagesize", "512"),
            ("oobsize", "64"),
            ("size", "134217728"),
        ] {
            fs::write(dir.join(name), format!("{value}\n")).unwrap();
        }
        let got = sysfs_geometry(&root, 3);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            got,
            Ok(Geometry {
                erase: 131072,
                write: 2048,
                subpage: 512,
                size: 134217728,
                oob: 64
            })
        );
        assert_eq!(index_of(Path::new("/dev/mtd3")), Ok(3));
        assert!(index_of(Path::new("/dev/mtdblock3")).is_err());
    }
}
