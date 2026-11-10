//! The capture node as the driver sees it, and the Linux implementation over the stock V4L2
//! device through `lemnos-v4l2`.

use std::io;
use std::path::PathBuf;

use lemnos_core::{Result, Status};
use lemnos_v4l2::media::{self, InterfaceType, MediaDevice, Topology};
use lemnos_v4l2::v4l2::{
    BufType, BufferFlags, CapabilityFlags, DequeuedBuffer, Format, MmapQueue, PixFormat,
    PixFormatMplane, PlaneFormat, VideoDevice,
};
use lemnos_v4l2::{Error, FourCc};

/// `V4L2_FIELD_NONE`: progressive frames.
const FIELD_NONE: u32 = 1;

/// The format the driver asks the node for.
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

/// What the node answered `VIDIOC_S_FMT` with, for its first plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Granted {
    pub fourcc: FourCc,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub size: u32,
}

/// A filled buffer taken off the node.
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

/// The media graph the node is in, and the node's entity in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    pub topology: Topology,
    pub capture: u32,
}

/// What the driver asks of the capture node. The real one is [`LinuxHardware`]; tests use a
/// stand-in.
pub trait Hardware: Send {
    /// Opens the node, selects the input and sets the format. Returns what the node chose.
    fn open(&mut self, wanted: &Wanted) -> Result<Granted>;
    /// Asks for `buffers` buffers, queues them all and starts streaming. After `open`.
    fn stream_on(&mut self, buffers: u32) -> Result<()>;
    /// Stops streaming, frees the buffers and closes the node. Nothing to stop is not an error.
    fn stop(&mut self) -> Result<()>;
    /// The next filled buffer, if one is ready. Never blocks.
    fn dequeue(&mut self) -> Result<Option<Frame>>;
    /// The bytes of `plane` of the buffer `dequeue` returned last, as the node reported them.
    fn payload(&self, frame: &Frame, plane: usize) -> Result<&[u8]>;
    /// Gives buffer `index` back to the node.
    fn requeue(&mut self, index: u32) -> Result<()>;
    /// The media graph as it stands.
    fn graph(&mut self) -> Result<Graph>;
}

/// Where the node is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The capture node's media entity name.
    capture: String,
}

impl Settings {
    /// `INVALID_ARGUMENT` for an empty name or one with a NUL in it.
    pub fn new(capture: &str) -> Result<Self> {
        if capture.is_empty() || capture.contains('\0') {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            capture: capture.to_owned(),
        })
    }
}

/// Where the capture entity was found.
struct Found {
    media: PathBuf,
    video: PathBuf,
}

/// The node through `lemnos-v4l2`. Found and opened as it's needed, so a node not yet there is a
/// failed call rather than a failed bind, and a call can succeed once it is.
pub struct LinuxHardware {
    settings: Settings,
    found: Option<Found>,
    /// The node and its format, between `open` and `stream_on`.
    opened: Option<(VideoDevice, BufType)>,
    /// The node's mapped queue, while streaming.
    queue: Option<MmapQueue>,
    /// The buffer `dequeue` returned last, for `payload`.
    current: Option<DequeuedBuffer>,
}

impl LinuxHardware {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            found: None,
            opened: None,
            queue: None,
            current: None,
        }
    }

    /// The media device holding an entity named as configured with a video node behind it, and
    /// that node's path.
    fn find(&mut self) -> Result<&Found> {
        if self.found.is_none() {
            let found = media::list_media_devices().into_iter().find_map(|path| {
                let topology = MediaDevice::open_read_only(&path).ok()?.topology().ok()?;
                let entity = topology.entity_by_name(&self.settings.capture)?;
                let interface = topology.interface_of(entity.id)?;
                if interface.intf_type != InterfaceType::V4L_VIDEO {
                    return None;
                }
                Some(Found {
                    video: interface.devnode?.path()?,
                    media: path,
                })
            });
            self.found = Some(found.ok_or(Status::NO_DEVICE)?);
        }
        self.found.as_ref().ok_or(Status::NO_DEVICE)
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
    match kind {
        io::ErrorKind::NotFound => Status::NO_DEVICE,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        io::ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
        io::ErrorKind::ResourceBusy => Status::BUSY,
        _ if error.is_no_device() => Status::NO_DEVICE,
        _ => Status::IO,
    }
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
        let video = self.find()?.video.clone();
        let dev = VideoDevice::open(&video).map_err(|e| status(&e))?;
        let caps = dev.capabilities().device_caps;
        let buf_type = if caps.contains(CapabilityFlags::VIDEO_CAPTURE_MPLANE) {
            BufType::VideoCaptureMplane
        } else if caps.contains(CapabilityFlags::VIDEO_CAPTURE) {
            BufType::VideoCapture
        } else {
            return Err(Status::NOT_SUPPORTED);
        };
        // The kernel driver sets up its pipeline here, and needs this before the format.
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

    fn stop(&mut self) -> Result<()> {
        self.current = None;
        self.opened = None;
        // Found again at the next open, in case the node came back under another number.
        self.found = None;
        match self.queue.take() {
            Some(queue) => queue.close().map_err(|e| status(&e)),
            None => Ok(()),
        }
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

    fn graph(&mut self) -> Result<Graph> {
        let media = self.find()?.media.clone();
        let topology = MediaDevice::open_read_only(&media)
            .and_then(|m| m.topology())
            .map_err(|e| status(&e))?;
        let capture = topology
            .entity_by_name(&self.settings.capture)
            .ok_or(Status::NO_DEVICE)?
            .id;
        Ok(Graph { topology, capture })
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
        let missing = Error::Open {
            path: "/dev/video99".into(),
            source: io::ErrorKind::NotFound.into(),
        };
        assert_eq!(status(&missing), Status::NO_DEVICE);
        assert_eq!(status(&Error::Invalid("crafted".into())), Status::IO);
    }

    #[test]
    fn the_granted_format_is_the_first_planes() {
        let multi = Format::Multi(PixFormatMplane {
            width: 64,
            height: 32,
            fourcc: FourCc::new(b"RG10"),
            planes: vec![
                PlaneFormat {
                    bytes_per_line: 128,
                    size_image: 4096,
                },
                PlaneFormat {
                    bytes_per_line: 1,
                    size_image: 1,
                },
            ],
            ..Default::default()
        });
        assert_eq!(
            granted(&multi).unwrap(),
            Granted {
                fourcc: FourCc::new(b"RG10"),
                width: 64,
                height: 32,
                stride: 128,
                size: 4096,
            }
        );
        let single = Format::Single(PixFormat {
            width: 8,
            height: 2,
            fourcc: FourCc::GREY,
            bytes_per_line: 8,
            size_image: 16,
            ..Default::default()
        });
        assert_eq!(granted(&single).unwrap().size, 16);
        let empty = Format::Multi(PixFormatMplane::default());
        assert_eq!(granted(&empty).unwrap_err(), Status::IO);
    }

    #[test]
    fn a_capture_name_is_a_name() {
        assert!(Settings::new("vin_video0").is_ok());
        assert_eq!(Settings::new("").unwrap_err(), Status::INVALID_ARGUMENT);
        assert_eq!(Settings::new("a\0b").unwrap_err(), Status::INVALID_ARGUMENT);
    }

    #[test]
    fn calls_out_of_order_are_refused_without_touching_a_device() {
        let mut hw = LinuxHardware::new(Settings::new("vin_video0").unwrap());
        assert_eq!(hw.dequeue().unwrap_err(), Status::IO, "not streaming");
        assert_eq!(
            hw.stream_on(4).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "not open"
        );
        hw.stop().unwrap();
    }
}
