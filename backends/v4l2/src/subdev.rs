//! V4L2 subdevices: media bus formats, frame sizes and intervals, selections, routing,
//! controls and events.
//!
//! ```no_run
//! use lemnos_v4l2::subdev::{Subdev, Which};
//!
//! let sd = Subdev::open("/dev/v4l-subdev0")?;
//! for code in sd.mbus_codes(0, Which::Active)? {
//!     println!("{code}: {:?}", sd.frame_sizes(0, code, Which::Active)?);
//! }
//! println!("{:?}", sd.format(0, Which::Active)?);
//! # Ok::<(), lemnos_v4l2::Error>(())
//! ```

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::poll::{POLLPRI, Ready};
use lemnos_linux_sys::v4l2::device::subdev::{
    VIDIOC_SUBDEV_ENUM_FRAME_INTERVAL, VIDIOC_SUBDEV_ENUM_FRAME_SIZE, VIDIOC_SUBDEV_ENUM_MBUS_CODE,
    VIDIOC_SUBDEV_G_FMT, VIDIOC_SUBDEV_G_FRAME_INTERVAL, VIDIOC_SUBDEV_G_SELECTION,
    VIDIOC_SUBDEV_QUERYCAP, VIDIOC_SUBDEV_S_CLIENT_CAP, VIDIOC_SUBDEV_S_FMT,
    VIDIOC_SUBDEV_S_FRAME_INTERVAL, VIDIOC_SUBDEV_S_SELECTION, filled_routes, g_routing, s_routing,
    v4l2_mbus_framefmt, v4l2_subdev_capability, v4l2_subdev_client_capability, v4l2_subdev_format,
    v4l2_subdev_frame_interval, v4l2_subdev_frame_interval_enum, v4l2_subdev_frame_size_enum,
    v4l2_subdev_mbus_code_enum, v4l2_subdev_route, v4l2_subdev_selection,
};
use lemnos_linux_sys::v4l2::device::zeroed;

use crate::flags::flags;
use crate::node;
use crate::v4l2::{Controls, SelectionTarget};
use crate::{Error, Fraction, Rect, Result};

mod mbus;

pub use mbus::MbusCode;

/// Which state an operation applies to: the device's active configuration, or the file
/// handle's private trial configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Which {
    /// Trial state (`V4L2_SUBDEV_FORMAT_TRY`): changes nothing on the device.
    Try = 0,
    /// Active state (`V4L2_SUBDEV_FORMAT_ACTIVE`).
    Active = 1,
}

flags! {
    /// Subdevice capabilities (`V4L2_SUBDEV_CAP_*`).
    pub struct SubdevCapabilityFlags: u32 {
        /// The node is read-only: set ioctls fail with `EPERM`.
        const RO_SUBDEV = 1 << 0;
        /// Routing and multiplexed streams are supported.
        const STREAMS = 1 << 1;
    }
}

flags! {
    /// Client capabilities (`V4L2_SUBDEV_CLIENT_CAP_*`).
    pub struct ClientCapabilities: u64 {
        /// The client understands streams.
        const STREAMS = 1 << 0;
        /// The client sets `which` in frame-interval calls.
        const INTERVAL_USES_WHICH = 1 << 1;
    }
}

/// The result of `VIDIOC_SUBDEV_QUERYCAP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubdevCapabilities {
    /// Kernel version (`KERNEL_VERSION`-encoded).
    pub version: u32,
    /// Capabilities.
    pub capabilities: SubdevCapabilityFlags,
}

/// A media bus code a pad supports (`VIDIOC_SUBDEV_ENUM_MBUS_CODE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MbusCodeDesc {
    /// The code.
    pub code: MbusCode,
    /// `V4L2_SUBDEV_MBUS_CODE_*` flags.
    pub flags: u32,
}

/// A range of frame sizes (`VIDIOC_SUBDEV_ENUM_FRAME_SIZE`); discrete sizes have min == max.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameSizeRange {
    /// Minimum width.
    pub min_width: u32,
    /// Maximum width.
    pub max_width: u32,
    /// Minimum height.
    pub min_height: u32,
    /// Maximum height.
    pub max_height: u32,
}

impl FrameSizeRange {
    /// True for a single size.
    pub fn is_discrete(&self) -> bool {
        self.min_width == self.max_width && self.min_height == self.max_height
    }
}

