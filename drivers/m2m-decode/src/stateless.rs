//! A stateless V4L2 decoder given to a caller as `codec.block/1`: each job is one media
//! request, made of the caller's controls, its bitstream buffer and its picture buffers. No
//! bitstream is parsed here and no codec is named: which controls a codec takes and what goes
//! in them is the caller's.
//!
//! The block's name is `v4l2-stateless`. It serves any stateless codec the kernel has a V4L2
//! driver for: H.264 (`S264`, [`lemnos_linux_sys::v4l2::h264`]) and HEVC (`S265`,
//! [`lemnos_linux_sys::v4l2::hevc`]) take the same job shape, and the coded format is the
//! caller's to name. Its work is:
//!
//! - **Item [`TAG_SETUP`]**, exactly one, [`SETUP_BYTES`] long, five `u32`s: the coded pixel
//!   format (the source queue's fourcc), the picture's width and height, the picture pixel
//!   format asked for, and how many memory planes that has. The session is opened for the
//!   first job's setup and opened again when a job's differs, which forgets every picture
//!   decoded before.
//! - **Item [`TAG_STAMP`]**, at most one, 8 bytes: a `u64` of nanoseconds the job's picture is
//!   known by. It must be a whole number of microseconds (see [`TAG_STAMP`]). A later job's
//!   controls name a reference picture by the stamp of the job that decoded it, which is how
//!   the kernel finds it. 0 when left out.
//! - **Any item whose tag is a control id** ([`FIRST_CONTROL`] and up): one control of the
//!   request, at most [`MAX_CONTROLS`]. Four bytes are an integer control's value; any other
//!   length is a compound control's payload. The bytes are the caller's and go to the kernel
//!   as they are, by a pointer to the driver's own copy with that copy's length: the kernel
//!   checks a payload against the control's own size, and the bytes hold no pointer it
//!   follows.
//! - **Slot [`SLOT_SOURCE`]**, exactly one: the bitstream, `length` bytes at `offset` of a
//!   stream buffer (`offset` must be 0 on a device whose source queue is single-planar, which
//!   has nowhere to say it).
//! - **Slots [`SLOT_PICTURE`] and up**, one for each memory plane of the picture format, in
//!   order: the buffers the picture is decoded into, each from its first byte (`offset` and
//!   `length` 0).
//!
//! Anything else is refused: another tag, a slot not listed (a reference is not one of them,
//! see below), a slot twice, an item of the wrong size.
//!
//! # References
//!
//! A reference picture is not a slot. A decoder names one inside its controls, by the
//! timestamp of the capture buffer that holds it (H.264's `reference_ts`, HEVC's DPB entry
//! `timestamp`), and the kernel finds that buffer among its own. The job that decoded the
//! picture stamped it with [`TAG_STAMP`], so the stamp is the reference. Nothing a reference
//! names is an address this block reaches, so the block gives no buffer for it and checks
//! none: a stamp no capture buffer carries is for the device to fail, as it does any other
//! bad control.
//!
//! The picture a reference names is a capture buffer the block holds, the stream buffer the
//! consumer gave as that job's picture. The consumer keeps that buffer as it is for as long as
//! a later job names its stamp. A reference numbered as a slot, `2 + DPB index` for instance,
//! is refused on a one-plane picture and names the second plane of a two-plane one, so it is
//! not a convention of this block.
//!
//! The job's read-back is the bytes written to each picture plane, a little-endian `u32`
//! each. A picture the device flags as in error fails the job.
//!
//! # Buffers, and what is checked
//!
//! Both queues take the stream's dma-bufs (`V4L2_MEMORY_DMABUF`): nothing is copied. A stream
//! buffer without a handle refuses the job.
//!
//! - The source's `offset + length` must lie inside the real size of the dma-buf behind its
//!   buffer, asked of the kernel, and `length` must not be 0.
//! - Each picture plane's dma-buf must be at least the size the device's own format says the
//!   plane takes (`sizeimage`), so the device can't be told to write past it. The kernel
//!   checks both again when the buffers are queued.
//! - A picture is queued as the capture buffer with its first plane's stream buffer number,
//!   so a picture stays the same kernel buffer for as long as the caller keeps it in the same
//!   stream buffer, and a later job finds it by its stamp. The number must be below the
//!   buffers the device granted.
//!
//! # Failure
//!
//! The node is opened when the first job comes. A device call that fails, a request that
//! doesn't complete in [`Settings::timeout`], or a picture that doesn't come back closes the
//! session and fails the job; the next job opens it again after a backoff that doubles from
//! 250 ms to 8 s. It never gives up and never turns itself off. When the consumer goes the
//! session is closed, which makes the kernel let go of that consumer's buffers.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use lemnos_contract_codec_block::work::Work;
use lemnos_contract_codec_block::{BlockInfo, CodecBlock, Counters};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_driver_codec_jobs::{Block, Buffers, Server, Start, name_into};
use lemnos_hal::blocks::BufferKind;
use lemnos_linux_sys::dma_buf;
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::{
    self, Format, MAX_PLANES, Memory, PixFormat, PixFormatMplane, PlaneFormat, PlaneMemory,
    QueuePlane,
};

