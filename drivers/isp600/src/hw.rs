//! The vendor kernel's nodes as the driver sees them ([`Hardware`]), and the Linux implementation
//! over the capture node, the ISP subdevice and the statistics subdevice ([`LinuxHardware`]).

use std::io;
use std::os::fd::AsFd;
use std::path::PathBuf;

use lemnos_core::{Result, Status};
use lemnos_driver_isp600_sys as sys;
use lemnos_v4l2::media::{self, InterfaceType, MediaDevice, Topology};
use lemnos_v4l2::subdev::Subdev;
use lemnos_v4l2::v4l2::{
    BufType, BufferFlags, CapabilityFlags, DequeuedBuffer, Format, MmapQueue, PixFormat,
    PixFormatMplane, PlaneFormat, VideoDevice,
};
use lemnos_v4l2::{Error, FourCc};

use crate::layout::{self, STATISTICS_BYTES};

/// `V4L2_FIELD_NONE`: progressive frames.
const FIELD_NONE: u32 = 1;

/// The image the driver asks the capture node for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wanted {
    /// The `VIDIOC_S_INPUT` index.
    pub input: u32,
    pub fourcc: FourCc,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub size: u32,
}

/// What the capture node answered `VIDIOC_S_FMT` with, for its first plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Granted {
    pub fourcc: FourCc,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub size: u32,
}

/// A filled image buffer taken off the capture node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The node's buffer index, to queue again.
    pub index: u32,
    /// The node's frame sequence number.
    pub sequence: u32,
    pub timestamp_ns: u64,
    /// The node flagged the frame as bad.
    pub bad: bool,
    /// Planes holding data.
    pub planes: usize,
}

/// Statistics copied out of the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Statistics {
    /// The frame they are of, as the kernel counts frames.
    pub frame_number: u32,
    /// Bytes filled from the start of the buffer.
    pub bytes: usize,
}

/// What the driver asks of the kernel's nodes. The real one is [`LinuxHardware`]; tests use a
/// stand-in.
pub trait Hardware: Send {
    /// Finds and opens the nodes, selects the input, sets the capture format and turns the
    /// statistics on. Returns what the capture node chose.
    fn open(&mut self, wanted: &Wanted) -> Result<Granted>;
    /// Asks for `buffers` image buffers, queues them all and starts streaming. After `open`.
    fn stream_on(&mut self, buffers: u32) -> Result<()>;
    /// Turns the statistics off, stops streaming and closes every node. Nothing to stop is fine.
    fn stop(&mut self);
    /// Hands the kernel a load image, which it copies during the call.
    fn load(&mut self, image: layout::Checked<'_>) -> Result<()>;
    /// Copies the oldest finished statistics into `into`. `None` when none is finished.
    fn statistics(&mut self, into: &mut [u8; STATISTICS_BYTES]) -> Result<Option<Statistics>>;
    /// The next filled image buffer, if one is ready. Never blocks.
    fn dequeue(&mut self) -> Result<Option<Frame>>;
    /// The bytes of `plane` of the buffer `dequeue` returned last, as the node reported them.
    fn payload(&self, frame: &Frame, plane: usize) -> Result<&[u8]>;
    /// Gives image buffer `index` back to the node.
    fn requeue(&mut self, index: u32) -> Result<()>;
}

/// Where the nodes are: three media entity names in one media graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    capture: String,
    isp: String,
    statistics: String,
}

impl Settings {
    /// `INVALID_ARGUMENT` for an empty name or one with a NUL in it.
    pub fn new(capture: &str, isp: &str, statistics: &str) -> Result<Self> {
        for name in [capture, isp, statistics] {
            if name.is_empty() || name.contains('\0') {
                return Err(Status::INVALID_ARGUMENT);
            }
        }
        Ok(Self {
            capture: capture.to_owned(),
            isp: isp.to_owned(),
            statistics: statistics.to_owned(),
        })
    }
}

/// The three nodes' paths, found in one media graph.
#[derive(Debug, Clone)]
struct Found {
    video: PathBuf,
    isp: PathBuf,
    statistics: PathBuf,
}

/// The device node behind `name` in `topology`, if its interface is of `kind`.
fn devnode(topology: &Topology, name: &str, kind: InterfaceType) -> Option<PathBuf> {
    let entity = topology.entity_by_name(name)?;
    let interface = topology.interface_of(entity.id)?;
    if interface.intf_type != kind {
        return None;
    }
    interface.devnode?.path()
}

/// The nodes through `lemnos-v4l2` and the `-sys` crate. Found and opened as they're needed, so a
/// node not yet there is a failed call rather than a failed bind, and a call can succeed once it
/// is.
pub struct LinuxHardware {
    settings: Settings,
    /// The capture node and its buffer type, between `open` and `stream_on`.
    opened: Option<(VideoDevice, BufType)>,
    /// The capture node's mapped queue, while streaming.
    queue: Option<MmapQueue>,
    /// The buffer `dequeue` returned last, for `payload`.
    current: Option<DequeuedBuffer>,
    isp: Option<Subdev>,
    statistics: Option<Subdev>,
}

