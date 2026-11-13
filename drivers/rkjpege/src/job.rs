//! The job for the JPEG encoder: the values the caller chooses, the picture and its sources, and
//! the buffers behind the address registers, in the form `lemnos-driver-rk-mpp`'s [`Job`] takes.
//!
//! The builder owns the words that describe the picture and its planes (the picture size and
//! fill, the source format and strides, the picture offset) and the address registers. The
//! caller sets every other field through [`RegisterSet::set`], with the header's width, sign and
//! encodings checked. A job is built only when every check below holds, so a refused value or
//! buffer never reaches the message.
//!
//! What is checked, against what:
//!
//! - **Picture**: 1 to 65536 pixels each way. The encoded picture is the picture rounded up to 16,
//!   the size the header's `pic_wd8_m1` and `pic_hd8_m1` count in 8-pixel units, and the fill is
//!   the difference (`pic_wfill_jpeg`, 6 bits).
//! - **Strides**: each plane's row in bytes is at least its row of the encoded picture (packed
//!   YUV 4:2:2 is two bytes a pixel). A stride is 20 bits (luma) or 19 bits (chroma).
//! - **Address registers**: only the file's address registers, each at the alignment its header
//!   comment gives (the link table is refused: the builder never runs in link mode), at an offset
//!   below the buffer's size, placed once. The size is the caller's own measure of the buffer
//!   (`Claim::buffer_size`); rk-mpp checks the offset against it again at submit.
//! - **Planes**: each source plane, from its offset, lies in its buffer for the encoded picture: a
//!   luma plane is `stride0` times the encoded height; a chroma plane is `stride1` times the rows
//!   its format has (half for 4:2:0, all for 4:2:2 and 4:4:4). The sources give no reach beyond the
//!   encoded picture (the frame's own padding rows), so none is claimed here; see `TODO.md`.
//! - **Bitstream**: the top and bottom sit in one buffer, top before bottom; the read and start
//!   pointers sit between them in that buffer.
//! - **Required**: the luma plane, the chroma planes its format has, the quantisation table, and
//!   the bitstream's four pointers. The ECS length buffer is not required: the vendor HAL does
//!   not set it on the frame-by-frame path (`hal_jpege_vpu720.c`).
//! - **Start**: the block starts on the kernel's own write of the start word (its `START_EN` bit
//!   is `vepu_cmd` 1 at bit 8), which the job leaves to the kernel.

use std::fmt;

use lemnos_driver_rk_mpp::{Address, BufferId, Job, MAX_ADDRESSES, ReadBack};

use crate::layout::{AddressReg, Field};
use crate::vpu720::{
    reg009_lkt_base_addr, reg016_adr_qtbl, reg017_adr_bsbt, reg018_adr_bsbb, reg019_adr_bsbr,
    reg020_adr_bsbs, reg022_adr_src0, reg023_adr_src1, reg024_adr_src2, reg029_sw_enc_rsl,
    reg030_sw_src_fill, reg032_sw_src_fmt, reg033_sw_pic_ofst, reg034_sw_src_strd_0,
    reg035_sw_src_strd_1,
};

/// The words a job writes: the base struct, words 0 to 42. The status words are read-only and
/// come back in the read-back window.
pub const WRITE_WORDS: usize = 43;

/// The read-back window: from the interrupt state (word 7) to the end of the file (word 78), so
/// every status word is copied back.
pub const READ_BACK: ReadBack = ReadBack {
    first: 7,
    count: 72,
};

/// The largest picture side, in pixels: `pic_wd8_m1` is 13 bits of 8-pixel units.
pub const MAX_SIDE: u32 = 65_536;

/// The words the builder owns: set through [`RegisterSet::set_picture`], [`RegisterSet::set_source`]
/// and [`RegisterSet::set_address`] only.
const OWNED: [u16; 6] = [29, 30, 32, 33, 34, 35];

/// The address registers the builder places, by word: the quantisation table, the bitstream's
/// four pointers, the ECS length buffer and the three source planes. The link table (word 9) is
/// refused.
const ADDRESS_WORDS: [u16; 9] = [16, 17, 18, 19, 20, 21, 22, 23, 24];

