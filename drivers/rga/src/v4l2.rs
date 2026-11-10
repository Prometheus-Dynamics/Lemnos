//! The mainline route: a V4L2 memory-to-memory device (`rockchip-rga` on a board, any mem2mem
//! scaler or converter in principle) through `lemnos-driver-m2m-decode`'s [`M2mSession`], which
//! knows queues, formats and buffers and not what the device does. This module maps a conversion
//! onto it: one session per (source layout, destination layout, crop, transform, memory),
//! configured once and reused.
//!
//! What the device is handed, and what checks it:
//!
//! - Byte buffers are copied into and out of the device's own MMAP buffers: the source only when
//!   it holds the device's whole frame, and the result only up to the bytes the device reported,
//!   the mapping's length and the caller's buffer, whichever is least.
//! - A dma-buf is passed by descriptor with its real size, asked of the kernel (not a length the
//!   caller states), and refused when smaller than the frame the device took. The kernel's
//!   videobuf2 checks the size again when it imports the buffer.
//!
//! Steady state: a conversion on a session that already exists allocates nothing; a new
//! (layout, crop, transform) opens a session, and at most [`MAX_SESSIONS`] are kept.

use std::io;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_driver_m2m_decode::{
    Format, LinuxNode, M2mSession, Memory as QueueMemory, Node, PixFormat, PixFormatMplane,
    PlaneFormat, PlaneMemory, QueuePlane, QueueSetup, Ready, Rect as V4l2Rect,
};
use lemnos_linux_sys::v4l2::{CID_HFLIP, CID_ROTATE, CID_VFLIP, MAX_PLANES};

use crate::Transform;
use crate::caps::Route;
use crate::format::{Layout, PixelFormat, Rect};
use crate::resize::{Filter, source_size};
use crate::transform::{check_crop, check_size};

/// `V4L2_FIELD_NONE`: progressive frames.
const FIELD_NONE: u32 = 1;

/// How long one conversion may take before it is an error.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(5);

/// Sessions kept open at once; the oldest is closed for a new one past this.
pub const MAX_SESSIONS: usize = 8;

/// The first `/dev/video*` mem2mem device whose driver is `driver` (`"rockchip-rga"`, `"vim2m"`).
pub fn find_m2m_device(driver: &str) -> Option<PathBuf> {
    LinuxNode::find(driver).ok().flatten()
}

/// How a session's buffers are provided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Memory {
    /// Driver buffers mapped into the process: bytes are copied in and out.
    Mmap,
    /// The caller's dma-bufs, queued per conversion: no copies.
    Dmabuf,
}

