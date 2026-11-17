//! The owner's side of a ring: making it, handing out its ends, counting subscribers.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::Ordering;

use crate::layout::{Geometry, Spec, W_END, W_SUBSCRIBERS, format};
use crate::{Mem, MemMut, Reader, RingError, WriteMap, Writer, map};

/// A ring and the right to hand out its ends. The owner (the broker that hands out ends) holds one per topic.
///
/// The memory is a memfd sealed against resizing, so a mapping of it can never fault. The write
/// end goes to the one publisher. Read ends are opened read-only: a subscriber cannot map them
/// writable, so it cannot disturb the topic.
#[derive(Debug)]
pub struct Ring {
    map: WriteMap,
    geometry: Geometry,
}

impl Ring {
    /// Makes an empty ring sized from `spec`.
    ///
    /// # Errors
    ///
    /// [`RingError`] if the shape isn't allowed or the memory can't be made.
    pub fn create(spec: &Spec) -> Result<Self, RingError> {
        let geometry = Geometry::for_spec(spec)?;
        let map = WriteMap::new(map::sealed_memfd(geometry.bytes())?)?;
        format(&map, spec)?;
        Ok(Self { map, geometry })
    }

    /// Takes over a ring that already exists, from its write end: what an owner that has
    /// just started does with the rings its publishers kept running without it.
    ///
    /// # Errors
    ///
    /// [`RingError`] if `fd` isn't sealed, can't be mapped for writing or doesn't hold a ring.
    pub fn adopt(fd: OwnedFd) -> Result<Self, RingError> {
        let map = WriteMap::new(fd)?;
        let geometry = Geometry::read(&map)?;
        Ok(Self { map, geometry })
    }

    /// The ring's shape.
    #[must_use]
    pub const fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// The bytes of memory the ring takes, rounded to pages.
    #[must_use]
    pub fn mapped_bytes(&self) -> usize {
        self.map.words() * 8
    }

    /// The write end, to pass to the publisher.
    ///
    /// # Errors
    ///
    /// If the descriptor can't be duplicated.
    pub fn write_fd(&self) -> Result<OwnedFd, RingError> {
        Ok(self.map.fd().try_clone_to_owned()?)
    }

    /// A read end, to pass to a subscriber. It can only ever be mapped read-only.
    ///
    /// # Errors
    ///
    /// If the descriptor can't be reopened, which needs `/proc`.
    pub fn read_fd(&self) -> Result<OwnedFd, RingError> {
        Ok(map::read_only(self.map.fd())?)
    }

    /// Makes the ring something readers can be handed and nothing can write through: see
    /// [`map::share_read_only`]. The owner's own write end keeps working.
    ///
    /// # Errors
    ///
    /// As [`map::share_read_only`].
    pub fn share_read_only(&self) -> Result<(), RingError> {
        map::share_read_only(self.map.fd())
    }

    /// A writer in this process.
    ///
    /// # Errors
    ///
    /// As [`Writer::attach`].
    pub fn writer(&self) -> Result<Writer, RingError> {
        Writer::attach(self.write_fd()?)
    }

    /// A reader in this process.
    ///
    /// # Errors
    ///
    /// As [`Reader::attach`].
    pub fn reader(&self) -> Result<Reader, RingError> {
        Reader::attach(self.read_fd()?)
    }

    /// Sets how many subscribers the topic has, for the publisher to see.
    pub fn set_subscribers(&self, count: u32) {
        self.map
            .store(W_SUBSCRIBERS, u64::from(count), Ordering::Relaxed);
    }

    /// How many subscribers the topic is recorded as having.
    #[must_use]
    pub fn subscribers(&self) -> u32 {
        self.map.load(W_SUBSCRIBERS, Ordering::Relaxed) as u32
    }

    /// Records that the topic has ended, with `code` (nonzero) for why. Readers see it through
    /// [`Reader::end`](crate::Reader::end). Nothing else changes: samples already published stay
    /// readable, and the publisher may still publish after this, which its owner should not allow.
    pub fn set_end(&self, code: u32) {
        self.map.store(W_END, u64::from(code), Ordering::Release);
    }

    /// The end code the owner recorded, 0 while the topic runs.
    #[must_use]
    pub fn end(&self) -> u32 {
        self.map.load(W_END, Ordering::Acquire) as u32
    }
}

impl AsFd for Ring {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.map.fd()
    }
}
