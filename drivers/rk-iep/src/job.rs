//! Jobs: three input frames, two output frames and the block's two scratch buffers, each placed in
//! a dma-buf the caller holds and checked against that dma-buf's size.
//!
//! # Where a caller's numbers become addresses
//!
//! The vendor driver turns every address word of the parameters into a bus address: the dma-buf
//! the word names, plus an offset. It checks neither the offset nor anything the block then reads
//! or writes from there, and other dma-bufs sit in the same address space. So everything that
//! decides where the block reaches is checked here:
//!
//! - **Descriptors.** A word holds a [`Buffer`]'s descriptor and nothing else, and only a [`Job`]
//!   writes one. A descriptor of 0 is refused: the kernel skips a word of 0 and hands the block
//!   address 0. The job borrows every descriptor until it is dropped, so none can be closed and
//!   reused under it.
//! - **Offsets.** They go in a `CMD_SET_REG_ADDR_OFFSET` list keyed by the register, with
//!   `FLAG_REG_NO_OFFSET` set, so a descriptor never shares its word with an offset. Each is at
//!   most `u32::MAX` and is checked with the plane it starts.
//! - **Extents.** A plane of `lines` lines `stride` apart is checked as `offset + lines * stride`
//!   bytes, in `u64` with overflow refused, against the dma-buf's size as the kernel reports it.
//!   Luma has `height` lines, chroma `height` (4:2:2) or `height / 2` (4:2:0). A semi-planar chroma
//!   line holds `width` bytes, a planar one `width / 2`.
//! - **Strides.** Given in bytes, a multiple of 4, at least a line, at most what the 16-bit field
//!   holds in 4-byte words; the block gets the stride in words. The source does not say the
//!   field's unit. In words, the block uses exactly the checked pitch; were it bytes, the pitch
//!   would be a quarter of it and the block would stay inside the checked extent. The output's
//!   chroma is taken to use the output stride, the only one it has.
//! - **Size.** Whole tiles of 16 by 4, at most 1920 by 1088 (`IEP2_TILE_W_MAX` and
//!   `IEP2_TILE_H_MAX`, the size of the kernel's ROI buffer), inside the 11-bit size fields.
//! - **Scratch.** The motion data and motion vector buffers are read and written by the block. The
//!   source does not give their size; each must hold [`scratch_len`] bytes, two per pixel, a bound
//!   chosen to be safe that the board run measures.
//! - **Modes.** Every frame a mode might read is required, so no register is left at address 0.
//!   The one mode the vendor driver says reads past its buffer (`I1O1T`) does not exist here. The
//!   output-mode bit, whose effect on the written extent the source does not give, and the ROI,
//!   are held at zero. `osd_area_num` is at most 8: the kernel indexes its arrays by it.
//!
//! The parameters are sent with their exact size, and the results read with theirs: the vendor
//! driver copies a parameter message of any size into its fixed structure, and copies results out
//! at the size asked.

use std::fmt;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::mpp::{
    CMD_POLL_HW_FINISH, CMD_SET_REG_ADDR_OFFSET, CMD_SET_REG_READ, CMD_SET_REG_WRITE,
    FLAG_LAST_MSG, FLAG_MULTI_MSG, FLAG_REG_NO_OFFSET, Msg, OffsetElem,
};
use crate::params::{Addr, DilMode, FieldOrder, Params, PdComp, Results, Sampling, Swap, Tuning};
use crate::regs::{ADDRESS_REGISTERS, ADDRESS_WORDS, STRIDE_FIELD_BITS};