use crate::node::{LinuxNode, Node};
use crate::session::{M2mSession, QueueSetup, Ready};

/// The block's name.
pub const NAME: &str = "v4l2-stateless";
/// The item that says what the session is set up for.
pub const TAG_SETUP: u32 = 1;
/// The item that says what the job's picture is known by.
///
/// The stamp is nanoseconds, and must be a whole number of microseconds (a multiple of 1000).
/// The kernel keeps a buffer's timestamp as a `timeval`, in microseconds, and a reference names
/// it by that value: a stamp finer than a microsecond could never be found, so the job is
/// refused rather than rounded.
pub const TAG_STAMP: u32 = 2;
/// The lowest tag that is a control id (`V4L2_CTRL_CLASS_USER`).
pub const FIRST_CONTROL: u32 = 0x0098_0000;
/// Bytes of a [`TAG_SETUP`] item.
pub const SETUP_BYTES: usize = 5 * 4;
/// The reference slot of the bitstream.
pub const SLOT_SOURCE: u32 = 0;
/// The reference slot of the picture's first memory plane. The next planes follow.
pub const SLOT_PICTURE: u32 = 1;
/// Controls one job may carry.
pub const MAX_CONTROLS: usize = 12;
/// Bytes of compound-control payload one job may carry.
pub const MAX_PAYLOAD_BYTES: usize = 8192;
/// Picture buffers asked of the device: the stream buffers a picture's first plane may be in.
pub const PICTURE_BUFFERS: u32 = 32;

/// First delay before a reopen after a device failure.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub(crate) const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// What health says while the device is closed after a failure.
pub const REOPENING: &str = "the decoder failed: it is opened again by a later job";

/// How the block uses the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// How long a request may take before the device is counted as failed.
    pub timeout: Duration,
}

impl Default for Settings {
    /// A 1 s timeout.
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(1),
        }
    }
}

/// What a job's [`TAG_SETUP`] item says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Setup {
    coded: u32,
    width: u32,
    height: u32,
    picture: u32,
    planes: u32,
}

impl Setup {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let words = bytes.as_chunks::<4>().0;
        if bytes.len() != SETUP_BYTES {
            return None;
        }
        let word = |at: usize| words.get(at).map(|four| u32::from_le_bytes(*four));
        let setup = Self {
            coded: word(0)?,
            width: word(1)?,
            height: word(2)?,
            picture: word(3)?,
            planes: word(4)?,
        };
        let sane = setup.width != 0
            && setup.height != 0
            && (1..=MAX_PLANES as u32).contains(&setup.planes);
        sane.then_some(setup)
    }
}

/// An open session and what it was set up for.
struct Open<N: Node> {
    session: M2mSession<N>,
    request: N::Request,
    setup: Setup,
}

/// A stateless decoder as `codec.block/1` has it, on the node `open` opens.
pub struct StatelessBlock<N: Node, F: FnMut() -> io::Result<N>> {
    open: F,
    settings: Settings,
    link: Option<Open<N>>,
    /// The job's compound-control payloads, copied here for the kernel to read.
    payloads: Box<[u8]>,
    failures: u32,
    retry_at_ns: Option<u64>,
}

impl<N: Node, F: FnMut() -> io::Result<N>> StatelessBlock<N, F> {
    /// The block on the node `open` opens. Nothing is opened until a job comes.
    pub fn new(open: F, settings: Settings) -> Self {
        Self {
            open,
            settings,
            link: None,
            payloads: vec![0; MAX_PAYLOAD_BYTES].into_boxed_slice(),
            failures: 0,
            retry_at_ns: None,
        }
    }

    /// Whether a session is open.
    pub fn is_open(&self) -> bool {
        self.link.is_some()
    }

    /// The open session, for a test to look at.
    pub fn session(&self) -> Option<&M2mSession<N>> {
        self.link.as_ref().map(|open| &open.session)
    }

    /// Closes the session after a device failure and schedules the reopen.
    fn fail(&mut self, now_ns: u64) -> Start {
        self.link = None;
        self.retry_at_ns = Some(now_ns.saturating_add(backoff_ns(self.failures)));
        self.failures = self.failures.saturating_add(1);
        Start::Failed
    }