/// A pad's media bus format (`struct v4l2_mbus_framefmt`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MbusFormat {
    /// Width in pixels.
    pub width: u32,
    /// Height in lines.
    pub height: u32,
    /// Bus code.
    pub code: MbusCode,
    /// `enum v4l2_field` (1 = progressive).
    pub field: u32,
    /// `enum v4l2_colorspace`.
    pub colorspace: u32,
    /// `enum v4l2_ycbcr_encoding`.
    pub ycbcr_enc: u16,
    /// `enum v4l2_quantization`.
    pub quantization: u16,
    /// `enum v4l2_xfer_func`.
    pub xfer_func: u16,
    /// `V4L2_MBUS_FRAMEFMT_*` flags.
    pub flags: u16,
}

impl MbusFormat {
    fn from_raw(f: &v4l2_mbus_framefmt) -> Self {
        Self {
            width: f.width,
            height: f.height,
            code: MbusCode(f.code),
            field: f.field,
            colorspace: f.colorspace,
            ycbcr_enc: f.ycbcr_enc,
            quantization: f.quantization,
            xfer_func: f.xfer_func,
            flags: f.flags,
        }
    }

    fn to_raw(self) -> v4l2_mbus_framefmt {
        v4l2_mbus_framefmt {
            width: self.width,
            height: self.height,
            code: self.code.0,
            field: self.field,
            colorspace: self.colorspace,
            ycbcr_enc: self.ycbcr_enc,
            quantization: self.quantization,
            xfer_func: self.xfer_func,
            flags: self.flags,
            reserved: [0; 10],
        }
    }
}

/// The `V4L2_SUBDEV_ROUTE_FL_ACTIVE` flag of a route.
const ROUTE_FL_ACTIVE: u32 = 1;

/// A route through a subdevice from a sink pad/stream to a source pad/stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Route {
    /// Sink pad.
    pub sink_pad: u32,
    /// Stream on the sink pad.
    pub sink_stream: u32,
    /// Source pad.
    pub source_pad: u32,
    /// Stream on the source pad.
    pub source_stream: u32,
    /// True when the route is active (`V4L2_SUBDEV_ROUTE_FL_ACTIVE`).
    pub active: bool,
}

impl Route {
    fn from_raw(r: &v4l2_subdev_route) -> Self {
        Self {
            sink_pad: r.sink_pad,
            sink_stream: r.sink_stream,
            source_pad: r.source_pad,
            source_stream: r.source_stream,
            active: r.flags & ROUTE_FL_ACTIVE != 0,
        }
    }

    fn to_raw(self) -> v4l2_subdev_route {
        v4l2_subdev_route {
            sink_pad: self.sink_pad,
            sink_stream: self.sink_stream,
            source_pad: self.source_pad,
            source_stream: self.source_stream,
            flags: u32::from(self.active),
            ..Default::default()
        }
    }
}

/// An open V4L2 subdevice node (`/dev/v4l-subdevN`).
///
/// All pad operations use stream 0; routing is available through [`Subdev::routing`].
#[derive(Debug)]
pub struct Subdev {
    fd: OwnedFd,
    path: PathBuf,
}

