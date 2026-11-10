//! A job: a source frame copied into a destination frame, turned and mirrored on the way, each
//! plane at a byte offset into a buffer the caller holds. Every place a number of the caller's
//! becomes memory the engine reads or writes is proved inside that buffer here, before anything
//! is built.
//!
//! # Where a caller's numbers become memory
//!
//! The engine walks one descriptor per plane. Each names a source address, a destination
//! address, a width and height in elements, an element size, and a stride on each side. It reads
//! `height` rows of `width` elements from the source, `src_stride` bytes apart, and writes them
//! turned: for a quarter or three-quarter turn, `width` rows of `height` elements,
//! `dst_stride` bytes apart; otherwise `height` rows of `width` elements. It has no MMU in front
//! of it, and the kernel driver checks only that each address lies in DDR (with
//! `pitch * height` bytes after it, the destination's counted the source's way). So here, for
//! every plane, in `u64`, with no step that can overflow:
//!
//! - **Size.** Width and height are 1 to 65535 (the descriptor's 16-bit fields), the destination's
//!   the source's turned. A plane's size is the frame's divided by the format's subsampling,
//!   rounded up, as the kernel computes it (`drm_format_info_plane_width` and `_height`).
//! - **Stride.** 1 to 65535 bytes (the descriptor's 16-bit fields), and at least a row: the
//!   plane's row in bytes on that side (a turned row on the destination), so rows never overlap.
//! - **Extent.** `offset + stride * (rows - 1) + row` is at most the buffer's size, measured when
//!   the claim took it. That is the last byte the engine touches on that side, turned or not.
//! - **Buffers.** Every plane names a buffer the claim holds ([`BufferId`] with its generation).
//!   No destination plane overlaps a source plane or another destination plane in the same
//!   buffer: the engine would read what it already wrote.
//! - **Format.** One of the ten the driver takes, the same on both sides, with exactly its plane
//!   count.
//! - **Rotation.** Exactly one turn and any mirroring, as the driver allows.
//!
//! Turning a checked job into descriptors ([`Plan::descriptors`]) needs each buffer's bus
//! address; it checks that every plane's whole extent ends at or below [`DDR_END`], so each
//! address fits the engine's 32 bits and stays in the memory the kernel allows.

use crate::uapi::{
    DDR_END, DESCRIPTOR_BYTES, Descriptor, MAX_DESCRIPTORS, MAX_PLANES, drm, fourcc, pixel, turn,
};

/// The largest width, height or stride a descriptor holds.
pub const MAX_FIELD: u32 = u16::MAX as u32;

/// A buffer the claim holds, as [`crate::Claim::add_buffer`] named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferId {
    pub(crate) slot: u8,
    pub(crate) generation: u16,
}

/// The formats the driver takes (`k230_gdma_build_format_desc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// `DRM_FORMAT_ARGB1555`.
    Argb1555,
    /// `DRM_FORMAT_ARGB4444`.
    Argb4444,
    /// `DRM_FORMAT_RGB565`.
    Rgb565,
    /// `DRM_FORMAT_RGB888`.
    Rgb888,
    /// `DRM_FORMAT_BGR888`.
    Bgr888,
    /// `DRM_FORMAT_ARGB8888`.
    Argb8888,
    /// `DRM_FORMAT_NV12`: luma, then interleaved chroma at half width and half height.
    Nv12,
    /// `DRM_FORMAT_NV21`.
    Nv21,
    /// `DRM_FORMAT_NV16`: luma, then interleaved chroma at half width and full height.
    Nv16,
    /// `DRM_FORMAT_NV61`.
    Nv61,
}

impl Format {
    /// Every format.
    pub const ALL: [Self; 10] = [
        Self::Argb1555,
        Self::Argb4444,
        Self::Rgb565,
        Self::Rgb888,
        Self::Bgr888,
        Self::Argb8888,
        Self::Nv12,
        Self::Nv21,
        Self::Nv16,
        Self::Nv61,
    ];

    /// The DRM four-character code.
    #[must_use]
    pub const fn fourcc(self) -> u32 {
        match self {
            Self::Argb1555 => fourcc::ARGB1555,
            Self::Argb4444 => fourcc::ARGB4444,
            Self::Rgb565 => fourcc::RGB565,
            Self::Rgb888 => fourcc::RGB888,
            Self::Bgr888 => fourcc::BGR888,
            Self::Argb8888 => fourcc::ARGB8888,
            Self::Nv12 => fourcc::NV12,
            Self::Nv21 => fourcc::NV21,
            Self::Nv16 => fourcc::NV16,
            Self::Nv61 => fourcc::NV61,
        }
    }