    /// Opens the node and sets up both queues for `setup`, on the caller's dma-bufs.
    fn open(&mut self, setup: Setup) -> io::Result<Open<N>> {
        let node = (self.open)()?;
        let multi = node.capability()?.is_m2m_multiplanar();
        let (output, capture) = if multi {
            let source = PixFormatMplane {
                width: setup.width,
                height: setup.height,
                pixelformat: setup.coded,
                num_planes: 1,
                ..PixFormatMplane::default()
            };
            let picture = PixFormatMplane {
                width: setup.width,
                height: setup.height,
                pixelformat: setup.picture,
                planes: [PlaneFormat::default(); MAX_PLANES],
                num_planes: setup.planes,
                ..PixFormatMplane::default()
            };
            (Format::Multi(source), Format::Multi(picture))
        } else {
            let of = |pixelformat| {
                Format::Single(PixFormat {
                    width: setup.width,
                    height: setup.height,
                    pixelformat,
                    ..PixFormat::default()
                })
            };
            (of(setup.coded), of(setup.picture))
        };
        let on = |format, buffers| QueueSetup {
            format,
            memory: Memory::DmaBuf,
            buffers,
            crop: None,
        };
        let mut session = M2mSession::new(node, on(output, 1), on(capture, PICTURE_BUFFERS))?;
        // `S_FMT` adjusts what it doesn't take: a device that answered with other formats is
        // not one this job's work was made for.
        let kept = |format: &Format| match format {
            Format::Multi(f) => (f.pixelformat, f.num_planes),
            Format::Single(f) => (f.pixelformat, 1),
        };
        if kept(session.output_format()).0 != setup.coded
            || kept(session.capture_format()) != (setup.picture, setup.planes)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the device does not take these formats",
            ));
        }
        let request = session.node().new_request()?;
        session.stream_on()?;
        Ok(Open {
            session,
            request,
            setup,
        })
    }

    /// One job, from its work to its picture, with `payloads` as the memory its compound
    /// controls are copied into.
    fn run(
        &mut self,
        payloads: &mut [u8],
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        // The work's shape first.
        let mut setup = None;
        let mut stamp = None;
        let mut controls: [ExtControl<'_>; MAX_CONTROLS] =
            std::array::from_fn(|_| ExtControl::value(0, 0));
        let mut count = 0;
        let mut room: &mut [u8] = payloads;
        for (tag, bytes) in work.items() {
            match tag {
                TAG_SETUP if setup.is_none() => match Setup::parse(bytes) {
                    Some(parsed) => setup = Some(parsed),
                    None => return Start::Refused,
                },
                TAG_STAMP if stamp.is_none() && bytes.len() == 8 => {
                    let mut eight = [0u8; 8];
                    eight.copy_from_slice(bytes);
                    stamp = Some(u64::from_le_bytes(eight));
                }
                id if id >= FIRST_CONTROL && count < MAX_CONTROLS => {
                    controls[count] = if let Ok(four) = <[u8; 4]>::try_from(bytes) {
                        ExtControl::value(id, i32::from_le_bytes(four))
                    } else {
                        // The kernel reads the driver's copy, by its length.
                        let Some((copy, rest)) =
                            std::mem::take(&mut room).split_at_mut_checked(bytes.len())
                        else {
                            return Start::Refused;
                        };
                        copy.copy_from_slice(bytes);
                        room = rest;
                        ExtControl::payload(id, copy)
                    };
                    count += 1;
                }
                _ => return Start::Refused,
            }
        }
        let Some(setup) = setup else {
            return Start::Refused;
        };
        // A stamp the kernel keeps: a whole number of microseconds, its timestamp being a timeval.
        if stamp.is_some_and(|ns| ns % 1_000 != 0) {
            return Start::Refused;
        }
        // The references: one source, and the picture's planes in order.
        let mut source = None;
        let mut picture = [None; MAX_PLANES];
        for reference in work.refs() {
            let slot = match reference.slot {
                SLOT_SOURCE => &mut source,
                slot => {
                    let plane = (slot - SLOT_PICTURE) as usize;
                    if plane >= setup.planes as usize
                        || reference.offset != 0
                        || reference.length != 0
                    {
                        return Start::Refused;
                    }
                    &mut picture[plane]
                }
            };
            if slot.replace(reference).is_some() {
                return Start::Refused;
            }
        }
        let planes = setup.planes as usize;
        let (Some(source), true) = (source, picture[..planes].iter().all(Option::is_some)) else {
            return Start::Refused;
        };
        // The source lies inside the memory really behind its buffer.
        let Some(source_fd) = buffers.handle(source.buffer) else {
            return Start::Refused;
        };
        let end = u64::from(source.offset) + u64::from(source.length);
        let Ok(source_size) = dma_buf::size(source_fd.fd()) else {
            return Start::Refused;
        };
        let (Ok(source_len), true) = (
            u32::try_from(source_size),
            source.length != 0 && end <= source_size,
        ) else {
            return Start::Refused;
        };

        if let Some(retry_at_ns) = self.retry_at_ns
            && now_ns < retry_at_ns
        {
            return Start::Failed;
        }
        if self.link.as_ref().is_some_and(|open| open.setup != setup) {
            // Another setup: the queues are made again, which is no failure.
            self.link = None;
        }
        if self.link.is_none() {
            match self.open(setup) {
                Ok(open) => self.link = Some(open),
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                    // The device is there and doesn't take this setup: the job's, not its.
                    return Start::Refused;
                }
                Err(_) => return self.fail(now_ns),
            }
        }
        let timeout = self.settings.timeout;
        let Some(open) = self.link.as_mut() else {
            return self.fail(now_ns);
        };
        let Open {
            session, request, ..
        } = open;
        if source.offset != 0 && !matches!(session.output_format(), Format::Multi(_)) {
            return Start::Refused;
        }

        // Each picture plane: a dma-buf at least as large as the device's plane.
        let mut queue = [QueuePlane {
            memory: PlaneMemory::Mmap,
            length: 0,
            bytes_used: 0,
            data_offset: 0,
        }; MAX_PLANES];
        let mut index = 0;
        for (plane, reference) in picture[..planes].iter().flatten().enumerate() {
            let Some(handle) = buffers.handle(reference.buffer) else {
                return Start::Refused;
            };
            let Ok(size) = dma_buf::size(handle.fd()) else {
                return Start::Refused;
            };
            let needs = u64::from(plane_bytes(session.capture_format(), plane));
            let Ok(length) = u32::try_from(size) else {
                return Start::Refused;
            };
            if size < needs {
                return Start::Refused;
            }
            if plane == 0 {
                index = reference.buffer;
            }
            queue[plane] = QueuePlane {
                memory: PlaneMemory::Dmabuf(handle.fd()),
                length,
                bytes_used: 0,
                data_offset: 0,
            };
        }
        if index >= session.capture_buffers() {
            return Start::Refused;
        }

        // The request: the picture, the controls, the source, then run it.
        let run = (|| -> io::Result<Option<usize>> {
            session.queue_capture(index, &queue[..planes])?;
            session.set_request_controls(request, &mut controls[..count])?;
            let from = [QueuePlane {
                memory: PlaneMemory::Dmabuf(source_fd.fd()),
                length: source_len,
                bytes_used: source.offset + source.length,
                data_offset: source.offset,
            }];
            let stamp = Duration::from_nanos(stamp.unwrap_or(0));
            session.queue_output_in_request(0, &from, stamp, request)?;
            session.node().queue_request(request)?;
            if !session.node().wait_request(request, Some(timeout))? {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the request did not complete",
                ));
            }
            session
                .dequeue_output()?
                .ok_or_else(|| broken("the source buffer did not come back"))?;
            session.node().reinit_request(request)?;
            let want = Ready {
                capture: true,
                ..Ready::default()
            };
            if !session.wait(want, Some(timeout))?.capture {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no picture"));
            }
            let done = session
                .dequeue_capture()?
                .ok_or_else(|| broken("no picture"))?;
            if done.index != index {
                return Err(broken("another picture came back"));
            }
            if done.flags & v4l2::BUF_FLAG_ERROR != 0 {
                return Ok(None);
            }
            let Some(into) = read_back.get_mut(..4 * planes) else {
                return Err(broken("no room for the read-back"));
            };
            for (four, used) in into.as_chunks_mut::<4>().0.iter_mut().zip(done.bytes_used) {
                *four = used.to_le_bytes();
            }
            Ok(Some(4 * planes))
        })();
        match run {
            Ok(Some(bytes)) => {
                self.failures = 0;
                self.retry_at_ns = None;
                Start::Done(bytes)
            }
            // The device ran the request and flagged the picture: the stream's, not its.
            Ok(None) => Start::Failed,
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                // The session refused a buffer before anything was queued with the device.
                Start::Refused
            }
            Err(_) => self.fail(now_ns),
        }
    }
}