impl LinuxHardware {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            opened: None,
            queue: None,
            current: None,
            isp: None,
            statistics: None,
        }
    }

    /// The media graph holding all three entities, each with its node.
    fn find(&self) -> Result<Found> {
        let s = &self.settings;
        media::list_media_devices()
            .into_iter()
            .find_map(|path| {
                let topology = MediaDevice::open_read_only(&path).ok()?.topology().ok()?;
                Some(Found {
                    video: devnode(&topology, &s.capture, InterfaceType::V4L_VIDEO)?,
                    isp: devnode(&topology, &s.isp, InterfaceType::V4L_SUBDEV)?,
                    statistics: devnode(&topology, &s.statistics, InterfaceType::V4L_SUBDEV)?,
                })
            })
            .ok_or(Status::NO_DEVICE)
    }
}

fn status_of_kind(kind: io::ErrorKind) -> Status {
    match kind {
        io::ErrorKind::NotFound => Status::NO_DEVICE,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        io::ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
        io::ErrorKind::ResourceBusy => Status::BUSY,
        _ => Status::IO,
    }
}

fn status(error: &Error) -> Status {
    let kind = match error {
        Error::Open { source, .. } | Error::Sys { source, .. } => source.kind(),
        Error::Invalid(_) => return Status::IO,
        other => match other.errno() {
            Some(errno) => io::Error::from_raw_os_error(errno).kind(),
            None => return Status::IO,
        },
    };
    match status_of_kind(kind) {
        Status::IO if error.is_no_device() => Status::NO_DEVICE,
        s => s,
    }
}

fn io_status(error: &io::Error) -> Status {
    status_of_kind(error.kind())
}

fn granted(format: &Format) -> Result<Granted> {
    let (fourcc, width, height, plane) = match format {
        Format::Single(p) => (
            p.fourcc,
            p.width,
            p.height,
            PlaneFormat {
                bytes_per_line: p.bytes_per_line,
                size_image: p.size_image,
            },
        ),
        Format::Multi(p) => (
            p.fourcc,
            p.width,
            p.height,
            p.planes.first().copied().ok_or(Status::IO)?,
        ),
        Format::Meta(_) => return Err(Status::IO),
    };
    Ok(Granted {
        fourcc,
        width,
        height,
        stride: plane.bytes_per_line,
        size: plane.size_image,
    })
}

impl Hardware for LinuxHardware {
    fn open(&mut self, wanted: &Wanted) -> Result<Granted> {
        let found = self.find()?;
        let dev = VideoDevice::open(&found.video).map_err(|e| status(&e))?;
        let caps = dev.capabilities().device_caps;
        let buf_type = if caps.contains(CapabilityFlags::VIDEO_CAPTURE_MPLANE) {
            BufType::VideoCaptureMplane
        } else if caps.contains(CapabilityFlags::VIDEO_CAPTURE) {
            BufType::VideoCapture
        } else {
            return Err(Status::NOT_SUPPORTED);
        };
        // The kernel driver sets up its pipeline, the ISP included, here, and needs this before
        // the format.
        dev.set_input(wanted.input).map_err(|e| status(&e))?;
        let format = if buf_type.is_multiplanar() {
            Format::Multi(PixFormatMplane {
                width: wanted.width,
                height: wanted.height,
                fourcc: wanted.fourcc,
                field: FIELD_NONE,
                planes: vec![PlaneFormat {
                    bytes_per_line: wanted.stride,
                    size_image: wanted.size,
                }],
                ..Default::default()
            })
        } else {
            Format::Single(PixFormat {
                width: wanted.width,
                height: wanted.height,
                fourcc: wanted.fourcc,
                field: FIELD_NONE,
                bytes_per_line: wanted.stride,
                size_image: wanted.size,
                ..Default::default()
            })
        };
        let chosen = dev.set_format(buf_type, &format).map_err(|e| status(&e))?;
        let granted = granted(&chosen)?;
        let statistics = Subdev::open(&found.statistics).map_err(|e| status(&e))?;
        // The kernel makes its statistics buffers here, each a saved buffer's size.
        let bytes = sys::configure_statistics(statistics.as_fd()).map_err(|e| io_status(&e))?;
        if bytes as usize != layout::SAVE_BYTES {
            return Err(Status::NOT_SUPPORTED);
        }
        sys::enable_statistics(statistics.as_fd(), true).map_err(|e| io_status(&e))?;
        self.statistics = Some(statistics);
        self.isp = Some(Subdev::open(&found.isp).map_err(|e| status(&e))?);
        self.opened = Some((dev, buf_type));
        Ok(granted)
    }

    fn stream_on(&mut self, buffers: u32) -> Result<()> {
        let (dev, buf_type) = self.opened.take().ok_or(Status::INVALID_ARGUMENT)?;
        let queue = MmapQueue::new(dev, buf_type, buffers, "capture").map_err(|e| status(&e))?;
        for index in 0..queue.len() {
            let index = u32::try_from(index).map_err(|_| Status::IO)?;
            queue.queue(index, &[]).map_err(|e| status(&e))?;
        }
        queue.stream_on().map_err(|e| status(&e))?;
        self.queue = Some(queue);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(statistics) = self.statistics.take() {
            // Off before the node closes; a node that has gone has nothing to turn off.
            let _ = sys::enable_statistics(statistics.as_fd(), false);
        }
        self.isp = None;
        self.current = None;
        self.opened = None;
        if let Some(queue) = self.queue.take() {
            let _ = queue.close();
        }
    }

