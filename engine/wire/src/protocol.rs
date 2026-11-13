//! The protocol itself: frames, calls, values and descriptions. Everything here but the ring;
//! built with the `protocol` feature.

mod description;

pub use description::{
    DeviceDescription, Identity, OwnedContract, OwnedItem, OwnedMethod, OwnedParam, OwnedShape,
    decode_description, device_kind, effective_reading, encode_description,
};

use lemnos_core::{Decode, Encode, Reader, Shape, Status, Writer};

pub use lemnos_core::Tail;

use crate::hash;

/// The Lemnos version this build was compiled from, as its handshake carries it.
pub const LEMNOS: &str = env!("CARGO_PKG_VERSION");

constants! {
    /// The protocol's version. A host answers any other version with a refusal.
    pub const VERSION: u16 = 1;
    /// The largest frame, header included. A sender refuses to build a larger one and a receiver
    /// closes the connection on one.
    pub const MAX_FRAME: usize = 1 << 20;
    /// Bytes of a Lemnos version string in a handshake, NUL-padded.
    pub const NAME_BYTES: usize = 32;
    /// Bytes before a frame's kind that say how long it is.
    pub const LENGTH_BYTES: usize = 4;
}

/// The constants of the stream-handles part: its limits and its lengths. They fold into that
/// part's hash, not the core's.
pub mod handles {
    constants! {
        /// The most buffer descriptors a stream may have when each buffer, or each plane of a
        /// buffer, is its own descriptor. They, and the ring's and the doorbell's, travel in one
        /// `SCM_RIGHTS` message, which holds 253.
        pub const MAX_HANDLE_BUFFERS: u32 = 128;
        /// The most planes a buffer may have (`VIDEO_MAX_PLANES` of V4L2).
        pub const MAX_PLANES: u32 = 8;
        /// Bytes one plane's length takes in StreamRingHandles' `plane_bytes`.
        pub const PLANE_LENGTH_BYTES: usize = 4;
        /// The descriptors that come before the buffers' in StreamRingHandles: the ring and the doorbell.
        pub const HANDLE_DESCRIPTORS_BEFORE: u32 = crate::ring::fds::BEFORE as u32;
    }
}
pub use handles::{HANDLE_DESCRIPTORS_BEFORE, MAX_HANDLE_BUFFERS, MAX_PLANES, PLANE_LENGTH_BYTES};
/// Answers queued for one client before that client is closed.
pub const MAX_QUEUED_BYTES: usize = 4 << 20;
/// Bytes a reply frame takes beside its payload. A call's out buffer may be at most
/// `MAX_FRAME - REPLY_OVERHEAD`. Not one of the layout's constants: it is a limit each side
/// checks for itself.
pub const REPLY_OVERHEAD: usize = 32;

const HEADER: usize = LENGTH_BYTES + 2;

/// The kinds of the two handshake frames. Their first fields keep their place in every version:
/// see [`Build`].
const HELLO: u16 = 1;
const WELCOME: u16 = 2;

/// The build a handshake frame came from: its core hash and its Lemnos version. A Hello and a
/// Welcome begin with these, in the same place in every version, and they are checked before
/// anything else in the frame is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Build {
    /// The core hash the peer was built with.
    pub core: u64,
    name: [u8; NAME_BYTES],
}

impl Build {
    /// The peer's Lemnos version, as it wrote it. Empty when it wrote none it could read.
    #[must_use]
    pub fn lemnos(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_BYTES);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// A declared length is impossible. The connection must be closed.
    FrameTooLarge(usize),
    /// The frame doesn't parse. The connection must be closed.
    Malformed,
    UnknownKind(u16),
    /// The value doesn't fit in a frame. Nothing was sent.
    TooLarge,
    /// The peer's core hash differs: its core protocol (handshake, framing, call and reply
    /// envelopes, the event envelope, stream framing and the description's framing) is not the
    /// one this build speaks. Nothing after the handshake prefix was read. The connection must be
    /// closed. Only the core is compared here; see `docs/writing-a-driver.md`.
    LayoutMismatch(Build),
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FrameTooLarge(n) => write!(f, "a frame of {n} bytes is over the limit"),
            Self::Malformed => f.write_str("malformed frame"),
            Self::UnknownKind(kind) => write!(f, "unknown frame kind {kind}"),
            Self::TooLarge => f.write_str("too large for one frame"),
            Self::LayoutMismatch(peer) => write!(
                f,
                "core protocol differs (hash {:#018x} here, {:#018x} there; the other side is Lemnos {})",
                CORE_HASH,
                peer.core,
                peer.lemnos()
            ),
        }
    }
}

