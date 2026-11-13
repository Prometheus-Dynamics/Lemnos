//! A pool of buffers: the publisher's, and a subscriber's view of it.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use shm_ring::{ReadMap, RingError, WriteMap, read_only, sealed_memfd};

use crate::map::Mapped;
use crate::{BufferIndex, MAX_BUFFERS, page};

/// Where one buffer is: a slice of one of the pool's descriptors. This is what crosses to the
/// ring's owner and to subscribers, next to the descriptors themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferDesc {
    /// Which of the pool's descriptors backs it.
    pub descriptor: u8,
    /// Where in that descriptor it starts.
    pub offset: u64,
    /// Its size in bytes.
    pub size: u64,
}

/// How much of each descriptor the buffers reach, after checking every buffer.
fn extents(descriptors: usize, buffers: &[BufferDesc]) -> Result<Vec<usize>, RingError> {
    if buffers.is_empty() || buffers.len() > MAX_BUFFERS as usize {
        return Err(RingError::Spec("a pool holds 1 to 1024 buffers"));
    }
    let mut extents = vec![0_usize; descriptors];
    for buffer in buffers {
        let extent =
            extents
                .get_mut(usize::from(buffer.descriptor))
                .ok_or(RingError::Malformed(
                    "a buffer names a descriptor that isn't there",
                ))?;
        let end = buffer
            .offset
            .checked_add(buffer.size)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(RingError::Malformed("a buffer's end is out of range"))?;
        *extent = (*extent).max(end);
    }
    Ok(extents)
}

fn map_all(
    fds: Vec<OwnedFd>,
    buffers: &[BufferDesc],
    writable: bool,
) -> Result<Vec<Mapped>, RingError> {
    let extents = extents(fds.len(), buffers)?;
    fds.into_iter()
        .zip(extents)
        .map(|(fd, extent)| Mapped::new(fd, extent, writable))
        .collect()
}

/// What a pool's buffers are in.
#[derive(Debug)]
enum Backing {
    /// Descriptors something else fills (a camera driver). The publisher only passes them on.
    Device(Vec<OwnedFd>),
    /// Memory this process fills, mapped for writing.
    Memory(Vec<Mapped>),
}

/// The publisher's pool: its buffers and the pool page it writes.
#[derive(Debug)]
pub struct Pool {
    generation: u32,
    page: WriteMap,
    backing: Backing,
    buffers: Vec<BufferDesc>,
}

impl Pool {
    /// A pool of `buffers`, each a slice of one of `fds`, which something else fills: a camera
    /// driver's dma-bufs. `generation` must be higher than that of any pool this topic had
    /// before.
    ///
    /// # Errors
    ///
    /// [`RingError`] if a buffer names a descriptor that isn't there or the pool page can't be
    /// made.
    pub fn new(
        generation: u32,
        fds: Vec<OwnedFd>,
        buffers: Vec<BufferDesc>,
    ) -> Result<Self, RingError> {
        extents(fds.len(), &buffers)?;
        Self::with(generation, Backing::Device(fds), buffers)
    }

    fn with(
        generation: u32,
        backing: Backing,
        buffers: Vec<BufferDesc>,
    ) -> Result<Self, RingError> {
        let count = buffers.len() as u32;
        let page = WriteMap::new(sealed_memfd(page::bytes(count))?)?;
        page::format(&page, generation, count)?;
        Ok(Self {
            generation,
            page,
            backing,
            buffers,
        })
    }

