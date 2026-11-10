//! A frame in dma-bufs, checked against what each dma-buf really holds.
//!
//! The engine reads and writes each plane from the start of its dma-buf, for the plane's whole
//! size, at the address the kernel's `vb2_dma_contig` gives the buffer (the driver never adds a
//! data offset). A [`Frame`] is the only thing a conversion takes, and one is built only when
//! there is a dma-buf for every plane and each is at least as long as its plane, by the size the
//! kernel answers for the dma-buf (`lseek` to its end), not one a caller states.

use std::fmt;
use std::os::fd::BorrowedFd;

use lemnos_linux_sys::dma_buf;

use crate::layout::{Layout, MAX_PLANES};

/// Why a frame is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Not one dma-buf per plane.
    Planes {
        /// Dma-bufs given.
        given: usize,
        /// Planes the layout has.
        needed: usize,
    },
    /// A dma-buf shorter than its plane.
    Short {
        /// The plane.
        plane: usize,
        /// The dma-buf's size.
        have: u64,
        /// The plane's size.
        need: u32,
    },
    /// The kernel would not say how long a dma-buf is: the descriptor isn't one, or is closed.
    Unsized {
        /// The plane.
        plane: usize,
        /// The errno, when there was one.
        errno: Option<i32>,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planes { given, needed } => {
                write!(f, "{given} dma-bufs for a frame of {needed} planes")
            }
            Self::Short { plane, have, need } => write!(
                f,
                "plane {plane} needs {need} bytes and its dma-buf holds {have}"
            ),
            Self::Unsized { plane, errno } => {
                write!(
                    f,
                    "the size of plane {plane}'s dma-buf is unknown ({errno:?})"
                )
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// A frame of `layout` in dma-bufs, one per plane, each checked to hold its plane.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'fd> {
    layout: Layout,
    buffers: [Option<BorrowedFd<'fd>>; MAX_PLANES],
    /// Each dma-buf's size as the kernel answered it, at most `u32::MAX`.
    lengths: [u32; MAX_PLANES],
}

impl<'fd> Frame<'fd> {
    /// A frame of `layout` in `buffers`, one dma-buf per plane in plane order. Allocates nothing.
    pub fn new(layout: Layout, buffers: &[BorrowedFd<'fd>]) -> Result<Self, FrameError> {
        let planes = layout.planes();
        if buffers.len() != planes.len() {
            return Err(FrameError::Planes {
                given: buffers.len(),
                needed: planes.len(),
            });
        }
        let mut frame = Self {
            layout,
            buffers: [None; MAX_PLANES],
            lengths: [0; MAX_PLANES],
        };
        for (index, (fd, plane)) in buffers.iter().zip(planes.iter()).enumerate() {
            let have = dma_buf::size(*fd).map_err(|error| FrameError::Unsized {
                plane: index,
                errno: error.raw_os_error(),
            })?;
            if have < u64::from(plane.size) {
                return Err(FrameError::Short {
                    plane: index,
                    have,
                    need: plane.size,
                });
            }
            frame.buffers[index] = Some(*fd);
            // A larger dma-buf is offered as the most a plane length can say.
            frame.lengths[index] = u32::try_from(have).unwrap_or(u32::MAX);
        }
        Ok(frame)
    }

    /// The frame's layout.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Plane `index`: its dma-buf and that dma-buf's size (at most `u32::MAX`).
    pub fn plane(&self, index: usize) -> Option<(BorrowedFd<'fd>, u32)> {
        let fd = (*self.buffers.get(index)?)?;
        Some((fd, self.lengths[index]))
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, OwnedFd};

    use lemnos_measure::test_alloc::allocations;

    use super::*;
    use crate::layout::PixelFormat;

    fn buffer(len: u64) -> OwnedFd {
        dma_buf::sealed_memfd(c"nonai2d-test", len).expect("memfd")
    }

    fn nv12() -> Layout {
        Layout::new(PixelFormat::Nv12, 64, 8).expect("layout")
    }

    #[test]
    fn each_plane_needs_a_large_enough_dma_buf() {
        let (y, uv) = (buffer(512), buffer(256));
        let frame = Frame::new(nv12(), &[y.as_fd(), uv.as_fd()]).expect("fits exactly");
        assert_eq!(frame.plane(1).map(|(_, len)| len), Some(256));
        assert!(frame.plane(2).is_none());
        let big = buffer(4096);
        assert!(Frame::new(nv12(), &[big.as_fd(), big.as_fd()]).is_ok());
        let short = buffer(255);
        assert_eq!(
            Frame::new(nv12(), &[y.as_fd(), short.as_fd()]).err(),
            Some(FrameError::Short {
                plane: 1,
                have: 255,
                need: 256
            })
        );
        assert_eq!(
            Frame::new(nv12(), &[y.as_fd()]).err(),
            Some(FrameError::Planes {
                given: 1,
                needed: 2
            })
        );
        assert_eq!(
            Frame::new(nv12(), &[y.as_fd(), uv.as_fd(), uv.as_fd()]).err(),
            Some(FrameError::Planes {
                given: 3,
                needed: 2
            })
        );
    }

    #[test]
    fn a_descriptor_the_kernel_cannot_size_is_refused() {
        let (reader, _writer) = std::io::pipe().expect("pipe");
        let refused = Frame::new(
            Layout::new(PixelFormat::Rgb565, 4, 4).expect("layout"),
            &[reader.as_fd()],
        );
        assert!(matches!(refused, Err(FrameError::Unsized { plane: 0, .. })));
    }

    #[test]
    fn building_a_frame_allocates_nothing() {
        let (y, uv) = (buffer(512), buffer(256));
        let (frame, count) = allocations(|| Frame::new(nv12(), &[y.as_fd(), uv.as_fd()]));
        assert!(frame.is_ok());
        assert_eq!(count, 0);
    }
}
