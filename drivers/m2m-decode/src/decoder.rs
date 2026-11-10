//! A stateless HEVC decoder over one memory-to-memory node: an access unit in, a decoded frame
//! out, through one media request per frame.
//!
//! The caller hands [`HevcDecoder::decode`] an access unit as an Annex B byte stream. The
//! decoder parses its parameter sets and the header of its slice with `lemnos-driver-hevc`,
//! turns them into the kernel's stateless HEVC controls, copies the slice (start code in front)
//! into a source buffer, and runs the request. The decoded frame's planes are lent to the
//! caller's closure, as the device wrote them, and the buffer goes back to the device when the
//! closure returns. The decoder never reads the picture: its layout is the device's, described
//! by [`Frame::format`].
//!
//! What it takes: IDR pictures of one slice each, without scaling lists, tiles or wavefronts,
//! which is what the parser reads (see `lemnos-driver-hevc`). Anything else is refused as
//! [`DecodeError::Unsupported`] without touching the device.
//!
//! # Failures
//!
//! A bad access unit is the caller's: it is refused with [`DecodeError::Stream`] or
//! [`DecodeError::Unsupported`] and the device is not touched. A frame the device flags in error
//! is [`DecodeError::Corrupt`], and its buffer goes back. Anything the device itself fails at
//! (opening, a refused call, a request that does not complete in time) closes the session and
//! starts a backoff: until it is due, [`HevcDecoder::decode`] answers
//! [`DecodeError::BackingOff`] without touching the device, and the first call after it reopens
//! everything. The delay doubles from 250 ms and is capped at 8 s, and clears when a frame
//! decodes. The decoder never gives up and never turns itself off: [`HevcDecoder::stop`] closes
//! it, and the next `decode` opens it again.
//!
//! # Allocation
//!
//! Opening allocates (the session's queue records and mappings). A frame decoded on an open
//! session allocates nothing: the parse runs over a scratch buffer made with the decoder, and
//! the controls are borrowed for the call.

use std::io;
use std::time::Duration;

use lemnos_driver_hevc as hevc;
use lemnos_linux_sys::v4l2::device::ExtControl;
use lemnos_linux_sys::v4l2::hevc as sys;
use lemnos_linux_sys::v4l2::{
    self as v4l2, Format, MAX_PLANES, Memory, PixFormat, PixFormatMplane, PlaneFormat, PlaneMemory,
    QueuePlane,
};

use crate::hevc::{START_CODE_LEN, idr_slice, pps_control, sps_control};
use crate::node::Node;
use crate::session::{M2mSession, QueueSetup, Ready};

/// First delay before a reopen.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// The scratch buffer a unit's header is unescaped into. A header is far smaller; a longer
/// unit is unescaped as far as this, which is a prefix of its RBSP.
const RBSP_BYTES: usize = 4096;
/// `sps_seq_parameter_set_id` is 0 to 15.
const SPS_IDS: usize = 16;
/// `pps_pic_parameter_set_id` is 0 to 63.
const PPS_IDS: usize = 64;

/// The delay before reopen attempt `attempt` (from 0), doubling to the 8 s cap.
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// How the decoder sets up the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Bytes of each source buffer: the largest slice it takes, start code included.
    pub output_bytes: u32,
    /// Source buffers.
    pub output_buffers: u32,
    /// Decoded-frame buffers.
    pub capture_buffers: u32,
    /// The decoded frames' pixel format to ask for (the driver may answer with another; see
    /// [`Frame::format`]).
    pub capture_fourcc: u32,
    /// The memory planes of that format.
    pub capture_planes: u32,
    /// How long a request may take before the device is counted as failed.
    pub timeout: Duration,
}

impl Default for Settings {
    /// 1 MiB source buffers (2), 4 frame buffers of two-plane NV12, a 1 s timeout.
    fn default() -> Self {
        Self {
            output_bytes: 1 << 20,
            output_buffers: 2,
            capture_buffers: 4,
            capture_fourcc: v4l2::fourcc(b"NV12"),
            capture_planes: 2,
            timeout: Duration::from_secs(1),
        }
    }
}