    /// A pool of `count` buffers of `size` bytes in one new memfd, mapped for writing: for a
    /// publisher that makes its own frames instead of receiving them from a device.
    ///
    /// # Errors
    ///
    /// [`RingError`] if the shape isn't allowed or the memory can't be made.
    /// A pool of `buffers`, each a slice of one of `fds`, which this process fills: the publisher
    /// maps the descriptors writable and writes buffers it has taken back ([`Pool::bytes_mut`] reaches
    /// them through [`crate::BufferWriter::buffer_mut`]). The descriptors go to subscribers too, as
    /// with [`Pool::new`]; a subscriber maps them read-only unless it asks for [`PoolView::attach_writable`].
    ///
    /// # Errors
    ///
    /// [`RingError`] if a buffer names a descriptor that isn't there or runs past its end, or a
    /// descriptor can't be mapped writable.
    pub fn mapped(
        generation: u32,
        fds: Vec<OwnedFd>,
        buffers: Vec<BufferDesc>,
    ) -> Result<Self, RingError> {
        let maps = map_all(fds, &buffers, true)?;
        Self::with(generation, Backing::Memory(maps), buffers)
    }

    /// The pool's buffers, mapped in this process for writing.
    pub fn in_memory(generation: u32, count: u32, size: usize) -> Result<Self, RingError> {
        let total = (count as usize)
            .checked_mul(size)
            .ok_or(RingError::Spec("pool is too large"))?;
        let buffers = (0..count as usize)
            .map(|index| BufferDesc {
                descriptor: 0,
                offset: (index * size) as u64,
                size: size as u64,
            })
            .collect::<Vec<_>>();
        let maps = map_all(vec![sealed_memfd(total)?], &buffers, true)?;
        Self::with(generation, Backing::Memory(maps), buffers)
    }

    /// The pool's generation.
    #[must_use]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Where each buffer is. Sent to subscribers with the descriptors.
    #[must_use]
    pub fn buffers(&self) -> &[BufferDesc] {
        &self.buffers
    }

    /// The pool's descriptors, in the order [`BufferDesc::descriptor`] numbers them.
    pub fn descriptors(&self) -> impl ExactSizeIterator<Item = BorrowedFd<'_>> {
        let (fds, maps) = match &self.backing {
            Backing::Device(fds) => (fds.as_slice(), [].as_slice()),
            Backing::Memory(maps) => ([].as_slice(), maps.as_slice()),
        };
        fds.iter()
            .map(AsFd::as_fd)
            .chain(maps.iter().map(Mapped::fd))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The pool page, read-only, to send to subscribers.
    ///
    /// # Errors
    ///
    /// If the descriptor can't be reopened, which needs `/proc`.
    pub fn page_fd(&self) -> Result<OwnedFd, RingError> {
        Ok(read_only(self.page.fd())?)
    }