    /// The format with this code, if the driver takes it.
    #[must_use]
    pub fn from_fourcc(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.fourcc() == code)
    }

    /// Its planes.
    #[must_use]
    pub const fn planes(self) -> usize {
        match self {
            Self::Nv12 | Self::Nv21 | Self::Nv16 | Self::Nv61 => 2,
            _ => 1,
        }
    }

    /// Bytes of one element of `plane`, and the descriptor's code for it.
    #[must_use]
    pub const fn element(self, plane: usize) -> (u32, u32) {
        match self {
            Self::Argb1555 | Self::Argb4444 | Self::Rgb565 => (2, pixel::BITS_16),
            Self::Rgb888 | Self::Bgr888 => (3, pixel::BITS_24),
            Self::Argb8888 => (4, pixel::BITS_32),
            Self::Nv12 | Self::Nv21 | Self::Nv16 | Self::Nv61 if plane == 0 => (1, pixel::BITS_8),
            Self::Nv12 | Self::Nv21 | Self::Nv16 | Self::Nv61 => (2, pixel::BITS_16),
        }
    }

    /// How far `plane` is subsampled across and down.
    #[must_use]
    pub const fn subsampling(self, plane: usize) -> (u32, u32) {
        match self {
            Self::Nv12 | Self::Nv21 if plane > 0 => (2, 2),
            Self::Nv16 | Self::Nv61 if plane > 0 => (2, 1),
            _ => (1, 1),
        }
    }
}

/// Clockwise quarter turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Turn {
    /// None.
    #[default]
    R0,
    /// 90 degrees.
    R90,
    /// 180 degrees.
    R180,
    /// 270 degrees.
    R270,
}

/// A turn and any mirroring, as the driver takes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rotation {
    /// The turn.
    pub turn: Turn,
    /// Mirror across (`DRM_MODE_REFLECT_X`).
    pub reflect_x: bool,
    /// Mirror down (`DRM_MODE_REFLECT_Y`).
    pub reflect_y: bool,
}

impl Rotation {
    /// From DRM rotation bits: exactly one `ROTATE_*` bit, and `REFLECT_*` bits, nothing else
    /// (`k230_gdma_rotation_valid`).
    #[must_use]
    pub fn from_drm(bits: u32) -> Option<Self> {
        if bits & !(drm::ROTATE_MASK | drm::REFLECT_MASK) != 0 {
            return None;
        }
        let turn = match bits & drm::ROTATE_MASK {
            drm::ROTATE_0 => Turn::R0,
            drm::ROTATE_90 => Turn::R90,
            drm::ROTATE_180 => Turn::R180,
            drm::ROTATE_270 => Turn::R270,
            _ => return None,
        };
        Some(Self {
            turn,
            reflect_x: bits & drm::REFLECT_X != 0,
            reflect_y: bits & drm::REFLECT_Y != 0,
        })
    }

    /// The DRM rotation bits.
    #[must_use]
    pub fn drm(self) -> u32 {
        let turn = match self.turn {
            Turn::R0 => drm::ROTATE_0,
            Turn::R90 => drm::ROTATE_90,
            Turn::R180 => drm::ROTATE_180,
            Turn::R270 => drm::ROTATE_270,
        };
        turn | if self.reflect_x { drm::REFLECT_X } else { 0 }
            | if self.reflect_y { drm::REFLECT_Y } else { 0 }
    }

    /// The descriptor's `rotation_mode`.
    #[must_use]
    pub fn code(self) -> u32 {
        match self.turn {
            Turn::R0 => turn::R0,
            Turn::R90 => turn::R90,
            Turn::R180 => turn::R180,
            Turn::R270 => turn::R270,
        }
    }

    /// Whether it swaps width and height.
    #[must_use]
    pub fn swaps(self) -> bool {
        matches!(self.turn, Turn::R90 | Turn::R270)
    }
}

/// Where one plane of a frame is: a buffer the claim holds, the byte its first row starts at, and
/// the bytes from one row to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    /// The buffer.
    pub buffer: BufferId,
    /// Bytes from the buffer's start to the plane's first row.
    pub offset: u64,
    /// Bytes from one row to the next.
    pub pitch: u32,
}

/// A frame: its format, its size in pixels, and its planes.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// The format.
    pub format: Format,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// One per plane of the format.
    pub planes: &'a [Plane],
}

/// One copy: `source` into `destination`, turned and mirrored by `rotation`.
#[derive(Clone, Copy, Debug)]
pub struct Job<'a> {
    /// The caller's name for the job. Carried to its completion, never read.
    pub identity: u64,
    /// What the engine reads.
    pub source: Frame<'a>,
    /// What the engine writes: the source's format, and its size turned.
    pub destination: Frame<'a>,
    /// The turn and mirroring.
    pub rotation: Rotation,
}