    fn load(&mut self, image: layout::Checked<'_>) -> Result<()> {
        let isp = self.isp.as_ref().ok_or(Status::IO)?;
        sys::load(isp.as_fd(), image.bytes()).map_err(|e| io_status(&e))
    }

    fn statistics(&mut self, into: &mut [u8; STATISTICS_BYTES]) -> Result<Option<Statistics>> {
        let statistics = self.statistics.as_ref().ok_or(Status::IO)?;
        let got = sys::request_statistics(statistics.as_fd(), into).map_err(|e| io_status(&e))?;
        Ok(got.map(|r| Statistics {
            frame_number: r.frame_number,
            bytes: r.bytes,
        }))
    }

    fn dequeue(&mut self) -> Result<Option<Frame>> {
        let queue = self.queue.as_ref().ok_or(Status::IO)?;
        let Some(done) = queue.try_dequeue().map_err(|e| status(&e))? else {
            return Ok(None);
        };
        let frame = Frame {
            index: done.index,
            sequence: done.sequence,
            timestamp_ns: u64::try_from(done.timestamp.as_nanos()).unwrap_or(u64::MAX),
            bad: done.flags.contains(BufferFlags::ERROR),
            planes: done.planes.len(),
        };
        self.current = Some(done);
        Ok(Some(frame))
    }

    fn payload(&self, frame: &Frame, plane: usize) -> Result<&[u8]> {
        let (Some(queue), Some(done)) = (self.queue.as_ref(), self.current.as_ref()) else {
            return Err(Status::IO);
        };
        if done.index != frame.index {
            return Err(Status::INVALID_ARGUMENT);
        }
        queue.payload(done, plane).map_err(|e| status(&e))
    }

    fn requeue(&mut self, index: u32) -> Result<()> {
        let queue = self.queue.as_ref().ok_or(Status::IO)?;
        queue.queue(index, &[]).map_err(|e| status(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kernel_error_becomes_the_status_a_caller_can_act_on() {
        let ioctl = |errno| Error::Ioctl {
            name: "VIDIOC_DQBUF",
            errno,
        };
        assert_eq!(status(&ioctl(19)), Status::NO_DEVICE, "ENODEV");
        assert_eq!(status(&ioctl(16)), Status::BUSY, "EBUSY");
        assert_eq!(status(&ioctl(22)), Status::INVALID_ARGUMENT, "EINVAL");
        assert_eq!(status(&ioctl(13)), Status::ACCESS, "EACCES");
        assert_eq!(status(&ioctl(5)), Status::IO, "EIO");
        assert_eq!(
            io_status(&io::Error::from_raw_os_error(22)),
            Status::INVALID_ARGUMENT
        );
        assert_eq!(io_status(&io::Error::from_raw_os_error(25)), Status::IO);
        assert_eq!(status(&Error::Invalid("crafted".into())), Status::IO);
    }

    #[test]
    fn the_granted_format_is_the_first_planes() {
        let multi = Format::Multi(PixFormatMplane {
            width: 64,
            height: 32,
            fourcc: FourCc::new(b"NV12"),
            planes: vec![PlaneFormat {
                bytes_per_line: 64,
                size_image: 3072,
            }],
            ..Default::default()
        });
        assert_eq!(
            granted(&multi).unwrap(),
            Granted {
                fourcc: FourCc::new(b"NV12"),
                width: 64,
                height: 32,
                stride: 64,
                size: 3072,
            }
        );
        let empty = Format::Multi(PixFormatMplane::default());
        assert_eq!(granted(&empty).unwrap_err(), Status::IO);
    }

    #[test]
    fn every_name_is_a_name() {
        assert!(Settings::new("vin_video0", "isp", "stat").is_ok());
        for (c, i, s) in [
            ("", "i", "s"),
            ("c", "", "s"),
            ("c", "i", ""),
            ("c", "i\0", "s"),
        ] {
            assert_eq!(
                Settings::new(c, i, s).unwrap_err(),
                Status::INVALID_ARGUMENT
            );
        }
    }

    #[test]
    fn calls_out_of_order_are_refused_without_touching_a_device() {
        let mut hw = LinuxHardware::new(Settings::new("c", "i", "s").unwrap());
        assert_eq!(hw.dequeue().unwrap_err(), Status::IO, "not streaming");
        let mut into = Box::new([0u8; STATISTICS_BYTES]);
        assert_eq!(hw.statistics(&mut into).unwrap_err(), Status::IO);
        let image = vec![0u8; layout::LOAD_BYTES];
        let checked = layout::check_load(&image).unwrap();
        assert_eq!(hw.load(checked).unwrap_err(), Status::IO);
        assert_eq!(
            hw.stream_on(4).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "not open"
        );
        hw.stop();
    }
}
