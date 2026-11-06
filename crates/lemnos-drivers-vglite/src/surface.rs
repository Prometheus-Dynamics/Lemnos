//! The buffers VGLite reads and writes: the caller's dmabuf mapped in place, or a staging buffer
//! the kernel allocates (`VG_LITE_ALLOCATE`, physically contiguous) with the caller's pixels
//! copied in and out.
//!
//! The kernel driver maps no user memory, and its dmabuf map keeps only the last scatter-list
//! entry's address (`vg_lite_hal_map`), so a dmabuf maps correctly only when it is one
//! physically contiguous block. Which it is cannot be read from user space: the caller says
//! ([`Backing`]).

use std::io;
use std::os::fd::BorrowedFd;

use lemnos_linux::dma::{Access, DmaBufRef, Mapping};
use lemnos_linux_sys::vg_lite::{Device, Memory, clean_invalidate_dcache};

use crate::command::Bound;

use crate::plan::Image;

/// What backs a dmabuf, as far as VGLite's kernel driver is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backing {
    /// One physically contiguous block: a CMA heap (`linux,cma`) or the K230's MMZ. VGLite reads
    /// and writes it in place when its stride meets the alignment.
    Contiguous,
    /// Pages (the `system` heap, most V4L2 and DRM buffers): copied through a contiguous
    /// staging buffer. Mapping it directly would read and write the wrong memory.
    Scattered,
}

/// A dmabuf holding one image.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// The image's layout.
    pub image: Image,
    /// The dmabuf's descriptor, borrowed for the call.
    pub fd: BorrowedFd<'a>,
    /// What backs the dmabuf.
    pub backing: Backing,
}

impl Frame<'_> {
    /// VGLite can take it in place.
    pub(crate) fn direct(&self) -> bool {
        self.backing == Backing::Contiguous && self.image.direct()
    }
}

/// What an operation cost beyond the unit's own work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transfer {
    /// Bytes the CPU copied into and out of staging buffers; zero when every buffer was a
    /// directly mapped contiguous dmabuf.
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

/// A buffer the GPU reads or writes for one operation, released when dropped.
///
/// The field order matters: `memory` is released (`VG_LITE_FREE` or `VG_LITE_UNMAP`) before
/// `mapping`, the CPU view of a dmabuf that the kernel holds its logical address to.
pub(crate) struct Surface {
    image: Image,
    /// The stride the GPU sees: the caller's for a mapped dmabuf, the library's for a staged one.
    stride: u32,
    memory: Memory,
    mapping: Option<Mapping>,
    staged: bool,
}

