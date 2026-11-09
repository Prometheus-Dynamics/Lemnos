//! Taking a frame stream: the descriptors arrive once, and after that frames come off the shared
//! ring with no socket involved.
//!
//! Passing descriptors and mapping memory need `unsafe`, which this crate doesn't allow. So the
//! few things a stream needs from the operating system are the [`StreamPlatform`] trait, and the
//! program brings the implementation: on Linux, `lemnos-linux-stream`.

use std::fmt::Debug;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_wire::ring::{ByteMemory, Consumer, Layout, RingError, STATE_OPEN, WordMemory};

use crate::error::Error;

/// How a stream reaches the operating system.
pub trait StreamPlatform: Debug + Send + Sync {
    /// Reads from `socket` into `buf`, appending any descriptors that arrived to `fds`. Returns
    /// the bytes read, 0 at end of file, and the socket's own timeout applies. Implementors own
    /// what they append from the moment they append it.
    fn recv_with_descriptors(
        &self,
        socket: BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> io::Result<usize>;

    /// Maps `bytes` of `fd` shared and writable. Fails unless `fd` is exactly `bytes` long, since
    /// a mapping past the end would fault when touched.
    fn map_ring(&self, fd: BorrowedFd<'_>, bytes: usize) -> io::Result<Arc<dyn WordMemory>>;

    /// Maps `bytes` of `fd` shared and read-only, with the same check of its length.
    fn map_buffers(&self, fd: BorrowedFd<'_>, bytes: usize) -> io::Result<Box<dyn ByteMemory>>;

    /// Waits for `fd` to be signalled, then clears it. `false` means `timeout` passed first.
    fn wait_doorbell(&self, fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool>;
}

/// One frame taken from a stream, held until it's given back with [`FrameStream::release`].
/// Only [`FrameStream`] can make one, and it can't be cloned, so a frame is given back once.
#[derive(Debug, PartialEq, Eq)]
pub struct HeldFrame {
    buffer: u32,
    bytes: u32,
    sequence: u64,
    identity: u64,
    timestamp_ns: u64,
    fence: u32,
}

impl HeldFrame {
    #[must_use]
    pub const fn buffer(&self) -> u32 {
        self.buffer
    }

    /// Bytes this frame's buffer holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes as usize
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Means something only to whoever made the frame. Lemnos never reads it.
    #[must_use]
    pub const fn identity(&self) -> u64 {
        self.identity
    }

    #[must_use]
    pub const fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }
}

/// The consumer's end of a stream. A malformed peer is an error here and only here.
#[derive(Debug)]
pub struct FrameStream {
    device: u32,
    name: String,
    ring: Consumer,
    pool: Pool,
    doorbell: OwnedFd,
    platform: Arc<dyn StreamPlatform>,
}

/// The buffer memory. Debug-printable so a stream can derive it.
struct Pool(Box<dyn ByteMemory>);

impl Debug for Pool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("<buffers>")
    }
}

/// The ring, the doorbell and the buffers.
pub(crate) const DESCRIPTORS: usize = 3;

impl FrameStream {
    /// Builds the stream from what StreamReady carried. Any failure closes every descriptor.
    pub(crate) fn new(
        device: u32,
        name: &str,
        layout: Layout,
        fds: Vec<OwnedFd>,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<Self, Error> {
        let layout = layout.checked().map_err(Error::Stream)?;
        let pool_bytes =
            usize::try_from(layout.pool_bytes()).map_err(|_| Error::Stream(RingError::TooSmall))?;
        let [ring_fd, doorbell, buffers_fd] = <[OwnedFd; DESCRIPTORS]>::try_from(fds)
            .map_err(|_| Error::Stream(RingError::BadHeader))?;
        let failed = |_: io::Error| Error::Stream(RingError::TooSmall);
        let ring_memory = platform
            .map_ring(ring_fd.as_fd(), layout.ring_bytes())
            .map_err(failed)?;
        let pool = platform
            .map_buffers(buffers_fd.as_fd(), pool_bytes)
            .map_err(failed)?;
        let ring = Consumer::attach(ring_memory, layout).map_err(Error::Stream)?;
        Ok(Self {
            device,
            name: name.to_string(),
            ring,
            pool: Pool(pool),
            doorbell,
            platform,
        })
    }

    #[must_use]
    pub const fn device(&self) -> u32 {
        self.device
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn layout(&self) -> &Layout {
        self.ring.layout()
    }

    /// Whether the host has ended this stream.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.ring.state() == STATE_OPEN
    }

    /// The next frame, if one has arrived. Never waits and allocates nothing.
    pub fn try_next(&mut self) -> Result<Option<HeldFrame>, Error> {
        let frame = self.ring.pop_frame().map_err(Error::Stream)?;
        Ok(frame.map(|slot| HeldFrame {
            buffer: slot.buffer,
            bytes: slot.bytes,
            sequence: slot.sequence,
            identity: slot.identity,
            timestamp_ns: slot.timestamp_ns,
            fence: slot.fence,
        }))
    }

    /// The next frame, or `None` after `timeout` passes or the host ends the stream.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<HeldFrame>, Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.try_next()? {
                return Ok(Some(frame));
            }
            if !self.is_open() {
                return Ok(None);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match self.platform.wait_doorbell(self.doorbell.as_fd(), left) {
                Ok(_) => {}
                Err(_) => return Ok(None),
            }
        }
    }

    /// The bytes of a frame this stream handed out. A frame from another stream reads as whatever
    /// its numbers fall on here, and within this stream's memory.
    #[must_use]
    pub fn bytes(&self, frame: &HeldFrame) -> &[u8] {
        let layout = self.ring.layout();
        let Some(offset) = layout.buffer_offset(frame.buffer) else {
            return &[];
        };
        let end = offset.saturating_add(frame.bytes as usize);
        self.pool.0.bytes().get(offset..end).unwrap_or(&[])
    }

    /// Gives a frame's buffer back to the producer. The frame is gone either way.
    pub fn release(&mut self, frame: HeldFrame) -> Result<(), Error> {
        match self.ring.release(frame.buffer, frame.fence) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::Stream(RingError::BadIndex)),
            Err(error) => Err(Error::Stream(error)),
        }
    }

    /// Posts a job for the producer. `false` means too many are waiting.
    pub fn post_job(&mut self, job: &[u8]) -> Result<bool, Error> {
        self.ring.push_job(job).map_err(Error::Stream)
    }
}
