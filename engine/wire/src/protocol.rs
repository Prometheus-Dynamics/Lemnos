//! The protocol itself: frames, calls, values and descriptions. Everything here but the ring;
//! built with the `protocol` feature.

mod description;

pub use description::{
    DeviceDescription, OwnedContract, OwnedItem, OwnedMethod, OwnedParam, OwnedShape,
    decode_description, encode_description,
};

use lemnos_core::{Decode, Encode, Reader, Writer};

pub const VERSION: u16 = 1;
/// The largest frame, header included. A sender refuses to build a larger one
/// and a receiver closes the connection on one.
pub const MAX_FRAME: usize = 1 << 20;
/// Answers queued for one client before that client is closed.
pub const MAX_QUEUED_BYTES: usize = 4 << 20;
/// What a host can do beyond the first form of this version, as bits of `features` in Welcome.
/// A client uses what a bit stands for only when the host set it; a host that doesn't know a bit
/// leaves it clear.
pub mod feature {
    /// The host takes StreamOpenHandles and may answer it with StreamHandles: a stream whose
    /// buffers are one descriptor each.
    pub const STREAM_HANDLES: u64 = 1;
    /// The host takes Packages, and answers it with what became of its driver packages.
    pub const PACKAGES: u64 = 2;
    /// Every bit this build knows.
    pub const ALL: u64 = STREAM_HANDLES | PACKAGES;
}

/// The most buffer descriptors a stream may have when each buffer, or each plane of a buffer,
/// is its own descriptor. They, and the ring's and the doorbell's, travel in one `SCM_RIGHTS`
/// message, which holds 253.
pub const MAX_HANDLE_BUFFERS: u32 = 128;
/// The most planes a buffer may have (`VIDEO_MAX_PLANES` of V4L2).
pub const MAX_PLANES: u32 = 8;
/// Bytes one plane's length takes in StreamHandles' `plane_bytes`.
pub const PLANE_LENGTH_BYTES: usize = 4;
/// The descriptors that come before the buffers' in StreamHandles: the ring and the doorbell.
pub const HANDLE_DESCRIPTORS_BEFORE: u32 = 2;

/// Bytes before a frame's kind that say how long it is.
const LENGTH_BYTES: usize = 4;
const HEADER: usize = LENGTH_BYTES + 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// A declared length is impossible. The connection must be closed.
    FrameTooLarge(usize),
    /// The frame doesn't parse. The connection must be closed.
    Malformed,
    UnknownKind(u16),
    /// The value doesn't fit in a frame. Nothing was sent.
    TooLarge,
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FrameTooLarge(n) => write!(f, "a frame of {n} bytes is over the limit"),
            Self::Malformed => f.write_str("malformed frame"),
            Self::UnknownKind(kind) => write!(f, "unknown frame kind {kind}"),
            Self::TooLarge => f.write_str("too large for one frame"),
        }
    }
}

impl std::error::Error for WireError {}

/// How a device stands, as carried on the wire.
pub mod state {
    pub const RUNNING: u8 = 0;
    pub const BACKOFF: u8 = 1;
    pub const OFF: u8 = 2;
}

/// How a device's health is carried on the wire.
pub mod health {
    pub const OK: u8 = 0;
    pub const DEGRADED: u8 = 1;
    pub const FAILED: u8 = 2;
}

/// Text appended to a frame after the frame's first form. A peer from before the addition
/// doesn't send it, and then it reads as empty: rule 8 of `docs/wire.md` from the reader's
/// side. It can only be a frame's last field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Appended<'a>(pub &'a str);

impl Encode for Appended<'_> {
    const SHAPE: lemnos_core::Shape = lemnos_core::Shape::Str;
    fn encode(&self, out: &mut Writer<'_>) -> lemnos_core::Result<()> {
        self.0.encode(out)
    }
}

impl<'a> Decode<'a> for Appended<'a> {
    fn decode(from: &mut Reader<'a>) -> lemnos_core::Result<Self> {
        if from.remaining() == 0 {
            return Ok(Self(""));
        }
        <&str>::decode(from).map(Self)
    }
}

