//! The rp1-csi2 receiver's kernel node, `/dev/lemnos-rp1-csi2-<device>` (`kernel/lemnos_rp1_csi2.h`,
//! `kernel/lemnos.h`): its uAPI and safe wrappers over its ioctls, nothing else.
//!
//! The node never takes a memory address. The only addresses the receiver writes are computed in
//! the kernel from the buffers of the stream [`Node::start`] names, each of which is a dma-buf the
//! module pinned when [`Stream::set_buffer`] gave it, so nothing here can name memory. Every
//! request is one ioctl per function, and every one is safe to call; the rest reads and writes
//! the stream's ring, which the module treats as untrusted.
//!
//! Provenance: ported from `crates/lemnos-linux-sys/src/lemnos_rp1_csi2.rs` on the dev branch, for
//! the frame stream.

#![cfg(target_os = "linux")]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use lemnos_linux_sys::stream::ring::{Layout, Overrun, Ring};
use lemnos_linux_sys::{ior, iow, iowr};

/// `LEMNOS_RP1_CSI2_ABI`: the node's interface revision this crate speaks.
pub const ABI: u32 = 1;
/// `LEMNOS_RP1_CSI2_INFO_WEDGED`: a stop couldn't be confirmed, so nothing more runs until the
/// block is bound again.
pub const INFO_WEDGED: u32 = 1;
/// `LEMNOS_RP1_CSI2_UNPACK16`: each pixel unpacked to 16 bits, most significant bit aligned.
pub const UNPACK16: u32 = 1;
/// `LEMNOS_RING_STATE_OPEN`: the frame stream is running.
pub const STATE_OPEN: u32 = 0;
/// `LEMNOS_RP1_CSI2_EMBEDDED_DT`: the CSI-2 data type of embedded data (0x12).
pub const EMBEDDED_DT: u32 = 0x12;
/// `LEMNOS_RP1_CSI2_EMBEDDED_MAX`: the longest embedded line the channel takes, in bytes.
pub const EMBEDDED_MAX: u32 = 65_520;
/// `LEMNOS_STREAM_MAX_OWN_BUFFERS`: the most buffers a stream of dma-bufs may have.
pub const MAX_BUFFERS: u32 = 64;
/// `LEMNOS_STREAM_CREATE_BUFFERS`: no pool, each buffer a dma-buf of its own.
const CREATE_BUFFERS: u32 = 1;

/// `struct lemnos_rp1_csi2_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `LEMNOS_RP1_CSI2_ABI` of the loaded module.
    pub abi: u32,
    /// The CSI-2 host's version register.
    pub host_version: u32,
    /// Data lanes wired.
    pub max_lanes: u32,
    /// `LEMNOS_RP1_CSI2_INFO_*`.
    pub flags: u32,
    /// Nonzero between START and STOP.
    pub streaming: u32,
    /// Zero.
    pub reserved: u32,
    /// Frame starts since START.
    pub frames: u64,
    /// Frames completed.
    pub written: u64,
    /// Frame starts with no buffer armed.
    pub dropped: u64,
    /// Frames the block flagged as bad.
    pub errors: u64,
}

/// `LEMNOS_RP1_CSI2_DMA_IDLE`: the part's DMA holds no buffer.
pub const DMA_IDLE: u32 = 0;
/// `LEMNOS_RP1_CSI2_DMA_STREAMING`: the part is streaming into its streams' buffers.
pub const DMA_STREAMING: u32 = 1;
/// `LEMNOS_RP1_CSI2_DMA_WEDGED`: a stop wasn't confirmed; its buffers stay pinned, START is refused.
pub const DMA_WEDGED: u32 = 2;
/// `LEMNOS_RP1_CSI2_STOP_NONE`: the part hasn't stopped since it was made.
pub const STOP_NONE: u32 = 0;
/// `LEMNOS_RP1_CSI2_STOP_ACKED`: the frame end after FORCE came within the bound.
pub const STOP_ACKED: u32 = 1;
/// `LEMNOS_RP1_CSI2_STOP_IDLE`: between frames at FORCE, and no frame started within the bound.
pub const STOP_IDLE: u32 = 2;
/// `LEMNOS_RP1_CSI2_STOP_UNCONFIRMED`: a frame was in flight and no frame end came: wedged.
pub const STOP_UNCONFIRMED: u32 = 3;
/// `LEMNOS_RP1_CSI2_STOP_LATE`: a wedge whose frame end came later: buffers released.
pub const STOP_LATE: u32 = 4;

