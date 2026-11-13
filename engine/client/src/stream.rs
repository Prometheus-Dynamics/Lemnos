//! Taking a frame stream: the descriptors arrive once, and after that frames come off the shared
//! ring with no socket involved.
//!
//! A consumer reads frames, and on a stream its contract has it fill it also writes buffers: a
//! job's input image, a configuration. Such a stream is taken writable
//! ([`Session::open_stream_writable`](crate::Session::open_stream_writable)); every other is
//! mapped read-only, so a frame can't be scribbled on by mistake.
//!
//! Passing descriptors and mapping memory need `unsafe`, which this crate doesn't allow. So the
//! few things a stream needs from the operating system are the [`StreamPlatform`] trait, and the
//! program brings the implementation: on Linux, `lemnos-linux-stream`.

use std::fmt::Debug;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_wire::handle_descriptors;
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

    /// Maps `bytes` of `fd` shared, with the same check of its length: read-only, or writable
    /// too when `writable` is set, which a consumer asks for on a stream it fills. The memory's
    /// [`ByteMemory::write`] works only on a writable mapping.
    fn map_buffers(
        &self,
        fd: BorrowedFd<'_>,
        bytes: usize,
        writable: bool,
    ) -> io::Result<Box<dyn ByteMemory>>;

    /// Maps the first `bytes` of `fd` shared, read-only or writable as
    /// [`map_buffers`](Self::map_buffers) does: one buffer of a stream whose buffers are a
    /// descriptor each (dma-bufs). Fails unless `fd` is at least `bytes` long and can never
    /// get shorter, since a mapping past the end would fault when touched. It may be longer: a
    /// dma-heap rounds up.
    ///
    /// A device shares this memory. The mapping tells the kernel around each access by the
    /// processor (`DMA_BUF_IOCTL_SYNC` on Linux): inside [`ByteMemory::write`], and between
    /// [`ByteMemory::begin_read`] and [`ByteMemory::end_read`].
    ///
    /// A platform without this can't take such a stream, and opening one fails with nothing
    /// mapped. That is what this default does.
    fn map_buffer(
        &self,
        fd: BorrowedFd<'_>,
        bytes: usize,
        writable: bool,
    ) -> io::Result<Box<dyn ByteMemory>> {
        let _ = (fd, bytes, writable);
        Err(io::ErrorKind::Unsupported.into())
    }

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
    /// Whether the buffers were mapped to be written too.
    writable: bool,
    doorbell: OwnedFd,
    platform: Arc<dyn StreamPlatform>,
}

/// The buffer memory. Debug-printable so a stream can derive it.
enum Pool {
    /// One mapping, a buffer every `stride` bytes.
    Whole(Box<dyn ByteMemory>),
    /// A mapping per buffer, each from its first byte, and the buffer's descriptor.
    Each(Box<[(Box<dyn ByteMemory>, OwnedFd)]>),
}

impl Debug for Pool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("<buffers>")
    }
}

/// The ring, the doorbell and the buffers.
pub(crate) const DESCRIPTORS: usize = 3;
/// The most descriptors one answer brings: a stream with a descriptor per buffer, at its limit.
pub(crate) const MAX_DESCRIPTORS: usize =
    (lemnos_wire::HANDLE_DESCRIPTORS_BEFORE + lemnos_wire::MAX_HANDLE_BUFFERS) as usize;

impl FrameStream {
    /// Builds the stream from what StreamReady carried. Any failure closes every descriptor.
    pub(crate) fn new(
        device: u32,
        name: &str,
        layout: Layout,
        fds: Vec<OwnedFd>,
        writable: bool,
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
            .map_buffers(buffers_fd.as_fd(), pool_bytes, writable)
            .map_err(failed)?;
        let ring = Consumer::attach(ring_memory, layout).map_err(Error::Stream)?;
        Ok(Self {
            device,
            name: name.to_string(),
            ring,
            pool: Pool::Whole(pool),
            writable,
            doorbell,
            platform,
        })
    }

