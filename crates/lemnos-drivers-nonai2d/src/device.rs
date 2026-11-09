use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux::dma::DmaBufRef;
use lemnos_linux::v4l2::m2m::{M2mSession, QueueSetup, Ready};
use lemnos_linux::v4l2::{
    self as lv, Format, PixFormatMplane, PlaneFormat, PlaneMemory, QueuePlane,
};
use lemnos_v4l2::FourCc;

use crate::layout::{Layout, Plane};

/// The driver's name, as `VIDIOC_QUERYCAP` reports it.
pub const DRIVER: &str = "nonai-2d";

/// `V4L2_FIELD_NONE`: progressive frames.
const FIELD_NONE: u32 = 1;

/// How long one conversion may take before it is an error.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(5);

/// The engine's `/dev/video*` node, if this kernel has it.
pub fn find() -> Option<PathBuf> {
    lv::find_m2m(DRIVER).ok().flatten().map(|(path, _)| path)
}

fn format(layout: Layout) -> Format {
    let (width, height) = layout.size();
    Format::Multi(PixFormatMplane {
        width,
        height,
        fourcc: FourCc(layout.format().fourcc()),
        field: FIELD_NONE,
        planes: layout
            .planes()
            .iter()
            .map(|p| PlaneFormat {
                bytes_per_line: p.stride,
                size_image: p.size,
            })
            .collect(),
        ..Default::default()
    })
}

/// Refuses a format the driver changed: it falls back to its first format and clamps sizes.
fn check_kept(got: &Format, layout: Layout) -> io::Result<()> {
    if matches_ignoring_extras(got, layout) {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "the device did not keep {layout:?}: it chose {got:?}"
    )))
}

/// Whether the driver kept the fields that describe the frame.
fn matches_ignoring_extras(got: &Format, layout: Layout) -> bool {
    let Format::Multi(got) = got else {
        return false;
    };
    let planes = layout.planes();
    (got.width, got.height) == layout.size()
        && got.fourcc == FourCc(layout.format().fourcc())
        && got.planes.len() == planes.len()
        && got
            .planes
            .iter()
            .zip(&planes)
            .all(|(g, p)| g.bytes_per_line == p.stride && g.size_image == p.size)
}

/// Checks a frame's dma-bufs against its planes.
fn check_buffers(what: &str, buffers: &[DmaBufRef<'_>], planes: &[Plane]) -> io::Result<()> {
    let lens: Vec<usize> = buffers.iter().map(DmaBufRef::len).collect();
    if fits(&lens, planes) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what}: dma-bufs of {lens:?} bytes for planes {planes:?}"),
        ))
    }
}

/// Whether there is one buffer per plane, each large enough.
fn fits(lens: &[usize], planes: &[Plane]) -> bool {
    lens.len() == planes.len() && lens.iter().zip(planes).all(|(&l, p)| l >= p.size as usize)
}

fn queue_plane<'a>(buffer: &DmaBufRef<'a>, bytes_used: u32) -> io::Result<QueuePlane<'a>> {
    Ok(QueuePlane {
        memory: PlaneMemory::Dmabuf(buffer.as_fd()),
        length: u32::try_from(buffer.len()).map_err(io::Error::other)?,
        bytes_used,
        data_offset: 0,
    })
}

/// A conversion context: frames of one layout into frames of another, of the same size.
pub struct Nonai2d {
    session: M2mSession,
    src: Vec<Plane>,
    dst: Vec<Plane>,
}

impl Nonai2d {
    /// Opens the engine at `path` to convert `src` frames into `dst` frames. The engine does not
    /// scale, so the two must have one size.
    pub fn open(path: &Path, src: Layout, dst: Layout) -> io::Result<Self> {
        if src.size() != dst.size() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the engine does not scale: {src:?} into {dst:?}"),
            ));
        }
        let cap = lv::query(path)?;
        if !cap.is_m2m_multiplanar() {
            return Err(io::Error::other(format!(
                "{} is not a multi-planar mem2mem device",
                path.display()
            )));
        }
        let setup = |layout| QueueSetup {
            format: format(layout),
            memory: lv::Memory::DmaBuf,
            buffers: 1,
            crop: None,
        };
        let mut session = M2mSession::open(path, setup(src), setup(dst))?;
        check_kept(session.output_format(), src)?;
        check_kept(session.capture_format(), dst)?;
        session.stream_on()?;
        Ok(Self {
            session,
            src: src.planes(),
            dst: dst.planes(),
        })
    }

    /// Converts one frame and waits for it. `src` and `dst` hold one dma-buf per plane, physically
    /// contiguous.
    pub fn convert(&mut self, src: &[DmaBufRef<'_>], dst: &[DmaBufRef<'_>]) -> io::Result<()> {
        check_buffers("source", src, &self.src)?;
        check_buffers("destination", dst, &self.dst)?;
        let capture = dst
            .iter()
            .map(|b| queue_plane(b, 0))
            .collect::<io::Result<Vec<_>>>()?;
        let output = src
            .iter()
            .zip(&self.src)
            .map(|(b, p)| queue_plane(b, p.size))
            .collect::<io::Result<Vec<_>>>()?;
        self.session.queue_capture(0, &capture)?;
        self.session.queue_output(0, &output, Duration::ZERO)?;
        self.finish()
    }

    /// Waits for the device to give both buffers back.
    fn finish(&mut self) -> io::Result<()> {
        let (mut source_back, mut written) = (false, false);
        while !source_back || !written {
            let want = Ready {
                output: !source_back,
                capture: !written,
                event: false,
            };
            if self.session.wait(want, Some(CONVERT_TIMEOUT))? == Ready::default() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the device did not finish the frame",
                ));
            }
            source_back |= !source_back && self.session.dequeue_output()?.is_some();
            written |= !written && self.session.dequeue_capture()?.is_some();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PixelFormat;

    fn layout(format: PixelFormat) -> Layout {
        Layout::new(format, 64, 8).expect("layout")
    }

    #[test]
    fn nv12_asks_for_two_planes() {
        let Format::Multi(f) = format(layout(PixelFormat::Nv12)) else {
            panic!("multi-planar");
        };
        assert_eq!(f.planes.len(), 2);
        assert_eq!(f.field, FIELD_NONE);
    }

    #[test]
    fn a_format_the_driver_changed_is_refused() {
        let want = layout(PixelFormat::Nv12);
        assert!(check_kept(&format(want), want).is_ok());
        assert!(check_kept(&format(layout(PixelFormat::Argb32)), want).is_err());
        let clamped = Layout::new(PixelFormat::Nv12, 32, 8).expect("layout");
        assert!(check_kept(&format(clamped), want).is_err());
    }

    #[test]
    fn each_plane_needs_a_large_enough_buffer() {
        let planes = layout(PixelFormat::Nv12).planes();
        assert!(fits(&[512, 256], &planes));
        assert!(fits(&[4096, 4096], &planes));
        assert!(!fits(&[512], &planes));
        assert!(!fits(&[512, 255], &planes));
        assert!(!fits(&[512, 256, 256], &planes));
    }

    #[test]
    fn a_missing_device_is_an_error() {
        let frame = layout(PixelFormat::Nv12);
        assert!(Nonai2d::open(Path::new("/nonexistent/video0"), frame, frame).is_err());
    }

    #[test]
    fn the_engine_does_not_scale() {
        let small = Layout::new(PixelFormat::Nv12, 32, 8).expect("layout");
        let error = Nonai2d::open(
            Path::new("/nonexistent/video0"),
            small,
            layout(PixelFormat::Nv12),
        )
        .err()
        .expect("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
