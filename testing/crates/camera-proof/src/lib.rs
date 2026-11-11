//! The camera proof tool: finds a camera by its sensor tag, follows `link` from the sensor to its
//! receiver and from the receiver to its ISP, assembles them in the order the contracts require,
//! streams frames and saves them.
//!
//! It knows no chip. Every step is a call on a `camera.*` contract, reached through a [`Bus`]. The
//! [`host`] module is the only bus: it reaches devices in one process with simulated time. There
//! is no bus over `lemnosd`'s socket yet, though the client has each part one needs: calls, each
//! device's `link` (`lemnos_client::follow`), and a frame stream's consumer end
//! (`Client::open_stream`, with `lemnos-linux-stream`'s platform on Linux).
//!
//! Frames carry the sequence, identity and timestamp their producer gave them. A capture counts
//! sequence gaps, the receiver's own drop count, and the frame rate measured from timestamps.

pub mod host;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lemnos_contract_camera_isp::{CameraIsp, IspInput};
use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, MAX_STREAM_NAME, Packing,
};
use lemnos_contract_camera_sensor::{CameraSensor, Mode};
use lemnos_core::{Bound, Caller, Contract, Status};
use lemnos_wire::ring::{FrameSlot, RingError};

pub use host::HostBus;

/// A device a bus found: its index on the bus and its board id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub index: u32,
    pub id: String,
}

/// Reaches devices. The camera code only uses this trait, so the transport is the caller's.
pub trait Bus {
    /// The contract client's caller, which borrows the bus.
    type Caller<'a>: Caller
    where
        Self: 'a;

    /// Every device providing the tag, in board order.
    fn devices(&self, tag: &str) -> Vec<Device>;
    /// The device a board entry links to, if it names one.
    fn link(&self, device: &Device) -> Result<Option<Device>, Error>;
    /// A client for `C` on `device`. Refused when the device does not provide `C`.
    fn open<'a, C: Contract + ?Sized>(
        &'a self,
        device: &Device,
    ) -> Result<Bound<C, Self::Caller<'a>>, Error>;
    /// Takes the consumer's end of a frame stream the device offers.
    fn open_frames<'a>(
        &'a self,
        device: &Device,
        stream: &str,
    ) -> Result<Box<dyn Frames + 'a>, Error>;
    /// Lets time pass until the next thing on the bus is due. Refused when nothing is scheduled.
    fn wait(&self) -> Result<(), Error>;
}

/// A consumer's end of one frame stream.
pub trait Frames {
    /// Bytes in one buffer: the largest frame.
    fn buffer_bytes(&self) -> usize;
    /// The next frame. Its buffer is held until [`Frames::release`].
    fn pop(&mut self) -> Result<Option<FrameSlot>, Error>;
    /// Copies the frame's bytes into `out`, which is as long as `frame.bytes`.
    fn read(&mut self, frame: &FrameSlot, out: &mut [u8]) -> Result<(), Error>;
    /// Gives the frame's buffer back to the producer.
    fn release(&mut self, frame: FrameSlot) -> Result<(), Error>;
}

/// Where a captured frame goes.
pub trait FrameSink {
    fn save(&mut self, frame: &FrameSlot, bytes: &[u8]) -> io::Result<()>;
}

/// Writes each frame to `frame-<sequence>.raw` in a directory.
pub struct DirSink {
    dir: PathBuf,
}

impl DirSink {
    /// A sink in `dir`, which is created if it is missing.
    pub fn new(dir: impl AsRef<Path>) -> io::Result<Self> {
        fs::create_dir_all(dir.as_ref())?;
        Ok(Self {
            dir: dir.as_ref().to_path_buf(),
        })
    }
}

impl FrameSink for DirSink {
    fn save(&mut self, frame: &FrameSlot, bytes: &[u8]) -> io::Result<()> {
        fs::write(
            self.dir.join(format!("frame-{:08}.raw", frame.sequence)),
            bytes,
        )
    }
}

