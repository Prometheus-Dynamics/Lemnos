//! The engine's V4L2 mem2mem node, through `lemnos-driver-m2m-decode`'s [`M2mSession`]: one
//! open of the node set up for a source layout and a destination layout, both queues of one
//! dma-buf buffer, converting one frame at a time.
//!
//! What the node is handed, and what checks it:
//!
//! - The formats are set multi-planar, with the driver's own strides and sizes, and read back:
//!   the driver falls back to its first format and clamps sizes rather than failing, so a format
//!   it changed is refused ([`io::ErrorKind::InvalidInput`]).
//! - Each plane is queued as its own dma-buf, from a [`Frame`] whose dma-bufs were checked to hold
//!   their planes, with the dma-buf's real size as the plane's length. The kernel's videobuf2
//!   checks the size again on import, and the driver checks each plane against its layout.
//! - Nothing a caller gives becomes an address: the engine is given each dma-buf's own start.
//!
//! A conversion on an open device allocates nothing, and so does drawing on a frame
//! ([`Device::draw`]; the checks on what is drawn are [`crate::overlay`]'s).

use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemnos_driver_m2m_decode::{
    Format, LinuxNode, M2mSession, MAX_PLANES, Memory, Node, PixFormatMplane, PlaneFormat,
    PlaneMemory, QueuePlane, QueueSetup, Ready,
};
use lemnos_linux_sys::v4l2::BUF_FLAG_ERROR;
use lemnos_linux_sys::v4l2::device::ExtControl;

use crate::frame::Frame;
use crate::layout::{self, Layout};
use crate::overlay::{self, BORDERS, Mode, Overlay};

/// The driver's name, as `VIDIOC_QUERYCAP` reports it.
pub const DRIVER: &str = "nonai-2d";

/// `V4L2_FIELD_NONE`: progressive frames.
const FIELD_NONE: u32 = 1;

/// How long one conversion may take before it is an error.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(5);

/// The engine's `/dev/video*` node, if this kernel has it.
pub fn find() -> Option<PathBuf> {
    LinuxNode::find(DRIVER).ok().flatten()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The multi-planar V4L2 format of `layout`, with the driver's strides and sizes.
fn format(layout: Layout) -> Format {
    let (width, height) = layout.size();
    let mut planes = [PlaneFormat::default(); MAX_PLANES];
    let ours = layout.planes();
    for (to, from) in planes.iter_mut().zip(ours.iter()) {
        *to = PlaneFormat {
            bytes_per_line: from.stride,
            size_image: from.size,
        };
    }
    Format::Multi(PixFormatMplane {
        width,
        height,
        pixelformat: layout.format().fourcc(),
        field: FIELD_NONE,
        colorspace: 0,
        planes,
        // At most three.
        num_planes: ours.len() as u32,
    })
}

/// Whether the driver kept the fields that describe the frame.
fn kept(got: &Format, layout: Layout) -> bool {
    let Format::Multi(got) = got else {
        return false;
    };
    let planes = layout.planes();
    (got.width, got.height) == layout.size()
        && got.pixelformat == layout.format().fourcc()
        && got.num_planes as usize == planes.len()
        && got
            .planes
            .iter()
            .zip(planes.iter())
            .all(|(g, p)| g.bytes_per_line == p.stride && g.size_image == p.size)
}

/// The planes of `frame` to queue, with `bytes_used` set for a source.
fn queue_planes<'fd>(
    frame: &Frame<'fd>,
    source: bool,
) -> ([QueuePlane<'fd>; layout::MAX_PLANES], usize) {
    let mut out = [QueuePlane {
        memory: PlaneMemory::Mmap,
        length: 0,
        bytes_used: 0,
        data_offset: 0,
    }; layout::MAX_PLANES];
    let planes = frame.layout().planes();
    for (index, (slot, plane)) in out.iter_mut().zip(planes.iter()).enumerate() {
        if let Some((fd, length)) = frame.plane(index) {
            *slot = QueuePlane {
                memory: PlaneMemory::Dmabuf(fd),
                length,
                bytes_used: if source { plane.size } else { 0 },
                data_offset: 0,
            };
        }
    }
    (out, planes.len())
}

/// Setting a control whose value is a byte array (`VIDIOC_S_EXT_CTRLS`), on the handle the
/// queues are on: what [`Device::draw`] needs beside [`Node`].
pub trait Payloads {
    /// Sets control `id` to `payload`. The kernel reads the payload during the call.
    fn set_payload(&self, id: u32, payload: &mut [u8]) -> io::Result<()>;
}

impl Payloads for LinuxNode {
    fn set_payload(&self, id: u32, payload: &mut [u8]) -> io::Result<()> {
        let mut controls = [ExtControl::payload(id, payload)];
        lemnos_linux_sys::v4l2::s_ext_ctrls_in_place(self.as_fd(), None, &mut controls)
    }
}

/// The engine set up to convert frames of one layout into frames of another, of the same size.
/// Dropping it closes the node; the kernel stops streaming and lets go of the dma-bufs and of
/// the OSD images it copied.
pub struct Device<N: Node = LinuxNode> {
    session: M2mSession<N>,
    src: Layout,
    dst: Layout,
    /// The engine's mode as last set, `None` when a draw failed part way and it is not known.
    /// The kernel starts every open in conversion mode.
    mode: Option<Mode>,
    /// Borders set on this open: the kernel keeps each until the node is closed.
    borders: usize,
}

impl<N: Node> std::fmt::Debug for Device<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("src", &self.src)
            .field("dst", &self.dst)
            .finish()
    }
}

