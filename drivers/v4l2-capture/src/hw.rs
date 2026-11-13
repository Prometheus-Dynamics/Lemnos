//! The capture node as a driver sees it, and the Linux implementation over the stock V4L2
//! device through `lemnos-v4l2`.

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::PathBuf;

use lemnos_contract_camera_receiver::{INPUT_RECORD, Input, MAX_INPUTS, write_input};
use lemnos_core::{Result, Status};
use lemnos_hal::blocks::BufferHandle;
use lemnos_linux_sys::dma_buf;
use lemnos_v4l2::media::{self, InterfaceType, MediaDevice, Topology};
use lemnos_v4l2::v4l2::{
    BufType, BufferFlags, CapabilityFlags, DequeuedBuffer, Format, InputInfo, MetaFormat,
    MmapQueue, PixFormat, PixFormatMplane, PlaneFormat, VideoDevice,
};
use lemnos_v4l2::{Error, FourCc};

/// `V4L2_FIELD_NONE`: progressive frames.
const FIELD_NONE: u32 = 1;

/// The most memory planes a frame has (`VIDEO_MAX_PLANES`).
pub const MAX_PLANES: usize = lemnos_v4l2::v4l2::MAX_PLANES;

/// The format the driver asks the node for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wanted {
    /// The `VIDIOC_S_INPUT` index to select before the format, for a node whose kernel driver
    /// sets its pipeline up there. `None` selects nothing.
    pub input: Option<u32>,
    pub fourcc: FourCc,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub size: u32,
    /// A mode of the receiver's own, read by nothing here: the receiver's
    /// [`Prepare`] call is given it before the format is set. 0 for none.
    pub mode: u32,
}

/// What the node answered `VIDIOC_S_FMT` with: the first plane's numbers, and every memory
/// plane's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Granted {
    pub fourcc: FourCc,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub size: u32,
    /// Memory planes a frame is written in: 1 for a single-planar format.
    pub planes: usize,
    /// Each memory plane's size, the first `planes` of them.
    pub plane_sizes: [u32; MAX_PLANES],
}

impl Granted {
    /// Each memory plane's size.
    #[must_use]
    pub fn plane_sizes(&self) -> &[u32] {
        self.plane_sizes.get(..self.planes).unwrap_or(&[])
    }
}