/// Width of a tile in pixels (`TILE_WIDTH`).
pub const TILE_WIDTH: u32 = 16;
/// Height of a tile in lines (`TILE_HEIGHT`).
pub const TILE_HEIGHT: u32 = 4;
/// The widest image: `IEP2_TILE_W_MAX` tiles.
pub const MAX_WIDTH: u32 = 120 * TILE_WIDTH;
/// The tallest image: `IEP2_TILE_H_MAX` tiles.
pub const MAX_HEIGHT: u32 = 272 * TILE_HEIGHT;
/// What every stride must be a multiple of.
pub const STRIDE_ALIGN: u32 = 4;
/// The largest stride in bytes: the 16-bit field, in 4-byte words.
pub const MAX_STRIDE: u32 = ((1 << STRIDE_FIELD_BITS) - 1) * STRIDE_ALIGN;
/// OSD areas the block has.
pub const MAX_OSD_AREAS: u32 = crate::params::OSD_AREAS as u32;

/// The bytes each scratch buffer (motion data, motion vectors) must hold for an image of this
/// size: two per pixel.
#[must_use]
pub const fn scratch_len(width: u32, height: u32) -> u64 {
    2 * width as u64 * height as u64
}

/// A dma-buf the caller holds, with the size the kernel gives it.
#[derive(Clone, Copy, Debug)]
pub struct Buffer<'a> {
    fd: BorrowedFd<'a>,
    len: u64,
}

impl<'a> Buffer<'a> {
    /// The dma-buf `fd`, measured. Descriptor 0 is refused (the kernel reads a word of 0 as no
    /// buffer). A descriptor that is not a dma-buf is refused by the kernel when a job names it.
    pub fn new(fd: BorrowedFd<'a>) -> std::io::Result<Self> {
        let len = lemnos_linux_sys::dma_buf::size(fd)?;
        Self::with_len(fd, len)
    }

    /// `fd` taken to be `len` bytes. Only tests and [`Self::new`] make one.
    pub(crate) fn with_len(fd: BorrowedFd<'a>, len: u64) -> std::io::Result<Self> {
        if fd.as_raw_fd() <= 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "descriptor 0 reads as no buffer to the kernel",
            ));
        }
        Ok(Self { fd, len })
    }

    /// The size in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the dma-buf is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn word(&self) -> u32 {
        // Positive, checked when made.
        self.fd.as_raw_fd().unsigned_abs()
    }
}

/// Where a plane starts: a dma-buf and a byte offset into it.
#[derive(Clone, Copy, Debug)]
pub struct At<'a> {
    /// The dma-buf.
    pub buffer: Buffer<'a>,
    /// Bytes from its start.
    pub offset: u64,
}

impl<'a> At<'a> {
    /// The plane at `offset` in `buffer`.
    #[must_use]
    pub fn new(buffer: Buffer<'a>, offset: u64) -> Self {
        Self { buffer, offset }
    }
}

/// A frame's chroma.
#[derive(Clone, Copy, Debug)]
pub enum Chroma<'a> {
    /// One interleaved plane.
    Semi(At<'a>),
    /// Separate Cb and Cr planes.
    Planar {
        /// Cb.
        cb: At<'a>,
        /// Cr.
        cr: At<'a>,
    },
}

/// One frame's planes.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// Luma.
    pub luma: At<'a>,
    /// Chroma.
    pub chroma: Chroma<'a>,
}

/// The input: three frames of one layout.
#[derive(Clone, Copy, Debug)]
pub struct Input<'a> {
    /// Chroma sampling.
    pub sampling: Sampling,
    /// Chroma layout; [`Swap::Planar`] needs [`Chroma::Planar`] frames, the others
    /// [`Chroma::Semi`].
    pub swap: Swap,
    /// Bytes per luma line.
    pub luma_stride: u32,
    /// Bytes per chroma line.
    pub chroma_stride: u32,
    /// The current frame.
    pub cur: Frame<'a>,
    /// The next frame.
    pub next: Frame<'a>,
    /// The previous frame.
    pub prev: Frame<'a>,
}

