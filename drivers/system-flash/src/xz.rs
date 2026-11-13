//! The xz container (https://tukaani.org/xz/xz-file-format.txt), decoded as a push-based stream.
//!
//! A stream is a header, one or more blocks (each an LZMA2 stream with its check), an index of
//! the blocks, and a footer. The decoder takes bytes as they come and hands the decoded bytes to a
//! sink in order; the blocks' checks and the index are verified as they are read. Only LZMA2 is
//! accepted as the filter: a stream that names any other filter is refused. Trailing zero bytes
//! after the footer are padding; anything else after it is refused.
//!
//! The dictionary a block declares is checked before anything is allocated for it, against the
//! board's limit (`max_dict_mb`).
//!
//! A first-flash file is one image stream, optionally followed by one second stream that decodes
//! to the file's signature record (see `manifest`). The decoder takes at most two streams; a third
//! stream, or data after the second that is not zero padding, is refused as "not a first-flash
//! image". The sink is told which stream each decoded byte belongs to (0 for the first).

use lemnos_core::Status;

use crate::crc::{Check, Crc32};
use crate::lzma::{Lzma2, MIN_WINDOW, Pushed, Window, dict_size};

const MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
const FOOTER_MAGIC: [u8; 2] = *b"YZ";
const FILTER_LZMA2: u64 = 0x21;
/// Most blocks an index may list. A stream's blocks are kept until its index to be checked.
const MAX_BLOCKS: usize = 1 << 13;
/// Streams a first-flash file may hold: the image, and the optional signature record.
const MAX_STREAMS: u32 = 2;
/// The sentence for a file that is not an image with an optional record after it.
const NOT_AN_IMAGE: &str = "not a first-flash image";
/// The largest block header: 12 words of 4 bytes.
const MAX_BLOCK_HEADER: usize = 1024;