impl Device<LinuxNode> {
    /// Opens the engine at `path` to convert `src` frames into `dst` frames.
    pub fn open(path: &Path, src: Layout, dst: Layout) -> io::Result<Self> {
        if src.size() != dst.size() {
            return Err(invalid("the engine does not scale"));
        }
        Self::new(LinuxNode::open(path)?, src, dst)
    }
}

impl<N: Node> Device<N> {
    /// Sets `node` up to convert `src` frames into `dst` frames. The engine does not scale, so
    /// the two must have one size ([`io::ErrorKind::InvalidInput`] otherwise, as for a format the
    /// driver did not keep).
    pub fn new(node: N, src: Layout, dst: Layout) -> io::Result<Self> {
        if src.size() != dst.size() {
            return Err(invalid("the engine does not scale"));
        }
        if !node.capability()?.is_m2m_multiplanar() {
            return Err(io::Error::other(
                "the node is not a multi-planar mem2mem device",
            ));
        }
        let setup = |layout| QueueSetup {
            format: format(layout),
            memory: Memory::DmaBuf,
            buffers: 1,
            crop: None,
        };
        let mut session = M2mSession::new(node, setup(src), setup(dst))?;
        if !kept(session.output_format(), src) || !kept(session.capture_format(), dst) {
            return Err(invalid("the device did not keep a format it was given"));
        }
        session.stream_on()?;
        Ok(Self {
            session,
            src,
            dst,
            mode: Some(Mode::Convert),
            borders: 0,
        })
    }

    /// The source and destination layouts the device was set up for.
    pub fn layouts(&self) -> (Layout, Layout) {
        (self.src, self.dst)
    }

    /// Converts `src` into `dst` and waits for it. Both must be in the layouts the device was
    /// set up for ([`io::ErrorKind::InvalidInput`] otherwise, before anything is queued). The
    /// device has let go of both frames when this returns `Ok`.
    pub fn convert(&mut self, src: &Frame<'_>, dst: &Frame<'_>) -> io::Result<()> {
        if src.layout() != self.src || dst.layout() != self.dst {
            return Err(invalid("a frame in another layout than the device's"));
        }
        self.converting()?;
        let (capture, capture_len) = queue_planes(dst, false);
        let (output, output_len) = queue_planes(src, true);
        self.session.queue_capture(0, &capture[..capture_len])?;
        self.session
            .queue_output(0, &output[..output_len], Duration::ZERO)?;
        self.finish()
    }