    /// Builds the stream from what StreamHandles carried: the ring, the doorbell, then one
    /// descriptor per buffer. Any failure closes every descriptor. Each buffer's descriptor is
    /// kept beside its mapping, for [`buffer_handle`](Self::buffer_handle), and closes with the
    /// stream.
    pub(crate) fn with_handles(
        device: u32,
        name: &str,
        layout: Layout,
        fds: Vec<OwnedFd>,
        writable: bool,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<Self, Error> {
        let layout = layout.checked().map_err(Error::Stream)?;
        // Checked by the session too. Here it is what makes the split below right.
        let expected = handle_descriptors(layout.buffers).map(usize::from);
        if expected != Some(fds.len()) {
            return Err(Error::Stream(RingError::BadHeader));
        }
        let mut fds = fds.into_iter();
        let (Some(ring_fd), Some(doorbell)) = (fds.next(), fds.next()) else {
            return Err(Error::Stream(RingError::BadHeader));
        };
        let failed = |_: io::Error| Error::Stream(RingError::TooSmall);
        let ring_memory = platform
            .map_ring(ring_fd.as_fd(), layout.ring_bytes())
            .map_err(failed)?;
        let buffer_bytes = layout.buffer_bytes as usize;
        let mut buffers = Vec::with_capacity(fds.len());
        for fd in fds {
            let memory = platform
                .map_buffer(fd.as_fd(), buffer_bytes, writable)
                .map_err(|error| match error.kind() {
                    io::ErrorKind::Unsupported => Error::StreamUnsupported,
                    _ => failed(error),
                })?;
            buffers.push((memory, fd));
        }
        let ring = Consumer::attach(ring_memory, layout).map_err(Error::Stream)?;
        Ok(Self {
            device,
            name: name.to_string(),
            ring,
            pool: Pool::Each(buffers.into()),
            writable,
            doorbell,
            platform,
        })
    }

    /// Whether each buffer is its own mapping, which is how a stream of dma-bufs arrives.
    /// Frames read the same either way.
    #[must_use]
    pub const fn has_buffer_handles(&self) -> bool {
        matches!(self.pool, Pool::Each(_))
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
        if let Some(memory) = frame.as_ref().and_then(|slot| self.own_memory(slot.buffer)) {
            // The frame is read in place from here until it is given back.
            memory.begin_read();
        }
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
        match &self.pool {
            Pool::Whole(pool) => {
                let layout = self.ring.layout();
                let Some(offset) = layout.buffer_offset(frame.buffer) else {
                    return &[];
                };
                let end = offset.saturating_add(frame.bytes as usize);
                pool.bytes().get(offset..end).unwrap_or(&[])
            }
            Pool::Each(buffers) => buffers
                .get(frame.buffer as usize)
                .and_then(|(buffer, _)| buffer.bytes().get(..frame.bytes as usize))
                .unwrap_or(&[]),
        }
    }

    /// The descriptor of `buffer`, on a stream whose buffers are a descriptor each (dma-bufs),
    /// for handing the buffer to something else that takes memory that way: a GPU importing a
    /// frame, an encoder. `None` for a stream of shared memory, whose buffers have no descriptor
    /// of their own, and for a buffer the stream doesn't have.
    ///
    /// The descriptor stays the stream's and closes with it; clone it
    /// ([`BorrowedFd::try_clone_to_owned`]) to keep the memory past that. Whatever is handed the
    /// buffer is under the same rule as a write: a frame is the consumer's from when it is taken
    /// until it is released, and a buffer the consumer fills is its own only until it posts the
    /// job that names it.
    #[must_use]
    pub fn buffer_handle(&self, buffer: u32) -> Option<BorrowedFd<'_>> {
        match &self.pool {
            Pool::Whole(_) => None,
            Pool::Each(buffers) => buffers.get(buffer as usize).map(|(_, fd)| fd.as_fd()),
        }
    }