    /// A subscriber's view of this pool, in this process.
    ///
    /// # Errors
    ///
    /// As [`PoolView::attach`].
    pub fn view(&self) -> Result<PoolView, RingError> {
        let fds = self
            .descriptors()
            .map(|fd| read_only(fd).or_else(|_| fd.try_clone_to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        PoolView::attach(self.page_fd()?, fds, &self.buffers)
    }

    /// A subscriber's view of this pool in this process whose buffers it may write, for a
    /// consumer that fills buffers for the publisher. The pool page stays read-only.
    ///
    /// # Errors
    ///
    /// As [`PoolView::attach_writable`].
    pub fn writable_view(&self) -> Result<PoolView, RingError> {
        let fds = self
            .descriptors()
            .map(|fd| fd.try_clone_to_owned())
            .collect::<Result<Vec<_>, _>>()?;
        PoolView::attach_writable(self.page_fd()?, fds, &self.buffers)
    }

    /// The pool page, as the publisher writes it: tickets and cut-offs.
    #[must_use]
    pub const fn page(&self) -> &WriteMap {
        &self.page
    }

    /// The bytes of `buffer`, to fill, if this process fills the pool. The caller must have
    /// taken the buffer back.
    pub(crate) fn bytes_mut(&mut self, buffer: BufferIndex) -> Option<&mut [u8]> {
        let desc = self.buffers.get(buffer.0 as usize)?;
        let Backing::Memory(maps) = &mut self.backing else {
            return None;
        };
        maps.get_mut(usize::from(desc.descriptor))?
            .bytes_mut(desc.offset as usize, desc.size as usize)
    }
}

/// Why a view hands out no bytes of a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesError {
    /// The view maps none of the buffers' memory ([`PoolView::attach_unmapped`]).
    Unmapped,
    /// The view maps the buffers read-only, and writing was asked for.
    NotWritable,
    /// The buffer is not in the pool, or its bytes are not in its descriptor.
    OutOfPool,
}

/// What a view holds of the pool's descriptors: each one mapped, or the descriptors themselves when
/// nothing is mapped.
#[derive(Debug)]
enum Views {
    Mapped(Vec<Mapped>),
    Unmapped(Vec<OwnedFd>),
}

/// A subscriber's view of a pool: the same buffers, mapped read-only, and the pool page. Or, from
/// [`PoolView::attach_unmapped`], the descriptors without any mapping of the buffers.
#[derive(Debug)]
pub struct PoolView {
    generation: u32,
    page: ReadMap,
    views: Views,
    buffers: Vec<BufferDesc>,
    writable: bool,
}

impl PoolView {
    /// Attaches to a pool handed over by its publisher: the pool page, the descriptors and where
    /// each buffer is. Nothing here is trusted: every buffer is checked against its descriptor,
    /// and a descriptor that could shrink is refused.
    ///
    /// # Errors
    ///
    /// [`RingError`] if the page isn't a pool page for these buffers, a buffer doesn't fit its
    /// descriptor, a descriptor could shrink, or the memory can't be mapped.
    pub fn attach(
        page: OwnedFd,
        fds: Vec<OwnedFd>,
        buffers: &[BufferDesc],
    ) -> Result<Self, RingError> {
        Self::with_access(page, fds, buffers, false)
    }

    /// As [`attach`](Self::attach), but the buffers are mapped writable, for a consumer that fills
    /// them for the publisher. The descriptors must be writable too: a read-only one fails here.
    ///
    /// # Errors
    ///
    /// As [`attach`](Self::attach), and [`RingError`] if a descriptor can't be mapped writable.
    pub fn attach_writable(
        page: OwnedFd,
        fds: Vec<OwnedFd>,
        buffers: &[BufferDesc],
    ) -> Result<Self, RingError> {
        Self::with_access(page, fds, buffers, true)
    }

    fn with_access(
        page: OwnedFd,
        fds: Vec<OwnedFd>,
        buffers: &[BufferDesc],
        writable: bool,
    ) -> Result<Self, RingError> {
        let page = ReadMap::new(page)?;
        let (generation, count) = page::read(&page)?;
        if count as usize != buffers.len() {
            return Err(RingError::Malformed("the pool page is for another pool"));
        }
        Ok(Self {
            generation,
            page,
            views: Views::Mapped(map_all(fds, buffers, writable)?),
            buffers: buffers.to_vec(),
            writable,
        })
    }

    /// As [`attach`](Self::attach), but no buffer's memory is mapped. Every descriptor is checked as far
    /// as that allows without a mapping (a memfd must be sealed against shrinking and reach its
    /// buffers; a dma-buf must be non-empty), and the view keeps the descriptors themselves: the process
    /// holds exactly the descriptors it was passed, one for each, and no mapping. Byte access on such a
    /// view is [`BytesError::Unmapped`]; [`descriptor`](Self::descriptor) still hands each buffer's
    /// descriptor on.
    ///
    /// # Errors
    ///
    /// As [`attach`](Self::attach), except that nothing is mapped, so a failed mapping is not one.
    pub fn attach_unmapped(
        page: OwnedFd,
        fds: Vec<OwnedFd>,
        buffers: &[BufferDesc],
    ) -> Result<Self, RingError> {
        let page = ReadMap::new(page)?;
        let (generation, count) = page::read(&page)?;
        if count as usize != buffers.len() {
            return Err(RingError::Malformed("the pool page is for another pool"));
        }
        for (fd, extent) in fds.iter().zip(extents(fds.len(), buffers)?) {
            crate::map::check(fd, extent)?;
        }
        Ok(Self {
            generation,
            page,
            views: Views::Unmapped(fds),
            buffers: buffers.to_vec(),
            writable: false,
        })
    }

