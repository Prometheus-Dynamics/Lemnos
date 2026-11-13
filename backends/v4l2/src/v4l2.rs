//! V4L2 video nodes: capabilities, formats, sizes and frame intervals, buffers (mmap and
//! dma-buf), streaming, controls.
//!
//! ```no_run
//! use lemnos_v4l2::v4l2::{BufType, VideoDevice};
//!
//! let dev = VideoDevice::open("/dev/video0")?;
//! println!("{} ({})", dev.capabilities().card, dev.capabilities().driver);
//! for fmt in dev.formats(BufType::VideoCapture)? {
//!     println!("{} {}", fmt.fourcc, fmt.description);
//! }
//! # Ok::<(), lemnos_v4l2::Error>(())
//! ```

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::poll::{POLLIN, POLLOUT, POLLPRI, Ready};
use lemnos_linux_sys::v4l2::device::{VIDIOC_QUERYCAP, v4l2_capability, zeroed};

use crate::Result;
use crate::flags::flags;
use crate::node::{self, cstr_field};

mod buffer;
mod capture;
pub mod cid;
mod control;
mod format;
mod mmap_queue;

pub use buffer::{
    BufferCapabilities, BufferFlags, BufferInfo, DequeuedBuffer, MAX_PLANES, Memory, PlaneInfo,
    Planes, QueueBuffer, QueuePlane, RequestedBuffers,
};
pub use capture::DmaBufAccess;
pub use control::{
    ControlFlags, ControlInfo, ControlType, ControlValue, ControlWhich, Controls, MenuItem,
    MenuValue,
};
pub use format::{
    Format, FormatDesc, FormatFlags, FrameIntervals, FrameSize, FrameSizes, InputInfo, MAX_INPUTS,
    MetaFormat, PixFormat, PixFormatMplane, PlaneFormat, SelectionTarget, StepwiseSize,
    StreamParams,
};
pub use lemnos_linux_sys::v4l2::device::ExtControl;
pub use mmap_queue::{MmapQueue, QueueTimer};

pub use lemnos_linux_sys::v4l2::device::BufType;

flags! {
    /// Device capability flags (`V4L2_CAP_*`).
    pub struct CapabilityFlags: u32 {
        const VIDEO_CAPTURE = 0x0000_0001;
        const VIDEO_OUTPUT = 0x0000_0002;
        const VIDEO_OVERLAY = 0x0000_0004;
        const VBI_CAPTURE = 0x0000_0010;
        const VBI_OUTPUT = 0x0000_0020;
        const SLICED_VBI_CAPTURE = 0x0000_0040;
        const SLICED_VBI_OUTPUT = 0x0000_0080;
        const RDS_CAPTURE = 0x0000_0100;
        const VIDEO_OUTPUT_OVERLAY = 0x0000_0200;
        const HW_FREQ_SEEK = 0x0000_0400;
        const RDS_OUTPUT = 0x0000_0800;
        const VIDEO_CAPTURE_MPLANE = 0x0000_1000;
        const VIDEO_OUTPUT_MPLANE = 0x0000_2000;
        const VIDEO_M2M_MPLANE = 0x0000_4000;
        const VIDEO_M2M = 0x0000_8000;
        const TUNER = 0x0001_0000;
        const AUDIO = 0x0002_0000;
        const RADIO = 0x0004_0000;
        const MODULATOR = 0x0008_0000;
        const SDR_CAPTURE = 0x0010_0000;
        const EXT_PIX_FORMAT = 0x0020_0000;
        const SDR_OUTPUT = 0x0040_0000;
        const META_CAPTURE = 0x0080_0000;
        const READWRITE = 0x0100_0000;
        const EDID = 0x0200_0000;
        const STREAMING = 0x0400_0000;
        const META_OUTPUT = 0x0800_0000;
        const TOUCH = 0x1000_0000;
        const IO_MC = 0x2000_0000;
        const DEVICE_CAPS = 0x8000_0000;
    }
}