/// What can go wrong. Each contract call names the step that made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// This many `camera.sensor/1` devices, when exactly one was wanted.
    SensorCount(usize),
    NoSuchSensor(String),
    NoSuchDevice(String),
    /// The sensor's link is missing: there is no receiver to follow it to.
    NotLinked(String),
    /// The device does not provide the contract the link needs.
    WrongContract {
        device: String,
        contract: &'static str,
    },
    /// A contract call failed.
    Call {
        step: &'static str,
        status: Status,
    },
    /// A frame stream could not be taken.
    Stream {
        stream: String,
        status: Status,
    },
    NotStarted,
    /// Nothing is scheduled, so no frame can come.
    NoTicks,
    /// No frame arrived within the wait budget.
    Stalled {
        frames: u64,
    },
    Ring(RingError),
    ReturnQueueFull,
    BadBuffer,
    ReadFailed,
    FrameTooLarge {
        bytes: u32,
        buffer: usize,
    },
    Save(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SensorCount(n) => write!(f, "{n} camera sensors found, expected one"),
            Self::NoSuchSensor(id) => write!(f, "no camera sensor {id:?}"),
            Self::NoSuchDevice(id) => write!(f, "no device {id:?}"),
            Self::NotLinked(id) => write!(f, "sensor {id:?} has no link to a receiver"),
            Self::WrongContract { device, contract } => {
                write!(f, "device {device:?} does not provide {contract}")
            }
            Self::Call { step, status } => write!(f, "{step} failed: {status:?}"),
            Self::Stream { stream, status } => write!(f, "frame stream {stream:?}: {status:?}"),
            Self::NotStarted => write!(f, "the camera is not started"),
            Self::NoTicks => write!(f, "nothing is scheduled, so no frame can come"),
            Self::Stalled { frames } => write!(f, "stalled after {frames} frames"),
            Self::Ring(e) => write!(f, "frame ring: {e:?}"),
            Self::ReturnQueueFull => write!(f, "the return queue is full"),
            Self::BadBuffer => write!(f, "a frame names a buffer outside the stream"),
            Self::ReadFailed => write!(f, "a frame's bytes could not be read"),
            Self::FrameTooLarge { bytes, buffer } => {
                write!(
                    f,
                    "a {bytes} byte frame does not fit a {buffer} byte buffer"
                )
            }
            Self::Save(e) => write!(f, "saving a frame: {e}"),
        }
    }
}

impl std::error::Error for Error {}

fn step<T>(name: &'static str, result: lemnos_core::Result<T>) -> Result<T, Error> {
    result.map_err(|status| Error::Call { step: name, status })
}

/// Which sensor to use and which mode it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// The sensor's board id. When unset, the board must have exactly one sensor.
    pub sensor: Option<String>,
    pub mode: u16,
}

/// A camera assembled from a sensor, the receiver its link names, and the ISP the receiver links
/// to if there is one.
pub struct Camera<'a, B: Bus> {
    bus: &'a B,
    sensor: Device,
    receiver: Device,
    isp: Option<Device>,
    sensor_api: Bound<dyn CameraSensor, B::Caller<'a>>,
    receiver_api: Bound<dyn CameraReceiver, B::Caller<'a>>,
    isp_api: Option<Bound<dyn CameraIsp, B::Caller<'a>>>,
    mode_index: u16,
    mode: Mode,
    stream: String,
    layout: Option<FrameLayout>,
}

/// Finds the sensor, follows its link to the receiver and the receiver's link to the ISP. Nothing
/// is started.
pub fn assemble<'a, B: Bus>(bus: &'a B, options: &Options) -> Result<Camera<'a, B>, Error> {
    let found = bus.devices(<dyn CameraSensor as Contract>::TAG.as_str());
    let sensor = match options.sensor.as_deref() {
        Some(id) => found
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| Error::NoSuchSensor(id.to_string()))?,
        None => {
            if found.len() != 1 {
                return Err(Error::SensorCount(found.len()));
            }
            found.into_iter().next().ok_or(Error::SensorCount(0))?
        }
    };
    let mut sensor_api = bus.open::<dyn CameraSensor>(&sensor)?;
    let receiver = bus
        .link(&sensor)?
        .ok_or_else(|| Error::NotLinked(sensor.id.clone()))?;
    let mut receiver_api = bus.open::<dyn CameraReceiver>(&receiver)?;
    let isp = bus.link(&receiver)?;
    let isp_api = match &isp {
        Some(device) => Some(bus.open::<dyn CameraIsp>(device)?),
        None => None,
    };
    let mode = step("sensor.mode", sensor_api.mode(options.mode))?;
    let mut name = [0u8; MAX_STREAM_NAME];
    let len = step(
        "receiver.frame_stream",
        receiver_api.frame_stream(&mut name),
    )?;
    let stream = String::from_utf8_lossy(name.get(..len).ok_or(Error::BadBuffer)?).into_owned();
    Ok(Camera {
        bus,
        sensor,
        receiver,
        isp,
        sensor_api,
        receiver_api,
        isp_api,
        mode_index: options.mode,
        mode,
        stream,
        layout: None,
    })
}