fn other(message: String) -> io::Error {
    io::Error::other(message)
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// A configured mem2mem context: `src` into `dst`, scaling to `dst`'s size, from `crop` of the
/// source when given, flipped or turned by `transform`. One buffer per queue, index 0.
pub(crate) struct Session<N: Node> {
    session: M2mSession<N>,
    memory: Memory,
    in_len: usize,
    out_len: usize,
}

impl<N: Node> Session<N> {
    pub(crate) fn open(
        node: N,
        src: Layout,
        dst: Layout,
        crop: Option<Rect>,
        transform: Option<Transform>,
        memory: Memory,
    ) -> io::Result<Self> {
        let cap = node.capability()?;
        if !cap.is_m2m() {
            return Err(other("the node is not mem2mem".into()));
        }
        let mplane = cap.is_m2m_multiplanar();
        let mem = match memory {
            Memory::Mmap => QueueMemory::Mmap,
            Memory::Dmabuf => QueueMemory::DmaBuf,
        };
        let setup = |layout: Layout, crop: Option<Rect>| -> io::Result<QueueSetup> {
            Ok(QueueSetup {
                format: format(layout, mplane)?,
                memory: mem,
                buffers: 1,
                crop: crop.map(|r| V4l2Rect {
                    left: r.left,
                    top: r.top,
                    width: r.width,
                    height: r.height,
                }),
            })
        };
        let mut session = M2mSession::new(node, setup(src, crop)?, setup(dst, None)?)?;
        let in_len = kept(session.output_format(), src)?;
        let out_len = kept(session.capture_format(), dst)?;
        if let Some(r) = crop {
            let got = session.output_crop();
            if got.map(|g| (g.left, g.top, g.width, g.height))
                != Some((r.left, r.top, r.width, r.height))
            {
                return Err(other(format!("the device adjusted crop {r:?} to {got:?}")));
            }
        }
        if let Some(t) = transform {
            let controls: &[(u32, i32)] = match t {
                Transform::FlipHorizontal => &[(CID_HFLIP, 1)],
                Transform::FlipVertical => &[(CID_VFLIP, 1)],
                Transform::Rotate90 => &[(CID_ROTATE, 90)],
                Transform::Rotate180 => &[(CID_ROTATE, 180)],
                Transform::Rotate270 => &[(CID_ROTATE, 270)],
            };
            for &(id, value) in controls {
                let kept = session.set_control(id, value).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("the device does not take {t:?} (control {id:#x}: {e})"),
                    )
                })?;
                if kept != value {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("the device kept {kept} for {t:?} (control {id:#x})"),
                    ));
                }
            }
        }
        session.stream_on()?;
        Ok(Self {
            session,
            memory,
            in_len,
            out_len,
        })
    }

    /// The frame sizes the device expects in and gives out.
    pub(crate) fn frame_lens(&self) -> (usize, usize) {
        (self.in_len, self.out_len)
    }

    /// Convert one frame between byte buffers (`Mmap` sessions); returns the bytes written.
    pub(crate) fn convert_bytes(&mut self, src: &[u8], dst: &mut [u8]) -> io::Result<usize> {
        if self.memory != Memory::Mmap {
            return Err(other("a dma-buf session takes dma-bufs".into()));
        }
        let in_len = self.in_len;
        let out = self
            .session
            .output_plane_mut(0, 0)
            .ok_or_else(|| other("the source buffer is not mapped".into()))?;
        if src.len() < in_len || out.len() < in_len {
            return Err(invalid(format!(
                "a source of {} bytes for a device frame of {in_len} (mapped {})",
                src.len(),
                out.len()
            )));
        }
        out[..in_len].copy_from_slice(&src[..in_len]);
        let used = u32::try_from(in_len).map_err(|_| other("a frame over 4 GiB".into()))?;
        let mmap = |bytes_used| QueuePlane {
            memory: PlaneMemory::Mmap,
            length: 0,
            bytes_used,
            data_offset: 0,
        };
        self.session.queue_capture(0, &[mmap(0)])?;
        self.session
            .queue_output(0, &[mmap(used)], Duration::ZERO)?;
        let used = self.finish()?;
        let cap = self
            .session
            .capture_plane(0, 0)
            .ok_or_else(|| other("the destination buffer is not mapped".into()))?;
        let used = used.min(cap.len()).min(dst.len());
        dst[..used].copy_from_slice(&cap[..used]);
        Ok(used)
    }

    /// Convert one frame from dma-buf `src` into dma-buf `dst` (`Dmabuf` sessions): no copies.
    pub(crate) fn convert_dmabuf(
        &mut self,
        src: BorrowedFd<'_>,
        dst: BorrowedFd<'_>,
    ) -> io::Result<usize> {
        if self.memory != Memory::Dmabuf {
            return Err(other("a byte session takes byte buffers".into()));
        }
        let size = |fd| -> io::Result<u32> {
            let bytes = lemnos_linux_sys::dma_buf::size(fd)?;
            // A larger buffer is offered as the most a plane length can say.
            Ok(u32::try_from(bytes).unwrap_or(u32::MAX))
        };
        let (src_len, dst_len) = (size(src)?, size(dst)?);
        if (src_len as usize) < self.in_len || (dst_len as usize) < self.out_len {
            return Err(invalid(format!(
                "dma-bufs of {src_len} and {dst_len} bytes; the device needs {} and {}",
                self.in_len, self.out_len
            )));
        }
        let plane = |fd, length, bytes_used| QueuePlane {
            memory: PlaneMemory::Dmabuf(fd),
            length,
            bytes_used,
            data_offset: 0,
        };
        let used = u32::try_from(self.in_len).map_err(|_| other("a frame over 4 GiB".into()))?;
        self.session.queue_capture(0, &[plane(dst, dst_len, 0)])?;
        self.session
            .queue_output(0, &[plane(src, src_len, used)], Duration::ZERO)?;
        self.finish()
    }

    /// Wait for the device to give both buffers back; returns the bytes it wrote.
    fn finish(&mut self) -> io::Result<usize> {
        let (mut source_back, mut written) = (false, None);
        while !source_back || written.is_none() {
            let want = Ready {
                output: !source_back,
                capture: written.is_none(),
                event: false,
            };
            let ready = self.session.wait(want, Some(CONVERT_TIMEOUT))?;
            if ready == Ready::default() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the device did not finish the frame",
                ));
            }
            if !source_back && self.session.dequeue_output()?.is_some() {
                source_back = true;
            }
            if written.is_none()
                && let Some(done) = self.session.dequeue_capture()?
            {
                written = Some(done.bytes_used[0] as usize);
            }
        }
        Ok(written.unwrap_or_default())
    }
}