/// The kernel version a driver reports (`KERNEL_VERSION(major, minor, patch)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KernelVersion {
    /// Major.
    pub major: u8,
    /// Minor.
    pub minor: u8,
    /// Patch level.
    pub patch: u8,
}

impl KernelVersion {
    /// Decodes a `KERNEL_VERSION()` value.
    pub fn from_raw(v: u32) -> Self {
        Self {
            major: (v >> 16) as u8,
            minor: (v >> 8) as u8,
            patch: v as u8,
        }
    }
}

impl std::fmt::Display for KernelVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What a video node is and can do (`VIDIOC_QUERYCAP`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Driver name, e.g. `uvcvideo`.
    pub driver: String,
    /// Device name, e.g. `UVC Camera (046d:0825)`.
    pub card: String,
    /// Bus location, e.g. `usb-xhci-hcd.1-1`.
    pub bus_info: String,
    /// Driver (kernel) version.
    pub version: KernelVersion,
    /// Capabilities of the whole physical device.
    pub capabilities: CapabilityFlags,
    /// Capabilities of this node (equal to `capabilities` for drivers that do not report them
    /// separately).
    pub device_caps: CapabilityFlags,
}

impl Capabilities {
    fn from_raw(raw: &v4l2_capability) -> Self {
        let capabilities = CapabilityFlags(raw.capabilities);
        let device_caps = if capabilities.contains(CapabilityFlags::DEVICE_CAPS) {
            CapabilityFlags(raw.device_caps)
        } else {
            capabilities
        };
        Self {
            driver: cstr_field(&raw.driver),
            card: cstr_field(&raw.card),
            bus_info: cstr_field(&raw.bus_info),
            version: KernelVersion::from_raw(raw.version),
            capabilities,
            device_caps,
        }
    }

    /// The buffer queue types this node supports, from its device capabilities.
    pub fn buffer_types(&self) -> Vec<BufType> {
        let caps = self.device_caps;
        let mut out = Vec::new();
        let table = [
            (CapabilityFlags::VIDEO_CAPTURE, BufType::VideoCapture),
            (
                CapabilityFlags::VIDEO_CAPTURE_MPLANE,
                BufType::VideoCaptureMplane,
            ),
            (CapabilityFlags::VIDEO_OUTPUT, BufType::VideoOutput),
            (
                CapabilityFlags::VIDEO_OUTPUT_MPLANE,
                BufType::VideoOutputMplane,
            ),
            (CapabilityFlags::META_CAPTURE, BufType::MetaCapture),
            (CapabilityFlags::META_OUTPUT, BufType::MetaOutput),
        ];
        for (flag, ty) in table {
            if caps.contains(flag) {
                out.push(ty);
            }
        }
        if caps.contains(CapabilityFlags::VIDEO_M2M) {
            out.extend([BufType::VideoCapture, BufType::VideoOutput]);
        }
        if caps.contains(CapabilityFlags::VIDEO_M2M_MPLANE) {
            out.extend([BufType::VideoCaptureMplane, BufType::VideoOutputMplane]);
        }
        out.dedup();
        out
    }

    /// True when the node's inputs/outputs are configured through the media controller
    /// (`V4L2_CAP_IO_MC`).
    pub fn is_media_controller_centric(&self) -> bool {
        self.device_caps.contains(CapabilityFlags::IO_MC)
    }
}

/// An open V4L2 video node (`/dev/videoN`).
///
/// The descriptor is non-blocking: [`VideoDevice::dequeue`] returns `Ok(None)` when no buffer is
/// ready. Wait for readiness with [`VideoDevice::wait`] or by registering [`AsFd::as_fd`] with an
/// async reactor (readable for capture, writable for output, priority for events).
#[derive(Debug)]
pub struct VideoDevice {
    fd: OwnedFd,
    path: PathBuf,
    caps: Capabilities,
}