impl Surface {
    /// A staging buffer for `image`, at the stride the library's allocator gives.
    fn staging(device: &Device, image: Image) -> io::Result<Self> {
        let stride = image.staged_stride();
        let bytes = stride
            .checked_mul(image.height)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "image too large"))?;
        let memory = device.allocate(bytes)?;
        Ok(Self {
            image,
            stride,
            memory,
            mapping: None,
            staged: true,
        })
    }

    /// The caller's contiguous dmabuf, in place.
    fn mapped(device: &Device, frame: Frame<'_>) -> io::Result<Self> {
        let dmabuf = DmaBufRef::from_fd(frame.fd)?;
        frame.image.check(dmabuf.len())?;
        let mut mapping = dmabuf.map()?;
        let bytes = frame
            .image
            .stride
            .checked_mul(frame.image.height)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "image too large"))?;
        // The kernel keeps the logical address for as long as `memory` lives; `memory` is
        // dropped before `mapping` (the field order).
        let memory = device.map_dmabuf(frame.fd, mapping.as_mut_slice(), bytes)?;
        Ok(Self {
            image: frame.image,
            stride: frame.image.stride,
            memory,
            mapping: Some(mapping),
            staged: false,
        })
    }

    /// The surface for reading `input`, its pixels copied in when staged.
    pub(crate) fn input(
        device: &Device,
        image: Image,
        input: &Input<'_>,
        transfer: &mut Transfer,
    ) -> io::Result<Self> {
        match input {
            Input::Frame(frame) if frame.direct() => Self::mapped(device, *frame),
            _ => {
                let mut surface = Self::staging(device, image)?;
                surface.upload(input, transfer)?;
                Ok(surface)
            }
        }
    }

    /// The surface for writing `output`. With `keep`, its current pixels matter (a blend or a
    /// drawing onto it) and are copied in when staged.
    pub(crate) fn output(
        device: &Device,
        image: Image,
        output: &mut Output<'_>,
        keep: bool,
        transfer: &mut Transfer,
    ) -> io::Result<Self> {
        match output {
            Output::Frame(frame) if frame.direct() => Self::mapped(device, *frame),
            _ => {
                let mut surface = Self::staging(device, image)?;
                if keep {
                    let input = match output {
                        Output::Bytes(bytes) => Input::Bytes(bytes),
                        Output::Frame(frame) => Input::Frame(*frame),
                    };
                    surface.upload(&input, transfer)?;
                }
                Ok(surface)
            }
        }
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

    /// The bytes the surface's pixels live in: the mapping of a dmabuf, else the allocation.
    fn pixels(&mut self) -> io::Result<&mut [u8]> {
        if let Some(mapping) = self.mapping.as_mut() {
            return Ok(mapping.as_mut_slice());
        }
        self.memory
            .as_mut_slice()
            .ok_or_else(|| io::Error::other("a staging buffer without a CPU view"))
    }

    /// Writes the CPU's stores back before the GPU reads the surface (the library cleans the
    /// source before a blit).
    pub(crate) fn clean_for_gpu(&mut self) -> io::Result<()> {
        clean_invalidate_dcache(self.pixels()?);
        Ok(())
    }

    /// Invalidates the CPU's cache lines of the surface after the GPU wrote it (the library does
    /// this for the target when a submit completes).
    pub(crate) fn sync_after_gpu(&mut self) -> io::Result<()> {
        clean_invalidate_dcache(self.pixels()?);
        Ok(())
    }

    /// Copies pixels in from `input` (a staged surface only needs it; the caller checks that).
    fn upload(&mut self, input: &Input<'_>, transfer: &mut Transfer) -> io::Result<()> {
        let image = self.image;
        let stride = self.stride as usize;
        match input {
            Input::Bytes(bytes) => {
                let pixels = self.pixels()?;
                copy_rows(bytes, image.stride as usize, pixels, stride, image);
            }
            Input::Frame(frame) => {
                let dmabuf = DmaBufRef::from_fd(frame.fd)?;
                frame.image.check(dmabuf.len())?;
                let mapping = dmabuf.map()?;
                dmabuf.begin_cpu_access(Access::Read)?;
                let pixels = self.pixels()?;
                copy_rows(
                    mapping.as_slice(),
                    image.stride as usize,
                    pixels,
                    stride,
                    image,
                );
                dmabuf.end_cpu_access(Access::Read)?;
            }
        }
        transfer.staged_bytes += image.row_len() * image.height as usize;
        self.clean_for_gpu()
    }

    /// Copies a staged result out to `output`; nothing to do for a mapped dmabuf. Call after
    /// the submit completed and the cache was invalidated.
    pub(crate) fn download(
        &mut self,
        output: &mut Output<'_>,
        transfer: &mut Transfer,
    ) -> io::Result<()> {
        if !self.staged {
            return Ok(());
        }
        let image = self.image;
        let stride = self.stride as usize;
        match output {
            Output::Bytes(bytes) => {
                let pixels = self.pixels()?;
                copy_rows(pixels, stride, bytes, image.stride as usize, image);
            }
            Output::Frame(frame) => {
                let dmabuf = DmaBufRef::from_fd(frame.fd)?;
                let mut mapping = dmabuf.map()?;
                dmabuf.begin_cpu_access(Access::Write)?;
                let pixels = self.pixels()?;
                copy_rows(
                    pixels,
                    stride,
                    mapping.as_mut_slice(),
                    image.stride as usize,
                    image,
                );
                dmabuf.end_cpu_access(Access::Write)?;
            }
        }
        transfer.staged_bytes += image.row_len() * image.height as usize;
        Ok(())
    }
}

fn copy_rows(from: &[u8], from_stride: usize, to: &mut [u8], to_stride: usize, image: Image) {
    let row = image.row_len();
    for y in 0..image.height as usize {
        to[y * to_stride..][..row].copy_from_slice(&from[y * from_stride..][..row]);
    }
}
