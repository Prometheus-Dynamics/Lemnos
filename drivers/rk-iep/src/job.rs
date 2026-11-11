//! Jobs: three input frames, two output frames and the block's two scratch buffers, each placed in
//! a buffer the claim holds and checked against that buffer's size, made into the parameter
//! structure and the addresses of a [`ParamJob`].
//!
//! # Where a caller's numbers become addresses
//!
//! The vendor driver turns every descriptor word of the parameters into a bus address: the
//! dma-buf the word names, plus an offset. It checks neither the offset nor anything the block
//! then reads or writes from there, and the IOMMU fault handler of the block maps a spare page
//! wherever it faults, so a read past a buffer does not stop the block. Two layers check what
//! decides where the block reaches:
//!
//! - **Descriptors and offsets**, in `lemnos-driver-rk-mpp` ([`ParamJob`]): the driver writes
//!   every descriptor word itself, with a buffer the claim holds, named once, its offset below
//!   the size the claim measured and carried in the offset list keyed by the word's register
//!   (`MPP_FLAGS_REG_NO_OFFSET` set, so no offset rides in a word). Every one of the kernel's
//!   descriptor words is named: a semi-planar frame's Cr word holds its chroma plane, which the
//!   block does not read in that layout. The structure is sent at its exact size and the
//!   results read at theirs.
//! - **Extents**, here, for what the block reaches from each address. A plane of `lines` lines
//!   `stride` apart is checked as `offset + lines * stride` bytes, in `u64` with overflow
//!   refused, against the buffer's size as the kernel reports it. Luma has `height` lines,
//!   chroma `height` (4:2:2) or `height / 2` (4:2:0). A semi-planar chroma line holds `width`
//!   bytes, a planar one `width / 2`.
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
//! - **Modes.** Every frame a mode might read is required, so no descriptor word is left 0. The
//!   one mode the vendor driver says reads past its buffer (`I1O1T`) does not exist here. The
//!   output-mode bit, whose effect on the written extent the source does not give, and the ROI,
//!   are held at zero. `osd_area_num` is at most 8: the kernel indexes its arrays by it.
//!   `lemnos-driver-rk-mpp` checks the same words again before anything is sent.

use std::fmt;
use std::mem::offset_of;

use lemnos_driver_rk_mpp::{BufferId, ParamAddress, ParamJob};

use crate::params::{
    DilMode, FieldOrder, PARAMS_BYTES, Params, PdComp, Sampling, Swap, TUNING_BYTES, TUNING_START,
};
use crate::regs::STRIDE_FIELD_BITS;

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

/// A buffer the claim holds, with the size the claim measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buffer {
    /// The claim's buffer.
    pub id: BufferId,
    /// Its size in bytes, as the kernel gave it to the claim.
    pub len: u64,
}

/// Where a plane starts: a buffer and a byte offset into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    /// The buffer.
    pub buffer: Buffer,
    /// Bytes from its start.
    pub offset: u64,
}

impl At {
    /// The plane at `offset` in `buffer`.
    #[must_use]
    pub fn new(buffer: Buffer, offset: u64) -> Self {
        Self { buffer, offset }
    }
}

/// A frame's chroma.
#[derive(Clone, Copy, Debug)]
pub enum Chroma {
    /// One interleaved plane.
    Semi(At),
    /// Separate Cb and Cr planes.
    Planar {
        /// Cb.
        cb: At,
        /// Cr.
        cr: At,
    },
}

/// One frame's planes.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Luma.
    pub luma: At,
    /// Chroma.
    pub chroma: Chroma,
}

/// The input: three frames of one layout.
#[derive(Clone, Copy, Debug)]
pub struct Input {
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
    pub cur: Frame,
    /// The next frame.
    pub next: Frame,
    /// The previous frame.
    pub prev: Frame,
}

/// The output: two semi-planar frames, the top and bottom results.
#[derive(Clone, Copy, Debug)]
pub struct Output {
    /// Chroma sampling.
    pub sampling: Sampling,
    /// [`Swap::SemiUv`] or [`Swap::SemiVu`].
    pub swap: Swap,
    /// Bytes per line, luma and chroma.
    pub stride: u32,
    /// The top (or only) output.
    pub top: Frame,
    /// The bottom output.
    pub bottom: Frame,
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

/// A place the block reads or writes, by the descriptor word it fills. Its position in
/// [`Slot::ALL`] is its slot number in `codec.block/1` work.
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

/// Descriptor words the kernel translates: one per [`Slot`].
pub const SLOTS: usize = 15;

impl Slot {
    /// Every slot, in slot-number order.
    pub const ALL: [Self; SLOTS] = [
        Self::CurY,
        Self::CurC,
        Self::CurCr,
        Self::NextY,
        Self::NextC,
        Self::NextCr,
        Self::PrevY,
        Self::PrevC,
        Self::PrevCr,
        Self::TopY,
        Self::TopC,
        Self::BottomY,
        Self::BottomC,
        Self::Mv,
        Self::Md,
    ];