/// `struct lemnos_rp1_csi2_dma_part`: one part's DMA and its last stop.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DmaPart {
    /// `DMA_*`.
    pub state: u32,
    /// `STOP_*`.
    pub stop: u32,
    /// How long the last stop took or waited, in us (`STOP_LATE`: from the stop to the late end).
    pub stop_us: u32,
    /// The last stop's bound, in us.
    pub bound_us: u32,
    /// Dma-bufs the part's DMA can write: its streams', or the ones a wedge keeps.
    pub pinned: u32,
    /// Zero.
    pub reserved: u32,
    /// Their bytes.
    pub pinned_bytes: u64,
}

/// `struct lemnos_rp1_csi2_dma`: the receiver's part and the front end's.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dma {
    /// The receiver's DMA.
    pub csi2: DmaPart,
    /// The front end's DMA, all zero when the block has none.
    pub fe: DmaPart,
}

/// `struct lemnos_rp1_csi2_embedded`: the embedded stream for the next START.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Embedded {
    /// A stream made on this node, or -1 for none.
    pub stream_fd: i32,
    /// The embedded line's length: a multiple of 16 from 16 to [`EMBEDDED_MAX`]; 0 with -1.
    pub bytes: u32,
    /// Zero.
    pub reserved: [u32; 2],
}

/// `LEMNOS_RP1_CSI2_PART_CSI2`: the receiver part (START, STOP, EMBEDDED, stream creation).
pub const PART_CSI2: u32 = 1 << 0;
/// `LEMNOS_RP1_CSI2_PART_FE`: the front end on the same block.
pub const PART_FE: u32 = 1 << 1;

/// `struct lemnos_rp1_csi2_claim`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Claim {
    /// `PART_*` bits.
    pub parts: u32,
    /// Zero.
    pub reserved: u32,
}

/// `struct lemnos_rp1_csi2_start`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Start {
    /// The stream's descriptor, made on this node.
    pub stream_fd: i32,
    /// Data lanes, 1 to [`Info::max_lanes`].
    pub lanes: u32,
    /// Per-lane rate, 80 to 1500 Mbit/s.
    pub mbps: u32,
    /// Virtual channel, 0 to 3.
    pub vc: u32,
    /// CSI-2 data type, 1 to 63.
    pub dt: u32,
    /// Pixels per line, 1 to 65535.
    pub width: u32,
    /// Lines, 1 to 65535.
    pub height: u32,
    /// Bytes per line, a multiple of 16.
    pub stride: u32,
    /// Bytes per frame, a multiple of 16, within a buffer of the stream.
    pub size: u32,
    /// [`UNPACK16`].
    pub flags: u32,
    /// Zero.
    pub reserved: [u32; 2],
}

/// A blocking eventfd for a stream's doorbell: the module adds to it at each finished frame, and the
/// driver's wait descriptor reads it, blocking, in place of a poll. Given to [`stream_create`] as its doorbell.
pub fn doorbell() -> io::Result<OwnedFd> {
    lemnos_linux_sys::eventfd::create_blocking()
}

/// `struct lemnos_stream_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StreamCreate {
    block: u32,
    flags: u32,
    buffers: u32,
    buffer_bytes: u32,
    frame_slots: u32,
    job_slots: u32,
    job_bytes: u32,
    pool_fd: i32,
    doorbell_fd: i32,
    fd: i32,
    ring_bytes: u64,
    pool_addr: u64,
}

/// `struct lemnos_stream_buffer`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct StreamBuffer {
    buffer: u32,
    fd: i32,
    addr: u64,
    size: u64,
}

/// `struct lemnos_stream_status`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStatus {
    /// `LEMNOS_RING_STATE_*`.
    pub state: u32,
    /// The errno that ended the stream, or 0.
    pub fault: i32,
    /// Frames published.
    pub frames: u64,
    /// No room on the ring.
    pub dropped: u64,
    /// Buffers given back.
    pub returned: u64,
    /// Jobs accepted.
    pub jobs: u64,
}

