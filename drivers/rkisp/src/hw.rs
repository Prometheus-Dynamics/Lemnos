//! The ISP's nodes as the driver sees them, and the Linux implementation over the vendor kernel
//! driver's media device, subdevices and video nodes through `lemnos-v4l2`.
//!
//! Every buffer here is one the kernel allocated and this side mapped (`MMAP`): no caller's
//! number becomes an address, an offset into device memory or a file descriptor. Indices and
//! payload sizes the kernel reports are checked by `lemnos-v4l2` against the buffers actually
//! mapped before a slice is lent out.

use std::io;

use lemnos_core::{Result, Status};
use lemnos_v4l2::media::{LinkFlags, MediaDevice, PadRef, Topology, find_media_devices};
use lemnos_v4l2::subdev::{MbusCode, MbusFormat, Subdev, Which};
use lemnos_v4l2::v4l2::{
    BufType, BufferFlags, DequeuedBuffer, Format, MmapQueue, PixFormatMplane, PlaneFormat,
    VideoDevice,
};
use lemnos_v4l2::{Error, FourCc};

use crate::graph::{ISP_ENTITY, MAINPATH_ENTITY, Nodes, PAD_SINK, PAD_SOURCE, SELFPATH_ENTITY};

/// `V4L2_FIELD_NONE`: progressive frames.
const FIELD_NONE: u32 = 1;

/// The ISP's kernel queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    /// Parameter buffers in (`rkisp-input-params`, a meta output).
    Params = 0,
    /// Statistics out (`rkisp-statistics`, a meta capture).
    Statistics = 1,
    /// Output 0's images (`rkisp_mainpath`).
    Main = 2,
    /// Output 1's images (`rkisp_selfpath`).
    SelfPath = 3,
}

/// Number of [`Queue`]s.
pub const QUEUES: usize = 4;

impl Queue {
    /// The queue that makes output `output`'s images.
    #[must_use]
    pub const fn output(output: usize) -> Option<Self> {
        match output {
            0 => Some(Self::Main),
            1 => Some(Self::SelfPath),
            _ => None,
        }
    }
}

/// An image format on an output node: what is asked, and what the kernel answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Little-endian four-character code.
    pub fourcc: u32,
    /// Bytes per line, as the kernel lays the image out; 0 when asking.
    pub stride: u32,
    /// Bytes per image, as the kernel lays the image out; 0 when asking.
    pub size: u32,
}

/// What the path is opened with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    /// The path's entities, from [`graph::plan`](crate::graph::plan).
    pub nodes: Nodes,
    /// Bytes a parameter buffer must hold: the generation's parameters.
    pub params_bytes: u32,
    /// Bytes a statistics buffer must hold.
    pub statistics_bytes: u32,
    /// Parameter buffers to ask for.
    pub params_buffers: u32,
    /// Statistics and image buffers to ask for, per queue.
    pub buffers: u32,
    /// Each output's image, as agreed with [`Hardware::try_output`]; `None` for an output off.
    pub outputs: [Option<Image>; 2],
}

/// What the kernel gave when the path opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Opened {
    /// Buffers on each queue, by [`Queue`]; 0 for a queue not opened.
    pub buffers: [u32; QUEUES],
    /// Each output's image as the kernel set it.
    pub outputs: [Option<Image>; 2],
}

/// A buffer the kernel finished with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Done {
    /// The kernel's buffer index on its queue: below that queue's count in [`Opened`].
    pub index: u32,
    /// The kernel's frame sequence number. On the parameter queue: the frame the parameters
    /// were applied to.
    pub sequence: u32,
    pub timestamp_ns: u64,
    /// The kernel flagged the buffer as failed.
    pub bad: bool,
}