/// The source formats the builder takes: the header's `src_fmt` encodings it names. Tiled formats
/// are not taken: their plane layout is not in the sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Greyscale: one luma plane.
    Yuv400,
    /// 4:2:0, chroma interleaved (NV12 order): luma and one chroma plane.
    Yuv420Sp,
    /// 4:2:0, planar: luma, U and V planes.
    Yuv420P,
    /// 4:2:2, chroma interleaved.
    Yuv422Sp,
    /// 4:2:2, planar.
    Yuv422P,
    /// 4:4:4, chroma interleaved (two bytes a pixel).
    Yuv444Sp,
    /// 4:4:4, planar.
    Yuv444P,
    /// Packed 4:2:2, Y0 U Y1 V.
    Yuyv,
    /// Packed 4:2:2, U Y0 V Y1.
    Uyvy,
}

impl Source {
    /// The planes the source has: 1 (luma only), 2 (luma and interleaved chroma) or 3.
    #[must_use]
    pub const fn planes(self) -> usize {
        match self {
            Self::Yuv400 | Self::Yuyv | Self::Uyvy => 1,
            Self::Yuv420Sp | Self::Yuv422Sp | Self::Yuv444Sp => 2,
            Self::Yuv420P | Self::Yuv422P | Self::Yuv444P => 3,
        }
    }

    /// The value of `src_fmt` the header gives the format (`hal_jpege_vpu720.c`'s input formats).
    #[must_use]
    pub const fn format(self) -> u32 {
        match self {
            Self::Yuv400 => 0xa,
            Self::Yuv420Sp => 6,
            Self::Yuv420P => 7,
            Self::Yuv422Sp => 4,
            Self::Yuv422P => 5,
            Self::Yuv444Sp => 0xc,
            Self::Yuv444P => 0xd,
            Self::Yuyv => 8,
            Self::Uyvy => 9,
        }
    }

    /// The value of `out_fmt`: the sampling of the source, which the block encodes as it is.
    #[must_use]
    pub const fn output(self) -> u32 {
        match self {
            Self::Yuv400 => 0,
            Self::Yuv420Sp | Self::Yuv420P => 1,
            Self::Yuv422Sp | Self::Yuv422P | Self::Yuyv | Self::Uyvy => 2,
            Self::Yuv444Sp | Self::Yuv444P => 3,
        }
    }

    /// Bytes in a luma row of the encoded picture of width `coded_w`.
    const fn luma_row(self, coded_w: u32) -> u32 {
        match self {
            Self::Yuyv | Self::Uyvy => 2 * coded_w,
            _ => coded_w,
        }
    }

    /// Bytes in a chroma row of the encoded picture (for the interleaved and planar forms).
    const fn chroma_row(self, coded_w: u32) -> u32 {
        match self {
            Self::Yuv420P | Self::Yuv422P => coded_w / 2,
            Self::Yuv444Sp => 2 * coded_w,
            _ => coded_w,
        }
    }

    /// Rows of chroma in the encoded picture of height `coded_h`.
    const fn chroma_rows(self, coded_h: u32) -> u32 {
        match self {
            Self::Yuv420Sp | Self::Yuv420P => coded_h / 2,
            _ => coded_h,
        }
    }
}

/// A buffer the claim holds, with the size the caller measured for it (`Claim::buffer_size`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buffer {
    /// The claim's name for the buffer.
    pub id: BufferId,
    /// Its size in bytes.
    pub size: u64,
}