/// A receiver's own call on the node, made after the node opens and before the input and the
/// format are set, with what is wanted: a vendor driver's private memory mode, say.
pub type Prepare = Box<dyn Fn(BorrowedFd<'_>, &Wanted) -> Result<()> + Send>;

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
    /// Bytes the node wrote, over all planes.
    pub bytes: u32,
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
    /// Asks for `slots` slots that take the caller's dma-bufs and starts streaming with
    /// nothing queued: the node fills what [`queue_handles`](Self::queue_handles) gives it.
    /// After `open`, in place of `stream_on`. `NOT_SUPPORTED` when the node's queue takes no
    /// dma-bufs. Open the node again before trying `stream_on`.
    fn stream_on_importing(&mut self, slots: u32) -> Result<()>;
    /// Queues slot `index` on the dma-bufs behind `planes`, one per memory plane of the
    /// node's format, for the node to fill. The node takes its own hold on the memory, so the
    /// borrow ends with the call. Refused for a count of planes other than the format's, and
    /// when a dma-buf is smaller than its plane.
    fn queue_handles(&mut self, index: u32, planes: &[BufferHandle<'_>]) -> Result<()>;
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
    /// The node's video inputs in index order (`VIDIOC_ENUMINPUT`, see
    /// [`VideoDevice::inputs`]). `NOT_SUPPORTED` when the node has no input support.
    fn inputs(&mut self) -> Result<Vec<InputInfo>>;
    /// The input the node has selected (`VIDIOC_G_INPUT`). `NOT_SUPPORTED` when the node has no
    /// input support.
    fn input(&mut self) -> Result<u32>;
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
    /// A metadata node (`V4L2_BUF_TYPE_META_CAPTURE`): its format is a data format and a buffer
    /// size, and a frame is a line of that size, not an image.
    metadata: bool,
    found: Option<Found>,
    /// The node and its format, between `open` and `stream_on`.
    opened: Option<(VideoDevice, BufType)>,
    /// The node's queue, while streaming: mapped, or importing dma-bufs.
    queue: Option<MmapQueue>,
    /// The node's format: each memory plane's size, what the dma-buf for it must hold.
    granted: Option<Granted>,
    /// The receiver's own call before the format.
    prepare: Option<Prepare>,
    /// The buffer `dequeue` returned last, for `payload`.
    current: Option<DequeuedBuffer>,
}

impl LinuxHardware {
    /// One read-only call on the node: on the open node when there is one, else on the node
    /// opened for the call alone. A node with no input support answers `NOT_SUPPORTED`.
    fn query<T>(&mut self, call: impl FnOnce(&VideoDevice) -> lemnos_v4l2::Result<T>) -> Result<T> {
        let refused = |e: &Error| {
            if e.is_not_supported() {
                Status::NOT_SUPPORTED
            } else {
                status(e)
            }
        };
        if let Some((dev, _)) = &self.opened {
            return call(dev).map_err(|e| refused(&e));
        }
        let video = self.find()?.video.clone();
        let dev = VideoDevice::open_read_only(&video).map_err(|e| refused(&e))?;
        call(&dev).map_err(|e| refused(&e))
    }

    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            metadata: false,
            found: None,
            opened: None,
            queue: None,
            granted: None,
            prepare: None,
            current: None,
        }
    }

    /// The node as a metadata node: the same calls, on `V4L2_BUF_TYPE_META_CAPTURE`, with the
    /// wanted fourcc as the data format and the wanted size as the buffer size. Lines of
    /// embedded data come off it (see [`crate::embedded`]).
    #[must_use]
    pub fn metadata(settings: Settings) -> Self {
        Self {
            metadata: true,
            ..Self::new(settings)
        }
    }

    /// The node, with `prepare` called on it at each open before the input and format.
    #[must_use]
    pub fn with_prepare(mut self, prepare: Prepare) -> Self {
        self.prepare = Some(prepare);
        self
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

/// Writes the inputs as the `camera.receiver` answer to `inputs`: one record per input (see
/// `lemnos_contract_camera_receiver::write_input`), into `into`. Returns the bytes written.
/// `BUFFER_TOO_SMALL` for more inputs than the answer holds, or a buffer too short for them.
pub fn input_records(list: &[InputInfo], into: &mut [u8]) -> Result<usize> {
    if list.len() > MAX_INPUTS {
        return Err(Status::BUFFER_TOO_SMALL);
    }
    for (i, input) in list.iter().enumerate() {
        let at = i * INPUT_RECORD;
        let record = into
            .get_mut(at..at + INPUT_RECORD)
            .ok_or(Status::BUFFER_TOO_SMALL)?;
        write_input(
            &Input {
                index: input.index,
                kind: input.kind,
                status: input.status,
                name: &input.name,
            },
            record,
        )?;
    }
    Ok(list.len() * INPUT_RECORD)
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
    let mut plane_sizes = [0; MAX_PLANES];
    let (fourcc, width, height, plane, planes) = match format {
        Format::Single(p) => {
            plane_sizes[0] = p.size_image;
            (
                p.fourcc,
                p.width,
                p.height,
                PlaneFormat {
                    bytes_per_line: p.bytes_per_line,
                    size_image: p.size_image,
                },
                1,
            )
        }
        Format::Multi(p) => {
            if p.planes.len() > MAX_PLANES {
                return Err(Status::IO);
            }
            for (size, plane) in plane_sizes.iter_mut().zip(&p.planes) {
                *size = plane.size_image;
            }
            (
                p.fourcc,
                p.width,
                p.height,
                p.planes.first().copied().ok_or(Status::IO)?,
                p.planes.len(),
            )
        }
        Format::Meta(m) => {
            // A line, not an image: its size is the one memory plane, and it has no stride.
            plane_sizes[0] = m.buffer_size;
            (
                m.fourcc,
                0,
                0,
                PlaneFormat {
                    bytes_per_line: 0,
                    size_image: m.buffer_size,
                },
                1,
            )
        }
    };
    Ok(Granted {
        fourcc,
        width,
        height,
        stride: plane.bytes_per_line,
        size: plane.size_image,
        planes,
        plane_sizes,
    })
}

/// The two calls `LinuxHardware::open` makes on the node. [`VideoDevice`] is the real one; the
/// call-order test gives a recorder instead, so the order is pinned without a device.
pub(crate) trait CaptureNode {
    /// `VIDIOC_S_INPUT`.
    fn set_input(&self, index: u32) -> lemnos_v4l2::Result<()>;
    /// `VIDIOC_S_FMT`: returns what the node chose.
    fn set_format(&self, buf_type: BufType, format: &Format) -> lemnos_v4l2::Result<Format>;
}

impl CaptureNode for VideoDevice {
    fn set_input(&self, index: u32) -> lemnos_v4l2::Result<()> {
        VideoDevice::set_input(self, index)
    }

    fn set_format(&self, buf_type: BufType, format: &Format) -> lemnos_v4l2::Result<Format> {
        VideoDevice::set_format(self, buf_type, format)
    }
}

/// The node setup after the receiver's own [`Prepare`] call: the input first, when the receiver
/// names one, then the format. A kernel driver may set up its pipeline in `VIDIOC_S_INPUT`, and the
/// format is only valid on that pipeline. If the input is refused, the format is not set.
pub(crate) fn select_and_negotiate(
    node: &dyn CaptureNode,
    wanted: &Wanted,
    buf_type: BufType,
    format: &Format,
) -> Result<Format> {
    if let Some(input) = wanted.input {
        node.set_input(input).map_err(|e| status(&e))?;
    }
    node.set_format(buf_type, format).map_err(|e| status(&e))
}

impl Hardware for LinuxHardware {
    fn open(&mut self, wanted: &Wanted) -> Result<Granted> {
        let video = self.find()?.video.clone();
        let dev = VideoDevice::open(&video).map_err(|e| status(&e))?;
        let caps = dev.capabilities().device_caps;
        let buf_type = if self.metadata {
            if !caps.contains(CapabilityFlags::META_CAPTURE) {
                return Err(Status::NOT_SUPPORTED);
            }
            BufType::MetaCapture
        } else if caps.contains(CapabilityFlags::VIDEO_CAPTURE_MPLANE) {
            BufType::VideoCaptureMplane
        } else if caps.contains(CapabilityFlags::VIDEO_CAPTURE) {
            BufType::VideoCapture
        } else {
            return Err(Status::NOT_SUPPORTED);
        };
        if let Some(prepare) = &self.prepare {
            prepare(dev.as_fd(), wanted)?;
        }
        let format = if buf_type.is_meta() {
            Format::Meta(MetaFormat {
                fourcc: wanted.fourcc,
                buffer_size: wanted.size,
                width: 0,
                height: 0,
                bytes_per_line: 0,
            })
        } else if buf_type.is_multiplanar() {
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
        let chosen = select_and_negotiate(&dev, wanted, buf_type, &format)?;
        let granted = granted(&chosen)?;
        self.granted = Some(granted);
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

    fn stream_on_importing(&mut self, slots: u32) -> Result<()> {
        let (dev, buf_type) = self.opened.take().ok_or(Status::INVALID_ARGUMENT)?;
        // A queue that takes no dma-bufs refuses the request, and the node is closed with it.
        let queue = MmapQueue::importing(dev, buf_type, slots, "capture")
            .map_err(|_| Status::NOT_SUPPORTED)?;
        queue.stream_on().map_err(|e| status(&e))?;
        self.queue = Some(queue);
        Ok(())
    }

    fn queue_handles(&mut self, index: u32, planes: &[BufferHandle<'_>]) -> Result<()> {
        let queue = self.queue.as_ref().ok_or(Status::IO)?;
        let granted = self.granted.as_ref().ok_or(Status::IO)?;
        let (Some(first), true) = (planes.first(), planes.len() == granted.planes) else {
            return Err(Status::INVALID_ARGUMENT);
        };
        // The node writes each plane whole: each dma-buf's real size must hold its plane.
        let mut fds = [first.fd(); MAX_PLANES];
        for ((fd, plane), need) in fds.iter_mut().zip(planes).zip(granted.plane_sizes()) {
            let size = dma_buf::size(plane.fd()).map_err(|_| Status::IO)?;
            if size < u64::from(*need) {
                return Err(Status::INVALID_ARGUMENT);
            }
            *fd = plane.fd();
        }
        queue
            .queue_dmabuf(index, fds.get(..planes.len()).unwrap_or(&[]))
            .map_err(|e| status(&e))
    }

    fn stop(&mut self) -> Result<()> {
        self.current = None;
        self.opened = None;
        self.granted = None;
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
            // 0, no time, only if `CLOCK_MONOTONIC` cannot be read, which Linux never refuses.
            timestamp_ns: done.monotonic_ns().unwrap_or(0),
            bad: done.flags.contains(BufferFlags::ERROR),
            planes: done.planes.len(),
            bytes: u32::try_from(done.bytes_used()).unwrap_or(u32::MAX),
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

    fn inputs(&mut self) -> Result<Vec<InputInfo>> {
        self.query(|dev| dev.inputs())
    }

    fn input(&mut self) -> Result<u32> {
        self.query(|dev| dev.input())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Records the calls in order; `refuse_input` makes `VIDIOC_S_INPUT` fail.
    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        refuse_input: bool,
    }

    impl CaptureNode for Recorder {
        fn set_input(&self, index: u32) -> lemnos_v4l2::Result<()> {
            self.calls.borrow_mut().push(format!("input {index}"));
            if self.refuse_input {
                return Err(Error::Ioctl {
                    name: "VIDIOC_S_INPUT",
                    errno: 22,
                });
            }
            Ok(())
        }

        fn set_format(&self, _: BufType, _: &Format) -> lemnos_v4l2::Result<Format> {
            self.calls.borrow_mut().push("format".to_string());
            Ok(Format::Single(PixFormat::default()))
        }
    }

    fn wanted(input: Option<u32>) -> Wanted {
        Wanted {
            input,
            fourcc: FourCc::new(b"NV12"),
            width: 64,
            height: 32,
            stride: 64,
            size: 3072,
            mode: 0,
        }
    }

    #[test]
    fn the_input_is_selected_before_the_format_is_set() {
        let node = Recorder::default();
        let format = Format::Single(PixFormat::default());
        select_and_negotiate(&node, &wanted(Some(2)), BufType::VideoCapture, &format).unwrap();
        // The order the sunxi-vin pipeline needs: S_INPUT, then S_FMT.
        assert_eq!(*node.calls.borrow(), ["input 2", "format"]);
    }

    #[test]
    fn no_input_named_means_no_input_call() {
        let node = Recorder::default();
        let format = Format::Single(PixFormat::default());
        select_and_negotiate(&node, &wanted(None), BufType::VideoCapture, &format).unwrap();
        assert_eq!(*node.calls.borrow(), ["format"]);
    }

    #[test]
    fn a_refused_input_stops_before_the_format() {
        let node = Recorder {
            refuse_input: true,
            ..Recorder::default()
        };
        let format = Format::Single(PixFormat::default());
        let err = select_and_negotiate(&node, &wanted(Some(7)), BufType::VideoCapture, &format);
        assert!(err.is_err());
        // The format is never set on a pipeline the input did not select.
        assert_eq!(*node.calls.borrow(), ["input 7"]);
    }

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
                planes: 2,
                plane_sizes: [4096, 1, 0, 0, 0, 0, 0, 0],
            }
        );
        assert_eq!(granted(&multi).unwrap().plane_sizes(), &[4096, 1]);
        let single = Format::Single(PixFormat {
            width: 8,
            height: 2,
            fourcc: FourCc::GREY,
            bytes_per_line: 8,
            size_image: 16,
            ..Default::default()
        });
        assert_eq!(granted(&single).unwrap().size, 16);
        assert_eq!(granted(&single).unwrap().plane_sizes(), &[16]);
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
    fn a_format_of_more_planes_than_a_buffer_has_is_refused() {
        let over = Format::Multi(PixFormatMplane {
            planes: vec![
                PlaneFormat {
                    bytes_per_line: 1,
                    size_image: 1,
                };
                MAX_PLANES + 1
            ],
            ..Default::default()
        });
        assert_eq!(granted(&over).unwrap_err(), Status::IO);
    }

    #[test]
    fn handles_are_refused_while_nothing_streams() {
        let mut hw = LinuxHardware::new(Settings::new("vin_video0").unwrap());
        let file = std::fs::File::open("/dev/null").unwrap();
        let handle = BufferHandle::from(file.as_fd());
        assert_eq!(hw.queue_handles(0, &[handle]).unwrap_err(), Status::IO);
        assert_eq!(hw.queue_handles(0, &[]).unwrap_err(), Status::IO);
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