impl std::error::Error for WireError {}

/// How a device stands, as carried on the wire.
pub mod state {
    constants! {
        pub const RUNNING: u8 = 0;
        pub const BACKOFF: u8 = 1;
        pub const OFF: u8 = 2;
    }
}

/// How a device's health is carried on the wire.
pub mod health {
    constants! {
        pub const OK: u8 = 0;
        pub const DEGRADED: u8 = 1;
        pub const FAILED: u8 = 2;
    }
}

/// Text appended to a frame after the frame's first form: a [`Tail`] of text. A peer from before
/// the addition doesn't send it, and then it reads as empty. It can only be a frame's last field.
pub type Appended<'a> = Tail<&'a str>;

/// The Lemnos version string in a handshake: [`NAME_BYTES`] bytes, NUL-padded, so the handshake's
/// prefix is the same size in every version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LemnosName<'a>(pub &'a str);

impl Encode for LemnosName<'_> {
    const SHAPE: Shape = Shape::Array(&Shape::U8, NAME_BYTES as u32);
    fn encode(&self, out: &mut Writer<'_>) -> lemnos_core::Result<()> {
        if self.0.len() > NAME_BYTES {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut padded = [0u8; NAME_BYTES];
        padded[..self.0.len()].copy_from_slice(self.0.as_bytes());
        out.write(&padded)
    }
}

impl<'a> Decode<'a> for LemnosName<'a> {
    fn decode(from: &mut Reader<'a>) -> lemnos_core::Result<Self> {
        let bytes = from.take(NAME_BYTES)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(NAME_BYTES);
        let name = core::str::from_utf8(&bytes[..end]).map_err(|_| Status::INVALID_ARGUMENT)?;
        Ok(Self(name))
    }
}

/// Which driver packages a host runs, as carried on the wire.
pub mod package_mode {
    constants! {
        /// None: no package directory is configured.
        pub const OFF: u8 = 0;
        /// Only packages signed by a key the host trusts.
        pub const SIGNED: u8 = 1;
        /// Any package in its directory.
        pub const OPEN: u8 = 2;
    }
}

/// Which kind of value an added frame or field is: `added` marks one appended after the base.
macro_rules! is_added {
    () => {
        false
    };
    (added) => {
        true
    };
}