/// The output: two semi-planar frames, the top and bottom results.
#[derive(Clone, Copy, Debug)]
pub struct Output<'a> {
    /// Chroma sampling.
    pub sampling: Sampling,
    /// [`Swap::SemiUv`] or [`Swap::SemiVu`].
    pub swap: Swap,
    /// Bytes per line, luma and chroma.
    pub stride: u32,
    /// The top (or only) output.
    pub top: Frame<'a>,
    /// The bottom output.
    pub bottom: Frame<'a>,
}

/// How the block deinterlaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    /// The mode.
    pub dil: DilMode,
    /// Which field comes first.
    pub field_order: FieldOrder,
    /// The frames the fields come from in [`DilMode::Pd`]; ignored otherwise.
    pub pd: PdComp,
}

/// A place the block reads or writes, by the address word it fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The current frame's luma.
    CurY,
    /// The current frame's chroma, or Cb.
    CurC,
    /// The current frame's Cr.
    CurCr,
    /// The next frame's luma.
    NextY,
    /// The next frame's chroma, or Cb.
    NextC,
    /// The next frame's Cr.
    NextCr,
    /// The previous frame's luma.
    PrevY,
    /// The previous frame's chroma, or Cb.
    PrevC,
    /// The previous frame's Cr.
    PrevCr,
    /// The top output's luma.
    TopY,
    /// The top output's chroma.
    TopC,
    /// The bottom output's luma.
    BottomY,
    /// The bottom output's chroma.
    BottomC,
    /// The motion vectors.
    Mv,
    /// The motion data.
    Md,
}

impl Slot {
    /// The address word this slot fills.
    const fn word(self) -> usize {
        match self {
            Self::CurY => 0,
            Self::CurC => 1,
            Self::CurCr => 2,
            Self::NextY => 3,
            Self::NextC => 4,
            Self::NextCr => 5,
            Self::PrevY => 6,
            Self::PrevC => 7,
            Self::PrevCr => 8,
            Self::TopY => 9,
            Self::TopC => 10,
            Self::BottomY => 12,
            Self::BottomC => 13,
            Self::Mv => 15,
            Self::Md => 16,
        }
    }
}

/// Why a job was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// The size is not whole tiles, or is outside 16x4 to 1920x1088.
    Size {
        /// Width asked.
        width: u32,
        /// Height asked.
        height: u32,
    },
    /// A stride is under a line, over [`MAX_STRIDE`], or not a multiple of 4.
    Stride {
        /// The plane the stride is for.
        slot: Slot,
        /// The stride asked.
        stride: u32,
    },
    /// A plane runs past its dma-buf, or its offset is past `u32::MAX`.
    OutOfBuffer {
        /// The plane.
        slot: Slot,
        /// The byte it would end at, or `u64::MAX` if that overflows.
        end: u64,
        /// The dma-buf's size.
        len: u64,
    },
    /// The frame's chroma does not match the layout, or the output is planar.
    Layout {
        /// The first plane of the frame.
        slot: Slot,
    },
    /// More OSD areas than the block has.
    OsdAreas(u32),
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size { width, height } => {
                write!(f, "size {width}x{height} is not one IEP2 takes")
            }
            Self::Stride { slot, stride } => write!(f, "stride {stride} does not fit {slot:?}"),
            Self::OutOfBuffer { slot, end, len } => {
                write!(f, "{slot:?} ends at {end}, past its {len}-byte dma-buf")
            }
            Self::Layout { slot } => write!(f, "{slot:?} has the wrong chroma layout"),
            Self::OsdAreas(n) => write!(f, "{n} OSD areas, the block has {MAX_OSD_AREAS}"),
        }
    }
}

impl std::error::Error for JobError {}

/// The data a message of [`Job::messages`] points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data {
    /// [`Job::params`], `size_of::<Params>()` bytes.
    Params,
    /// [`Job::offsets`].
    Offsets,
    /// The caller's [`Results`], `size_of::<Results>()` bytes, written by the kernel.
    Results,
    /// Nothing; the pointer stays zero.
    None,
}