/// `struct lemnos_rp1_csi2_frame_starts`: the frame-start count, read with
/// [`Node::frame_starts`]. `valid` is nonzero while the receiver is streaming with a buffer armed,
/// when the count is the one since START (with none armed no start can show); zero means it is unknown, and is not a count of zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStarts {
    /// Frame starts since START.
    pub frames: u64,
    /// CLOCK_MONOTONIC at the latest start, or zero while `frames` is zero.
    pub latest_ns: u64,
    /// Nonzero while streaming with a buffer armed.
    pub valid: u32,
    /// Zero.
    pub reserved: u32,
}

const IOC_INFO: u32 = ior::<Info>(b'L', 0xc0);
const IOC_STREAM_CREATE: u32 = iowr::<StreamCreate>(b'L', 0xc1);
const IOC_START: u32 = iow::<Start>(b'L', 0xc2);
const IOC_EMBEDDED: u32 = iow::<Embedded>(b'L', 0xc5);
const IOC_STOP: u32 = lemnos_linux_sys::ioc(0, b'L', 0xc3, 0);
const IOC_FRAME_STARTS: u32 = ior::<FrameStarts>(b'L', 0xc4);
const IOC_CLAIM: u32 = iow::<Claim>(b'L', 0xc6);
const IOC_DMA: u32 = ior::<Dma>(b'L', 0xc7);
const IOC_STREAM_STATUS: u32 = ior::<StreamStatus>(b'L', 0x84);
const IOC_STREAM_BUFFER: u32 = iowr::<StreamBuffer>(b'L', 0x86);
/// `sizeof(struct lemnos_stream_job)`: a job's fixed size.
const JOB_BYTES: u32 = 8 + 8 + 16 * 4;

/// One frame the module published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The buffer it is in.
    pub buffer: u32,
    /// Bytes.
    pub bytes: u32,
    /// Counts frame starts from the start of the stream.
    pub sequence: u64,
    /// When the frame started.
    pub timestamp_ns: u64,
    /// What its return must carry.
    pub fence: u32,
}

/// What a stream asks of the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    /// Buffers, at most [`MAX_BUFFERS`].
    pub buffers: u32,
    /// Bytes per buffer.
    pub buffer_bytes: u32,
    /// Slots on the frame ring, a power of two.
    pub frame_slots: u32,
    /// Slots on the job ring, a power of two.
    pub job_slots: u32,
}

/// An open receiver node. Several files may open it; each drives the part it claims, and closing
/// it ends the claims it holds (closing the receiver's claim stops the receiver).
#[derive(Debug)]
pub struct Node {
    file: File,
}