    /// The byte offset in [`Params`] of the descriptor word this slot fills.
    #[must_use]
    pub const fn word(self) -> usize {
        let src = offset_of!(Params, src);
        let dst = offset_of!(Params, dst);
        // `struct iep2_addr` is `{y, cbcr, cr}`, 12 bytes.
        match self {
            Self::CurY => src,
            Self::CurC => src + 4,
            Self::CurCr => src + 8,
            Self::NextY => src + 12,
            Self::NextC => src + 16,
            Self::NextCr => src + 20,
            Self::PrevY => src + 24,
            Self::PrevC => src + 28,
            Self::PrevCr => src + 32,
            Self::TopY => dst,
            Self::TopC => dst + 4,
            Self::BottomY => dst + 12,
            Self::BottomC => dst + 16,
            Self::Mv => offset_of!(Params, mv_addr),
            Self::Md => offset_of!(Params, md_addr),
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
    /// A plane runs past its buffer, or its offset is past `u32::MAX`.
    OutOfBuffer {
        /// The plane.
        slot: Slot,
        /// The byte it would end at, or `u64::MAX` if that overflows.
        end: u64,
        /// The buffer's size.
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
                write!(f, "{slot:?} ends at {end}, past its {len}-byte buffer")
            }
            Self::Layout { slot } => write!(f, "{slot:?} has the wrong chroma layout"),
            Self::OsdAreas(n) => write!(f, "{n} OSD areas, the block has {MAX_OSD_AREAS}"),
        }
    }
}

impl std::error::Error for JobError {}

/// A checked job: the parameter structure and the address of every descriptor word.
#[derive(Clone, Debug)]
pub struct Job {
    params: [u8; PARAMS_BYTES],
    addresses: [ParamAddress; SLOTS],
}

impl Job {
    /// A job deinterlacing `input` into `output` at `width` by `height`, with `md` and `mv` as the
    /// block's scratch and `tuning` the caller's settings ([`Tuning::encode`](crate::Tuning)),
    /// or why not (see the module documentation for every check). Allocates nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: u32,
        height: u32,
        input: &Input,
        output: &Output,
        md: At,
        mv: At,
        mode: Mode,
        tuning: &[u8; TUNING_BYTES],
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
        let osd = offset_of!(Params, osd_area_num) - TUNING_START;
        let osd_areas = u32::from_ne_bytes([
            tuning[osd],
            tuning[osd + 1],
            tuning[osd + 2],
            tuning[osd + 3],
        ]);
        if osd_areas > MAX_OSD_AREAS {
            return Err(JobError::OsdAreas(osd_areas));
        }
        if matches!(output.swap, Swap::Planar) {
            return Err(JobError::Layout { slot: Slot::TopY });
        }
        // Every slot is placed below before the job is returned; until then each holds this.
        let nowhere = ParamAddress {
            word: 0,
            buffer: md.buffer.id,
            offset: 0,
        };
        let mut job = Self {
            params: [0; PARAMS_BYTES],
            addresses: [nowhere; SLOTS],
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
                    // Not read in this layout; the kernel translates the word all the same,
                    // so it holds a plane already checked.
                    job.place(cr, at, input.chroma_stride, in_chroma)?;
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

        job.params[TUNING_START..TUNING_START + TUNING_BYTES].copy_from_slice(tuning);
        for (at, value) in [
            (offset_of!(Params, src_fmt), input.sampling as u32),
            (offset_of!(Params, src_yuv_swap), input.swap as u32),
            (offset_of!(Params, dst_fmt), output.sampling as u32),
            (offset_of!(Params, dst_yuv_swap), output.swap as u32),
            (offset_of!(Params, tile_cols), width / TILE_WIDTH),
            (offset_of!(Params, tile_rows), height / TILE_HEIGHT),
            (
                offset_of!(Params, src_y_stride),
                input.luma_stride / STRIDE_ALIGN,
            ),
            (
                offset_of!(Params, src_uv_stride),
                input.chroma_stride / STRIDE_ALIGN,
            ),
            (
                offset_of!(Params, dst_y_stride),
                output.stride / STRIDE_ALIGN,
            ),
            (offset_of!(Params, dil_mode), mode.dil as u32),
            (offset_of!(Params, dil_out_mode), 0),
            (offset_of!(Params, dil_field_order), mode.field_order as u32),
            (offset_of!(Params, pd_mode), mode.pd as u32),
        ] {
            job.params[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        }
        Ok(job)
    }

    /// Places `at` in `slot`, as `lines` lines `stride` apart.
    fn place(&mut self, slot: Slot, at: At, stride: u32, lines: u32) -> Result<(), JobError> {
        self.place_len(slot, at, u64::from(stride) * u64::from(lines))
    }

    /// Places `at` in `slot`, as `bytes` bytes.
    fn place_len(&mut self, slot: Slot, at: At, bytes: u64) -> Result<(), JobError> {
        let len = at.buffer.len;
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
        let index = Slot::ALL.iter().position(|&s| s == slot).unwrap_or(0);
        self.addresses[index] = ParamAddress {
            // Every word is below the structure's 656 bytes.
            word: u16::try_from(slot.word()).unwrap_or(u16::MAX),
            buffer: at.buffer.id,
            offset,
        };
        Ok(())
    }

    /// The parameter structure, in the kernel's layout and the machine's byte order. Its
    /// descriptor words are 0: the claim writes them.
    #[must_use]
    pub fn params(&self) -> &[u8; PARAMS_BYTES] {
        &self.params
    }

    /// The address of every descriptor word, in [`Slot::ALL`] order.
    #[must_use]
    pub fn addresses(&self) -> &[ParamAddress; SLOTS] {
        &self.addresses
    }

    /// The job for the claim, named `identity`.
    #[must_use]
    pub fn param_job(&self, identity: u64) -> ParamJob<'_> {
        ParamJob {
            identity,
            params: &self.params,
            addresses: &self.addresses,
        }
    }
}

fn check_stride(slot: Slot, stride: u32, line: u32) -> Result<(), JobError> {
    if stride < line || stride > MAX_STRIDE || !stride.is_multiple_of(STRIDE_ALIGN) {
        return Err(JobError::Stride { slot, stride });
    }
    Ok(())
}