/// A checked job, holding the descriptors it names until it is dropped.
#[derive(Clone, Debug)]
pub struct Job<'a> {
    params: Params,
    offsets: [OffsetElem; ADDRESS_WORDS],
    count: usize,
    fds: PhantomData<BorrowedFd<'a>>,
}

impl<'a> Job<'a> {
    /// A job deinterlacing `input` into `output` at `width` by `height`, with `md` and `mv` as the
    /// block's scratch, or why not (see the module documentation for every check). Allocates
    /// nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: u32,
        height: u32,
        input: &Input<'a>,
        output: &Output<'a>,
        md: At<'a>,
        mv: At<'a>,
        mode: Mode,
        tuning: &Tuning,
    ) -> Result<Self, JobError> {
        if width == 0
            || height == 0
            || width > MAX_WIDTH
            || height > MAX_HEIGHT
            || !width.is_multiple_of(TILE_WIDTH)
            || !height.is_multiple_of(TILE_HEIGHT)
        {
            return Err(JobError::Size { width, height });
        }
        if tuning.osd_area_num > MAX_OSD_AREAS {
            return Err(JobError::OsdAreas(tuning.osd_area_num));
        }
        if matches!(output.swap, Swap::Planar) {
            return Err(JobError::Layout { slot: Slot::TopY });
        }
        let mut job = Self {
            params: Params::default(),
            offsets: [OffsetElem::default(); ADDRESS_WORDS],
            count: 0,
            fds: PhantomData,
        };

        let in_chroma_line = match input.swap {
            Swap::Planar => width / 2,
            Swap::SemiUv | Swap::SemiVu => width,
        };
        check_stride(Slot::CurY, input.luma_stride, width)?;
        check_stride(Slot::CurC, input.chroma_stride, in_chroma_line)?;
        check_stride(Slot::TopY, output.stride, width)?;
        let in_chroma = input.sampling.chroma_lines(height);
        let out_chroma = output.sampling.chroma_lines(height);

        for (frame, [y, c, cr]) in [
            (&input.cur, [Slot::CurY, Slot::CurC, Slot::CurCr]),
            (&input.next, [Slot::NextY, Slot::NextC, Slot::NextCr]),
            (&input.prev, [Slot::PrevY, Slot::PrevC, Slot::PrevCr]),
        ] {
            job.place(y, frame.luma, input.luma_stride, height)?;
            match (input.swap, frame.chroma) {
                (Swap::Planar, Chroma::Planar { cb, cr: v }) => {
                    job.place(c, cb, input.chroma_stride, in_chroma)?;
                    job.place(cr, v, input.chroma_stride, in_chroma)?;
                }
                (Swap::SemiUv | Swap::SemiVu, Chroma::Semi(at)) => {
                    job.place(c, at, input.chroma_stride, in_chroma)?;
                }
                _ => return Err(JobError::Layout { slot: y }),
            }
        }
        for (frame, [y, c]) in [
            (&output.top, [Slot::TopY, Slot::TopC]),
            (&output.bottom, [Slot::BottomY, Slot::BottomC]),
        ] {
            let Chroma::Semi(at) = frame.chroma else {
                return Err(JobError::Layout { slot: y });
            };
            job.place(y, frame.luma, output.stride, height)?;
            job.place(c, at, output.stride, out_chroma)?;
        }
        let scratch = scratch_len(width, height);
        job.place_len(Slot::Md, md, scratch)?;
        job.place_len(Slot::Mv, mv, scratch)?;

        let p = &mut job.params;
        p.src_fmt = input.sampling as u32;
        p.src_yuv_swap = input.swap as u32;
        p.dst_fmt = output.sampling as u32;
        p.dst_yuv_swap = output.swap as u32;
        p.tile_cols = width / TILE_WIDTH;
        p.tile_rows = height / TILE_HEIGHT;
        p.src_y_stride = input.luma_stride / STRIDE_ALIGN;
        p.src_uv_stride = input.chroma_stride / STRIDE_ALIGN;
        p.dst_y_stride = output.stride / STRIDE_ALIGN;
        p.dil_mode = mode.dil as u32;
        p.dil_out_mode = 0;
        p.dil_field_order = mode.field_order as u32;
        p.pd_mode = mode.pd as u32;
        tuning.apply(p);
        Ok(job)
    }

    /// Places `at` in `slot`, as `lines` lines `stride` apart.
    fn place(&mut self, slot: Slot, at: At<'a>, stride: u32, lines: u32) -> Result<(), JobError> {
        self.place_len(slot, at, u64::from(stride) * u64::from(lines))
    }

    /// Places `at` in `slot`, as `bytes` bytes.
    fn place_len(&mut self, slot: Slot, at: At<'a>, bytes: u64) -> Result<(), JobError> {
        let len = at.buffer.len();
        let end = at.offset.checked_add(bytes);
        let offset = u32::try_from(at.offset);
        let (Some(end), Ok(offset)) = (end, offset) else {
            return Err(JobError::OutOfBuffer {
                slot,
                end: end.unwrap_or(u64::MAX),
                len,
            });
        };
        if end > len {
            return Err(JobError::OutOfBuffer { slot, end, len });
        }
        let word = slot.word();
        let Some(index) = ADDRESS_REGISTERS[word] else {
            return Err(JobError::Layout { slot });
        };
        set_word(&mut self.params, word, at.buffer.word());
        self.offsets[self.count] = OffsetElem { index, offset };
        self.count += 1;
        Ok(())
    }

    /// The parameters, descriptors in the address words.
    #[must_use]
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// The offsets, one per placed plane, keyed by its register.
    #[must_use]
    pub fn offsets(&self) -> &[OffsetElem] {
        &self.offsets[..self.count]
    }

    /// The call that runs this job: the parameters, the offsets, where the results go, and the
    /// wait. Every `data_ptr` is zero; the code that issues the call points each at its [`Data`].
    #[must_use]
    pub fn messages(&self) -> [(Msg, Data); 4] {
        let msg = |cmd, flags, size: usize| Msg {
            cmd,
            flags: FLAG_MULTI_MSG | flags,
            // The structures are a few hundred bytes and the list at most 17 entries.
            size: u32::try_from(size).unwrap_or(u32::MAX),
            offset: 0,
            data_ptr: 0,
        };
        [
            (
                msg(CMD_SET_REG_WRITE, FLAG_REG_NO_OFFSET, size_of::<Params>()),
                Data::Params,
            ),
            (
                msg(
                    CMD_SET_REG_ADDR_OFFSET,
                    FLAG_REG_NO_OFFSET,
                    self.count * size_of::<OffsetElem>(),
                ),
                Data::Offsets,
            ),
            (
                msg(CMD_SET_REG_READ, FLAG_REG_NO_OFFSET, size_of::<Results>()),
                Data::Results,
            ),
            (
                msg(CMD_POLL_HW_FINISH, FLAG_REG_NO_OFFSET | FLAG_LAST_MSG, 0),
                Data::None,
            ),
        ]
    }
}

fn check_stride(slot: Slot, stride: u32, line: u32) -> Result<(), JobError> {
    if stride < line || stride > MAX_STRIDE || !stride.is_multiple_of(STRIDE_ALIGN) {
        return Err(JobError::Stride { slot, stride });
    }
    Ok(())
}

fn set_word(params: &mut Params, word: usize, value: u32) {
    let addr: &mut Addr = match word / 3 {
        0 => &mut params.src[0],
        1 => &mut params.src[1],
        2 => &mut params.src[2],
        3 => &mut params.dst[0],
        4 => &mut params.dst[1],
        _ => {
            if word == 15 {
                params.mv_addr = value;
            } else {
                params.md_addr = value;
            }
            return;
        }
    };
    match word % 3 {
        0 => addr.y = value,
        1 => addr.cbcr = value,
        _ => addr.cr = value,
    }
}