impl Node {
    /// Opens the node at `path`, refusing another interface revision.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        let node = Self { file };
        if node.info()?.abi != ABI {
            return Err(io::ErrorKind::Unsupported.into());
        }
        Ok(node)
    }

    fn ioctl<T>(&self, request: u32, arg: *mut T) -> io::Result<libc::c_int> {
        ioctl(self.file.as_fd(), request, arg.cast())
    }

    /// The receiver's state, from the kernel's own counters.
    pub fn info(&self) -> io::Result<Info> {
        let mut info = Info::default();
        self.ioctl(IOC_INFO, &raw mut info)?;
        Ok(info)
    }

    /// What each part's DMA holds and how its last stop ended (`LEMNOS_RP1_CSI2_IOC_DMA`). A module
    /// without the call answers `ENOTTY`.
    pub fn dma(&self) -> io::Result<Dma> {
        let mut dma = Dma::default();
        self.ioctl(IOC_DMA, &raw mut dma)?;
        Ok(dma)
    }

    /// The frame-start count since START (`LEMNOS_RP1_CSI2_IOC_FRAME_STARTS`). An ioctl the module
    /// doesn't have answers `ENOTTY`, which the driver reads as a receiver that can't count.
    pub fn frame_starts(&self) -> io::Result<FrameStarts> {
        let mut fs = FrameStarts::default();
        self.ioctl(IOC_FRAME_STARTS, &raw mut fs)?;
        Ok(fs)
    }

    /// Makes a frame stream on this block with no pool: each buffer is a dma-buf of its own,
    /// given with [`Stream::set_buffer`], and one without memory is never filled.
    pub fn stream_create(
        &self,
        params: &StreamParams,
        doorbell: Option<BorrowedFd<'_>>,
    ) -> io::Result<Stream> {
        let mut create = StreamCreate {
            block: 0,
            flags: CREATE_BUFFERS,
            buffers: params.buffers,
            buffer_bytes: params.buffer_bytes,
            frame_slots: params.frame_slots,
            job_slots: params.job_slots,
            job_bytes: JOB_BYTES,
            pool_fd: -1,
            doorbell_fd: doorbell.map_or(-1, |fd| fd.as_raw_fd()),
            fd: -1,
            ring_bytes: 0,
            pool_addr: 0,
        };
        self.ioctl(IOC_STREAM_CREATE, &raw mut create)?;
        // SAFETY: a successful create returns a new close-on-exec descriptor that nothing else
        // owns.
        let fd = unsafe { OwnedFd::from_raw_fd(create.fd) };
        let len = usize::try_from(create.ring_bytes)
            .map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))?;
        // Checks the mapping is the layout the stream was made with, before anything relies on it.
        let ring = Ring::map(
            fd.as_fd(),
            len,
            &Layout {
                frame_slots: params.frame_slots,
                buffers: params.buffers,
                job_slots: params.job_slots,
                job_bytes: JOB_BYTES,
            },
        )?;
        Ok(Stream { fd, ring })
    }

    /// Asks for the embedded stream `embedded.stream_fd` names, for the next START. Only while
    /// not streaming. `stream_fd` -1 asks for none.
    pub fn embedded(&self, embedded: &Embedded) -> io::Result<()> {
        let mut args = *embedded;
        self.ioctl(IOC_EMBEDDED, &raw mut args).map(|_| ())
    }

    /// Claims `parts` (`PART_*` bits) for this file. A part another file holds, or one this file
    /// already holds, is `EBUSY`. Needed before a driving call; observing needs no claim.
    pub fn claim(&self, parts: u32) -> io::Result<()> {
        let mut args = Claim { parts, reserved: 0 };
        self.ioctl(IOC_CLAIM, &raw mut args).map(|_| ())
    }

    /// Starts receiving into the stream `start.stream_fd` names.
    pub fn start(&self, start: &Start) -> io::Result<()> {
        let mut args = *start;
        self.ioctl(IOC_START, &raw mut args).map(|_| ())
    }

    /// Stops. Returns only when the block's DMA has stopped, or it could not be confirmed.
    pub fn stop(&self) -> io::Result<()> {
        ioctl(self.file.as_fd(), IOC_STOP, std::ptr::null_mut()).map(|_| ())
    }
}

/// A buffer's memory as the module pinned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferMemory {
    /// The buffer's device address.
    pub addr: u64,
    /// Bytes of it the block may reach.
    pub size: u64,
}

/// A stream: its mapped ring and where this end is on it.
#[derive(Debug)]
pub struct Stream {
    fd: OwnedFd,
    ring: Ring,
}

impl Stream {
    /// The descriptor, for [`Start::stream_fd`].
    #[must_use]
    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// Gives `buffer` the dma-buf `memory`, which the module pins for as long as the buffer has
    /// it, or swaps it for what the buffer had. Naming the dma-buf the buffer already has
    /// changes nothing and succeeds. `EBUSY` while the block is writing the buffer, or when the
    /// dma-buf is another buffer's; `EINVAL` for one that isn't a dma-buf the block can take or
    /// is smaller than a buffer; `EIO` once the block's DMA could not be seen to stop.
    pub fn set_buffer(&self, buffer: u32, memory: BorrowedFd<'_>) -> io::Result<BufferMemory> {
        let mut arg = StreamBuffer {
            buffer,
            fd: memory.as_raw_fd(),
            ..Default::default()
        };
        ioctl(self.fd.as_fd(), IOC_STREAM_BUFFER, (&raw mut arg).cast())?;
        Ok(BufferMemory {
            addr: arg.addr,
            size: arg.size,
        })
    }

    /// Turns the stream's buffer holds on: from now on each frame [`next_frame`](Self::next_frame)
    /// takes is held until [`release`](Self::release) gives it back. `max_in_hand` frames at most
    /// may be in hand at once: taken, and not yet passed on to the driver's own consumer
    /// ([`pass_on`](Self::pass_on)). A frame passed on stays held and is not counted, so a consumer
    /// that keeps frames does not stop the next one from being taken. `buffers` is the stream's
    /// pool size.
    pub fn add_marks(&mut self, buffers: u32, max_in_hand: u32) -> io::Result<()> {
        self.ring
            // A camera's frames come whether or not the driver reads them: late is a drop.
            .hold_passing(self.fd.as_fd(), buffers, max_in_hand, Overrun::Drops)
    }