/// Which driver packages a host runs, as carried on the wire.
pub mod package_mode {
    /// None: no package directory is configured.
    pub const OFF: u8 = 0;
    /// Only packages signed by a key the host trusts.
    pub const SIGNED: u8 = 1;
    /// Any package in its directory.
    pub const OPEN: u8 = 2;
}

macro_rules! frames {
    ($($(#[$doc:meta])* $kind:literal $name:ident { $($(#[$field_doc:meta])* $field:ident: $ty:ty),* $(,)? })*) => {
        /// One message, in either direction.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub enum Frame<'a> {
            $($(#[$doc])* $name { $($(#[$field_doc])* $field: $ty),* },)*
        }

        impl Frame<'_> {
            /// The frame's kind number.
            pub const fn kind(&self) -> u16 {
                match self {
                    $(Self::$name { .. } => $kind,)*
                }
            }

            fn encode_payload(&self, out: &mut Writer<'_>) -> lemnos_core::Result<()> {
                match self {
                    $(Self::$name { $($field),* } => {
                        $(Encode::encode($field, out)?;)*
                        Ok(())
                    })*
                }
            }
        }

        fn decode_payload<'a>(kind: u16, from: &mut Reader<'a>) -> Result<Frame<'a>, WireError> {
            match kind {
                $($kind => Ok(Frame::$name {
                    $($field: Decode::decode(from).map_err(|_| WireError::Malformed)?,)*
                }),)*
                other => Err(WireError::UnknownKind(other)),
            }
        }
    };
}