    /// The mapping of `buffer` alone, on a stream whose buffers are a descriptor each.
    fn own_memory(&self, buffer: u32) -> Option<&dyn ByteMemory> {
        match &self.pool {
            Pool::Whole(_) => None,
            Pool::Each(buffers) => buffers.get(buffer as usize).map(|(memory, _)| &**memory),
        }
    }

    /// The memory `len` bytes at `offset` of `buffer` are in, and where in it. Refused for a
    /// buffer the stream doesn't have and for a range that isn't inside the buffer.
    fn place(
        &self,
        buffer: u32,
        offset: usize,
        len: usize,
    ) -> Result<(&dyn ByteMemory, usize), Error> {
        let layout = self.ring.layout();
        if buffer >= layout.buffers {
            return Err(Error::Stream(RingError::BadBuffer));
        }
        let inside = offset
            .checked_add(len)
            .is_some_and(|end| end <= layout.buffer_bytes as usize);
        if !inside {
            return Err(Error::Stream(RingError::BadLength));
        }
        match &self.pool {
            Pool::Whole(pool) => {
                let start = layout
                    .buffer_offset(buffer)
                    .and_then(|at| at.checked_add(offset))
                    .ok_or(Error::Stream(RingError::BadBuffer))?;
                Ok((&**pool, start))
            }
            Pool::Each(buffers) => buffers
                .get(buffer as usize)
                .map(|(memory, _)| (&**memory, offset))
                .ok_or(Error::Stream(RingError::BadBuffer)),
        }
    }

    /// Whether the stream was taken to be filled ([`Session::open_stream_writable`]), so that
    /// [`write`](Self::write) works.
    ///
    /// [`Session::open_stream_writable`]: crate::Session::open_stream_writable
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.writable
    }

    /// Copies `data` into `buffer` at `offset`: a job's input image, a configuration, a
    /// program. Only on a stream taken with [`Session::open_stream_writable`]; on any other it
    /// is [`Error::StreamReadOnly`]. A buffer the stream doesn't have, or a range that isn't
    /// inside the buffer, is refused and nothing is written. Never waits and allocates nothing.
    ///
    /// The memory is shared with the producer, and nothing here knows which buffer is whose at
    /// the moment: the stream's contract does. Write a buffer before posting the job that names
    /// it, and not again until that job has completed; never write a frame the producer may
    /// still be filling. The producer takes nothing in a buffer on trust, so a write at the
    /// wrong time costs the writer its own result and nothing else.
    ///
    /// On a stream of dma-bufs the kernel is told around the copy, so a device that is handed
    /// the buffer next sees what was written.
    ///
    /// [`Session::open_stream_writable`]: crate::Session::open_stream_writable
    pub fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::StreamReadOnly);
        }
        let (memory, start) = self.place(buffer, offset, data.len())?;
        if memory.write(start, data) {
            Ok(())
        } else {
            Err(Error::Stream(RingError::BadLength))
        }
    }

    /// Copies from `buffer` at `offset` into `out`, for a buffer that isn't a frame: what a
    /// job left in a buffer the consumer named (a program's outputs, say). A frame is read in
    /// place with [`bytes`](Self::bytes). Refused as [`write`](Self::write) is for a buffer or
    /// a range the stream doesn't have. Never waits and allocates nothing.
    pub fn read(&self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<(), Error> {
        let (memory, start) = self.place(buffer, offset, out.len())?;
        memory.begin_read();
        let copied = start
            .checked_add(out.len())
            .and_then(|end| memory.bytes().get(start..end))
            .map(|from| out.copy_from_slice(from));
        memory.end_read();
        copied.ok_or(Error::Stream(RingError::BadLength))
    }

    /// Gives a frame's buffer back to the producer. The frame is gone either way.
    pub fn release(&mut self, frame: HeldFrame) -> Result<(), Error> {
        if let Some(memory) = self.own_memory(frame.buffer) {
            memory.end_read();
        }
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