/// The V4L2 format of `layout` (one memory plane).
fn format(layout: Layout, mplane: bool) -> io::Result<Format> {
    let size_image = u32::try_from(layout.frame_len())
        .map_err(|_| invalid(format!("{layout:?} is over 4 GiB")))?;
    Ok(if mplane {
        let mut planes = [PlaneFormat::default(); MAX_PLANES];
        planes[0] = PlaneFormat {
            size_image,
            bytes_per_line: layout.stride,
        };
        Format::Multi(PixFormatMplane {
            width: layout.width,
            height: layout.height,
            pixelformat: layout.format.fourcc(),
            field: FIELD_NONE,
            colorspace: 0,
            planes,
            num_planes: 1,
        })
    } else {
        Format::Single(PixFormat {
            width: layout.width,
            height: layout.height,
            pixelformat: layout.format.fourcc(),
            field: FIELD_NONE,
            bytes_per_line: layout.stride,
            size_image,
            colorspace: 0,
        })
    })
}

/// The frame size of the format the driver kept, or an error when it did not take `layout`.
fn kept(format: &Format, layout: Layout) -> io::Result<usize> {
    let (got, w, h, size) = match format {
        Format::Single(p) => (p.pixelformat, p.width, p.height, p.size_image),
        Format::Multi(m) => (m.pixelformat, m.width, m.height, m.planes[0].size_image),
    };
    if got != layout.format.fourcc() || (w, h) != (layout.width, layout.height) {
        return Err(other(format!(
            "the device does not take {:?} at {}x{} (it chose {w}x{h} of {got:#x})",
            layout.format, layout.width, layout.height
        )));
    }
    Ok(size as usize)
}

type Key = (Layout, Layout, Option<Rect>, Option<Transform>, Memory);

/// A 2D engine on a mem2mem device. Each distinct (source layout, destination layout, crop,
/// transform, memory) gets its own session on its own open of the node, configured once and
/// reused.
pub struct M2mRga<N: Node = LinuxNode> {
    open: Box<dyn FnMut() -> io::Result<N>>,
    sessions: Vec<(Key, Session<N>)>,
}

impl M2mRga<LinuxNode> {
    /// The engine, when the kernel exposes `rockchip-rga`.
    pub fn find() -> Option<Self> {
        Some(Self::on_device(&find_m2m_device("rockchip-rga")?))
    }

    /// The engine on any V4L2 mem2mem device (`vim2m` for tests).
    pub fn on_device(path: &Path) -> Self {
        let path = path.to_path_buf();
        Self::with_opener(move || LinuxNode::open(&path))
    }
}

impl<N: Node> M2mRga<N> {
    /// The engine on the nodes `open` gives, one per session (a test's played-back device).
    pub fn with_opener(open: impl FnMut() -> io::Result<N> + 'static) -> Self {
        Self {
            open: Box::new(open),
            sessions: Vec::with_capacity(MAX_SESSIONS),
        }
    }

    /// Sessions open now.
    pub fn sessions(&self) -> usize {
        self.sessions.len()
    }

