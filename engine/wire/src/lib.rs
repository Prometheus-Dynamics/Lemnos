//! The Lemnos socket protocol. `docs/wire.md` is its specification.
//!
//! A frame names a device, a contract position and a method number, and its
//! arguments are bytes this crate never reads. Nothing here names a kind of
//! device.
//!
//! Encoding and decoding allocate nothing on the paths used in steady state:
//! a [`Frame`] borrows from the bytes it was read from.

mod description;
pub mod ring;

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
    /// Every bit this build knows.
    pub const ALL: u64 = STREAM_HANDLES;
}

/// The most buffers a stream may have when each is its own descriptor. Its descriptors, and
/// the ring's and the doorbell's, travel in one `SCM_RIGHTS` message, which holds 253.
pub const MAX_HANDLE_BUFFERS: u32 = 128;
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
    /// StreamReady for a stream whose buffers are one descriptor each. `descriptors` is 2 plus
    /// `buffers`, all with this frame over `SCM_RIGHTS`: the ring, the doorbell, then the buffers
    /// in order. `buffers` is at most [`MAX_HANDLE_BUFFERS`].
    23 StreamHandles {
        id: u32,
        buffers: u32,
        descriptors: u8,
        buffer_bytes: u32,
        frame_slots: u32,
        job_slots: u32,
        job_bytes: u32,
    }
}

/// How many descriptors StreamHandles carries for a stream of `buffers` buffers, or `None` when
/// that many buffers can't each be a descriptor.
#[must_use]
pub const fn handle_descriptors(buffers: u32) -> Option<u8> {
    if buffers == 0 || buffers > MAX_HANDLE_BUFFERS {
        return None;
    }
    // At most 130, so it fits.
    Some((HANDLE_DESCRIPTORS_BEFORE + buffers) as u8)
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