/// Why a stream was refused, or what the sink refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The stream is not a valid xz stream, or its contents do not agree with each other.
    Malformed(&'static str),
    /// A block declares a dictionary larger than the board allows.
    Dictionary { mib: u64, limit_mib: u64 },
    /// The stream uses a feature Lemnos does not take (a filter other than LZMA2, a check it
    /// doesn't know).
    Unsupported(&'static str),
    /// The sink refused the bytes.
    Sink(Status),
}

impl Error {
    /// The job status this error ends a write with.
    pub fn status(&self) -> Status {
        match self {
            Self::Sink(status) => *status,
            _ => Status::INVALID_ARGUMENT,
        }
    }

    /// A sentence for the target's state.
    pub fn sentence(&self) -> String {
        match self {
            Self::Malformed(why) | Self::Unsupported(why) => (*why).to_string(),
            Self::Dictionary { mib, limit_mib } => format!(
                "the xz stream's dictionary is {mib} MiB, over the board's limit of {limit_mib} MiB: build the image with xz -6 or smaller"
            ),
            Self::Sink(status) => format!("the target refused the bytes ({})", status.0),
        }
    }
}

/// A number in xz's variable-length form, read a byte at a time.
#[derive(Default)]
struct Varint {
    value: u64,
    shift: u32,
}

impl Varint {
    /// Takes one byte. `Some` with the value when the number ends with it.
    fn push(&mut self, byte: u8) -> Result<Option<u64>, Error> {
        if self.shift >= 63 {
            return Err(Error::Malformed("a number in the xz stream is too long"));
        }
        self.value |= u64::from(byte & 0x7F) << self.shift;
        self.shift += 7;
        if byte & 0x80 == 0 {
            Ok(Some(self.value))
        } else {
            Ok(None)
        }
    }
}

/// Reads a number from a complete header slice.
fn varint_at(bytes: &[u8], at: &mut usize) -> Result<u64, Error> {
    let mut v = Varint::default();
    loop {
        let byte = *bytes
            .get(*at)
            .ok_or(Error::Malformed("a block header is cut short"))?;
        *at += 1;
        if let Some(value) = v.push(byte)? {
            return Ok(value);
        }
    }
}

/// Where the decoder is.
enum Stage {
    /// The 12 bytes of the stream header.
    StreamHeader,
    /// The first byte of a block header, or of the index (a zero).
    BlockOrIndex,
    /// The rest of a block header.
    BlockHeader,
    /// The LZMA2 data of a block.
    BlockData,
    /// The padding that makes a block's data a multiple of four bytes.
    BlockPadding(usize),
    /// A block's check.
    BlockCheck(usize),
    /// The index: its record count, then each block's two numbers.
    IndexCount,
    IndexRecords {
        left: u64,
        field: u8,
    },
    /// The index's padding to four bytes, then its CRC-32.
    IndexPadding,
    IndexCrc,
    /// The 12-byte footer.
    Footer,
    /// Zero bytes after the footer: padding, or the start of another stream.
    Padding,
}

/// One block, from its header.
struct Block {
    /// The header's declared sizes, when it declares them.
    packed: Option<u64>,
    unpacked: Option<u64>,
    /// Its header's size in bytes.
    header_size: usize,
    /// Compressed bytes of its LZMA2 data so far.
    packed_bytes: u64,
    /// Decoded bytes it emitted before it began.
    emitted_at_start: u64,
}

/// Where decoded bytes go: the stream they belong to (0 for the image, 1 for the record), and the
/// bytes. A refusal is the sink's status.
pub type Sink<'a> = dyn FnMut(u32, &[u8]) -> Result<(), Status> + 'a;

/// A push-based xz decoder. `max_dict` is the largest dictionary a block may declare.
pub struct Decoder {
    max_dict: u64,
    /// Streams begun so far (their headers read).
    streams: u32,
    /// Zero bytes read after the last footer.
    padding: u64,
    stage: Stage,
    /// A fixed-size unit being collected (the stream header, a block header, a check, a footer).
    unit: Vec<u8>,
    /// Stream flags of the stream being read.
    flags: [u8; 2],
    check_kind: u8,
    block: Option<Block>,
    /// The sizes of each block, as the index must list them: unpadded size and uncompressed size.
    blocks: Vec<(u64, u64)>,
    /// The index's own bytes so far, for its CRC and the footer's backward size.
    index_bytes: u64,
    index_crc: Crc32,
    varint: Varint,
    /// The record number and its unpadded size, between its two numbers.
    record: usize,
    record_unpadded: u64,
    /// Whether the index listed every block and the footer has been read.
    ended: bool,
    window: Option<Window>,
    lzma2: Lzma2,
}

impl Decoder {
    /// A decoder that refuses a dictionary larger than `max_dict` bytes.
    pub fn new(max_dict: u64) -> Self {
        Self {
            max_dict,
            streams: 0,
            padding: 0,
            stage: Stage::StreamHeader,
            unit: Vec::with_capacity(MAX_BLOCK_HEADER),
            flags: [0; 2],
            check_kind: 0,
            block: None,
            blocks: Vec::new(),
            index_bytes: 0,
            index_crc: Crc32::new(),
            varint: Varint::default(),
            record: 0,
            record_unpadded: 0,
            ended: false,
            window: None,
            lzma2: Lzma2::new(),
        }
    }

    /// Takes every byte of `data` (all of it, or an error), handing decoded bytes to `sink` with
    /// the number of the stream they belong to (0 for the image, 1 for the record).
    pub fn push(&mut self, mut data: &[u8], sink: &mut Sink<'_>) -> Result<(), Error> {
        while !data.is_empty() {
            match self.stage {
                Stage::StreamHeader => {
                    // A stream after the image must begin with the magic: anything else is not a record.
                    if self.streams > 0 {
                        let have = self.unit.len().min(MAGIC.len());
                        let check = (MAGIC.len() - have).min(data.len());
                        if data[..check] != MAGIC[have..have + check] {
                            return Err(Error::Malformed(NOT_AN_IMAGE));
                        }
                    }
                    if self.collect(&mut data, 12) {
                        self.stream_header()?;
                    }
                }
                Stage::BlockOrIndex => {
                    let byte = data[0];
                    data = &data[1..];
                    if byte == 0 {
                        // An index: its record count follows.
                        self.index_bytes = 1;
                        self.index_crc = Crc32::new();
                        self.index_crc.update(&[0]);
                        self.varint = Varint::default();
                        self.stage = Stage::IndexCount;
                    } else {
                        let size = (usize::from(byte) + 1) * 4;
                        self.unit.clear();
                        self.unit.push(byte);
                        self.block = Some(Block {
                            packed: None,
                            unpacked: None,
                            header_size: size,
                            packed_bytes: 0,
                            emitted_at_start: 0,
                        });
                        self.stage = Stage::BlockHeader;
                    }
                }
                Stage::BlockHeader => {
                    let size = self.block.as_ref().map_or(0, |b| b.header_size);
                    if self.collect(&mut data, size) {
                        self.block_header()?;
                    }
                }
                Stage::BlockData => {
                    let before = data.len();
                    let outcome = {
                        let window = self
                            .window
                            .as_mut()
                            .ok_or(Error::Malformed("a block has no dictionary"))?;
                        let stream = self.streams - 1;
                        self.lzma2
                            .push(&mut data, window, &mut |b| sink(stream, b))?
                    };
                    let used = (before - data.len()) as u64;
                    if let Some(block) = self.block.as_mut() {
                        block.packed_bytes += used;
                    }
                    // The decoded bytes so far go on to the sink now: a write shows progress as it comes.
                    let stream = self.streams - 1;
                    if let Some(window) = self.window.as_mut() {
                        window.flush(&mut |b| sink(stream, b))?;
                    }
                    if let Pushed::End = outcome {
                        self.stage = Stage::BlockPadding(((4 - (self.packed() % 4)) % 4) as usize);
                    }
                }
                Stage::BlockPadding(n) => {
                    if self.collect(&mut data, n) {
                        if self.unit.iter().any(|&b| b != 0) {
                            return Err(Error::Malformed("a block's padding is not zero"));
                        }
                        self.unit.clear();
                        self.end_block_data(sink)?;
                        let check = self.check_size();
                        self.stage = if check == 0 {
                            self.finish_block()?
                        } else {
                            Stage::BlockCheck(check)
                        };
                    }
                }
                Stage::BlockCheck(n) => {
                    if self.collect(&mut data, n) {
                        self.stage = self.finish_block()?;
                    }
                }
                Stage::IndexCount => {
                    let byte = data[0];
                    data = &data[1..];
                    self.index_bytes += 1;
                    self.index_crc.update(&[byte]);
                    if let Some(count) = self.varint.push(byte)? {
                        if count != self.blocks.len() as u64 {
                            return Err(Error::Malformed("the index does not list the blocks"));
                        }
                        self.varint = Varint::default();
                        self.stage = if count == 0 {
                            Stage::IndexPadding
                        } else {
                            Stage::IndexRecords {
                                left: count,
                                field: 0,
                            }
                        };
                    }
                }
                Stage::IndexRecords { left, field } => {
                    let byte = data[0];
                    data = &data[1..];
                    self.index_bytes += 1;
                    self.index_crc.update(&[byte]);
                    if let Some(value) = self.varint.push(byte)? {
                        self.varint = Varint::default();
                        let (unpadded, unpacked) = self.blocks[self.record];
                        if field == 0 {
                            if value != unpadded {
                                return Err(Error::Malformed(
                                    "the index's sizes do not match its blocks",
                                ));
                            }
                            self.record_unpadded = value;
                            self.stage = Stage::IndexRecords { left, field: 1 };
                        } else {
                            if value != unpacked {
                                return Err(Error::Malformed(
                                    "the index's sizes do not match its blocks",
                                ));
                            }
                            self.record += 1;
                            self.stage = if left == 1 {
                                Stage::IndexPadding
                            } else {
                                Stage::IndexRecords {
                                    left: left - 1,
                                    field: 0,
                                }
                            };
                        }
                    }
                }
                Stage::IndexPadding => {
                    if self.index_bytes.is_multiple_of(4) {
                        self.stage = Stage::IndexCrc;
                        continue;
                    }
                    let byte = data[0];
                    data = &data[1..];
                    self.index_bytes += 1;
                    self.index_crc.update(&[byte]);
                    if byte != 0 {
                        return Err(Error::Malformed("the index's padding is not zero"));
                    }
                }
                Stage::IndexCrc => {
                    if self.collect(&mut data, 4) {
                        let stored = u32::from_le_bytes([
                            self.unit[0],
                            self.unit[1],
                            self.unit[2],
                            self.unit[3],
                        ]);
                        let crc = std::mem::take(&mut self.index_crc).finish();
                        if stored != crc {
                            return Err(Error::Malformed("the index's CRC does not match"));
                        }
                        // The index is its records, its padding and its CRC: the footer's backward size counts all.
                        self.index_bytes += 4;
                        self.unit.clear();
                        self.stage = Stage::Footer;
                    }
                }
                Stage::Footer => {
                    if self.collect(&mut data, 12) {
                        self.footer()?;
                    }
                }
                Stage::Padding => {
                    let byte = data[0];
                    if byte != 0 {
                        // Another stream begins here; it must start on a four-byte boundary. Until its
                        // footer is read the file is not finished.
                        if !self.padding.is_multiple_of(4) {
                            return Err(Error::Malformed(NOT_AN_IMAGE));
                        }
                        self.padding = 0;
                        self.ended = false;
                        self.stage = Stage::StreamHeader;
                        continue;
                    }
                    data = &data[1..];
                    self.padding += 1;
                }
            }
        }
        Ok(())
    }

    /// The streams whose headers have been read so far: 1 for an image, 2 with a record.
    pub fn streams(&self) -> u32 {
        self.streams
    }

    /// Ends the stream: an error unless its footer has been read, which is what a truncated
    /// stream never does.
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.ended {
            Ok(())
        } else {
            Err(Error::Malformed(
                "the xz stream is cut short: it ends before its footer",
            ))
        }
    }

    /// The compressed bytes of the current block's LZMA2 data, as the block padding counts them.
    fn packed(&self) -> u64 {
        self.block.as_ref().map_or(0, |b| b.packed_bytes)
    }

    fn check_size(&self) -> usize {
        Check::of_kind(self.check_kind).map_or(0, |c| c.size())
    }

    /// Collects `n` bytes of a unit into `self.unit`. True when it holds them all.
    fn collect(&mut self, data: &mut &[u8], n: usize) -> bool {
        let have = self.unit.len();
        let take = (n - have).min(data.len());
        self.unit.extend_from_slice(&data[..take]);
        *data = &data[take..];
        self.unit.len() == n
    }

    fn stream_header(&mut self) -> Result<(), Error> {
        if self.unit[..6] != MAGIC {
            return Err(Error::Malformed(if self.streams == 0 {
                "the image is not an xz stream"
            } else {
                NOT_AN_IMAGE
            }));
        }
        if self.streams >= MAX_STREAMS {
            return Err(Error::Malformed(NOT_AN_IMAGE));
        }
        if self.unit[6] != 0 {
            return Err(Error::Malformed("an xz stream's flags are reserved"));
        }
        let stored = u32::from_le_bytes([self.unit[8], self.unit[9], self.unit[10], self.unit[11]]);
        if stored != crate::crc::crc32(&self.unit[6..8]) {
            return Err(Error::Malformed("an xz stream header's CRC does not match"));
        }
        self.flags = [self.unit[6], self.unit[7]];
        self.check_kind = self.unit[7] & 0x0F;
        if Check::of_kind(self.check_kind).is_none() {
            return Err(Error::Unsupported(
                "the xz stream uses a check this board does not take",
            ));
        }
        self.streams += 1;
        self.blocks.clear();
        self.record = 0;
        self.ended = false;
        self.unit.clear();
        self.stage = Stage::BlockOrIndex;
        Ok(())
    }

    fn block_header(&mut self) -> Result<(), Error> {
        let size = self.unit.len();
        let body = &self.unit[..size - 4];
        let stored = u32::from_le_bytes([
            self.unit[size - 4],
            self.unit[size - 3],
            self.unit[size - 2],
            self.unit[size - 1],
        ]);
        if stored != crate::crc::crc32(body) {
            return Err(Error::Malformed("a block header's CRC does not match"));
        }
        let flags = body[1];
        if flags & 0x3C != 0 {
            return Err(Error::Malformed("a block header's reserved flags are set"));
        }
        if flags & 0x03 != 0 {
            return Err(Error::Unsupported("a block chains more than one filter"));
        }
        let mut at = 2;
        let packed = if flags & 0x40 != 0 {
            Some(varint_at(body, &mut at)?)
        } else {
            None
        };
        let unpacked = if flags & 0x80 != 0 {
            Some(varint_at(body, &mut at)?)
        } else {
            None
        };
        let id = varint_at(body, &mut at)?;
        if id != FILTER_LZMA2 {
            return Err(Error::Unsupported(
                "the xz stream uses a filter other than LZMA2",
            ));
        }
        if varint_at(body, &mut at)? != 1 {
            return Err(Error::Malformed(
                "the LZMA2 filter's properties are not one byte",
            ));
        }
        let prop = *body
            .get(at)
            .ok_or(Error::Malformed("a block header is cut short"))?;
        at += 1;
        let dict = dict_size(prop)?;
        if body[at..].iter().any(|&b| b != 0) {
            return Err(Error::Malformed("a block header's padding is not zero"));
        }
        if dict > self.max_dict {
            return Err(Error::Dictionary {
                mib: dict.div_ceil(1 << 20),
                limit_mib: self.max_dict >> 20,
            });
        }
        // A window that is big enough serves the block; a block's dictionary starts empty.
        let want = dict.max(MIN_WINDOW as u64) as usize;
        match &mut self.window {
            Some(window) if window.capacity() >= want => {}
            slot => *slot = Some(Window::new(want)),
        }
        let window = self
            .window
            .as_mut()
            .ok_or(Error::Malformed("no dictionary"))?;
        window.check = Check::of_kind(self.check_kind).ok_or(Error::Unsupported("a check"))?;
        let emitted = window.emitted;
        let block = self
            .block
            .as_mut()
            .ok_or(Error::Malformed("a block has no header"))?;
        block.packed = packed;
        block.unpacked = unpacked;
        block.emitted_at_start = emitted;
        self.lzma2 = Lzma2::new();
        self.unit.clear();
        self.stage = Stage::BlockData;
        Ok(())
    }

    /// The LZMA2 data of the block is over: its last bytes are out, and its sizes are checked.
    fn end_block_data(&mut self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let stream = self.streams - 1;
        let window = self
            .window
            .as_mut()
            .ok_or(Error::Malformed("no dictionary"))?;
        window.flush(&mut |b| sink(stream, b))?;
        let block = self.block.as_ref().ok_or(Error::Malformed("no block"))?;
        if let Some(packed) = block.packed
            && packed != block.packed_bytes
        {
            return Err(Error::Malformed(
                "a block's compressed size is not the one its header gives",
            ));
        }
        let emitted = window.emitted - block.emitted_at_start;
        if let Some(unpacked) = block.unpacked
            && unpacked != emitted
        {
            return Err(Error::Malformed(
                "a block's size is not the one its header gives",
            ));
        }
        Ok(())
    }

    /// The block is complete: its check is verified (over its decoded bytes), and its sizes go
    /// in the index's list. Returns the next stage.
    fn finish_block(&mut self) -> Result<Stage, Error> {
        let block_check_bytes = self.unit.clone();
        let window = self
            .window
            .as_mut()
            .ok_or(Error::Malformed("no dictionary"))?;
        let check = std::mem::replace(&mut window.check, Check::None);
        if self.check_size() > 0 && !check.matches(&block_check_bytes) {
            return Err(Error::Malformed("a block's check does not match its data"));
        }
        let block = self.block.take().ok_or(Error::Malformed("no block"))?;
        let unpadded = block.header_size as u64 + block.packed_bytes + self.check_size() as u64;
        let unpacked = self.window.as_ref().map_or(0, |w| w.emitted) - block.emitted_at_start;
        if self.blocks.len() >= MAX_BLOCKS {
            return Err(Error::Unsupported(
                "the xz stream has more blocks than the board takes",
            ));
        }
        self.blocks.push((unpadded, unpacked));
        self.unit.clear();
        Ok(Stage::BlockOrIndex)
    }

    fn footer(&mut self) -> Result<(), Error> {
        if self.unit[10..12] != FOOTER_MAGIC {
            return Err(Error::Malformed("the xz footer's magic is wrong"));
        }
        let stored = u32::from_le_bytes([self.unit[0], self.unit[1], self.unit[2], self.unit[3]]);
        if stored != crate::crc::crc32(&self.unit[4..10]) {
            return Err(Error::Malformed("the xz footer's CRC does not match"));
        }
        let backward = (u64::from(u32::from_le_bytes([
            self.unit[4],
            self.unit[5],
            self.unit[6],
            self.unit[7],
        ])) + 1)
            * 4;
        if backward != self.index_bytes {
            return Err(Error::Malformed(
                "the xz footer's index size does not match the index",
            ));
        }
        if self.unit[8..10] != self.flags {
            return Err(Error::Malformed(
                "the xz footer's flags do not match the header's",
            ));
        }
        self.unit.clear();
        self.ended = true;
        self.padding = 0;
        self.stage = Stage::Padding;
        Ok(())
    }
}