/// Declares the frames, in sections: one section per part of the wire (see [`PARTS`]), numbered
/// as the part is. A frame marked `added` is one appended after the part's base, so it is left
/// out of the part's hash: a reader from before it refuses the kind alone, with
/// [`WireError::UnknownKind`]. A field whose type is [`Tail`] is likewise an addition, and is left
/// out of the hash of its frame.
macro_rules! frames {
    ($(
        $section:literal = $part:literal $(bit $bit:ident)? $(folds $fold:path)? {
            $(
                $(#[$doc:meta])*
                $($added:ident)? $kind:literal $name:ident {
                    $($(#[$field_doc:meta])* $field:ident: $ty:ty),* $(,)?
                }
            )*
        }
    )*) => {
        /// What a host can do beyond the core, as bits of `features` in Welcome: one bit for each
        /// part that has one, declared with `bit` in [`frames!`]. A client uses what a bit stands for
        /// only when the host set it; a host that doesn't know a bit leaves it clear.
        pub mod feature {
            $( $( pub const $bit: u64 = 1 << ($part - 1); )? )*
            /// Every bit this build knows.
            pub const EVERY: u64 = 0 $( $( | $bit )? )*;
        }

        /// The number of parts: every section but the core. A part is numbered where it is declared,
        /// from 1 in order, and is negotiated by its name.
        pub const PARTS: usize = [$($part),*].len() - 1;

        /// The name of a section, by its part number (0 is the core).
        pub(crate) const fn section_name(number: u8) -> &'static str {
            match number {
                $( $part => $section, )*
                _ => "",
            }
        }

        /// The feature bit of a section: 0 for the core and for a part without one.
        pub(crate) const fn section_bit(number: u8) -> u64 {
            match number {
                $( $part => 0 $( + feature::$bit )?, )*
                _ => 0,
            }
        }

        /// The hash of a part: its number, name and bit, its constants, and its base frames. The core
        /// (number 0) is not hashed this way; see `CORE_HASH`.
        pub(crate) const fn section_hash(number: u8) -> u64 {
            let mut hash = crate::hash::word(crate::hash::OFFSET, number as u64);
            hash = crate::hash::text(hash, section_name(number));
            hash = crate::hash::word(hash, section_bit(number));
            $( if number == $part { $( hash = $fold(hash); )? } )*
            Frame::fold_part(hash, number)
        }

        // Each part is numbered from 1 in declaration order, and each one has its bit, which is
        // `1 << (number - 1)`. Breaking either fails the build.
        const _: () = {
            let numbers = [$($part),*];
            let mut i = 0;
            while i < numbers.len() {
                assert!(numbers[i] as usize == i, "sections must be numbered 0, 1, 2 ... in order");
                if i > 0 {
                    assert!(
                        section_bit(i as u8) == 1u64 << (i - 1),
                        "a part's feature bit must be 1 << (its number - 1)"
                    );
                }
                i += 1;
            }
        };

        // Two frames sharing a kind number would be read as each other.
        const _: () = assert!(
            crate::protocol::kinds_are_unique(&[ $( $( $kind, )* )* ]),
            "two frames share a kind number"
        );

        /// One message, in either direction.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub enum Frame<'a> {
            $($($(#[$doc])* $name { $($(#[$field_doc])* $field: $ty),* },)*)*
        }

        impl Frame<'_> {
            /// The frame's kind number.
            pub const fn kind(&self) -> u16 {
                match self {
                    $($(Self::$name { .. } => $kind,)*)*
                }
            }

            fn encode_payload(&self, out: &mut Writer<'_>) -> lemnos_core::Result<()> {
                match self {
                    $($(Self::$name { $($field),* } => {
                        $(Encode::encode($field, out)?;)*
                        Ok(())
                    })*)*
                }
            }
        }

        impl<'a> Frame<'a> {
            /// Folds the base of one part into `hash`: each kind, name, field name and field shape
            /// of its frames, in the order the table declares them, leaving out the additions.
            /// The encoder and decoder above come from the same tokens, so a change to any of
            /// them moves the hash.
            pub(crate) const fn fold_part(mut hash: u64, part: u8) -> u64 {
                $(
                    if part == $part {
                        $(
                            if !is_added!($($added)?) {
                                hash = crate::hash::word(hash, $kind);
                                hash = crate::hash::text(hash, stringify!($name));
                                $(
                                    if !<$ty as Encode>::ADDED {
                                        hash = crate::hash::text(hash, stringify!($field));
                                        hash = lemnos_core::layout::shape(hash, &<$ty as Encode>::SHAPE);
                                    }
                                )*
                                hash = crate::hash::word(hash, u64::MAX);
                            }
                        )*
                    }
                )*
                hash
            }
        }

        fn decode_payload<'a>(kind: u16, from: &mut Reader<'a>) -> Result<Frame<'a>, WireError> {
            match kind {
                $($($kind => Ok(Frame::$name {
                    $($field: Decode::decode(from).map_err(|_| WireError::Malformed)?,)*
                }),)*)*
                other => Err(WireError::UnknownKind(other)),
            }
        }
    };
}

frames! {
    // The core: the handshake, the call and reply envelopes, devices, the event envelope and
    // stream framing. Its base is the core hash, which Hello and Welcome carry.
    "core" = 0 {
        /// The handshake, sent first. Its prefix is the core hash, then the Lemnos version, both
        /// fixed in size: a peer checks them before reading anything after. Then the protocol
        /// `version`, and the part hashes (see [`PART_HASHES`]), which are checked once the core
        /// agrees.
        1 Hello {
            core: u64,
            lemnos: LemnosName<'a>,
            version: u16,
            name: &'a str,
            events: bool,
            parts: Tail<&'a [u8]>,
        }
        /// The host's answer to Hello, with the same prefix. A host that refuses a Hello for its
        /// core still answers with this, so the client can say which revisions differ, and closes.
        2 Welcome {
            core: u64,
            lemnos: LemnosName<'a>,
            version: u16,
            features: u64,
            host: &'a str,
            parts: Tail<&'a [u8]>,
        }
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
        18 StreamRings {
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
    }
    // The stream-handles part: a stream whose buffers are one descriptor each. Its constants are
    // in [`handles`].
    "stream-handles" = 1 bit STREAM_HANDLES folds handles::fold_constants {
        /// StreamOpen from a client that can also take StreamRingHandles for an answer. Sent only to a
        /// host whose Welcome set [`feature::STREAM_HANDLES`].
        22 StreamOpenHandles { id: u32, device: u32, name: &'a str }
        /// StreamRings for a stream whose buffers are one descriptor each, or one per plane.
        /// `plane_bytes` is each plane's length, [`PLANE_LENGTH_BYTES`] little-endian bytes each,
        /// adding up to `buffer_bytes`; empty when a buffer is one plane. `descriptors` is 2 plus
        /// `buffers` times the planes, all with this frame over `SCM_RIGHTS`: the ring, the
        /// doorbell, then buffer 0's planes in order, buffer 1's, and so on. At most
        /// [`MAX_HANDLE_BUFFERS`] buffer descriptors.
        23 StreamRingHandles {
            id: u32,
            buffers: u32,
            descriptors: u8,
            buffer_bytes: u32,
            frame_slots: u32,
            job_slots: u32,
            job_bytes: u32,
            plane_bytes: &'a [u8],
        }
    }
    // The packages part: driver packages the host runs. Sent only to a host whose Welcome set
    // [`feature::PACKAGES`]. Its constants are the package modes.
    "packages" = 2 bit PACKAGES folds package_mode::fold_constants {
        /// Asks what became of the host's driver packages. Answered with one Package for each
        /// file, then PackagesDone.
        24 Packages { id: u32 }
        /// One package file. Loaded: `driver` is its driver's name and `reason` is empty. Refused:
        /// `driver` is empty and `reason` says why.
        25 Package { id: u32, path: &'a str, driver: &'a str, reason: &'a str }
        /// `mode` is one of [`package_mode`]. `note` is empty, or says why the package directory
        /// could not be used at all.
        26 PackagesDone { id: u32, mode: u8, note: &'a str }
    }
    // The jobs part: long jobs, with their progress and end sent to whoever started them. Sent
    // only to a host whose Welcome set [`feature::JOBS`].
    "jobs" = 3 bit JOBS {
        /// Starts a long job: a method the contract marks as one. Answered with JobStarted, or
        /// Refusal.
        27 CallJob { id: u32, device: u32, contract: u8, method: u16, args: &'a [u8] }
        /// The job is running, under this number. JobProgress and JobDone for it go to this
        /// connection alone.
        28 JobStarted { id: u32, job: u64 }
        /// How far a job has come: `done` of `total` (0 when not known), in the contract's unit.
        /// Only the latest matters: a host may drop one a client hasn't read yet.
        29 JobProgress { job: u64, done: u64, total: u64 }
        /// A job is over. `bytes` is its result when `status` is 0, and empty otherwise. Never
        /// dropped.
        30 JobDone { job: u64, status: i32, bytes: &'a [u8] }
        /// Cancels a job this connection started. Answered with an empty Reply, and the job's
        /// JobDone follows.
        31 JobCancel { id: u32, job: u64 }
    }
    // The shared-rings part: the stream answers are shared-memory rings. Its frames are
    // StreamRings and StreamRingHandles, which are declared in the core and the stream-handles
    // part; this part states the bit, so a peer can see whether the host's layout is the one
    // this build reads.
    "shared-rings" = 4 bit SHARED_RINGS folds crate::ring::fds::fold_constants {
    }
    // The endpoints part: a function's two bulk endpoints, lent to one connection at a time. Its
    // frames are all additions after the base, so its hash is its name, number and bit.
    "endpoints" = 5 bit ENDPOINTS {
        /// Borrows the bulk endpoints of a device's function for this connection. Sent only to a
        /// host whose Welcome set [`feature::ENDPOINTS`]. Answered with Endpoints, or Refusal.
        added 32 EndpointsOpen { id: u32, device: u32 }
        /// The two bulk endpoints, `[OUT, IN]`: two descriptors with this frame over `SCM_RIGHTS`,
        /// the OUT endpoint's first. Only the connection that asked holds them.
        added 33 Endpoints { id: u32 }
        /// Gives up the endpoints this connection holds. Answered with an empty Reply.
        added 34 EndpointsClose { id: u32, device: u32 }
    }
}

/// How many descriptors StreamRingHandles carries for a stream of `buffers` buffers of `planes`
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

/// A buffer's planes, by length: what StreamRingHandles' `plane_bytes` carries.
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

    /// Reads `plane_bytes` from StreamRingHandles, checked as [`new`](Self::new) checks; empty is
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

    /// The bytes StreamRingHandles carries for these planes: empty for one plane.
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
    if kind == HELLO || kind == WELCOME {
        check_handshake(from)?;
    }
    let decoded = decode_payload(kind, &mut from)?;
    Ok(Some((decoded, total)))
}

/// Checks a handshake's prefix, the core hash and the Lemnos version, before anything after it
/// is read. A peer whose core differs is refused here whatever it sends after.
fn check_handshake(mut from: Reader<'_>) -> Result<(), WireError> {
    let core = u64::decode(&mut from).map_err(|_| WireError::Malformed)?;
    let name = LemnosName::decode(&mut from).map_err(|_| WireError::Malformed)?;
    if core == CORE_HASH {
        return Ok(());
    }
    let mut bytes = [0u8; NAME_BYTES];
    bytes[..name.0.len()].copy_from_slice(name.0.as_bytes());
    Err(WireError::LayoutMismatch(Build { core, name: bytes }))
}

/// What a peer's parts say about this build's. A part both builds have, with the same hash, is
/// `agreed`; one both have with different hashes is `refused`, by name, and the rest of the
/// connection works without it. A part the peer does not list, or names this build does not
/// know, is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Parts {
    /// The feature bits of the parts both builds have and agree on.
    pub agreed: u64,
    /// The feature bits of the parts both builds have whose hashes differ.
    pub refused: u64,
    /// The peer's hash of each refused part, by its index in [`PART_NAMES`]; 0 for the others.
    pub theirs: [u64; PARTS],
}

/// Reads a Hello's or a Welcome's parts and compares them with this build's. Malformed records
/// are an error: the connection must be closed.
pub fn check_parts(bytes: &[u8]) -> Result<Parts, WireError> {
    let mut out = Parts {
        agreed: 0,
        refused: 0,
        theirs: [0; PARTS],
    };
    let mut at = 0;
    while at < bytes.len() {
        let len = *bytes.get(at).ok_or(WireError::Malformed)? as usize;
        let name = bytes
            .get(at + 1..at + 1 + len)
            .ok_or(WireError::Malformed)?;
        let raw = bytes
            .get(at + 1 + len..at + 9 + len)
            .ok_or(WireError::Malformed)?;
        at += 9 + len;
        let theirs = u64::from_le_bytes(raw.try_into().map_err(|_| WireError::Malformed)?);
        let Some(index) = PART_NAMES.iter().position(|known| known.as_bytes() == name) else {
            continue;
        };
        let bit = PART_BITS[index];
        if (out.agreed | out.refused) & bit != 0 {
            continue;
        }
        if theirs == PART_HASHES[index] {
            out.agreed |= bit;
        } else {
            out.refused |= bit;
            out.theirs[index] = theirs;
        }
    }
    Ok(out)
}

/// The names of the parts, in the order [`PART_HASHES`] lists them.
pub const PART_NAMES: [&str; PARTS] = part_names();

/// The feature bit of each part, in the order [`PART_NAMES`] lists them.
pub const PART_BITS: [u64; PARTS] = part_bits();

/// The hash of each part, in the order [`PART_NAMES`] lists them. Each one moves only when that
/// part's own declaration changes: see `section_hash`.
pub const PART_HASHES: [u64; PARTS] = part_hashes();

const fn part_names() -> [&'static str; PARTS] {
    let mut out = [""; PARTS];
    let mut i = 0;
    while i < PARTS {
        out[i] = section_name(i as u8 + 1);
        i += 1;
    }
    out
}

const fn part_bits() -> [u64; PARTS] {
    let mut out = [0; PARTS];
    let mut i = 0;
    while i < PARTS {
        out[i] = section_bit(i as u8 + 1);
        i += 1;
    }
    out
}

const fn part_hashes() -> [u64; PARTS] {
    let mut out = [0; PARTS];
    let mut i = 0;
    while i < PARTS {
        out[i] = section_hash(i as u8 + 1);
        i += 1;
    }
    out
}

/// Whether no two of `kinds` are the same number.
pub(crate) const fn kinds_are_unique(kinds: &[u16]) -> bool {
    let mut i = 0;
    while i < kinds.len() {
        let mut j = i + 1;
        while j < kinds.len() {
            if kinds[i] == kinds[j] {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// Each part as Hello and Welcome carry it: one record per part, `[name length: u8][name][hash:
/// u64 little-endian]`, in the order of [`PART_NAMES`]. A peer reads the records it knows by name.
pub const PART_BYTES_LEN: usize = {
    let mut n = 0;
    let mut i = 0;
    while i < PARTS {
        n += 1 + PART_NAMES[i].len() + 8;
        i += 1;
    }
    n
};

pub const PART_BYTES: [u8; PART_BYTES_LEN] = {
    let mut out = [0u8; PART_BYTES_LEN];
    let mut at = 0;
    let mut i = 0;
    while i < PARTS {
        let name = PART_NAMES[i].as_bytes();
        out[at] = name.len() as u8;
        at += 1;
        let mut j = 0;
        while j < name.len() {
            out[at] = name[j];
            at += 1;
            j += 1;
        }
        let hash = PART_HASHES[i].to_le_bytes();
        let mut k = 0;
        while k < 8 {
            out[at] = hash[k];
            at += 1;
            k += 1;
        }
        i += 1;
    }
    out
};

/// The core hash of this build: FNV-1a over the core's base frames, the constants of the framing,
/// the state and health vocabularies, the ring's layout and the description's framing. The parts
/// are not in it: adding one moves no core hash. It is
/// computed at compile time from the same definitions the encoder and decoder are generated
/// from, so it can't be kept by hand. Hello and Welcome carry it first; see `docs/wire.md`.
pub const CORE_HASH: u64 = {
    let mut hash = hash::OFFSET;
    hash = fold_constants(hash);
    hash = state::fold_constants(hash);
    hash = health::fold_constants(hash);
    hash = Frame::fold_part(hash, 0);
    hash = description::fold_framing(hash);
    hash = crate::ring::fold_constants(hash);
    hash
};

#[cfg(test)]
mod tests;