/// What the driver asks of the ISP. The real one is [`LinuxHardware`]; tests use a stand-in.
pub trait Hardware: Send {
    /// The ISP's media graph, opening its media device if it isn't open.
    fn topology(&mut self) -> Result<Topology>;
    /// Turns a data link on or off.
    fn setup_link(&mut self, source: PadRef, sink: PadRef, enable: bool) -> Result<()>;
    /// Sets `raw` on the pad `source` that feeds the ISP and on the ISP's raw input, and the
    /// ISP's image pad to YUV of the same size. `t` is the graph `source` and `isp` are from.
    fn set_formats(
        &mut self,
        t: &Topology,
        source: PadRef,
        isp: u32,
        raw: MbusFormat,
    ) -> Result<()>;
    /// What output `output`'s node would make of `image` (`VIDIOC_TRY_FMT`), stopped.
    fn try_output(&mut self, output: usize, image: Image) -> Result<Image>;
    /// Opens the parameter, statistics and image nodes, sets their formats, makes their
    /// buffers, queues every statistics and image buffer and starts streaming.
    fn open(&mut self, t: &Topology, setup: &Setup) -> Result<Opened>;
    /// Stops streaming, frees every buffer and closes every node. Nothing open is not an error.
    fn stop(&mut self);
    /// The next buffer `queue` finished, if one is ready. Never blocks; allocates nothing.
    fn dequeue(&mut self, queue: Queue) -> Result<Option<Done>>;
    /// The filled bytes of the buffer `dequeue` returned last on `queue`, as far as the kernel
    /// reported them and they lie inside the mapping.
    fn payload(&self, queue: Queue, done: &Done) -> Result<&[u8]>;
    /// Gives buffer `index` of a capture queue back to the kernel.
    fn requeue(&mut self, queue: Queue, index: u32) -> Result<()>;
    /// The mapped bytes of parameter buffer `index`, for filling. Only a buffer the kernel
    /// doesn't hold.
    fn params_buffer(&mut self, index: u32) -> Result<&mut [u8]>;
    /// Queues parameter buffer `index` with its first `bytes` filled.
    fn queue_params(&mut self, index: u32, bytes: u32) -> Result<()>;
}

/// Which ISP to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The ISP's media device driver name (`rkisp-vir0`), or `None` to use the only ISP.
    device: Option<String>,
}

/// A media device driver name is at most 15 bytes (`media_device_info.driver`).
const MAX_DRIVER: usize = 15;

impl Settings {
    /// `INVALID_ARGUMENT` for a name no media device could have.
    pub fn new(device: Option<&str>) -> Result<Self> {
        if device.is_some_and(|d| d.is_empty() || d.len() > MAX_DRIVER || d.contains('\0')) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            device: device.map(str::to_owned),
        })
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

/// The first plane of an image format the kernel answered, as an [`Image`].
fn image_of(format: &Format) -> Result<Image> {
    let Format::Multi(p) = format else {
        return Err(Status::NOT_SUPPORTED);
    };
    // One plane: the chroma follows the luma in the same buffer.
    let [plane] = p.planes.as_slice() else {
        return Err(Status::NOT_SUPPORTED);
    };
    Ok(Image {
        width: p.width,
        height: p.height,
        fourcc: p.fourcc.to_u32(),
        stride: plane.bytes_per_line,
        size: plane.size_image,
    })
}

fn image_format(image: Image) -> Format {
    Format::Multi(PixFormatMplane {
        width: image.width,
        height: image.height,
        fourcc: FourCc(image.fourcc),
        field: FIELD_NONE,
        planes: vec![PlaneFormat {
            bytes_per_line: image.stride,
            size_image: image.size,
        }],
        ..Default::default()
    })
}

/// The ISP through its media device. Opened as it's needed, so an ISP not yet probed is a failed
/// call rather than a failed bind, and a call can succeed once it is.
pub struct LinuxHardware {
    settings: Settings,
    media: Option<MediaDevice>,
    /// Each [`Queue`]'s mapped buffers, while open.
    queues: [Option<MmapQueue>; QUEUES],
    /// The buffer `dequeue` returned last on each queue, for `payload`.
    current: [Option<DequeuedBuffer>; QUEUES],
}