    /// Draws `overlay` on `frame` in place and waits for it. The device must be set up with one
    /// layout on both sides ([`Device::open`] with `src == dst`), `frame` and `overlay` in it,
    /// and `overlay` must have something to draw and no fewer borders than this open of the
    /// node was given before (the kernel keeps them until it is closed): otherwise
    /// [`io::ErrorKind::InvalidInput`] before anything is set or queued.
    ///
    /// The frame is queued on the capture side while the engine is in conversion mode, where
    /// it starts nothing; the regions, borders and mode are set; then the frame is queued on
    /// the output side, which runs the engine once (the kernel's own start on queueing a
    /// capture buffer in a drawing mode would run it a second time). The output side is never
    /// read in a drawing mode. The engine goes back to conversion mode after.
    pub fn draw(&mut self, frame: &Frame<'_>, overlay: &Overlay<'_>) -> io::Result<()>
    where
        N: Payloads,
    {
        if self.src != self.dst {
            return Err(invalid("drawing needs one layout on both sides"));
        }
        if frame.layout() != self.dst || overlay.layout() != self.dst {
            return Err(invalid("a frame in another layout than the device's"));
        }
        let mode = overlay.mode().ok_or_else(|| invalid("nothing to draw"))?;
        let borders = overlay.borders().len();
        if borders > 0 && borders < self.borders {
            return Err(invalid(
                "the node keeps the borders it was given until it is reopened",
            ));
        }
        self.converting()?;
        let node = self.session.node();
        let mut osd = overlay.osd_control();
        node.set_payload(overlay::CID_OSD_ATTR, &mut osd)?;
        for index in 0..borders.min(BORDERS) {
            if let Some(mut control) = overlay.border_control(index) {
                node.set_payload(overlay::CID_BORDER_ATTR, &mut control)?;
                self.borders = self.borders.max(index + 1);
            }
        }
        let (capture, capture_len) = queue_planes(frame, false);
        self.session.queue_capture(0, &capture[..capture_len])?;
        // From here on a failure leaves the node in a state this side cannot know.
        self.mode = None;
        self.session.set_control(overlay::CID_MODE, mode as i32)?;
        let (output, output_len) = queue_planes(frame, true);
        self.session
            .queue_output(0, &output[..output_len], Duration::ZERO)?;
        self.finish()?;
        self.session
            .set_control(overlay::CID_MODE, Mode::Convert as i32)?;
        self.mode = Some(Mode::Convert);
        Ok(())
    }

    /// Refuses to queue anything unless the engine is known to be in conversion mode.
    fn converting(&self) -> io::Result<()> {
        if self.mode == Some(Mode::Convert) {
            Ok(())
        } else {
            Err(io::Error::other(
                "a draw failed part way and the node's state is unknown: reopen it",
            ))
        }
    }

    /// Waits for the device to give both buffers back.
    fn finish(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + CONVERT_TIMEOUT;
        let (mut source_back, mut written) = (false, false);
        while !source_back || !written {
            let left = deadline.saturating_duration_since(Instant::now());
            let want = Ready {
                output: !source_back,
                capture: !written,
                event: false,
            };
            if left.is_zero() || self.session.wait(want, Some(left))? == Ready::default() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the device did not finish the frame",
                ));
            }
            if !source_back && self.session.dequeue_output()?.is_some() {
                source_back = true;
            }
            if !written && let Some(done) = self.session.dequeue_capture()? {
                if done.flags & BUF_FLAG_ERROR != 0 {
                    return Err(io::Error::other("the device marked the frame failed"));
                }
                written = true;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