/// The bytes plane `plane` of `format` takes.
fn plane_bytes(format: &Format, plane: usize) -> u32 {
    match format {
        Format::Multi(f) => f.planes.get(plane).map_or(u32::MAX, |p| p.size_image),
        Format::Single(f) => f.size_image,
    }
}

/// What a device call that failed, or an answer that makes no sense, comes to.
fn broken(what: &'static str) -> io::Error {
    io::Error::other(what)
}

impl<N: Node, F: FnMut() -> io::Result<N>> Block for StatelessBlock<N, F> {
    fn name(&self) -> &str {
        NAME
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: (MAX_PAYLOAD_BYTES + 1024) as u32,
            max_refs: 1 + MAX_PLANES as u16,
            max_items: 2 + MAX_CONTROLS as u16,
            max_read_back: 4 * MAX_PLANES as u32,
            max_buffers: PICTURE_BUFFERS,
        }
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        // The payload memory is lent to the job's controls for the length of the job.
        let mut payloads = std::mem::take(&mut self.payloads);
        let started = self.run(&mut payloads, work, buffers, read_back, now_ns);
        self.payloads = payloads;
        started
    }

    fn release(&mut self) {
        // Closing the node makes the kernel let go of every buffer it was given.
        self.link = None;
    }

    fn health(&self) -> Health {
        if self.retry_at_ns.is_some() {
            Health::Degraded(REOPENING)
        } else {
            Health::Ok
        }
    }
}

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";
/// What health says while the job stream fails.
pub const STREAM_FAILED: &str = "the job stream failed: trying it again";