impl VideoDevice {
    /// Opens a video node (read-write, non-blocking, close-on-exec) and queries its
    /// capabilities.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let fd = node::open(path, true, true)?;
        let caps = query_capabilities(fd.as_fd())?;
        Ok(Self {
            fd,
            path: path.to_owned(),
            caps,
        })
    }

    /// Another handle on the same node (a dup of the descriptor): the same queues, so buffer
    /// operations on either handle apply to one queue. For a helper thread.
    pub fn try_clone(&self) -> Result<Self> {
        let fd = self
            .fd
            .try_clone()
            .map_err(|source| crate::Error::sys_io("dup", source))?;
        Ok(Self {
            fd,
            path: self.path.clone(),
            caps: self.caps.clone(),
        })
    }

    /// Opens a video node read-only (non-blocking, close-on-exec): enough for querying,
    /// enumerating and reading controls; most drivers refuse changes on such a handle.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let fd = node::open(path, false, true)?;
        let caps = query_capabilities(fd.as_fd())?;
        Ok(Self {
            fd,
            path: path.to_owned(),
            caps,
        })
    }

    /// The path the node was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The capabilities queried when the node was opened.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Queries the capabilities again.
    pub fn query_capabilities(&self) -> Result<Capabilities> {
        query_capabilities(self.fd.as_fd())
    }

    /// Switches the descriptor between non-blocking (the default) and blocking mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<()> {
        node::set_nonblocking(self.fd.as_fd(), nonblocking)
    }

    /// Waits until a buffer can be dequeued (readable/writable), an event is pending
    /// (priority), or the timeout passes. `None` waits forever.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<Ready> {
        node::wait_events(self.fd.as_fd(), POLLIN | POLLOUT | POLLPRI, timeout)
    }
}

fn query_capabilities(fd: BorrowedFd<'_>) -> Result<Capabilities> {
    let mut raw: v4l2_capability = zeroed();
    node::ioctl(fd, VIDIOC_QUERYCAP, &mut raw)?;
    Ok(Capabilities::from_raw(&raw))
}

impl AsFd for VideoDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl AsRawFd for VideoDevice {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Controls for VideoDevice {}
impl crate::event::Events for VideoDevice {}

/// Lists `/dev/video*` nodes, sorted by number.
pub fn list_video_nodes() -> Vec<PathBuf> {
    node::list_dev_nodes("video")
}

/// A device on an unlinked ordinary file under the temporary directory: every ioctl on it
/// answers `ENOTTY`, so a call that reaches the kernel is told from one refused here. Also
/// returns the file, for `mmap` tests.
#[cfg(test)]
pub(crate) fn test_device() -> (VideoDevice, std::fs::File) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "lemnos-v4l2-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let fd = OwnedFd::from(file.try_clone().unwrap());
    let caps = Capabilities::from_raw(&zeroed());
    (VideoDevice { fd, path, caps }, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buf_type_round_trip() {
        for v in 1..=14 {
            assert_eq!(BufType::from_raw(v).unwrap().to_raw(), v);
        }
        assert!(BufType::from_raw(0).is_none());
        assert!(BufType::VideoCaptureMplane.is_multiplanar());
        assert!(BufType::MetaOutput.is_output());
    }

    #[test]
    fn capabilities_prefer_device_caps() {
        let mut raw: v4l2_capability = zeroed();
        raw.driver[..8].copy_from_slice(b"uvcvideo");
        raw.capabilities = 0x8400_0001 | 0x0080_0000;
        raw.device_caps = 0x0400_0001;
        raw.version = (6 << 16) | (12 << 8) | 47;
        let caps = Capabilities::from_raw(&raw);
        assert_eq!(caps.driver, "uvcvideo");
        assert_eq!(caps.version.to_string(), "6.12.47");
        assert_eq!(caps.buffer_types(), vec![BufType::VideoCapture]);
        assert!(!caps.device_caps.contains(CapabilityFlags::META_CAPTURE));
    }
}