/// Why a frame was not decoded.
#[derive(Debug)]
pub enum DecodeError {
    /// The access unit could not be parsed. The device was not touched.
    Stream(hevc::Error),
    /// The access unit is valid but needs what this decoder does not do, or names a parameter
    /// set it has not been given. The device was not touched.
    Unsupported(&'static str),
    /// The slice does not fit a source buffer ([`Settings::output_bytes`]).
    TooLarge,
    /// The device decoded the frame and flagged it in error. The session stays open.
    Corrupt,
    /// The device failed; the session is closed and reopens on the first call from
    /// `retry_at_ns`.
    Device {
        /// What failed.
        error: io::Error,
        /// When the next call may reopen it, on the caller's clock.
        retry_at_ns: u64,
    },
    /// An earlier failure's backoff has not run out. The device was not touched.
    BackingOff {
        /// When the next call may reopen it.
        retry_at_ns: u64,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(e) => write!(f, "the access unit: {e}"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::TooLarge => f.write_str("the slice is larger than a source buffer"),
            Self::Corrupt => f.write_str("the device flagged the frame in error"),
            Self::Device { error, retry_at_ns } => {
                write!(
                    f,
                    "the device failed ({error}); retrying at {retry_at_ns} ns"
                )
            }
            Self::BackingOff { retry_at_ns } => write!(f, "backing off until {retry_at_ns} ns"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<hevc::Error> for DecodeError {
    fn from(e: hevc::Error) -> Self {
        Self::Stream(e)
    }
}

/// Where the decoder is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing open; the next frame opens the device.
    Closed,
    /// The session is open.
    Running,
    /// A failure closed the session; the first call from `retry_at_ns` reopens it.
    BackingOff {
        /// Failures in a row so far.
        failures: u32,
        /// When the next attempt may run.
        retry_at_ns: u64,
    },
}

/// A decoded frame, lent to the closure given to [`HevcDecoder::decode`].
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// The capture format the driver set: size, pixel format and each plane's stride.
    pub format: &'a Format,
    /// Each memory plane's bytes, as the device wrote them; the first
    /// [`Frame::num_planes`] are in use.
    pub planes: [&'a [u8]; MAX_PLANES],
    /// How many planes are in use.
    pub num_planes: usize,
    /// The timestamp the access unit was given.
    pub timestamp: Duration,
    /// The device's frame sequence number.
    pub sequence: u32,
}

impl Frame<'_> {
    /// The planes in use.
    pub fn planes(&self) -> &[&[u8]] {
        self.planes.get(..self.num_planes).unwrap_or(&[])
    }
}

/// The open device: the session, its request and the picture size it was set up for.
struct Open<N: Node> {
    session: M2mSession<N>,
    request: N::Request,
    size: (u32, u32),
}

/// What one access unit's parse gives the device.
struct Parsed<'a> {
    sps: sys::Sps,
    pps: sys::Pps,
    slice: sys::SliceParams,
    decode: sys::DecodeParams,
    size: (u32, u32),
    /// The slice NAL unit as stored, without its start code.
    raw: &'a [u8],
}

/// The parameter sets seen so far, by id. A later set with the same id replaces an earlier.
struct Tables {
    sps: [Option<hevc::Sps>; SPS_IDS],
    pps: [Option<hevc::Pps>; PPS_IDS],
    rbsp: [u8; RBSP_BYTES],
}

/// An HEVC decoder on a stateless memory-to-memory node that `open` makes.
pub struct HevcDecoder<N: Node, F: FnMut() -> io::Result<N>> {
    open: F,
    settings: Settings,
    link: Option<Open<N>>,
    failures: u32,
    retry_at_ns: Option<u64>,
    tables: Box<Tables>,
}

impl<N: Node, F: FnMut() -> io::Result<N>> HevcDecoder<N, F> {
    /// A decoder that opens its node with `open` when the first frame comes (and again after a
    /// failure). Nothing is opened here.
    pub fn new(open: F, settings: Settings) -> Self {
        Self {
            open,
            settings,
            link: None,
            failures: 0,
            retry_at_ns: None,
            tables: Box::new(Tables {
                sps: [None; SPS_IDS],
                pps: [None; PPS_IDS],
                rbsp: [0; RBSP_BYTES],
            }),
        }
    }

    /// Where the decoder is.
    pub fn state(&self) -> State {
        match (self.retry_at_ns, &self.link) {
            (Some(retry_at_ns), _) => State::BackingOff {
                failures: self.failures,
                retry_at_ns,
            },
            (None, Some(_)) => State::Running,
            (None, None) => State::Closed,
        }
    }

    /// The open session, if any.
    pub fn session(&self) -> Option<&M2mSession<N>> {
        self.link.as_ref().map(|open| &open.session)
    }

    /// Closes the device and forgets any backoff. The next [`Self::decode`] opens it again.
    pub fn stop(&mut self) {
        self.link = None;
        self.failures = 0;
        self.retry_at_ns = None;
    }

    /// Decodes one access unit (Annex B: parameter sets and one IDR slice), at `now_ns` on the
    /// caller's monotonic clock, and lends the frame to `read`. Returns what `read` returned, or
    /// `None` when the unit carried no picture (parameter sets only: they are kept for the
    /// frames that follow).
    pub fn decode<R>(
        &mut self,
        now_ns: u64,
        access_unit: &[u8],
        timestamp: Duration,
        read: impl FnOnce(Frame<'_>) -> R,
    ) -> Result<Option<R>, DecodeError> {
        let Some(parsed) = parse(&mut self.tables, access_unit)? else {
            return Ok(None);
        };
        let len = START_CODE_LEN + parsed.raw.len();
        if len > self.settings.output_bytes as usize {
            return Err(DecodeError::TooLarge);
        }
        if let Some(retry_at_ns) = self.retry_at_ns
            && now_ns < retry_at_ns
        {
            return Err(DecodeError::BackingOff { retry_at_ns });
        }
        if self
            .link
            .as_ref()
            .is_some_and(|open| open.size != parsed.size)
        {
            // A new picture size: the queues are set up again, which is no failure.
            self.link = None;
        }
        if self.link.is_none() {
            match self.open(parsed.size) {
                Ok(open) => self.link = Some(open),
                Err(error) => return Err(self.fail(now_ns, error)),
            }
        }
        let Some(open) = self.link.as_mut() else {
            return Err(self.fail(now_ns, io::Error::other("no session")));
        };
        match run(open, &self.settings, parsed, timestamp, read) {
            Ok(Ok(value)) => {
                self.failures = 0;
                self.retry_at_ns = None;
                Ok(Some(value))
            }
            Ok(Err(e)) => Err(e),
            Err(error) => Err(self.fail(now_ns, error)),
        }
    }

    /// Closes the session after a device failure and schedules the reopen.
    fn fail(&mut self, now_ns: u64, error: io::Error) -> DecodeError {
        self.link = None;
        let retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
        self.failures = self.failures.saturating_add(1);
        self.retry_at_ns = Some(retry_at_ns);
        DecodeError::Device { error, retry_at_ns }
    }

    /// Opens the node and sets up both queues for a `size` picture: the slice format on the
    /// source, [`Settings::capture_fourcc`] on the frames, every frame buffer queued, both queues
    /// streaming, and one request.
    fn open(&mut self, (width, height): (u32, u32)) -> io::Result<Open<N>> {
        let node = (self.open)()?;
        let multi = node.capability()?.is_m2m_multiplanar();
        let s = &self.settings;
        let output = if multi {
            let mut planes = [PlaneFormat::default(); MAX_PLANES];
            planes[0].size_image = s.output_bytes;
            Format::Multi(PixFormatMplane {
                width,
                height,
                pixelformat: sys::PIX_FMT_HEVC_SLICE,
                planes,
                num_planes: 1,
                ..PixFormatMplane::default()
            })
        } else {
            Format::Single(PixFormat {
                width,
                height,
                pixelformat: sys::PIX_FMT_HEVC_SLICE,
                size_image: s.output_bytes,
                ..PixFormat::default()
            })
        };
        let capture = if multi {
            // A hint the driver adjusts: full-size luma first, half-size after it (4:2:0).
            let mut planes = [PlaneFormat::default(); MAX_PLANES];
            let luma = width.saturating_mul(height);
            for (i, plane) in planes
                .iter_mut()
                .take(s.capture_planes as usize)
                .enumerate()
            {
                *plane = PlaneFormat {
                    size_image: if i == 0 { luma } else { luma / 2 },
                    bytes_per_line: width,
                };
            }
            Format::Multi(PixFormatMplane {
                width,
                height,
                pixelformat: s.capture_fourcc,
                planes,
                num_planes: s.capture_planes,
                ..PixFormatMplane::default()
            })
        } else {
            Format::Single(PixFormat {
                width,
                height,
                pixelformat: s.capture_fourcc,
                ..PixFormat::default()
            })
        };
        let setup = |format, buffers| QueueSetup {
            format,
            memory: Memory::Mmap,
            buffers,
            crop: None,
        };
        let mut session = M2mSession::new(
            node,
            setup(output, s.output_buffers),
            setup(capture, s.capture_buffers.max(1)),
        )?;
        let request = session.node().new_request()?;
        let planes = mmap_planes();
        let n = session.capture_planes() as usize;
        let planes = planes.get(..n).unwrap_or(&[]);
        for index in 0..session.capture_buffers() {
            session.queue_capture(index, planes)?;
        }
        session.stream_on()?;
        Ok(Open {
            session,
            request,
            size: (width, height),
        })
    }
}

/// One MMAP plane per entry, for queueing.
fn mmap_planes() -> [QueuePlane<'static>; MAX_PLANES] {
    [QueuePlane {
        memory: PlaneMemory::Mmap,
        length: 0,
        bytes_used: 0,
        data_offset: 0,
    }; MAX_PLANES]
}

/// The controls refuse a header value that does not fit their fields.
fn unfit(_: io::Error) -> DecodeError {
    DecodeError::Unsupported("a header value the decoder's controls cannot hold")
}

/// Reads the parameter sets of `access_unit` into `tables` and, if it has a picture, its
/// slice's controls.
fn parse<'a>(
    tables: &mut Tables,
    access_unit: &'a [u8],
) -> Result<Option<Parsed<'a>>, DecodeError> {
    let mut slice = None;
    for nal in hevc::nal_units(access_unit) {
        if nal.layer_id != 0 {
            continue; // Layers above the base are not decoded.
        }
        match nal.unit_type {
            33 | 34 => {
                let rbsp = unescape_prefix(&mut tables.rbsp, nal.raw)?;
                if nal.unit_type == 33 {
                    let sps = hevc::parse_sps(rbsp)?;
                    if let Some(entry) = tables.sps.get_mut(usize::from(sps.id)) {
                        *entry = Some(sps);
                    }
                } else {
                    let pps = hevc::parse_pps(rbsp)?;
                    if let Some(entry) = tables.pps.get_mut(usize::from(pps.id)) {
                        *entry = Some(pps);
                    }
                }
            }
            0..=31 => {
                if !hevc::is_idr(nal.unit_type) {
                    return Err(DecodeError::Unsupported("a picture that is not IDR"));
                }
                if slice.is_some() {
                    return Err(DecodeError::Unsupported("more than one slice in a picture"));
                }
                slice = Some(nal);
            }
            _ => {} // VPS, delimiters, SEI and the rest carry nothing the device takes.
        }
    }
    let Some(nal) = slice else {
        return Ok(None);
    };
    let rbsp = unescape_prefix(&mut tables.rbsp, nal.raw)?;
    // The slice names its PPS (after two flags of an IRAP slice), which names its SPS.
    let mut r = hevc::BitReader::new(rbsp);
    r.skip(16 + 2)?;
    let pps_id = r.ue_max(63, "slice_pic_parameter_set_id")? as usize;
    let pps = tables
        .pps
        .get(pps_id)
        .copied()
        .flatten()
        .ok_or(DecodeError::Unsupported("a slice whose PPS was not given"))?;
    let sps = tables
        .sps
        .get(usize::from(pps.sps_id))
        .copied()
        .flatten()
        .ok_or(DecodeError::Unsupported("a PPS whose SPS was not given"))?;
    let header = hevc::parse_slice_header(rbsp, nal.unit_type, &sps, &pps)?;
    let buffer_len = START_CODE_LEN + nal.raw.len();
    let idr = idr_slice(
        &header,
        nal.raw,
        nal.unit_type,
        nal.temporal_id_plus1,
        buffer_len,
    )
    .map_err(unfit)?;
    Ok(Some(Parsed {
        sps: sps_control(&sps).map_err(unfit)?,
        pps: pps_control(&pps).map_err(unfit)?,
        slice: idr.slice,
        decode: idr.decode,
        size: (sps.width, sps.height),
        raw: nal.raw,
    }))
}

/// Unescapes as much of `raw` as fits the scratch buffer: a prefix of the RBSP, which holds
/// every header this decoder reads.
fn unescape_prefix<'a>(scratch: &'a mut [u8; RBSP_BYTES], raw: &[u8]) -> hevc::Result<&'a [u8]> {
    let take = raw.len().min(RBSP_BYTES);
    let n = hevc::unescape(raw.get(..take).unwrap_or(raw), scratch)?;
    Ok(scratch.get(..n).unwrap_or(&[]))
}