impl LinuxHardware {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            media: None,
            queues: [None, None, None, None],
            current: [None, None, None, None],
        }
    }

    fn media(&mut self) -> Result<&MediaDevice> {
        if self.media.is_none() {
            let wanted = |m: &MediaDevice| {
                let Ok(info) = m.device_info() else {
                    return false;
                };
                let named = match &self.settings.device {
                    Some(device) => info.driver == *device,
                    None => info.driver.starts_with("rkisp"),
                };
                named
                    && m.topology()
                        .is_ok_and(|t| t.entity_by_name(ISP_ENTITY).is_some())
            };
            let mut found = find_media_devices(wanted).into_iter();
            let path = match (found.next(), found.next()) {
                (Some(path), None) => path,
                (None, _) => return Err(Status::NO_DEVICE),
                // Two ISPs and none named: the board has to say which.
                (Some(_), Some(_)) => return Err(Status::INVALID_ARGUMENT),
            };
            self.media = Some(MediaDevice::open(path).map_err(|e| status(&e))?);
        }
        self.media.as_ref().ok_or(Status::NO_DEVICE)
    }

    fn queue(&self, queue: Queue) -> Result<&MmapQueue> {
        self.queues
            .get(queue as usize)
            .and_then(Option::as_ref)
            .ok_or(Status::IO)
    }
}

/// The node of entity `id` in `t`.
fn node(t: &Topology, id: u32) -> Result<VideoDevice> {
    let path = t.devnode_path(id).ok_or(Status::NO_DEVICE)?;
    VideoDevice::open(path).map_err(|e| status(&e))
}

/// A meta queue on entity `id` whose buffers hold at least `bytes`.
fn meta_queue(
    t: &Topology,
    id: u32,
    buf_type: BufType,
    bytes: u32,
    count: u32,
    name: &'static str,
) -> Result<MmapQueue> {
    let dev = node(t, id)?;
    let Format::Meta(meta) = dev.format(buf_type).map_err(|e| status(&e))? else {
        return Err(Status::NOT_SUPPORTED);
    };
    // The layouts are the vendor driver's; a node whose buffers are smaller runs another
    // version of them.
    if meta.buffer_size < bytes {
        return Err(Status::NOT_SUPPORTED);
    }
    MmapQueue::new(dev, buf_type, count, name).map_err(|e| status(&e))
}

/// An image queue on entity `id`, set to `image`: the queue and what the kernel set.
fn image_queue(t: &Topology, id: u32, image: Image, count: u32) -> Result<(MmapQueue, Image)> {
    let dev = node(t, id)?;
    let set = dev
        .set_format(BufType::VideoCaptureMplane, &image_format(image))
        .map_err(|e| status(&e))?;
    let set = image_of(&set)?;
    let queue = MmapQueue::new(dev, BufType::VideoCaptureMplane, count, "rkisp image")
        .map_err(|e| status(&e))?;
    Ok((queue, set))
}

fn count(queue: &MmapQueue) -> Result<u32> {
    u32::try_from(queue.len()).map_err(|_| Status::IO)
}

impl Hardware for LinuxHardware {
    fn topology(&mut self) -> Result<Topology> {
        self.media()?.topology().map_err(|e| status(&e))
    }

    fn setup_link(&mut self, source: PadRef, sink: PadRef, enable: bool) -> Result<()> {
        let flags = if enable {
            LinkFlags::ENABLED
        } else {
            LinkFlags::empty()
        };
        self.media()?
            .setup_link(source, sink, flags)
            .map_err(|e| status(&e))
    }

