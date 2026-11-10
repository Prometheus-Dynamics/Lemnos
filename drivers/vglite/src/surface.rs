//! The buffers the GPU reads and writes: always memory its kernel driver allocated
//! (`VG_LITE_ALLOCATE`, physically contiguous), with the caller's pixels copied in and out.
//!
//! The kernel driver can also map a dma-buf, but it takes the address of the dma-buf's last
//! scatter-list entry only (`vg_lite_hal_map`), so a dma-buf that is not one physically
//! contiguous block would be read and written in the wrong place. User space cannot tell which a
//! dma-buf is, so no dma-buf is ever handed to the GPU: a [`Frame`] is copied through a staging
//! buffer like a byte slice. Every call reports the bytes it copied ([`Transfer`]).

use std::io;
use std::os::fd::BorrowedFd;

use lemnos_driver_vglite_sys::{Device, Memory, clean_invalidate_dcache};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE};
use lemnos_linux_sys::mmap::Mapping;

use crate::command::Bound;
use crate::plan::{Image, invalid};

/// A dma-buf holding one image.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// The image's layout.
    pub image: Image,
    /// The dma-buf's descriptor, borrowed for the call.
    pub fd: BorrowedFd<'a>,
}

/// What an operation cost beyond the unit's own work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transfer {
    /// Bytes the CPU copied into and out of staging buffers.
    pub staged_bytes: usize,
}

/// Where an operation's pixels come from.
pub(crate) enum Input<'a> {
    Bytes(&'a [u8]),
    Frame(Frame<'a>),
}

/// Where an operation's pixels go.
pub(crate) enum Output<'a> {
    Bytes(&'a mut [u8]),
    Frame(Frame<'a>),
}

impl Output<'_> {
    /// The output as an input, to read its current pixels.
    pub(crate) fn as_input(&self) -> Input<'_> {
        match self {
            Self::Bytes(bytes) => Input::Bytes(bytes),
            Self::Frame(frame) => Input::Frame(*frame),
        }
    }
}

/// A staging buffer for one operation, freed when dropped (unless the operation's wait failed:
/// see `Vglite`).
pub(crate) struct Surface {
    /// The caller's image.
    image: Image,
    /// The stride of the staging buffer: `vg_lite_allocate`'s.
    stride: u32,
    memory: Memory,
}

impl Surface {
    /// A staging buffer for `image` (already checked), at the stride the library's allocator
    /// gives.
    pub(crate) fn staging(device: &Device, image: Image) -> io::Result<Self> {
        let stride = image.staged_stride();
        let bytes = stride
            .checked_mul(image.height)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "image too large"))?;
        let memory = device.allocate(bytes)?;
        Ok(Self {
            image,
            stride,
            memory,
        })
    }

    /// The buffer as the stream describes it.
    pub(crate) fn bound(&self) -> Bound<'_, Memory> {
        Bound {
            image: Image {
                stride: self.stride,
                ..self.image
            },
            buffer: &self.memory,
        }
    }

    /// The staging memory, to leak it when the GPU may still be using it.
    pub(crate) fn into_memory(self) -> Memory {
        self.memory
    }

    /// Writes the CPU's stores back before the GPU reads the surface.
    pub(crate) fn clean_for_gpu(&mut self) {
        clean_invalidate_dcache(self.memory.as_mut_slice());
    }

    /// Invalidates the CPU's lines of the surface after the GPU wrote it.
    pub(crate) fn sync_after_gpu(&mut self) {
        clean_invalidate_dcache(self.memory.as_mut_slice());
    }

    /// Copies pixels in from `input`.
    pub(crate) fn upload(&mut self, input: &Input<'_>, transfer: &mut Transfer) -> io::Result<()> {
        let image = self.image;
        let stride = self.stride as usize;
        match input {
            Input::Bytes(bytes) => {
                image.check(bytes.len())?;
                copy_rows(
                    bytes,
                    image.stride as usize,
                    self.memory.as_mut_slice(),
                    stride,
                    image,
                );
            }
            Input::Frame(frame) => {
                let mapping = map(*frame, false)?;
                dma_buf::sync(frame.fd, SYNC_START | SYNC_READ)?;
                copy_rows(
                    mapping.as_slice(),
                    image.stride as usize,
                    self.memory.as_mut_slice(),
                    stride,
                    image,
                );
                dma_buf::sync(frame.fd, SYNC_END | SYNC_READ)?;
            }
        }
        transfer.staged_bytes += image.row_len() * image.height as usize;
        self.clean_for_gpu();
        Ok(())
    }

    /// Copies the result out to `output`. Call after the GPU completed and the cache was
    /// invalidated.
    pub(crate) fn download(
        &mut self,
        output: &mut Output<'_>,
        transfer: &mut Transfer,
    ) -> io::Result<()> {
        let image = self.image;
        let stride = self.stride as usize;
        match output {
            Output::Bytes(bytes) => {
                image.check(bytes.len())?;
                copy_rows(
                    self.memory.as_mut_slice(),
                    stride,
                    bytes,
                    image.stride as usize,
                    image,
                );
            }
            Output::Frame(frame) => {
                let mut mapping = map(*frame, true)?;
                dma_buf::sync(frame.fd, SYNC_START | SYNC_WRITE)?;
                copy_rows(
                    self.memory.as_mut_slice(),
                    stride,
                    mapping.as_mut_slice(),
                    image.stride as usize,
                    image,
                );
                dma_buf::sync(frame.fd, SYNC_END | SYNC_WRITE)?;
            }
        }
        transfer.staged_bytes += image.row_len() * image.height as usize;
        Ok(())
    }
}

/// The CPU's mapping of a frame's dma-buf, whose real size (not the frame's claim) holds the
/// frame.
fn map(frame: Frame<'_>, write: bool) -> io::Result<Mapping> {
    let size = dma_buf::size(frame.fd)?;
    let len = usize::try_from(size).unwrap_or(usize::MAX);
    frame.image.check(len)?;
    if frame.image.frame_len() == 0 {
        return invalid(format!("an empty frame {:?}", frame.image));
    }
    Mapping::new(frame.fd, frame.image.frame_len(), 0, write)
}

/// Copies `image`'s rows from `from` to `to`, each at its stride. Both were checked to hold the
/// image at their stride.
fn copy_rows(from: &[u8], from_stride: usize, to: &mut [u8], to_stride: usize, image: Image) {
    let row = image.row_len();
    for y in 0..image.height as usize {
        to[y * to_stride..][..row].copy_from_slice(&from[y * from_stride..][..row]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Format;
    use std::os::fd::AsFd;

    #[test]
    fn a_frame_is_checked_against_its_dma_buf_real_size() {
        // A sealed memfd stands in for a dma-buf: a descriptor with a real, fixed size.
        let image = Image::packed(Format::Rgba8888, 16, 4);
        let short = dma_buf::sealed_memfd(c"vglite-test", image.frame_len() as u64 - 1).unwrap();
        let exact = dma_buf::sealed_memfd(c"vglite-test", image.frame_len() as u64).unwrap();
        let frame = |fd| Frame { image, fd };
        let err = map(frame(short.as_fd()), false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let mapping = map(frame(exact.as_fd()), false).unwrap();
        assert_eq!(mapping.len(), image.frame_len());
        // A stride the dma-buf cannot hold is refused the same way, whatever the width.
        let padded = Image {
            stride: 128,
            ..image
        };
        assert!(
            map(
                Frame {
                    image: padded,
                    fd: exact.as_fd()
                },
                true
            )
            .is_err()
        );
    }
}