impl Subdev {
    /// Opens a subdevice node read-write, non-blocking, close-on-exec.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Ok(Self {
            fd: node::open(path, true, true)?,
            path: path.to_owned(),
        })
    }

    /// Opens a subdevice node read-only (all set operations fail).
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Ok(Self {
            fd: node::open(path, false, true)?,
            path: path.to_owned(),
        })
    }

    /// The path it was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Queries the subdevice capabilities (`VIDIOC_SUBDEV_QUERYCAP`, Linux 5.10+).
    pub fn capabilities(&self) -> Result<SubdevCapabilities> {
        let mut raw: v4l2_subdev_capability = zeroed();
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_QUERYCAP, &mut raw)?;
        Ok(SubdevCapabilities {
            version: raw.version,
            capabilities: SubdevCapabilityFlags(raw.capabilities),
        })
    }

    /// Declares what this client understands (`VIDIOC_SUBDEV_S_CLIENT_CAP`); returns what the
    /// kernel accepted.
    pub fn set_client_capabilities(&self, caps: ClientCapabilities) -> Result<ClientCapabilities> {
        let mut raw = v4l2_subdev_client_capability {
            capabilities: caps.0,
        };
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_S_CLIENT_CAP, &mut raw)?;
        Ok(ClientCapabilities(raw.capabilities))
    }

    /// Enumerates the media bus codes of a pad (`VIDIOC_SUBDEV_ENUM_MBUS_CODE`).
    pub fn mbus_codes(&self, pad: u32, which: Which) -> Result<Vec<MbusCode>> {
        Ok(self
            .mbus_code_descs(pad, which)?
            .into_iter()
            .map(|d| d.code)
            .collect())
    }

    /// Enumerates the media bus codes of a pad with their flags.
    pub fn mbus_code_descs(&self, pad: u32, which: Which) -> Result<Vec<MbusCodeDesc>> {
        let mut out = Vec::new();
        for index in 0.. {
            node::check_enum_index(index)?;
            let mut raw = v4l2_subdev_mbus_code_enum {
                pad,
                index,
                which: which as u32,
                ..Default::default()
            };
            match node::ioctl(self.as_fd(), VIDIOC_SUBDEV_ENUM_MBUS_CODE, &mut raw) {
                Ok(()) => out.push(MbusCodeDesc {
                    code: MbusCode(raw.code),
                    flags: raw.flags,
                }),
                Err(e) if e.is_invalid_argument() => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Enumerates the frame sizes of a pad for a bus code (`VIDIOC_SUBDEV_ENUM_FRAME_SIZE`).
    pub fn frame_sizes(
        &self,
        pad: u32,
        code: MbusCode,
        which: Which,
    ) -> Result<Vec<FrameSizeRange>> {
        let mut out = Vec::new();
        for index in 0.. {
            node::check_enum_index(index)?;
            let mut raw = v4l2_subdev_frame_size_enum {
                index,
                pad,
                code: code.0,
                which: which as u32,
                ..Default::default()
            };
            match node::ioctl(self.as_fd(), VIDIOC_SUBDEV_ENUM_FRAME_SIZE, &mut raw) {
                Ok(()) => out.push(FrameSizeRange {
                    min_width: raw.min_width,
                    max_width: raw.max_width,
                    min_height: raw.min_height,
                    max_height: raw.max_height,
                }),
                Err(e) if e.is_invalid_argument() => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Enumerates the frame intervals of a pad for a bus code and size
    /// (`VIDIOC_SUBDEV_ENUM_FRAME_INTERVAL`). Sensors whose rate is set through blanking
    /// controls usually report none.
    pub fn frame_intervals(
        &self,
        pad: u32,
        code: MbusCode,
        width: u32,
        height: u32,
        which: Which,
    ) -> Result<Vec<Fraction>> {
        let mut out = Vec::new();
        for index in 0.. {
            node::check_enum_index(index)?;
            let mut raw = v4l2_subdev_frame_interval_enum {
                index,
                pad,
                code: code.0,
                width,
                height,
                which: which as u32,
                ..Default::default()
            };
            match node::ioctl(self.as_fd(), VIDIOC_SUBDEV_ENUM_FRAME_INTERVAL, &mut raw) {
                Ok(()) => out.push(raw.interval),
                Err(e) if e.is_invalid_argument() || (e.is_not_supported() && index == 0) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// The format of a pad (`VIDIOC_SUBDEV_G_FMT`).
    pub fn format(&self, pad: u32, which: Which) -> Result<MbusFormat> {
        let mut raw = v4l2_subdev_format {
            which: which as u32,
            pad,
            ..Default::default()
        };
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_G_FMT, &mut raw)?;
        Ok(MbusFormat::from_raw(&raw.format))
    }

    /// Sets the format of a pad (`VIDIOC_SUBDEV_S_FMT`); returns what the driver chose. With
    /// [`Which::Try`] nothing on the device changes.
    pub fn set_format(&self, pad: u32, which: Which, format: &MbusFormat) -> Result<MbusFormat> {
        let mut raw = v4l2_subdev_format {
            which: which as u32,
            pad,
            format: format.to_raw(),
            ..Default::default()
        };
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_S_FMT, &mut raw)?;
        Ok(MbusFormat::from_raw(&raw.format))
    }

    /// Reads a selection rectangle of a pad (`VIDIOC_SUBDEV_G_SELECTION`), e.g. the sensor's
    /// crop or [`SelectionTarget::NativeSize`].
    pub fn selection(&self, pad: u32, which: Which, target: SelectionTarget) -> Result<Rect> {
        let mut raw = v4l2_subdev_selection {
            which: which as u32,
            pad,
            target: target as u32,
            ..Default::default()
        };
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_G_SELECTION, &mut raw)?;
        Ok(raw.r)
    }

    /// Sets a selection rectangle (`VIDIOC_SUBDEV_S_SELECTION`) with `V4L2_SEL_FLAG_*` flags;
    /// returns the rectangle the driver chose.
    pub fn set_selection(
        &self,
        pad: u32,
        which: Which,
        target: SelectionTarget,
        rect: Rect,
        flags: u32,
    ) -> Result<Rect> {
        let mut raw = v4l2_subdev_selection {
            which: which as u32,
            pad,
            target: target as u32,
            flags,
            r: rect,
            ..Default::default()
        };
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_S_SELECTION, &mut raw)?;
        Ok(raw.r)
    }

    /// The frame interval of a pad (`VIDIOC_SUBDEV_G_FRAME_INTERVAL`). `which` is honoured
    /// only after [`ClientCapabilities::INTERVAL_USES_WHICH`] was set; otherwise the active
    /// interval is returned.
    pub fn frame_interval(&self, pad: u32, which: Which) -> Result<Fraction> {
        let mut raw = v4l2_subdev_frame_interval {
            pad,
            which: which as u32,
            ..Default::default()
        };
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_G_FRAME_INTERVAL, &mut raw)?;
        Ok(raw.interval)
    }

    /// Sets the frame interval of a pad (`VIDIOC_SUBDEV_S_FRAME_INTERVAL`); returns what the
    /// driver chose.
    pub fn set_frame_interval(
        &self,
        pad: u32,
        which: Which,
        interval: Fraction,
    ) -> Result<Fraction> {
        let mut raw = v4l2_subdev_frame_interval {
            pad,
            interval,
            which: which as u32,
            ..Default::default()
        };
        node::ioctl(self.as_fd(), VIDIOC_SUBDEV_S_FRAME_INTERVAL, &mut raw)?;
        Ok(raw.interval)
    }

    /// The routing table (`VIDIOC_SUBDEV_G_ROUTING`); needs [`ClientCapabilities::STREAMS`] and
    /// a driver with [`SubdevCapabilityFlags::STREAMS`].
    pub fn routing(&self, which: Which) -> Result<Vec<Route>> {
        const NAME: &str = "VIDIOC_SUBDEV_G_ROUTING";
        let mut len = 16usize;
        // The table can grow between calls, but not for ever.
        for _ in 0..4 {
            let mut routes = vec![v4l2_subdev_route::default(); len];
            let (num_routes, result) = g_routing(self.as_fd(), which as u32, &mut routes);
            match result {
                Ok(()) => {}
                // The table is larger than the buffer: ask again with room for all of it.
                Err(e) if e.raw_os_error() == Some(libc::ENOSPC) && num_routes as usize > len => {
                    len = grown_route_table(num_routes)?;
                    continue;
                }
                Err(e) => return Err(Error::ioctl_io(NAME, e)),
            }
            let filled = filled_routes(&routes, num_routes)
                .map_err(|source| Error::ioctl_io(NAME, source))?;
            return Ok(filled.iter().map(Route::from_raw).collect());
        }
        Err(Error::Invalid(
            "the routing table kept growing while being read".into(),
        ))
    }

    /// Replaces the routing table (`VIDIOC_SUBDEV_S_ROUTING`).
    pub fn set_routing(&self, which: Which, routes: &[Route]) -> Result<()> {
        let mut raw_routes: Vec<v4l2_subdev_route> = routes.iter().map(|r| r.to_raw()).collect();
        s_routing(self.as_fd(), which as u32, &mut raw_routes)
            .map_err(|e| Error::ioctl_io("VIDIOC_SUBDEV_S_ROUTING", e))
    }

    /// Waits until an event is pending (priority readiness), or the timeout passes.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<Ready> {
        node::wait_events(self.as_fd(), POLLPRI, timeout)
    }
}

impl AsFd for Subdev {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl AsRawFd for Subdev {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Controls for Subdev {}
impl crate::event::Events for Subdev {}

/// The most routes read from a subdevice. The count sizes an allocation, so one past this from
/// the kernel is refused.
const MAX_ROUTES: u32 = 256;

/// The table length to retry with after the kernel asked for `num_routes`.
fn grown_route_table(num_routes: u32) -> Result<usize> {
    if num_routes > MAX_ROUTES {
        return Err(Error::Invalid(format!(
            "the kernel reports {num_routes} routes (at most {MAX_ROUTES} allowed)"
        )));
    }
    Ok(num_routes as usize)
}

/// Lists `/dev/v4l-subdev*` nodes, sorted by number.
pub fn list_subdev_nodes() -> Vec<PathBuf> {
    node::list_dev_nodes("v4l-subdev")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbus_format_round_trips() {
        let f = MbusFormat {
            width: 1280,
            height: 800,
            code: MbusCode::Y10_1X10,
            field: 1,
            colorspace: 11,
            ..Default::default()
        };
        assert_eq!(MbusFormat::from_raw(&f.to_raw()), f);
    }

    #[test]
    fn frame_size_range_discrete() {
        let r = FrameSizeRange {
            min_width: 640,
            max_width: 640,
            min_height: 400,
            max_height: 400,
        };
        assert!(r.is_discrete());
    }

    #[test]
    fn routes_round_trip_their_active_flag() {
        let r = Route {
            sink_pad: 0,
            sink_stream: 0,
            source_pad: 1,
            source_stream: 2,
            active: true,
        };
        let raw = r.to_raw();
        assert_eq!(raw.flags, ROUTE_FL_ACTIVE);
        assert_eq!(Route::from_raw(&raw), r);
    }
}