frames! {
    1 Hello { version: u16, name: &'a str, events: bool }
    2 Welcome { version: u16, features: u64, host: &'a str }
    3 List { id: u32 }
    4 Device {
        id: u32,
        device: u32,
        name: &'a str,
        driver: &'a str,
        /// Comma-separated.
        tags: &'a str,
        state: u8,
        health: u8,
        reason: &'a str,
        /// The name of the device this one is wired to, from the board file's `link`. Empty
        /// when it names none, and from a host older than this field.
        link: Appended<'a>,
    }
    5 ListDone { id: u32 }
    /// An empty `device` means any device providing `tag`.
    6 Open { id: u32, device: &'a str, tag: &'a str }
    7 Opened { id: u32, device: u32, contract: u8 }
    8 Describe { id: u32, device: u32 }
    9 Description { id: u32, bytes: &'a [u8] }
    10 Call {
        id: u32,
        device: u32,
        contract: u8,
        method: u16,
        reply_capacity: u32,
        args: &'a [u8],
    }
    11 Reply { id: u32, bytes: &'a [u8] }
    12 Subscribe {
        id: u32,
        device: u32,
        contract: u8,
        method: u16,
        period_us: u64,
        only_changes: bool,
    }
    13 Subscribed { id: u32, subscription: u64, period_us: u64 }
    14 Unsubscribe { id: u32, subscription: u64 }
    15 Value { subscription: u64, stamp_ns: u64, bytes: &'a [u8] }
    16 SetPower { id: u32, device: u32, on: bool }
    17 StreamOpen { id: u32, device: u32, name: &'a str }
    /// `descriptors` file descriptors travel with this frame over `SCM_RIGHTS`.
    18 StreamReady {
        id: u32,
        buffers: u32,
        descriptors: u8,
        buffer_bytes: u32,
        frame_slots: u32,
        job_slots: u32,
        job_bytes: u32,
    }
    19 Event { device: u32, state: u8, health: u8, reason: &'a str }
    /// The answer to any request that could not be done.
    20 Refusal { id: u32, status: i32, text: &'a str }
    /// Answered with an empty Reply.
    21 StreamClose { id: u32, device: u32, name: &'a str }
    /// StreamOpen from a client that can also take StreamHandles for an answer. Sent only to a
    /// host whose Welcome set [`feature::STREAM_HANDLES`].
    22 StreamOpenHandles { id: u32, device: u32, name: &'a str }
    /// StreamReady for a stream whose buffers are one descriptor each, or one per plane.
    /// `plane_bytes` is each plane's length, [`PLANE_LENGTH_BYTES`] little-endian bytes each,
    /// adding up to `buffer_bytes`; empty when a buffer is one plane. `descriptors` is 2 plus
    /// `buffers` times the planes, all with this frame over `SCM_RIGHTS`: the ring, the
    /// doorbell, then buffer 0's planes in order, buffer 1's, and so on. At most
    /// [`MAX_HANDLE_BUFFERS`] buffer descriptors.
    23 StreamHandles {
        id: u32,
        buffers: u32,
        descriptors: u8,
        buffer_bytes: u32,
        frame_slots: u32,
        job_slots: u32,
        job_bytes: u32,
        plane_bytes: &'a [u8],
    }
    /// Asks what became of the host's driver packages. Sent only to a host whose Welcome set
    /// [`feature::PACKAGES`]. Answered with one Package for each file, then PackagesDone.
    24 Packages { id: u32 }
    /// One package file. Loaded: `driver` is its driver's name and `reason` is empty. Refused:
    /// `driver` is empty and `reason` says why.
    25 Package { id: u32, path: &'a str, driver: &'a str, reason: &'a str }
    /// `mode` is one of [`package_mode`]. `note` is empty, or says why the package directory
    /// could not be used at all.
    26 PackagesDone { id: u32, mode: u8, note: &'a str }
}

/// How many descriptors StreamHandles carries for a stream of `buffers` buffers of `planes`
/// planes each, or `None` when that many can't each be a descriptor.
#[must_use]
pub const fn handle_descriptors(buffers: u32, planes: u32) -> Option<u8> {
    if buffers == 0 || planes == 0 || planes > MAX_PLANES {
        return None;
    }
    match buffers.checked_mul(planes) {
        // At most 130, so it fits.
        Some(total) if total <= MAX_HANDLE_BUFFERS => {
            Some((HANDLE_DESCRIPTORS_BEFORE + total) as u8)
        }
        _ => None,
    }
}

/// A buffer's planes, by length: what StreamHandles' `plane_bytes` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Planes {
    lengths: [u32; MAX_PLANES as usize],
    count: u32,
}

impl Planes {
    /// One plane of `buffer_bytes`.
    #[must_use]
    pub const fn one(buffer_bytes: u32) -> Self {
        let mut lengths = [0; MAX_PLANES as usize];
        lengths[0] = buffer_bytes;
        Self { lengths, count: 1 }
    }

    /// `lengths` as planes of a buffer of `buffer_bytes`: 1 to [`MAX_PLANES`] of them, none
    /// empty, adding up to the buffer. `None` otherwise.
    #[must_use]
    pub fn new(lengths: &[u32], buffer_bytes: u32) -> Option<Self> {
        let count = u32::try_from(lengths.len()).ok()?;
        if !(1..=MAX_PLANES).contains(&count) || lengths.contains(&0) {
            return None;
        }
        let sum = lengths
            .iter()
            .try_fold(0u32, |sum, length| sum.checked_add(*length))?;
        if sum != buffer_bytes {
            return None;
        }
        let mut out = [0; MAX_PLANES as usize];
        out.get_mut(..lengths.len())?.copy_from_slice(lengths);
        Some(Self {
            lengths: out,
            count,
        })
    }

    /// Reads `plane_bytes` from StreamHandles, checked as [`new`](Self::new) checks; empty is
    /// one plane of `buffer_bytes`.
    #[must_use]
    pub fn decode(plane_bytes: &[u8], buffer_bytes: u32) -> Option<Self> {
        if plane_bytes.is_empty() {
            return (buffer_bytes > 0).then(|| Self::one(buffer_bytes));
        }
        if !plane_bytes.len().is_multiple_of(PLANE_LENGTH_BYTES)
            || plane_bytes.len() > PLANE_LENGTH_BYTES * MAX_PLANES as usize
        {
            return None;
        }
        let mut lengths = [0; MAX_PLANES as usize];
        let mut count = 0;
        for (slot, chunk) in lengths
            .iter_mut()
            .zip(plane_bytes.as_chunks::<PLANE_LENGTH_BYTES>().0)
        {
            *slot = u32::from_le_bytes(*chunk);
            count += 1;
        }
        Self::new(lengths.get(..count)?, buffer_bytes)
    }

    /// The bytes StreamHandles carries for these planes: empty for one plane.
    #[must_use]
    pub fn encode(&self, out: &mut [u8; PLANE_LENGTH_BYTES * MAX_PLANES as usize]) -> usize {
        if self.count == 1 {
            return 0;
        }
        for (chunk, length) in out
            .as_chunks_mut::<PLANE_LENGTH_BYTES>()
            .0
            .iter_mut()
            .zip(self.lengths())
        {
            *chunk = length.to_le_bytes();
        }
        self.lengths().len() * PLANE_LENGTH_BYTES
    }

    /// How many planes.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Each plane's length, in order.
    #[must_use]
    pub fn lengths(&self) -> &[u32] {
        self.lengths.get(..self.count as usize).unwrap_or(&[])
    }

    /// Where plane `plane` starts in the buffer: the planes before it, end to end.
    #[must_use]
    pub fn offset(&self, plane: u32) -> Option<usize> {
        if plane >= self.count {
            return None;
        }
        Some(
            self.lengths()
                .iter()
                .take(plane as usize)
                .map(|length| *length as usize)
                .sum(),
        )
    }

    /// The plane `len` bytes at `offset` of a buffer begin in, where in it they begin, and how
    /// many of them it holds; `None` when `offset` is past the buffer.
    #[must_use]
    pub fn locate(&self, offset: usize) -> Option<(u32, usize, usize)> {
        let mut start = 0usize;
        for (plane, length) in self.lengths().iter().enumerate() {
            let length = *length as usize;
            if offset < start + length {
                return Some((
                    u32::try_from(plane).ok()?,
                    offset - start,
                    start + length - offset,
                ));
            }
            start += length;
        }
        None
    }
}

/// Writes a frame into `buf` and returns its length.
pub fn encode(frame: &Frame<'_>, buf: &mut [u8]) -> Result<usize, WireError> {
    let limit = buf.len().min(MAX_FRAME);
    let body = buf
        .get_mut(LENGTH_BYTES..limit)
        .ok_or(WireError::TooLarge)?;
    let mut out = Writer::new(body);
    frame
        .kind()
        .encode(&mut out)
        .and_then(|()| frame.encode_payload(&mut out))
        .map_err(|_| WireError::TooLarge)?;
    let body_len = out.len();
    buf[..LENGTH_BYTES].copy_from_slice(&(body_len as u32).to_le_bytes());
    Ok(LENGTH_BYTES + body_len)
}

/// Appends a frame to `out`, growing it if needed. A reused `out` stops
/// allocating once it has reached the largest frame it carries.
pub fn append(frame: &Frame<'_>, out: &mut Vec<u8>) -> Result<(), WireError> {
    let start = out.len();
    let mut room = 256usize.max(out.capacity() - start);
    loop {
        out.resize(start + room.min(MAX_FRAME), 0);
        match encode(frame, &mut out[start..]) {
            Ok(n) => {
                out.truncate(start + n);
                return Ok(());
            }
            Err(_) if room < MAX_FRAME => room *= 2,
            Err(error) => {
                out.truncate(start);
                return Err(error);
            }
        }
    }
}

/// Reads the first frame in `buf`. `Ok(None)` means more bytes are needed.
/// On `Ok(Some((frame, used)))`, `used` bytes belong to this frame.
///
/// Fields a newer peer appended after the ones known here are skipped, which
/// is how a compatible field is added. Any error means the connection is out
/// of step and must be closed.
pub fn decode(buf: &[u8]) -> Result<Option<(Frame<'_>, usize)>, WireError> {
    let Some(header) = buf.first_chunk::<LENGTH_BYTES>() else {
        return Ok(None);
    };
    let body_len = u32::from_le_bytes(*header) as usize;
    let total = body_len
        .checked_add(LENGTH_BYTES)
        .ok_or(WireError::FrameTooLarge(usize::MAX))?;
    if total > MAX_FRAME {
        return Err(WireError::FrameTooLarge(total));
    }
    if total < HEADER {
        return Err(WireError::Malformed);
    }
    let Some(frame) = buf.get(LENGTH_BYTES..total) else {
        return Ok(None);
    };
    let mut from = Reader::new(frame);
    let kind = u16::decode(&mut from).map_err(|_| WireError::Malformed)?;
    Ok(Some((decode_payload(kind, &mut from)?, total)))
}

#[cfg(test)]
mod tests;