/// Why a value or an address was refused. The job is not changed by a refused call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetError {
    /// The field is in a word the file does not let a job write.
    ReadOnly {
        /// The field's word.
        word: u16,
    },
    /// The field is past the words a job writes.
    OutsideWrite {
        /// The field's word.
        word: u16,
    },
    /// The word is one the builder owns: [`RegisterSet::set_picture`], [`RegisterSet::set_source`]
    /// or [`RegisterSet::set_address`] sets it.
    Owned {
        /// The field's word.
        word: u16,
    },
    /// The value does not fit the field.
    Range {
        /// The value given.
        value: i64,
        /// The field's width in bits.
        width: u8,
    },
    /// The value is one the header marks reserved for the field.
    Reserved {
        /// The value given.
        value: i64,
    },
    /// The picture is outside 1 to [`MAX_SIDE`] pixels a side.
    Picture {
        /// The width given.
        width: u32,
        /// The height given.
        height: u32,
    },
    /// A stride is below the row it must hold, or does not fit its register.
    Stride {
        /// 0 for the luma stride, 1 for the chroma stride.
        plane: u8,
        /// The bytes a row of the encoded picture needs.
        need: u32,
        /// The stride given.
        stride: u32,
    },
    /// A source was given before the picture.
    NeedsPicture,
    /// The link table is refused: the builder never runs the block in link mode.
    LinkTable,
    /// The register is not an address register of the block.
    NotAnAddress {
        /// Its word.
        word: u16,
    },
    /// The offset is not a multiple of the alignment the header gives.
    Misaligned {
        /// The alignment in bytes.
        align: u32,
    },
    /// The offset is at or past the buffer's size (rk-mpp refuses it too).
    OffsetOutside {
        /// The offset given.
        offset: u32,
        /// The buffer's size.
        size: u64,
    },
    /// The register already holds a buffer in this job.
    DuplicateAddress {
        /// Its word.
        word: u16,
    },
    /// The job already names [`MAX_ADDRESSES`] address registers.
    TooManyAddresses,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOnly { word } => write!(f, "word {word} is not writable"),
            Self::OutsideWrite { word } => write!(f, "word {word} is past the write side"),
            Self::Owned { word } => {
                write!(f, "word {word} is set by the picture, source or address")
            }
            Self::Range { value, width } => write!(f, "{value} does not fit a {width}-bit field"),
            Self::Reserved { value } => write!(f, "{value} is a reserved encoding of the field"),
            Self::Picture { width, height } => {
                write!(
                    f,
                    "a {width}x{height} picture is outside 1 to {MAX_SIDE} a side"
                )
            }
            Self::Stride {
                plane,
                need,
                stride,
            } => write!(
                f,
                "plane {plane} stride {stride} holds less than a {need}-byte row"
            ),
            Self::NeedsPicture => write!(f, "the picture is set before the source"),
            Self::LinkTable => write!(f, "the link table is refused"),
            Self::NotAnAddress { word } => write!(f, "word {word} is not an address register"),
            Self::Misaligned { align } => write!(f, "the offset is not {align}-byte aligned"),
            Self::OffsetOutside { offset, size } => {
                write!(f, "offset {offset} is outside a {size}-byte buffer")
            }
            Self::DuplicateAddress { word } => write!(f, "word {word} already holds a buffer"),
            Self::TooManyAddresses => write!(f, "more than {MAX_ADDRESSES} address registers"),
        }
    }
}

impl std::error::Error for SetError {}

/// Why a job is not built: a required part is missing, or a plane or pointer does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// The picture is not set.
    NoPicture,
    /// The source is not set.
    NoSource,
    /// A required address register holds no buffer.
    Missing {
        /// Its word.
        word: u16,
    },
    /// A source plane, from its offset, reaches past its buffer.
    Plane {
        /// The plane's address register.
        word: u16,
        /// Bytes the plane needs from its offset.
        need: u64,
        /// The buffer's size.
        size: u64,
    },
    /// The bitstream's pointers are not in one buffer.
    BitstreamBuffers,
    /// The bitstream's top is not below its bottom, or a pointer is outside the stream.
    BitstreamOrder,
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPicture => write!(f, "no picture is set"),
            Self::NoSource => write!(f, "no source is set"),
            Self::Missing { word } => write!(f, "address register {word} holds no buffer"),
            Self::Plane { word, need, size } => {
                write!(
                    f,
                    "plane at register {word} needs {need} bytes, its buffer holds {size}"
                )
            }
            Self::BitstreamBuffers => write!(f, "the bitstream's pointers are not in one buffer"),
            Self::BitstreamOrder => write!(f, "the bitstream's pointers are out of order"),
        }
    }
}