    /// Whether this view maps the buffers' memory at all. `false` for [`attach_unmapped`](Self::attach_unmapped).
    #[must_use]
    pub const fn is_mapped(&self) -> bool {
        matches!(self.views, Views::Mapped(_))
    }

    /// The bytes of `buffer`, to write, if this view was attached writable and the buffer is in
    /// the pool. What the consumer writes is only ever read by the publisher after the job that
    /// names it (see the contract's rule for consumer-filled buffers).
    pub fn bytes_mut(&mut self, buffer: BufferIndex) -> Option<&mut [u8]> {
        self.write_bytes(buffer).ok()
    }

    /// The bytes of `buffer` to write, or why there are none: [`BytesError::Unmapped`] for an unmapped
    /// view, [`BytesError::NotWritable`] for one attached read-only, [`BytesError::OutOfPool`] for a
    /// buffer that is not in the pool.
    ///
    /// # Errors
    ///
    /// As above.
    pub fn write_bytes(&mut self, buffer: BufferIndex) -> Result<&mut [u8], BytesError> {
        let Views::Mapped(maps) = &mut self.views else {
            return Err(BytesError::Unmapped);
        };
        if !self.writable {
            return Err(BytesError::NotWritable);
        }
        let desc = *self
            .buffers
            .get(buffer.0 as usize)
            .ok_or(BytesError::OutOfPool)?;
        maps.get_mut(usize::from(desc.descriptor))
            .and_then(|map| map.bytes_mut(desc.offset as usize, desc.size as usize))
            .ok_or(BytesError::OutOfPool)
    }

    /// The pool's generation.
    #[must_use]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// How many buffers the pool has.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.buffers.len() as u32
    }

    /// Whether the pool has no buffers. It never does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    /// The descriptor behind `buffer` and where the buffer is in it, for a reader that hands
    /// the buffer to something else (a GPU import) instead of reading it.
    #[must_use]
    pub fn descriptor(&self, buffer: BufferIndex) -> Option<(BorrowedFd<'_>, BufferDesc)> {
        let desc = *self.buffers.get(buffer.0 as usize)?;
        let fd = match &self.views {
            Views::Mapped(maps) => maps.get(usize::from(desc.descriptor))?.fd(),
            Views::Unmapped(fds) => fds.get(usize::from(desc.descriptor))?.as_fd(),
        };
        Some((fd, desc))
    }

    pub(crate) const fn page(&self) -> &ReadMap {
        &self.page
    }

    /// The bytes of `buffer`, to read. Only while the frame that names it is held, for a
    /// consumer's read of a frame's bytes; see [`crate::Held`].
    pub fn bytes(&self, buffer: BufferIndex) -> Option<&[u8]> {
        self.read_bytes(buffer).ok()
    }

    /// The bytes of `buffer` to read, or why there are none: [`BytesError::Unmapped`] for an unmapped
    /// view, [`BytesError::OutOfPool`] for a buffer that is not in the pool.
    ///
    /// # Errors
    ///
    /// As above.
    pub fn read_bytes(&self, buffer: BufferIndex) -> Result<&[u8], BytesError> {
        let Views::Mapped(maps) = &self.views else {
            return Err(BytesError::Unmapped);
        };
        let desc = self
            .buffers
            .get(buffer.0 as usize)
            .ok_or(BytesError::OutOfPool)?;
        maps.get(usize::from(desc.descriptor))
            .and_then(|map| map.bytes(desc.offset as usize, desc.size as usize))
            .ok_or(BytesError::OutOfPool)
    }
}