/// A stateless decoder as the driver holds it.
pub type BoxedBlock = Box<dyn Block + Send>;

/// A stateless V4L2 decoder as `codec.block/1`.
///
/// Settings, from the device's `config` in the board file:
///
/// - `device` (string, required): the decoder's video node. Its media device, which requests
///   need, is found from it.
/// - `timeout_ms` (integer, default 1000, 1 to 60000): how long one request may take.
///
/// The stream `jobs` is required, with `memory = "dma-buf"`: both queues take the stream's
/// buffers as dma-bufs, so a stream of plain memory fails the bind with `NOT_SUPPORTED`.
/// Nothing is opened at bind.
#[driver("v4l2-stateless", provides = [CodecBlock])]
pub struct M2mCodec {
    #[needs(job)]
    jobs: StreamHandle,
    block: BoxedBlock,
    server: Server,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl M2mCodec {
    /// The driver over `block`, for a test or a host that builds it directly.
    #[must_use]
    pub fn on(needs: M2mCodecNeeds, block: BoxedBlock) -> Self {
        Self {
            jobs: needs.jobs,
            block,
            server: Server::new(),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }
}

impl Lifecycle for M2mCodec {
    type Needs = M2mCodecNeeds;
    const TICKS: bool = true;

    fn start(needs: M2mCodecNeeds, cfg: &Config<'_>) -> Result<Self> {
        let path = PathBuf::from(cfg.str("device").ok_or(Status::INVALID_ARGUMENT)?);
        if path.as_os_str().is_empty() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let timeout = match cfg.get("timeout_ms") {
            None => 1_000,
            Some(_) => cfg
                .int("timeout_ms")
                .filter(|ms| (1..=60_000).contains(ms))
                .ok_or(Status::INVALID_ARGUMENT)?,
        };
        if needs.jobs.buffer_kind() != BufferKind::Handles {
            return Err(Status::NOT_SUPPORTED);
        }
        let settings = Settings {
            timeout: Duration::from_millis(timeout as u64),
        };
        let block = StatelessBlock::new(move || LinuxNode::open(&path), settings);
        Ok(Self::on(needs, Box::new(block)))
    }

    fn health(&self) -> Health {
        if self.stream_failures > 0 {
            return Health::Degraded(STREAM_FAILED);
        }
        self.block.health()
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if self.stream_failures > 0 && now_ns < self.retry_at_ns {
            return Some(self.retry_at_ns);
        }
        match self.server.serve(&mut *self.jobs, &mut *self.block, now_ns) {
            Ok(more) => {
                self.stream_failures = 0;
                Some(Server::next_tick_ns(more, now_ns))
            }
            Err(_) => {
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.stream_failures));
                self.stream_failures = self.stream_failures.saturating_add(1);
                Some(self.retry_at_ns)
            }
        }
    }
}

impl CodecBlock for M2mCodec {
    fn info(&mut self) -> Result<BlockInfo> {
        Ok(Server::info(&*self.block))
    }

    fn block(&mut self, into: &mut [u8]) -> Result<usize> {
        name_into(self.block.name(), into)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name_into(JOB_STREAM, into)
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.server.counters())
    }
}