/// Why a job was refused. Nothing was built or sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// The destination's format is not the source's.
    Format,
    /// A width or height is 0 or above 65535.
    Size {
        /// Whether it is the source's.
        source: bool,
    },
    /// The destination's size is not the source's turned.
    Turned,
    /// Not one plane per plane of the format.
    Planes {
        /// Whether it is the source's.
        source: bool,
    },
    /// A plane names a buffer the claim does not hold.
    UnknownBuffer {
        /// Whether it is the source's.
        source: bool,
        /// The plane.
        plane: usize,
    },
    /// A stride is 0, above 65535, or shorter than a row.
    Pitch {
        /// Whether it is the source's.
        source: bool,
        /// The plane.
        plane: usize,
        /// The stride asked for.
        pitch: u32,
        /// The row, in bytes.
        row: u64,
    },
    /// A plane runs past its buffer's end.
    Outside {
        /// Whether it is the source's.
        source: bool,
        /// The plane.
        plane: usize,
        /// The buffer's size.
        size: u64,
    },
    /// A destination plane overlaps a source plane or another destination plane.
    Overlap {
        /// The destination plane.
        plane: usize,
    },
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for JobError {}

/// One side of one plane, checked: bytes `start..end` of `buffer`, rows `pitch` apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The buffer.
    pub buffer: BufferId,
    /// The plane's first byte.
    pub start: u64,
    /// One past the last byte the engine touches; at most the buffer's size.
    pub end: u64,
    /// Bytes from one row to the next; 1 to 65535.
    pub pitch: u16,
}

impl Span {
    fn overlaps(&self, other: &Span) -> bool {
        self.buffer == other.buffer && self.start < other.end && other.start < self.end
    }
}

const NO_SPAN: Span = Span {
    buffer: BufferId {
        slot: 0,
        generation: 0,
    },
    start: 0,
    end: 0,
    pitch: 0,
};

/// A checked job: what a session is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    identity: u64,
    format: Format,
    rotation: Rotation,
    planes: usize,
    /// Per plane: the source plane's width and height in elements.
    size: [(u16, u16); MAX_PLANES],
    source: [Span; MAX_PLANES],
    destination: [Span; MAX_PLANES],
}

/// Why descriptors could not be made from a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressError {
    /// No bus address for a plane's buffer.
    Unknown,
    /// A plane would end past [`DDR_END`].
    Outside {
        /// Whether it is the source's.
        source: bool,
        /// The plane.
        plane: usize,
    },
    /// The descriptors themselves would end past [`DDR_END`].
    Chain,
}

impl Plan {
    /// The job's identity.
    #[must_use]
    pub fn identity(&self) -> u64 {
        self.identity
    }

    /// The format.
    #[must_use]
    pub fn format(&self) -> Format {
        self.format
    }

    /// The rotation.
    #[must_use]
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// The planes.
    #[must_use]
    pub fn planes(&self) -> usize {
        self.planes
    }

    /// Plane `i` of the source.
    #[must_use]
    pub fn source(&self, i: usize) -> Option<Span> {
        self.source[..self.planes].get(i).copied()
    }

    /// Plane `i` of the destination.
    #[must_use]
    pub fn destination(&self, i: usize) -> Option<Span> {
        self.destination[..self.planes].get(i).copied()
    }

    /// The source plane `i`'s width and height in elements.
    #[must_use]
    pub fn size(&self, i: usize) -> Option<(u16, u16)> {
        self.size[..self.planes].get(i).copied()
    }

    /// The buffers the job uses, as the claim's slots, one bit each.
    pub(crate) fn slots(&self) -> u64 {
        let spans = self.source[..self.planes]
            .iter()
            .chain(&self.destination[..self.planes]);
        spans.fold(0, |m, s| m | 1 << s.buffer.slot)
    }