    fn session(
        &mut self,
        src: Layout,
        dst: Layout,
        crop: Option<Rect>,
        transform: Option<Transform>,
        memory: Memory,
    ) -> io::Result<&mut Session<N>> {
        let key = (src, dst, crop, transform, memory);
        let at = match self.sessions.iter().position(|(k, _)| *k == key) {
            Some(at) => at,
            None => {
                let session = Session::open((self.open)()?, src, dst, crop, transform, memory)?;
                if self.sessions.len() >= MAX_SESSIONS {
                    self.sessions.remove(0);
                }
                self.sessions.push((key, session));
                self.sessions.len() - 1
            }
        };
        Ok(&mut self.sessions[at].1)
    }

    /// Convert, scale (to `dst`'s size) and crop (`crop` of the source) from dma-buf `src_fd`
    /// into dma-buf `dst_fd`, without a CPU copy. Each dma-buf's size is asked of the kernel.
    /// Returns the bytes the engine wrote.
    pub fn blit(
        &mut self,
        src: Layout,
        src_fd: BorrowedFd<'_>,
        crop: Option<Rect>,
        dst: Layout,
        dst_fd: BorrowedFd<'_>,
    ) -> io::Result<usize> {
        self.session(src, dst, crop, None, Memory::Dmabuf)?
            .convert_dmabuf(src_fd, dst_fd)
    }

    /// [`M2mRga::blit`] between byte buffers (the engine's own buffers, with a copy in and
    /// out). Returns the bytes written.
    pub fn convert(
        &mut self,
        src: Layout,
        src_bytes: &[u8],
        crop: Option<Rect>,
        dst: Layout,
        dst_bytes: &mut [u8],
    ) -> io::Result<usize> {
        self.session(src, dst, crop, None, Memory::Mmap)?
            .convert_bytes(src_bytes, dst_bytes)
    }

    /// The frame sizes the device expects for a conversion.
    pub fn frame_lens(&mut self, src: Layout, dst: Layout) -> io::Result<(usize, usize)> {
        Ok(self
            .session(src, dst, None, None, Memory::Mmap)?
            .frame_lens())
    }

    /// The source crop for a resize: the whole frame, or the top-left part an area
    /// downscale reads when the size is not a multiple of the factor.
    fn resize_crop(src: Layout, dst: Layout, filter: Filter) -> io::Result<Option<Rect>> {
        let (width, height) = source_size(
            (src.width, src.height),
            (dst.width, dst.height),
            filter,
            Route::V4l2,
        )?;
        Ok(
            ((width, height) != (src.width, src.height)).then_some(Rect {
                left: 0,
                top: 0,
                width,
                height,
            }),
        )
    }

    /// Resize dma-buf `src_fd` into dma-buf `dst_fd` (to `dst`'s size) with `filter`. The
    /// mainline driver has no filter control: it averages when it downscales and
    /// interpolates when it upscales, so [`Filter::Area`] must be an integer downscale and
    /// [`Filter::Bilinear`] an upscale; anything else is `Unsupported`. See
    /// [`crate::caps`] for what is known of each unit's exactness. Returns the bytes written.
    pub fn resize(
        &mut self,
        src: Layout,
        src_fd: BorrowedFd<'_>,
        dst: Layout,
        dst_fd: BorrowedFd<'_>,
        filter: Filter,
    ) -> io::Result<usize> {
        let crop = Self::resize_crop(src, dst, filter)?;
        self.blit(src, src_fd, crop, dst, dst_fd)
    }

    /// [`M2mRga::resize`] between byte buffers. Returns the bytes written.
    pub fn resize_bytes(
        &mut self,
        src: Layout,
        src_bytes: &[u8],
        dst: Layout,
        dst_bytes: &mut [u8],
        filter: Filter,
    ) -> io::Result<usize> {
        let crop = Self::resize_crop(src, dst, filter)?;
        self.convert(src, src_bytes, crop, dst, dst_bytes)
    }