/// Runs one frame on an open session. The outer error is the device's (the session is closed);
/// the inner one is the frame's.
fn run<N: Node, R>(
    open: &mut Open<N>,
    settings: &Settings,
    mut parsed: Parsed<'_>,
    timestamp: Duration,
    read: impl FnOnce(Frame<'_>) -> R,
) -> io::Result<Result<R, DecodeError>> {
    let Open {
        session, request, ..
    } = open;
    // The source buffer: the slice with its start code in front.
    let index = session
        .free_output()
        .ok_or_else(|| io::Error::other("no free source buffer"))?;
    let plane = session
        .output_plane_mut(index, 0)
        .ok_or_else(|| io::Error::other("the source buffer is not mapped"))?;
    let len = START_CODE_LEN + parsed.raw.len();
    let Some(dst) = plane.get_mut(..len) else {
        return Ok(Err(DecodeError::TooLarge));
    };
    let (code, unit) = dst.split_at_mut(START_CODE_LEN);
    code.copy_from_slice(&[0, 0, 1]);
    unit.copy_from_slice(parsed.raw);

    let mut controls = [
        ExtControl::payload(sys::CID_SPS, parsed.sps.as_bytes_mut()),
        ExtControl::payload(sys::CID_PPS, parsed.pps.as_bytes_mut()),
        ExtControl::payload(sys::CID_SLICE_PARAMS, parsed.slice.as_bytes_mut()),
        ExtControl::payload(sys::CID_DECODE_PARAMS, parsed.decode.as_bytes_mut()),
        ExtControl::value(sys::CID_DECODE_MODE, sys::DECODE_MODE_FRAME_BASED),
        ExtControl::value(sys::CID_START_CODE, sys::START_CODE_ANNEX_B),
    ];
    session.set_request_controls(request, &mut controls)?;
    let bytes_used = u32::try_from(len).map_err(|_| io::Error::other("slice size"))?;
    let source = [QueuePlane {
        memory: PlaneMemory::Mmap,
        length: 0,
        bytes_used,
        data_offset: 0,
    }];
    session.queue_output_in_request(index, &source, timestamp, request)?;
    session.node().queue_request(request)?;
    if !session
        .node()
        .wait_request(request, Some(settings.timeout))?
    {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the request did not complete",
        ));
    }
    // The request is done with the source buffer; it takes the next frame's.
    session
        .dequeue_output()?
        .ok_or_else(|| io::Error::other("the source buffer did not come back"))?;
    session.node().reinit_request(request)?;

    let want = Ready {
        capture: true,
        ..Ready::default()
    };
    if !session.wait(want, Some(settings.timeout))?.capture {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "no decoded frame"));
    }
    let done = session
        .dequeue_capture()?
        .ok_or_else(|| io::Error::other("no decoded frame"))?;
    let planes = mmap_planes();
    let n = session.capture_planes() as usize;
    let requeue = planes.get(..n).unwrap_or(&[]);
    if done.flags & v4l2::BUF_FLAG_ERROR != 0 {
        session.queue_capture(done.index, requeue)?;
        return Ok(Err(DecodeError::Corrupt));
    }
    let value = {
        let mut lent: [&[u8]; MAX_PLANES] = [&[]; MAX_PLANES];
        for (plane, slot) in lent.iter_mut().enumerate().take(n) {
            let bytes = session
                .capture_plane(done.index, plane)
                .ok_or_else(|| io::Error::other("the frame buffer is not mapped"))?;
            let used = done.bytes_used.get(plane).copied().unwrap_or(0);
            let offset = done.data_offset.get(plane).copied().unwrap_or(0);
            let range = v4l2::payload_range(used, offset, bytes.len())?;
            *slot = bytes.get(range).unwrap_or(&[]);
        }
        read(Frame {
            format: session.capture_format(),
            planes: lent,
            num_planes: n,
            timestamp: done.timestamp,
            sequence: done.sequence,
        })
    };
    session.queue_capture(done.index, requeue)?;
    Ok(Ok(value))
}

#[cfg(test)]
mod tests;