    /// The frame held in `buffer` went on to the driver's own consumer: it stays held until
    /// [`release`](Self::release), and no longer counts as in hand. `InvalidInput` when the stream
    /// holds no frame in hand in `buffer`.
    pub fn pass_on(&mut self, buffer: u32) -> io::Result<()> {
        self.ring.pass_on(buffer)
    }

    /// The next frame published, if any. With holds on, the frame's buffer is held until
    /// [`release`](Self::release).
    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        Ok(self.ring.next_frame()?.map(|f| Frame {
            buffer: f.buffer,
            bytes: f.bytes,
            sequence: f.sequence,
            timestamp_ns: f.timestamp_ns,
            fence: f.fence,
        }))
    }

    /// Lets go of a frame's buffer: the module may reuse it. `InvalidInput` when the stream holds
    /// no frame in `buffer`.
    pub fn release(&mut self, buffer: u32) -> io::Result<()> {
        self.ring.release(buffer)
    }

    /// Frames that went by untaken since the stream was made: at the in-hand limit, or written over
    /// or reused before the driver read them (it was late). Each is a gap in the frames' sequence.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.ring.dropped()
    }

    /// How the stream is doing.
    pub fn status(&self) -> io::Result<StreamStatus> {
        let mut status = StreamStatus::default();
        ioctl(self.fd.as_fd(), IOC_STREAM_STATUS, (&raw mut status).cast())?;
        Ok(status)
    }
}

fn ioctl(fd: BorrowedFd<'_>, request: u32, arg: *mut libc::c_void) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: every `request` in this crate is paired, by the callers above, with an `arg`
        // that points at the structure the kernel expects (or is null for one taking none), live
        // for the call. `fd` is live.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
        if ret >= 0 {
            return Ok(ret);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn layouts_and_requests_match_the_kernel_headers() {
        assert_eq!(size_of::<Info>(), 56);
        assert_eq!(offset_of!(Info, frames), 24);
        assert_eq!(offset_of!(Info, errors), 48);
        assert_eq!(size_of::<Start>(), 48);
        assert_eq!(size_of::<Embedded>(), 16);
        assert_eq!(offset_of!(Embedded, bytes), 4);
        assert_eq!(offset_of!(Embedded, reserved), 8);
        assert_eq!(offset_of!(Start, flags), 36);
        assert_eq!(size_of::<StreamCreate>(), 56);
        assert_eq!(offset_of!(StreamCreate, fd), 36);
        assert_eq!(offset_of!(StreamCreate, ring_bytes), 40);
        assert_eq!(size_of::<StreamStatus>(), 40);
        assert_eq!(size_of::<StreamBuffer>(), 24);
        assert_eq!(offset_of!(StreamBuffer, fd), 4);
        assert_eq!(offset_of!(StreamBuffer, addr), 8);
        assert_eq!(offset_of!(StreamBuffer, size), 16);
        assert_eq!(IOC_STREAM_BUFFER, 0xc018_4c86);
        assert_eq!(IOC_INFO, 0x8038_4cc0);
        assert_eq!(IOC_STREAM_CREATE, 0xc038_4cc1);
        assert_eq!(IOC_START, 0x4030_4cc2);
        assert_eq!(IOC_EMBEDDED, 0x4010_4cc5);
        assert_eq!(IOC_STOP, 0x0000_4cc3);
        assert_eq!(IOC_STREAM_STATUS, 0x8028_4c84);
        assert_eq!(JOB_BYTES, 80);
        assert_eq!(size_of::<FrameStarts>(), 24);
        assert_eq!(offset_of!(FrameStarts, latest_ns), 8);
        assert_eq!(offset_of!(FrameStarts, valid), 16);
        assert_eq!(offset_of!(FrameStarts, reserved), 20);
        assert_eq!(IOC_FRAME_STARTS, 0x8018_4cc4);
        assert_eq!(size_of::<DmaPart>(), 32);
        assert_eq!(offset_of!(DmaPart, pinned_bytes), 24);
        assert_eq!(size_of::<Dma>(), 64);
        assert_eq!(offset_of!(Dma, fe), 32);
        assert_eq!(IOC_DMA, 0x8040_4cc7);
    }
}