    /// Copy the `rect` of dma-buf `src_fd` into dma-buf `dst_fd`, whose layout must have the
    /// rectangle's size (a different size is a resize: [`M2mRga::resize`]). A full-frame
    /// rectangle is a copy. Returns the bytes written.
    pub fn crop(
        &mut self,
        src: Layout,
        src_fd: BorrowedFd<'_>,
        rect: Rect,
        dst: Layout,
        dst_fd: BorrowedFd<'_>,
    ) -> io::Result<usize> {
        let crop = Self::crop_rect(src, rect, dst)?;
        self.blit(src, src_fd, crop, dst, dst_fd)
    }

    /// [`M2mRga::crop`] between byte buffers. Returns the bytes written.
    pub fn crop_bytes(
        &mut self,
        src: Layout,
        src_bytes: &[u8],
        rect: Rect,
        dst: Layout,
        dst_bytes: &mut [u8],
    ) -> io::Result<usize> {
        let crop = Self::crop_rect(src, rect, dst)?;
        self.convert(src, src_bytes, crop, dst, dst_bytes)
    }

    /// Flip or turn dma-buf `src_fd` into dma-buf `dst_fd`, whose layout has
    /// [`Transform::output_size`] of the source (the driver's `V4L2_CID_HFLIP`, `VFLIP` and
    /// `ROTATE` controls). A device that does not keep the control is `Unsupported`.
    /// Returns the bytes written.
    pub fn transform(
        &mut self,
        src: Layout,
        src_fd: BorrowedFd<'_>,
        dst: Layout,
        dst_fd: BorrowedFd<'_>,
        transform: Transform,
    ) -> io::Result<usize> {
        check_size(
            (dst.width, dst.height),
            transform.output_size(src.width, src.height),
        )?;
        self.session(src, dst, None, Some(transform), Memory::Dmabuf)?
            .convert_dmabuf(src_fd, dst_fd)
    }

    /// [`M2mRga::transform`] between byte buffers. Returns the bytes written.
    pub fn transform_bytes(
        &mut self,
        src: Layout,
        src_bytes: &[u8],
        dst: Layout,
        dst_bytes: &mut [u8],
        transform: Transform,
    ) -> io::Result<usize> {
        check_size(
            (dst.width, dst.height),
            transform.output_size(src.width, src.height),
        )?;
        self.session(src, dst, None, Some(transform), Memory::Mmap)?
            .convert_bytes(src_bytes, dst_bytes)
    }

    /// The device crop for `rect`: none for the full frame.
    fn crop_rect(src: Layout, rect: Rect, dst: Layout) -> io::Result<Option<Rect>> {
        let (Ok(left), Ok(top)) = (u32::try_from(rect.left), u32::try_from(rect.top)) else {
            return Err(invalid(format!("crop {rect:?} starts outside the frame")));
        };
        check_crop((left, top, rect.width, rect.height), src.width, src.height)?;
        check_size((dst.width, dst.height), (rect.width, rect.height))?;
        let full = (left, top, rect.width, rect.height) == (0, 0, src.width, src.height);
        Ok((!full).then_some(rect))
    }

    /// RGB8 to RGBA8, tightly packed. The mainline driver has no R,G,B,A order, so the
    /// frames are relabelled: RGB bytes read as `BGR24` and written as `ABGR32` (B,G,R,A)
    /// come out R,G,B,A.
    pub fn rgb_to_rgba(
        &mut self,
        width: u32,
        height: u32,
        src: &[u8],
        dst: &mut [u8],
    ) -> io::Result<()> {
        let s = Layout::packed(PixelFormat::Bgr24, width, height);
        let d = Layout::packed(PixelFormat::Abgr32, width, height);
        self.convert(s, src, None, d, dst).map(drop)
    }

    /// RGBA8 to RGB8 (the reverse relabelling).
    pub fn rgba_to_rgb(
        &mut self,
        width: u32,
        height: u32,
        src: &[u8],
        dst: &mut [u8],
    ) -> io::Result<()> {
        let s = Layout::packed(PixelFormat::Abgr32, width, height);
        let d = Layout::packed(PixelFormat::Bgr24, width, height);
        self.convert(s, src, None, d, dst).map(drop)
    }
}

#[cfg(test)]
mod tests;