    fn set_formats(
        &mut self,
        t: &Topology,
        source: PadRef,
        isp: u32,
        raw: MbusFormat,
    ) -> Result<()> {
        let open = |id| {
            let path = t.devnode_path(id).ok_or(Status::NO_DEVICE)?;
            Subdev::open(path).map_err(|e| status(&e))
        };
        let same = |a: &MbusFormat, b: &MbusFormat| {
            (a.width, a.height, a.code) == (b.width, b.height, b.code)
        };
        let upstream = open(source.entity)?;
        let mut got = upstream
            .format(source.index, Which::Active)
            .map_err(|e| status(&e))?;
        if !same(&got, &raw) {
            got = upstream
                .set_format(source.index, Which::Active, &raw)
                .map_err(|e| status(&e))?;
        }
        if !same(&got, &raw) {
            return Err(Status::NOT_SUPPORTED);
        }
        let sd = open(isp)?;
        let sink = sd
            .set_format(PAD_SINK, Which::Active, &got)
            .map_err(|e| status(&e))?;
        if !same(&sink, &raw) {
            return Err(Status::NOT_SUPPORTED);
        }
        let yuv = MbusFormat {
            code: MbusCode::YUYV8_2X8,
            ..sink
        };
        let out = sd
            .set_format(PAD_SOURCE, Which::Active, &yuv)
            .map_err(|e| status(&e))?;
        if out.code != MbusCode::YUYV8_2X8 {
            return Err(Status::NOT_SUPPORTED);
        }
        Ok(())
    }

    fn try_output(&mut self, output: usize, image: Image) -> Result<Image> {
        let name = match output {
            0 => MAINPATH_ENTITY,
            1 => SELFPATH_ENTITY,
            _ => return Err(Status::NOT_FOUND),
        };
        let t = self.topology()?;
        let id = t.entity_by_name(name).ok_or(Status::NOT_FOUND)?.id;
        let dev = node(&t, id)?;
        let tried = dev
            .try_format(BufType::VideoCaptureMplane, &image_format(image))
            .map_err(|e| status(&e))?;
        image_of(&tried)
    }

    fn open(&mut self, t: &Topology, setup: &Setup) -> Result<Opened> {
        self.stop();
        let mut opened = Opened::default();
        let params = meta_queue(
            t,
            setup.nodes.params,
            BufType::MetaOutput,
            setup.params_bytes,
            setup.params_buffers,
            "rkisp params",
        )?;
        let statistics = meta_queue(
            t,
            setup.nodes.statistics,
            BufType::MetaCapture,
            setup.statistics_bytes,
            setup.buffers,
            "rkisp statistics",
        )?;
        opened.buffers[Queue::Params as usize] = count(&params)?;
        opened.buffers[Queue::Statistics as usize] = count(&statistics)?;
        self.queues[Queue::Params as usize] = Some(params);
        self.queues[Queue::Statistics as usize] = Some(statistics);
        let entities = [Some(setup.nodes.main), setup.nodes.self_path];
        for (output, (image, id)) in setup.outputs.iter().zip(entities).enumerate() {
            let (Some(image), Some(id), Some(queue)) = (image, id, Queue::output(output)) else {
                continue;
            };
            let (q, set) = image_queue(t, id, *image, setup.buffers)?;
            opened.buffers[queue as usize] = count(&q)?;
            opened.outputs[output] = Some(set);
            self.queues[queue as usize] = Some(q);
        }
        for queue in [Queue::Statistics, Queue::Main, Queue::SelfPath] {
            let Some(q) = self.queues[queue as usize].as_ref() else {
                continue;
            };
            for index in 0..opened.buffers[queue as usize] {
                q.queue(index, &[]).map_err(|e| status(&e))?;
            }
        }
        // The main path last: the vendor driver starts the sensor with it.
        for queue in [
            Queue::Params,
            Queue::Statistics,
            Queue::SelfPath,
            Queue::Main,
        ] {
            if let Some(q) = self.queues[queue as usize].as_ref() {
                q.stream_on().map_err(|e| status(&e))?;
            }
        }
        Ok(opened)
    }