impl<B: Bus> Camera<'_, B> {
    pub fn sensor(&self) -> &Device {
        &self.sensor
    }

    pub fn receiver(&self) -> &Device {
        &self.receiver
    }

    pub fn isp(&self) -> Option<&Device> {
        self.isp.as_ref()
    }

    /// The name of the receiver's frame stream, as the receiver reported it.
    pub fn stream(&self) -> &str {
        &self.stream
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// The frame layout the receiver answered, once started.
    pub fn layout(&self) -> Option<FrameLayout> {
        self.layout
    }

    /// Brings the camera up in the order the contracts require: the sensor selects its mode, the
    /// receiver is configured with that mode's link, the ISP is set up on the receiver's layout,
    /// the receiver starts, the ISP starts, and the sensor starts last.
    pub fn start(&mut self) -> Result<FrameLayout, Error> {
        step(
            "sensor.select_mode",
            self.sensor_api.select_mode(self.mode_index),
        )?;
        let layout = step(
            "receiver.configure",
            self.receiver_api.configure(self.mode.link, Packing::Packed),
        )?;
        if let Some(isp) = self.isp_api.as_mut() {
            let input = IspInput {
                width: layout.width,
                height: layout.height,
                bit_depth: layout.bit_depth,
                bayer: self.mode.bayer,
            };
            step("isp.set_input", isp.set_input(input))?;
        }
        step("receiver.start", self.receiver_api.start())?;
        if let Some(isp) = self.isp_api.as_mut() {
            step("isp.start", isp.start())?;
        }
        step("sensor.start", self.sensor_api.start())?;
        self.layout = Some(layout);
        Ok(layout)
    }

    /// Stops the sensor first, then the ISP, then the receiver.
    pub fn stop(&mut self) -> Result<(), Error> {
        step("sensor.stop", self.sensor_api.stop())?;
        if let Some(isp) = self.isp_api.as_mut() {
            step("isp.stop", isp.stop())?;
        }
        step("receiver.stop", self.receiver_api.stop())?;
        self.layout = None;
        Ok(())
    }

    /// The receiver's counters since it started.
    pub fn counters(&mut self) -> Result<Counters, Error> {
        step("receiver.counters", self.receiver_api.counters())
    }
}

/// What a capture saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Frames taken off the stream.
    pub frames: u64,
    /// Frames the sink saved.
    pub saved: u64,
    /// Frames the receiver dropped for want of a buffer, since it started.
    pub dropped: u64,
    /// Sequence numbers skipped between the first and last frame.
    pub gaps: u64,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub first_ns: u64,
    pub last_ns: u64,
}

impl Report {
    /// The frame rate from the timestamps, in thousandths of a frame per second.
    pub fn fps_milli(&self) -> Option<u64> {
        if self.frames < 2 || self.last_ns <= self.first_ns {
            return None;
        }
        let span = u128::from(self.last_ns - self.first_ns);
        let intervals = u128::from(self.frames - 1);
        u64::try_from(intervals * 1_000_000_000_000 / span).ok()
    }

    fn record(&mut self, frame: &FrameSlot) {
        if self.frames == 0 {
            self.first_sequence = frame.sequence;
            self.first_ns = frame.timestamp_ns;
        }
        self.frames += 1;
        self.last_sequence = frame.sequence;
        self.last_ns = frame.timestamp_ns;
        let span = self.last_sequence.saturating_sub(self.first_sequence) + 1;
        self.gaps = span.saturating_sub(self.frames);
    }
}

/// Takes `frames` frames off a started camera, saves each one, releases its buffer, and reads the
/// receiver's drop count at the end.
pub fn capture<B: Bus>(
    camera: &mut Camera<'_, B>,
    frames: u64,
    sink: &mut dyn FrameSink,
) -> Result<Report, Error> {
    if camera.layout.is_none() {
        return Err(Error::NotStarted);
    }
    let bus = camera.bus;
    let mut stream = bus.open_frames(&camera.receiver, &camera.stream)?;
    let mut buffer = vec![0u8; stream.buffer_bytes()];
    let mut report = Report::default();
    let mut waits = 0u64;
    while report.frames < frames {
        match stream.pop()? {
            Some(frame) => {
                let capacity = buffer.len();
                let bytes = buffer
                    .get_mut(..frame.bytes as usize)
                    .ok_or(Error::FrameTooLarge {
                        bytes: frame.bytes,
                        buffer: capacity,
                    })?;
                stream.read(&frame, bytes)?;
                sink.save(&frame, bytes)
                    .map_err(|e| Error::Save(e.to_string()))?;
                report.saved += 1;
                report.record(&frame);
                stream.release(frame)?;
            }
            None => {
                waits += 1;
                if waits > frames * 4 + 16 {
                    return Err(Error::Stalled {
                        frames: report.frames,
                    });
                }
                bus.wait()?;
            }
        }
    }
    report.dropped = camera.counters()?.dropped;
    Ok(report)
}
