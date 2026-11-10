//! What a caller submits: a source (bitstream to decode, or a frame to encode) and a destination
//! (where the block writes a frame or bitstream), each one dma-buf per memory plane. The driver
//! carries the bytes and never reads them; it checks only what the kernel would turn into memory
//! the block reaches.
//!
//! # Where a caller's numbers become memory
//!
//! A queued plane is a dma-buf descriptor, a length, a payload size (`bytesused`) and a data
//! offset. The kernel's videobuf2 core checks the length against the dma-buf and the queue's
//! plane size, and the `mvx` driver checks that the payload plus the offset fits the plane's
//! pages (`mvx_buffer_filled_set`, in 32-bit arithmetic). The block reaches memory only through
//! its MMU, with a page table per session holding the pages of that session's queued buffers
//! (`mvx_mmu.c`, `mvx_buffer_map`). Every number is still checked here first, against the size the
//! claim measured when it took the buffer:
//!
//! - **Descriptor.** Each plane names a buffer the claim holds ([`BufferId`] with its generation),
//!   so the descriptor is one the claim keeps open, never a number from the caller.
//! - **Length.** The buffer's real size, measured once when the claim took it, and at most
//!   `u32::MAX` (so it fits the field without truncation). It is at least the plane size the
//!   kernel answered for the queue's format, so the block's write of a whole plane stays inside.
//! - **Payload.** A source plane's `bytes` are 1 to the buffer's size. Zero is refused: the core
//!   turns a zero payload on a source into the whole length. A destination's payload is 0.
//! - **Data offset.** Always 0. The `mvx` driver reads `bytesused` as the bytes after the offset
//!   (it checks `bytesused + data_offset` against the plane), unlike V4L2, so an offset would move
//!   the end of what the block reads past the payload; 0 keeps the two readings the same.
//! - **Planes.** Exactly the queue's plane count, each a different buffer, none queued already
//!   (the block would read and write one buffer for two jobs at once).
//! - **Index.** The V4L2 buffer index is the lowest the session does not hold, below the count
//!   the kernel granted; never the caller's. On the source queue the kernel reads
//!   `bufs[index]` before the core checks it.
//!
//! What the block then writes inside a buffer (how far into a plane a frame of the stream's size
//! reaches) is bounded by the plane size checked above and by the session's page table.

use crate::uapi::MAX_PLANES;

/// A buffer the claim holds, as [`crate::Claim::add_buffer`] named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferId {
    pub(crate) slot: u8,
    pub(crate) generation: u16,
}

/// One plane of a source: a buffer the claim holds and how many bytes of it, from its start, the
/// block reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Payload {
    /// The buffer.
    pub buffer: BufferId,
    /// Bytes from the buffer's start; 1 to its size.
    pub bytes: u32,
}

/// Bitstream to decode or a frame to encode.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    /// The caller's name for it. Carried to its completion, never read.
    pub identity: u64,
    /// One payload per memory plane of the source format.
    pub planes: &'a [Payload],
    /// Goes with the source; the block copies it to what it makes from it.
    pub timestamp: std::time::Duration,
}

/// Where the block writes a frame (decoding) or bitstream (encoding).
#[derive(Clone, Copy, Debug)]
pub struct Destination<'a> {
    /// The caller's name for it. Carried to its completion, never read.
    pub identity: u64,
    /// One buffer per memory plane of the destination format.
    pub planes: &'a [BufferId],
}

/// Why a source or destination was refused. The device was not touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// Not one plane per memory plane of the queue's format.
    Planes {
        /// The format's planes.
        want: usize,
        /// The job's.
        got: usize,
    },
    /// A plane names a buffer the claim does not hold.
    UnknownBuffer {
        /// The plane.
        plane: usize,
    },
    /// A plane's buffer is queued already, or named twice in the job.
    Busy {
        /// The plane.
        plane: usize,
    },
    /// A plane's buffer is smaller than the queue's plane size.
    TooSmall {
        /// The plane.
        plane: usize,
        /// The buffer's size.
        size: u64,
        /// The plane size.
        need: u32,
    },
    /// A source plane's payload is empty or larger than its buffer.
    Payload {
        /// The plane.
        plane: usize,
        /// The bytes asked for.
        bytes: u32,
        /// The buffer's size.
        size: u64,
    },
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for JobError {}

/// A buffer as a plan sees it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Held {
    pub(crate) slot: u8,
    /// At most `u32::MAX`.
    pub(crate) size: u64,
    /// Queued now.
    pub(crate) busy: bool,
}

/// A checked job: per plane, the claim's buffer slot, the length and the payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) planes: usize,
    pub(crate) slots: [u8; MAX_PLANES],
    pub(crate) lengths: [u32; MAX_PLANES],
    pub(crate) bytes: [u32; MAX_PLANES],
}

/// Checks one job's planes: `buffers[i]` with payload `bytes[i]` (`None` for a destination)
/// against the queue's plane sizes `need` and the buffers the claim holds.
pub(crate) fn plan(
    need: &[u32],
    buffers: impl ExactSizeIterator<Item = (BufferId, Option<u32>)>,
    held: impl Fn(BufferId) -> Option<Held>,
) -> Result<Plan, JobError> {
    let got = buffers.len();
    if got != need.len() || got > MAX_PLANES {
        return Err(JobError::Planes {
            want: need.len(),
            got,
        });
    }
    let mut plan = Plan {
        planes: got,
        ..Plan::default()
    };
    for (plane, ((id, bytes), &need)) in buffers.zip(need).enumerate() {
        let b = held(id).ok_or(JobError::UnknownBuffer { plane })?;
        if b.busy || plan.slots[..plane].contains(&b.slot) {
            return Err(JobError::Busy { plane });
        }
        if b.size < u64::from(need) {
            return Err(JobError::TooSmall {
                plane,
                size: b.size,
                need,
            });
        }
        // The claim takes no buffer over `u32::MAX`; a shorter length would still be inside.
        let length = u32::try_from(b.size).unwrap_or(u32::MAX);
        let bytes = match bytes {
            None => 0,
            Some(bytes) if bytes == 0 || u64::from(bytes) > b.size => {
                return Err(JobError::Payload {
                    plane,
                    bytes,
                    size: b.size,
                });
            }
            Some(bytes) => bytes,
        };
        plan.slots[plane] = b.slot;
        plan.lengths[plane] = length;
        plan.bytes[plane] = bytes;
    }
    Ok(plan)
}