    fn stop(&mut self) {
        self.current = [None, None, None, None];
        // The main path first: the sensor stops with it. A failure to stop is nothing to act on:
        // the buffers are freed with the node either way.
        for queue in [
            Queue::Main,
            Queue::SelfPath,
            Queue::Statistics,
            Queue::Params,
        ] {
            if let Some(q) = self.queues[queue as usize].take() {
                let _ = q.close();
            }
        }
        // Found again at the next open, in case the ISP came back as another device.
        self.media = None;
    }

    fn dequeue(&mut self, queue: Queue) -> Result<Option<Done>> {
        let q = self.queue(queue)?;
        let Some(done) = q.try_dequeue().map_err(|e| status(&e))? else {
            return Ok(None);
        };
        let finished = Done {
            index: done.index,
            sequence: done.sequence,
            // 0, no time, only if `CLOCK_MONOTONIC` cannot be read, which Linux never refuses.
            timestamp_ns: done.monotonic_ns().unwrap_or(0),
            bad: done.flags.contains(BufferFlags::ERROR),
        };
        if let Some(slot) = self.current.get_mut(queue as usize) {
            *slot = Some(done);
        }
        Ok(Some(finished))
    }

    fn payload(&self, queue: Queue, done: &Done) -> Result<&[u8]> {
        let q = self.queue(queue)?;
        let current = self
            .current
            .get(queue as usize)
            .and_then(Option::as_ref)
            .ok_or(Status::IO)?;
        if current.index != done.index {
            return Err(Status::INVALID_ARGUMENT);
        }
        q.payload(current, 0).map_err(|e| status(&e))
    }

    fn requeue(&mut self, queue: Queue, index: u32) -> Result<()> {
        self.queue(queue)?.queue(index, &[]).map_err(|e| status(&e))
    }

    fn params_buffer(&mut self, index: u32) -> Result<&mut [u8]> {
        self.queues[Queue::Params as usize]
            .as_mut()
            .ok_or(Status::IO)?
            .plane_mut(index as usize, 0)
            .ok_or(Status::INVALID_ARGUMENT)
    }

    fn queue_params(&mut self, index: u32, bytes: u32) -> Result<()> {
        self.queue(Queue::Params)?
            .queue(index, &[bytes])
            .map_err(|e| status(&e))
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
        assert_eq!(status(&ioctl(5)), Status::IO, "EIO");
        assert_eq!(status(&Error::Invalid("crafted".into())), Status::IO);
    }

    #[test]
    fn an_image_is_one_plane_of_a_multi_planar_format() {
        let one = Format::Multi(PixFormatMplane {
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
            image_of(&one).unwrap(),
            Image {
                width: 64,
                height: 32,
                fourcc: u32::from_le_bytes(*b"NV12"),
                stride: 64,
                size: 3072,
            }
        );
        let two = Format::Multi(PixFormatMplane {
            planes: vec![PlaneFormat::default(); 2],
            ..Default::default()
        });
        assert_eq!(image_of(&two).unwrap_err(), Status::NOT_SUPPORTED);
        let none = Format::Multi(PixFormatMplane::default());
        assert_eq!(image_of(&none).unwrap_err(), Status::NOT_SUPPORTED);
    }

    #[test]
    fn a_device_name_is_a_name() {
        assert!(Settings::new(None).is_ok());
        assert!(Settings::new(Some("rkisp-vir0")).is_ok());
        assert!(Settings::new(Some("")).is_err());
        assert!(Settings::new(Some("a-name-far-too-long")).is_err());
        assert!(Settings::new(Some("a\0b")).is_err());
    }

    #[test]
    fn calls_out_of_order_are_refused_without_touching_a_device() {
        let mut hw = LinuxHardware::new(Settings::new(None).unwrap());
        assert_eq!(hw.dequeue(Queue::Statistics).unwrap_err(), Status::IO);
        assert_eq!(hw.requeue(Queue::Main, 0).unwrap_err(), Status::IO);
        assert_eq!(hw.params_buffer(0).unwrap_err(), Status::IO);
        assert_eq!(hw.queue_params(0, 1).unwrap_err(), Status::IO);
        hw.stop();
    }
}