    /// The descriptor chain for this job: `base` answers each buffer's bus address (the address
    /// of its first byte, for a buffer the engine sees in one piece), and `chain` is where the
    /// descriptors themselves will sit. Every plane's whole extent, from its address, must end at
    /// or below [`DDR_END`].
    pub fn descriptors(
        &self,
        base: impl Fn(BufferId) -> Option<u64>,
        chain: u64,
    ) -> Result<([Descriptor; MAX_DESCRIPTORS], usize), AddressError> {
        let n = self.planes;
        let chain_end = chain.checked_add((n * DESCRIPTOR_BYTES) as u64);
        if !chain_end.is_some_and(|end| end <= DDR_END) {
            return Err(AddressError::Chain);
        }
        let address = |span: &Span, source: bool, plane: usize| {
            let base = base(span.buffer).ok_or(AddressError::Unknown)?;
            let end = base.checked_add(span.end);
            if !end.is_some_and(|end| end <= DDR_END) {
                return Err(AddressError::Outside { source, plane });
            }
            // `base + start <= base + end <= DDR_END`, below 2^32.
            Ok((base + span.start) as u32)
        };
        let mut out = [Descriptor::default(); MAX_DESCRIPTORS];
        for (i, d) in out.iter_mut().enumerate().take(n) {
            let (width, height) = self.size[i];
            let last = i + 1 == n;
            *d = Descriptor {
                rotation_mode: self.rotation.code(),
                x_mirror: self.rotation.reflect_x,
                y_mirror: self.rotation.reflect_y,
                pixel_width: self.format.element(i).1,
                channel: 0,
                pause: false,
                node_intr: last,
                src_addr: address(&self.source[i], true, i)?,
                width,
                height,
                src_stride: self.source[i].pitch,
                dst_stride: self.destination[i].pitch,
                dst_addr: address(&self.destination[i], false, i)?,
                // Below `chain_end <= DDR_END`.
                next: if last {
                    0
                } else {
                    (chain + ((i + 1) * DESCRIPTOR_BYTES) as u64) as u32
                },
            };
        }
        Ok((out, n))
    }
}

/// One side of one plane: `rows` rows of `row` bytes, `pitch` apart, from `offset` in a buffer of
/// `size` bytes.
fn span(
    plane: &Plane,
    size: u64,
    rows: u64,
    row: u64,
    source: bool,
    index: usize,
) -> Result<Span, JobError> {
    let pitch_error = JobError::Pitch {
        source,
        plane: index,
        pitch: plane.pitch,
        row,
    };
    if plane.pitch == 0 || plane.pitch > MAX_FIELD || u64::from(plane.pitch) < row {
        return Err(pitch_error);
    }
    // `rows` and `pitch` are at most 65535, `row` at most 4 * 65535: no product overflows.
    let extent = u64::from(plane.pitch) * (rows - 1) + row;
    let outside = JobError::Outside {
        source,
        plane: index,
        size,
    };
    let end = plane.offset.checked_add(extent).ok_or(outside)?;
    if end > size {
        return Err(outside);
    }
    Ok(Span {
        buffer: plane.buffer,
        start: plane.offset,
        end,
        // At most `MAX_FIELD`.
        pitch: plane.pitch as u16,
    })
}

/// Checks `job` against the buffers the claim holds: `held` answers a buffer's size.
pub(crate) fn plan(
    job: &Job<'_>,
    held: impl Fn(BufferId) -> Option<u64>,
) -> Result<Plan, JobError> {
    let (src, dst) = (&job.source, &job.destination);
    let format = src.format;
    if dst.format != format {
        return Err(JobError::Format);
    }
    for (source, frame) in [(true, src), (false, dst)] {
        if frame.width == 0
            || frame.width > MAX_FIELD
            || frame.height == 0
            || frame.height > MAX_FIELD
        {
            return Err(JobError::Size { source });
        }
        if frame.planes.len() != format.planes() {
            return Err(JobError::Planes { source });
        }
    }
    let swaps = job.rotation.swaps();
    let turned = if swaps {
        (src.height, src.width)
    } else {
        (src.width, src.height)
    };
    if (dst.width, dst.height) != turned {
        return Err(JobError::Turned);
    }

    let mut plan = Plan {
        identity: job.identity,
        format,
        rotation: job.rotation,
        planes: format.planes(),
        size: [(0, 0); MAX_PLANES],
        source: [NO_SPAN; MAX_PLANES],
        destination: [NO_SPAN; MAX_PLANES],
    };
    for i in 0..plan.planes {
        let (bytes, _) = format.element(i);
        let (across, down) = format.subsampling(i);
        // The source plane, in elements; at most 65535 each.
        let width = src.width.div_ceil(across);
        let height = src.height.div_ceil(down);
        plan.size[i] = (width as u16, height as u16);
        let (width, height, bytes) = (u64::from(width), u64::from(height), u64::from(bytes));

        let size = |source: bool, plane: &Plane| {
            held(plane.buffer).ok_or(JobError::UnknownBuffer { source, plane: i })
        };
        let s = &src.planes[i];
        plan.source[i] = span(s, size(true, s)?, height, width * bytes, true, i)?;
        let d = &dst.planes[i];
        let (rows, row) = if swaps {
            (width, height * bytes)
        } else {
            (height, width * bytes)
        };
        plan.destination[i] = span(d, size(false, d)?, rows, row, false, i)?;
    }
    for i in 0..plan.planes {
        let d = &plan.destination[i];
        let others = plan.source[..plan.planes]
            .iter()
            .chain(plan.destination[..plan.planes].iter().take(i));
        if others.clone().any(|o| d.overlaps(o)) {
            return Err(JobError::Overlap { plane: i });
        }
    }
    Ok(plan)
}