impl std::error::Error for JobError {}

/// A job's register values and address registers. Built once and reused: [`Self::clear`] empties
/// it, and nothing allocates after [`Self::new`].
#[derive(Debug)]
pub struct RegisterSet {
    words: [u32; WRITE_WORDS],
    /// The picture as encoded: width and height rounded up to 16.
    coded: Option<(u32, u32)>,
    source: Option<Source>,
    strides: [u32; 2],
    addresses: Vec<Address>,
    sizes: Vec<u64>,
}

impl Default for RegisterSet {
    fn default() -> Self {
        Self::new()
    }
}

impl RegisterSet {
    /// An empty set. Its address lists are allocated here, once, at their full size.
    #[must_use]
    pub fn new() -> Self {
        Self {
            words: [0; WRITE_WORDS],
            coded: None,
            source: None,
            strides: [0; 2],
            addresses: Vec::with_capacity(MAX_ADDRESSES),
            sizes: Vec::with_capacity(MAX_ADDRESSES),
        }
    }

    /// Empties the set for the next job.
    pub fn clear(&mut self) {
        self.words = [0; WRITE_WORDS];
        self.coded = None;
        self.source = None;
        self.strides = [0; 2];
        self.addresses.clear();
        self.sizes.clear();
    }

    /// Sets a field the caller chooses. `value` must fit the field's width and be one of the
    /// field's encodings when the header lists them.
    ///
    /// # Errors
    ///
    /// [`SetError::OutsideWrite`] past the write side, [`SetError::ReadOnly`] for a word the header
    /// does not let a job write, [`SetError::Owned`] for a word the builder sets, and
    /// [`SetError::Range`] or [`SetError::Reserved`] for a value the field does not take.
    pub fn set(&mut self, field: Field, value: i64) -> Result<(), SetError> {
        if OWNED.contains(&field.word) || ADDRESS_WORDS.contains(&field.word) {
            return Err(SetError::Owned { word: field.word });
        }
        self.put(field, value)
    }

    /// Writes a field, with the checks [`Self::set`] makes except the owned words, which the
    /// builder's own setters use.
    fn put(&mut self, field: Field, value: i64) -> Result<(), SetError> {
        if usize::from(field.word) >= WRITE_WORDS {
            return Err(SetError::OutsideWrite { word: field.word });
        }
        if !field.writable {
            return Err(SetError::ReadOnly { word: field.word });
        }
        let bits = encode(field, value)?;
        let slot = &mut self.words[usize::from(field.word)];
        *slot = (*slot & !(field.mask() << field.lsb)) | bits;
        Ok(())
    }

    /// Sets the picture: its size, and the fill up to the encoded size (rounded up to 16).
    ///
    /// # Errors
    ///
    /// [`SetError::Picture`] outside 1 to [`MAX_SIDE`] a side.
    pub fn set_picture(&mut self, width: u32, height: u32) -> Result<(), SetError> {
        if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
            return Err(SetError::Picture { width, height });
        }
        let coded_w = width.next_multiple_of(16);
        let coded_h = height.next_multiple_of(16);
        self.put(reg029_sw_enc_rsl::PIC_WD8_M1, i64::from(coded_w / 8 - 1))?;
        self.put(reg029_sw_enc_rsl::PIC_HD8_M1, i64::from(coded_h / 8 - 1))?;
        self.put(
            reg030_sw_src_fill::PIC_WFILL_JPEG,
            i64::from(coded_w - width),
        )?;
        self.put(
            reg030_sw_src_fill::PIC_HFILL_JPEG,
            i64::from(coded_h - height),
        )?;
        self.put(reg033_sw_pic_ofst::PIC_OFST_X, 0)?;
        self.put(reg033_sw_pic_ofst::PIC_OFST_Y, 0)?;
        self.coded = Some((coded_w, coded_h));
        Ok(())
    }

    /// Sets the source: its format, the output sampling that follows it, and the strides of its
    /// luma and chroma planes in bytes. Needs the picture first.
    ///
    /// # Errors
    ///
    /// [`SetError::NeedsPicture`] before [`Self::set_picture`], [`SetError::Stride`] for a stride
    /// below its row (or past its register), and the field checks of [`Self::set`].
    pub fn set_source(
        &mut self,
        source: Source,
        stride0: u32,
        stride1: u32,
    ) -> Result<(), SetError> {
        let (coded_w, _) = self.coded.ok_or(SetError::NeedsPicture)?;
        // Every check first, so a refused call leaves the set as it was.
        let luma_need = source.luma_row(coded_w);
        if stride0 < luma_need {
            return Err(SetError::Stride {
                plane: 0,
                need: luma_need,
                stride: stride0,
            });
        }
        encode(reg034_sw_src_strd_0::SRC_STRD_0, i64::from(stride0))?;
        let chroma = source.planes() > 1;
        if chroma {
            let chroma_need = source.chroma_row(coded_w);
            if stride1 < chroma_need {
                return Err(SetError::Stride {
                    plane: 1,
                    need: chroma_need,
                    stride: stride1,
                });
            }
            encode(reg035_sw_src_strd_1::SRC_STRD_1, i64::from(stride1))?;
        }
        self.put(reg032_sw_src_fmt::SRC_FMT, i64::from(source.format()))?;
        self.put(reg032_sw_src_fmt::OUT_FMT, i64::from(source.output()))?;
        self.put(reg034_sw_src_strd_0::SRC_STRD_0, i64::from(stride0))?;
        self.strides[0] = stride0;
        if chroma {
            self.put(reg035_sw_src_strd_1::SRC_STRD_1, i64::from(stride1))?;
            self.strides[1] = stride1;
        }
        self.source = Some(source);
        Ok(())
    }

    /// Gives an address register a buffer the claim holds, `offset` bytes into it.
    ///
    /// # Errors
    ///
    /// [`SetError::LinkTable`] for the link table, [`SetError::NotAnAddress`] for any other word
    /// the block does not take as an address, [`SetError::Misaligned`] for an offset the header's
    /// alignment forbids, [`SetError::OffsetOutside`] for an offset at or past the buffer's size,
    /// [`SetError::DuplicateAddress`] for a register already placed, and
    /// [`SetError::TooManyAddresses`] past [`MAX_ADDRESSES`].
    pub fn set_address(
        &mut self,
        reg: AddressReg,
        buffer: Buffer,
        offset: u32,
    ) -> Result<(), SetError> {
        if reg.word == reg009_lkt_base_addr::ADDRESS.word {
            return Err(SetError::LinkTable);
        }
        if !ADDRESS_WORDS.contains(&reg.word) {
            return Err(SetError::NotAnAddress { word: reg.word });
        }
        if !offset.is_multiple_of(reg.align) {
            return Err(SetError::Misaligned { align: reg.align });
        }
        if u64::from(offset) >= buffer.size {
            return Err(SetError::OffsetOutside {
                offset,
                size: buffer.size,
            });
        }
        if self.addresses.iter().any(|a| a.register == reg.word) {
            return Err(SetError::DuplicateAddress { word: reg.word });
        }
        if self.addresses.len() == MAX_ADDRESSES {
            return Err(SetError::TooManyAddresses);
        }
        self.addresses.push(Address {
            register: reg.word,
            buffer: buffer.id,
            offset,
        });
        self.sizes.push(buffer.size);
        Ok(())
    }

    /// The register values of the write side.
    #[must_use]
    pub fn words(&self) -> &[u32; WRITE_WORDS] {
        &self.words
    }

    /// The address registers placed so far.
    #[must_use]
    pub fn addresses(&self) -> &[Address] {
        &self.addresses
    }

    /// The job for a claim holder: the write side from word 0, the address registers, and the
    /// read-back window. `identity` is carried to the completion, never read.
    ///
    /// # Errors
    ///
    /// [`JobError::NoPicture`] and [`JobError::NoSource`] before those are set,
    /// [`JobError::Missing`] for a required address register, [`JobError::Plane`] for a source
    /// plane that reaches past its buffer, and [`JobError::BitstreamBuffers`] and
    /// [`JobError::BitstreamOrder`] for a bitstream whose pointers are not one ordered range.
    pub fn job(&self, identity: u64) -> Result<Job<'_>, JobError> {
        let (_, coded_h) = self.coded.ok_or(JobError::NoPicture)?;
        let source = self.source.ok_or(JobError::NoSource)?;
        self.check_planes(source, coded_h)?;
        self.check_bitstream()?;
        Ok(Job {
            identity,
            first: 0,
            registers: &self.words,
            more: &[],
            addresses: &self.addresses,
            read: Some(READ_BACK),
        })
    }

    /// The placed address at `word`, with its buffer's size.
    fn placed(&self, word: u16) -> Option<(Address, u64)> {
        self.addresses
            .iter()
            .position(|a| a.register == word)
            .map(|i| (self.addresses[i], self.sizes[i]))
    }

    /// Every source plane the format has is placed, and lies in its buffer for the encoded picture.
    fn check_planes(&self, source: Source, coded_h: u32) -> Result<(), JobError> {
        let luma = u64::from(self.strides[0]) * u64::from(coded_h);
        let chroma = u64::from(self.strides[1]) * u64::from(source.chroma_rows(coded_h));
        let needs = [
            (reg022_adr_src0::ADDRESS.word, Some(luma)),
            (
                reg023_adr_src1::ADDRESS.word,
                (source.planes() > 1).then_some(chroma),
            ),
            (
                reg024_adr_src2::ADDRESS.word,
                (source.planes() > 2).then_some(chroma),
            ),
        ];
        for (word, need) in needs {
            let Some(need) = need else { continue };
            let (address, size) = self.placed(word).ok_or(JobError::Missing { word })?;
            let end = u64::from(address.offset) + need;
            if end > size {
                return Err(JobError::Plane {
                    word,
                    need: end,
                    size,
                });
            }
        }
        Ok(())
    }

    /// The bitstream's top and bottom are in one buffer with top below bottom; the read and start
    /// pointers are in that buffer between them. The quantisation table is required.
    fn check_bitstream(&self) -> Result<(), JobError> {
        let qtbl = reg016_adr_qtbl::ADDRESS.word;
        self.placed(qtbl).ok_or(JobError::Missing { word: qtbl })?;
        let (top, _) = self
            .placed(reg017_adr_bsbt::ADDRESS.word)
            .ok_or(JobError::Missing {
                word: reg017_adr_bsbt::ADDRESS.word,
            })?;
        let (bottom, _) = self
            .placed(reg018_adr_bsbb::ADDRESS.word)
            .ok_or(JobError::Missing {
                word: reg018_adr_bsbb::ADDRESS.word,
            })?;
        let (read, _) = self
            .placed(reg019_adr_bsbr::ADDRESS.word)
            .ok_or(JobError::Missing {
                word: reg019_adr_bsbr::ADDRESS.word,
            })?;
        let (start, _) = self
            .placed(reg020_adr_bsbs::ADDRESS.word)
            .ok_or(JobError::Missing {
                word: reg020_adr_bsbs::ADDRESS.word,
            })?;
        if read.buffer != top.buffer || start.buffer != top.buffer || bottom.buffer != top.buffer {
            return Err(JobError::BitstreamBuffers);
        }
        let between = |p: Address| top.offset <= p.offset && p.offset <= bottom.offset;
        if top.offset >= bottom.offset || !between(read) || !between(start) {
            return Err(JobError::BitstreamOrder);
        }
        Ok(())
    }
}

/// The field's bits in place for `value`, checked against its width and encodings.
fn encode(field: Field, value: i64) -> Result<u32, SetError> {
    let width = u32::from(field.width);
    let high = (1i64 << width) - 1;
    if value < 0 || value > high {
        return Err(SetError::Range {
            value,
            width: field.width,
        });
    }
    if !field.values.is_empty() && !field.values.iter().any(|&v| i64::from(v) == value) {
        return Err(SetError::Reserved { value });
    }
    Ok(((value as u32) & field.mask()) << field.lsb)
}
