//! The codec block as `codec.block/1` has it. The block's name is `mvx`.
//!
//! The block is a stateful codec: bitstream goes in and pictures come out on the block's own
//! schedule, each side on its own queue. So a job here is one thing done to a session, and a
//! job that hands the block a buffer stays pending until the block gives that buffer back,
//! which may be before or after jobs posted earlier. Each job's work has exactly one of these
//! items:
//!
//! - **[`TAG_SETUP`]**, [`SETUP_BYTES`], seventeen little-endian `u32`s: the direction (0
//!   decode, 1 encode); the source format and the destination format, each as its fourcc,
//!   width, height, memory planes (1 to 3) and the size of each of three planes (0 lets the
//!   kernel choose); the source buffers and the destination buffers to ask for. With it, any
//!   number up to [`MAX_CONTROLS`] of **[`TAG_CONTROL`]** items, 8 bytes each (the control's
//!   id and its value, an `i32`), set before the session streams. A session is opened for it;
//!   one already open is closed first, and its pending jobs fail. Read-back: what the kernel
//!   kept ([`FORMAT_BYTES`] for each format: fourcc, width, height, planes, then each plane's
//!   size and bytes per line) and the buffers it granted on each side, 88 bytes.
//! - **[`TAG_SOURCE`]**, 8 bytes: the source's timestamp, a `u64` of nanoseconds, which the
//!   block copies to what it makes from it. A reference per memory plane of the source format,
//!   slot `n` for plane `n`: the bytes the block reads are `length` from the buffer's start
//!   (`offset` must be 0; `length` 1 to the buffer's size). The first source starts the
//!   session. Pending until the block has read it; read-back: its flags, its sequence number
//!   and its timestamp, 16 bytes.
//! - **[`TAG_DESTINATION`]**, no bytes: a reference per memory plane of the destination
//!   format, slot `n` for plane `n`, `offset` and `length` 0. Pending until the block has
//!   filled it; read-back: its flags (`V4L2_BUF_FLAG_*`: `LAST` ends a drained stream, `ERROR`
//!   a picture in error or bytes the kernel reported past a buffer), its sequence number, the
//!   timestamp of the source it came from, its planes and the bytes written in each of three,
//!   32 bytes.
//! - **[`TAG_DRAIN`]**, no bytes: the block finishes what it holds; the last destination
//!   comes back flagged `LAST`.
//! - **[`TAG_EVENTS`]**, no bytes: takes the session's events. Read-back: the source-change
//!   flags (`V4L2_EVENT_SRC_CH_*`, 0 when the stream's format did not change), the destination
//!   format the kernel has now and the destination buffers it granted, 48 bytes.
//! - **[`TAG_ADOPT`]**, 4 bytes: a destination buffer count, 1 to 64. Sets the destinations up
//!   for the format the stream has now (a decoder that asked for none, or after a resolution
//!   change once drained); destinations the block held come back as failed. Read-back as for
//!   [`TAG_EVENTS`] with flags 0.
//! - **[`TAG_STOP`]**, no bytes: stops the session; everything the block held comes back as
//!   failed, and the next source starts it again.
//! - **[`TAG_CONTROL`]** items alone, up to [`MAX_CONTROLS`]: set on the open session.
//!   Read-back: the value the kernel kept for each, an `i32` each.
//! - **[`TAG_PRIVATE`]**: one of the block's private requests, as the request number (a
//!   `u32`) and then its structure. Only the requests `lemnos-driver-k230-vpu-sys` found sound
//!   are issued, with a structure of exactly their size; any other is refused. Read-back: the
//!   structure as the kernel wrote it back.
//!
//! Anything else is refused: another tag, two kinds in one job, an item of the wrong size, a
//! reference where the kind takes none, a slot past the format's planes or named twice.
//!
//! # Buffers, and what is checked
//!
//! Both queues take the stream's dma-bufs (`V4L2_MEMORY_DMABUF`): nothing is copied. The first
//! job that names a stream buffer duplicates its handle and adds it to the claim, which
//! measures the real size of the dma-buf once; later jobs use the same claim buffer, until the
//! session is closed. A stream buffer without a handle refuses the job. Then the claim's own
//! checks ([`crate::job`]): one plane per memory plane of the queue's format, each a different
//! buffer the block does not hold already, each at least the plane size the kernel answered
//! for the format, a source's payload 1 to the buffer's size, no data offset, the V4L2 index
//! the claim's own. A destination's byte counts come back only when the kernel's are inside
//! the buffer.
//!
//! # One consumer, one session
//!
//! A setup opens the session; a consumer that goes closes it, which makes the kernel tear it
//! down with every buffer it was given, and forgets its pending jobs. A session that can't be
//! opened fails the setup and is tried again by the next, behind a backoff doubling from
//! 250 ms to 8 s; it never gives up. A session whose completions can't be taken is closed and
//! its pending jobs fail; the caller sets it up again.

use std::io;
use std::os::fd::AsFd;
use std::time::Duration;

use lemnos_contract_codec_block::work::Work;
use lemnos_contract_codec_block::{BlockInfo, JobStatus};
use lemnos_core::Health;
use lemnos_driver_codec_jobs::{Block, Buffers, Finished, MAX_IN_FLIGHT, Start};
use lemnos_driver_m2m_decode::{PixFormatMplane, PlaneFormat};

use crate::claim::{
    ClaimError, Completion, ControlError, Device, MAX_BUFFERS, OwnedClaim, SubmitError, Vpu,
};
use crate::job::{BufferId, Destination, Payload, Source};
use crate::setup::{Control, Direction, Setup};
use crate::uapi::MAX_PLANES;

/// The block's name.
pub const NAME: &str = "mvx";
/// The item that opens a session.
pub const TAG_SETUP: u32 = 1;
/// The item that hands the block a source.
pub const TAG_SOURCE: u32 = 2;
/// The item that hands the block a destination.
pub const TAG_DESTINATION: u32 = 3;
/// The item that drains the stream.
pub const TAG_DRAIN: u32 = 4;
/// One control.
pub const TAG_CONTROL: u32 = 5;
/// One private request.
pub const TAG_PRIVATE: u32 = 6;
/// The item that takes the session's events.
pub const TAG_EVENTS: u32 = 7;
/// The item that sets the destinations up for the stream's format.
pub const TAG_ADOPT: u32 = 8;
/// The item that stops the session.
pub const TAG_STOP: u32 = 9;

/// Bytes of a [`TAG_SETUP`] item.
pub const SETUP_BYTES: usize = 17 * 4;
/// Bytes of a [`TAG_CONTROL`] item.
pub const CONTROL_BYTES: usize = 8;
/// Controls one job carries at most.
pub const MAX_CONTROLS: usize = 16;
/// The largest private request structure carried: every one issued is smaller.
pub const MAX_PRIVATE_BYTES: usize = 1024;
/// Bytes of a format in a read-back.
pub const FORMAT_BYTES: usize = (4 + 2 * MAX_PLANES) * 4;
/// Stream buffers a job may name: the claim's buffers.
pub const STREAM_BUFFERS: usize = MAX_BUFFERS;

/// What health says while the node can't be opened or set up; the next setup tries again.
pub const NOT_THERE: &str = "the codec node did not open a session: tried again by the next setup";
/// What health says when the node is another driver's.
pub const NOT_THE_BLOCK: &str = "the node is not the codec block's";
/// What health says after a session failed and was closed.
pub const CLOSED: &str = "the codec session failed and was closed: set it up again";

/// A little-endian `u32` at word `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(4 * at..4 * at + 4)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

/// A format from seven words of a [`TAG_SETUP`] item.
fn format(bytes: &[u8], at: usize) -> Option<PixFormatMplane> {
    let w = |i| word(bytes, at + i);
    let mut format = PixFormatMplane {
        pixelformat: w(0)?,
        width: w(1)?,
        height: w(2)?,
        num_planes: w(3)?,
        ..PixFormatMplane::default()
    };
    for (i, plane) in format.planes.iter_mut().take(MAX_PLANES).enumerate() {
        *plane = PlaneFormat {
            size_image: w(4 + i)?,
            bytes_per_line: 0,
        };
    }
    Some(format)
}

/// Writes `values` little-endian into `into` from its start; how many bytes.
fn put(into: &mut [u8], values: &[u32]) -> Option<usize> {
    let bytes = 4 * values.len();
    for (four, value) in into
        .get_mut(..bytes)?
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(values)
    {
        *four = value.to_le_bytes();
    }
    Some(bytes)
}

/// A format as a read-back has it.
fn format_words(format: &PixFormatMplane) -> [u32; 4 + 2 * MAX_PLANES] {
    let mut words = [0; 4 + 2 * MAX_PLANES];
    words[..4].copy_from_slice(&[
        format.pixelformat,
        format.width,
        format.height,
        format.num_planes,
    ]);
    for (i, plane) in format.planes.iter().take(MAX_PLANES).enumerate() {
        words[4 + 2 * i] = plane.size_image;
        words[5 + 2 * i] = plane.bytes_per_line;
    }
    words
}

/// The block, claimed or not.
enum Link<D: Device> {
    Idle(Box<Vpu<D>>),
    Claimed(Box<OwnedClaim<D>>),
    /// Between the two, for the length of a call.
    Moving,
}

/// A job the block holds a buffer for.
#[derive(Clone, Copy, Debug)]
struct Pending {
    token: u32,
    source: bool,
    /// The session went away under it: it is reported failed.
    failed: bool,
}

/// What one job's work asks for.
enum Kind<'w> {
    Setup(&'w [u8]),
    Source(u64),
    Destination,
    Drain,
    Events,
    Adopt(u32),
    Stop,
    Controls,
    Private(&'w [u8]),
}

/// The codec block as `codec.block/1` has it, on a session it owns across jobs.
pub struct VpuBlock<D: Device> {
    link: Link<D>,
    /// The claim's buffer for each stream buffer a job has named in this session.
    ids: [Option<BufferId>; STREAM_BUFFERS],
    pending: [Option<Pending>; MAX_IN_FLIGHT],
    next_token: u32,
    controls: [Control; MAX_CONTROLS],
    private: [u8; MAX_PRIVATE_BYTES],
    fault: Option<&'static str>,
}

impl<D: Device> VpuBlock<D>
where
    D::Node: AsFd,
{
    /// The block on `device`. Nothing is opened until a setup comes.
    pub fn new(device: D) -> Self {
        Self {
            link: Link::Idle(Box::new(Vpu::new(device))),
            ids: [None; STREAM_BUFFERS],
            pending: [None; MAX_IN_FLIGHT],
            next_token: 0,
            controls: [Control { id: 0, value: 0 }; MAX_CONTROLS],
            private: [0; MAX_PRIVATE_BYTES],
            fault: None,
        }
    }

    /// Whether a session is open.
    pub fn claimed(&self) -> bool {
        matches!(self.link, Link::Claimed(_))
    }

    fn claim(&mut self) -> Option<&mut OwnedClaim<D>> {
        match &mut self.link {
            Link::Claimed(claim) => Some(&mut **claim),
            _ => None,
        }
    }

    /// Closes the session, keeping the block and its backoff; every pending job fails.
    fn unclaim(&mut self) {
        self.ids = [None; STREAM_BUFFERS];
        self.fail_pending(|_| true);
        if let Link::Claimed(_) = self.link {
            let Link::Claimed(claim) = std::mem::replace(&mut self.link, Link::Moving) else {
                return;
            };
            self.link = Link::Idle(claim.release());
        }
    }

    fn fail_pending(&mut self, which: impl Fn(&Pending) -> bool) {
        for pending in self.pending.iter_mut().flatten() {
            if which(pending) {
                pending.failed = true;
            }
        }
    }

    /// A token no pending job has, and a place for it.
    fn token(&mut self, source: bool) -> Option<u32> {
        let free = self.pending.iter().position(Option::is_none)?;
        loop {
            self.next_token = self.next_token.wrapping_add(1);
            let token = self.next_token;
            if !self.pending.iter().flatten().any(|p| p.token == token) {
                self.pending[free] = Some(Pending {
                    token,
                    source,
                    failed: false,
                });
                return Some(token);
            }
        }
    }

    /// A pending job whose buffers came back without a completion, reported failed.
    fn take_failed(&mut self) -> Option<Finished> {
        let slot = self
            .pending
            .iter_mut()
            .find(|p| p.is_some_and(|p| p.failed))?;
        let pending = slot.take()?;
        Some(Finished {
            token: pending.token,
            status: JobStatus::Failed,
            read_back: 0,
        })
    }

    fn forget(&mut self, token: u32) {
        for slot in &mut self.pending {
            if slot.is_some_and(|p| p.token == token) {
                *slot = None;
            }
        }
    }

    /// The claim's buffer for stream buffer `index`, taken from the stream now if no job of
    /// this session named it before.
    fn buffer(&mut self, index: u32, buffers: &Buffers<'_>) -> Result<BufferId, Start> {
        let Link::Claimed(claim) = &mut self.link else {
            return Err(Start::Refused);
        };
        let Some(slot) = self.ids.get_mut(index as usize) else {
            return Err(Start::Refused);
        };
        if let Some(id) = *slot {
            return Ok(id);
        }
        let Some(handle) = buffers.handle(index) else {
            return Err(Start::Refused);
        };
        let Ok(fd) = handle.fd().try_clone_to_owned() else {
            return Err(Start::Failed);
        };
        let Ok(id) = claim.add_buffer(fd) else {
            return Err(Start::Refused);
        };
        *slot = Some(id);
        Ok(id)
    }

    /// The planes a job's references name, in slot order: `(buffer, length)` for each.
    fn planes(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        source: bool,
    ) -> Result<([(BufferId, u32); MAX_PLANES], usize), Start> {
        let mut named = [None; MAX_PLANES];
        for reference in work.refs() {
            let Some(slot) = named.get_mut(reference.slot as usize) else {
                return Err(Start::Refused);
            };
            let length_ok = if source {
                reference.length != 0
            } else {
                reference.length == 0
            };
            if slot.is_some() || reference.offset != 0 || !length_ok {
                return Err(Start::Refused);
            }
            *slot = Some((reference.buffer, reference.length));
        }
        let count = named.iter().take_while(|n| n.is_some()).count();
        if named[count..].iter().any(Option::is_some) {
            return Err(Start::Refused);
        }
        let none = BufferId {
            slot: 0,
            generation: 0,
        };
        let mut planes = [(none, 0); MAX_PLANES];
        for (plane, (buffer, length)) in planes.iter_mut().zip(named.into_iter().flatten()) {
            *plane = (self.buffer(buffer, buffers)?, length);
        }
        Ok((planes, count))
    }

    fn setup(&mut self, bytes: &[u8], work: &Work<'_>, read_back: &mut [u8], now_ns: u64) -> Start {
        let Some(direction) = (match word(bytes, 0) {
            Some(0) => Some(Direction::Decode),
            Some(1) => Some(Direction::Encode),
            _ => None,
        }) else {
            return Start::Refused;
        };
        let (Some(source), Some(destination), Some(sources), Some(destinations)) = (
            format(bytes, 1),
            format(bytes, 8),
            word(bytes, 15),
            word(bytes, 16),
        ) else {
            return Start::Refused;
        };
        let mut count = 0;
        for (tag, item) in work.items() {
            if tag != TAG_CONTROL {
                continue;
            }
            let (Some(id), Some(value)) = (word(item, 0), word(item, 1)) else {
                return Start::Refused;
            };
            self.controls[count] = Control {
                id,
                value: value.cast_signed(),
            };
            count += 1;
        }
        let checked = Setup {
            direction,
            source,
            destination,
            sources,
            destinations,
            controls: &self.controls[..count],
        }
        .check();
        if checked.is_err() {
            return Start::Refused;
        }
        // Another setup is another session: the one open is closed first.
        self.unclaim();
        let Link::Idle(vpu) = std::mem::replace(&mut self.link, Link::Moving) else {
            return Start::Failed;
        };
        let setup = Setup {
            direction,
            source,
            destination,
            sources,
            destinations,
            controls: &self.controls[..count],
        };
        match vpu.claim_owned(&setup, now_ns) {
            Ok(claim) => {
                self.fault = None;
                self.link = Link::Claimed(Box::new(claim));
            }
            Err((vpu, error)) => {
                self.link = Link::Idle(vpu);
                return match error {
                    ClaimError::Setup(_) | ClaimError::Refused(_) => Start::Refused,
                    ClaimError::NotTheBlock => {
                        self.fault = Some(NOT_THE_BLOCK);
                        Start::Failed
                    }
                    ClaimError::Device { .. } | ClaimError::BackingOff { .. } => {
                        self.fault = Some(NOT_THERE);
                        Start::Failed
                    }
                };
            }
        }
        let Some(claim) = self.claim() else {
            return Start::Failed;
        };
        let mut words = [0u32; 2 * (4 + 2 * MAX_PLANES) + 2];
        let one = 4 + 2 * MAX_PLANES;
        words[..one].copy_from_slice(&format_words(&claim.source_format()));
        words[one..2 * one].copy_from_slice(&format_words(&claim.destination_format()));
        words[2 * one] = claim.source_slots();
        words[2 * one + 1] = claim.destination_slots();
        put(read_back, &words).map_or(Start::Failed, Start::Done)
    }

    /// The destination format and slots, after `changes`.
    fn destinations(&mut self, changes: u32, read_back: &mut [u8]) -> Start {
        let Some(claim) = self.claim() else {
            return Start::Failed;
        };
        let mut words = [0u32; 1 + 4 + 2 * MAX_PLANES + 1];
        words[0] = changes;
        words[1..1 + 4 + 2 * MAX_PLANES]
            .copy_from_slice(&format_words(&claim.destination_format()));
        words[1 + 4 + 2 * MAX_PLANES] = claim.destination_slots();
        put(read_back, &words).map_or(Start::Failed, Start::Done)
    }

    /// A buffer handed to the block: queued, and pending under a token.
    fn hand(&mut self, work: &Work<'_>, buffers: &Buffers<'_>, stamp: Option<u64>) -> Start {
        let source = stamp.is_some();
        if !self.claimed() {
            return Start::Refused;
        }
        let (planes, count) = match self.planes(work, buffers, source) {
            Ok(planes) => planes,
            Err(start) => return start,
        };
        let Some(token) = self.token(source) else {
            return Start::Refused;
        };
        let Some(claim) = self.claim() else {
            return Start::Failed;
        };
        let submitted = if let Some(stamp) = stamp {
            let payloads = planes.map(|(buffer, bytes)| Payload { buffer, bytes });
            claim.submit(&Source {
                identity: u64::from(token),
                planes: &payloads[..count],
                timestamp: Duration::from_nanos(stamp),
            })
        } else {
            let ids = planes.map(|(buffer, _)| buffer);
            claim.give(&Destination {
                identity: u64::from(token),
                planes: &ids[..count],
            })
        };
        match submitted {
            Ok(()) => Start::Pending(token),
            Err(error) => {
                self.forget(token);
                match error {
                    SubmitError::Job(_) | SubmitError::Full | SubmitError::Pending => {
                        Start::Refused
                    }
                    // The kernel refused it; nothing is queued, the session stays.
                    SubmitError::Device(_) => Start::Failed,
                }
            }
        }
    }

    fn controls(&mut self, work: &Work<'_>, read_back: &mut [u8]) -> Start {
        let Some(claim) = self.claim() else {
            return Start::Refused;
        };
        let mut at = 0;
        for (_, item) in work.items() {
            let (Some(id), Some(value)) = (word(item, 0), word(item, 1)) else {
                return Start::Refused;
            };
            let kept = match claim.set_control(id, value.cast_signed()) {
                Ok(kept) => kept,
                Err(ControlError::Refused) => return Start::Refused,
                Err(ControlError::Device(_)) => return Start::Failed,
            };
            let Some(into) = read_back.get_mut(at..at + 4) else {
                return Start::Failed;
            };
            into.copy_from_slice(&kept.to_le_bytes());
            at += 4;
        }
        Start::Done(at)
    }

    fn private(&mut self, bytes: &[u8], read_back: &mut [u8]) -> Start {
        let Some(request) = word(bytes, 0) else {
            return Start::Refused;
        };
        let payload = &bytes[4..];
        if payload.len() > MAX_PRIVATE_BYTES || !lemnos_driver_k230_vpu_sys::takes(request, payload)
        {
            return Start::Refused;
        }
        let staged = &mut self.private[..payload.len()];
        staged.copy_from_slice(payload);
        let Link::Claimed(claim) = &mut self.link else {
            return Start::Refused;
        };
        match claim.private(request, staged) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::InvalidInput
                    && error.raw_os_error().is_none() =>
            {
                return Start::Refused;
            }
            Err(_) => return Start::Failed,
        }
        let Some(into) = read_back.get_mut(..staged.len()) else {
            return Start::Failed;
        };
        into.copy_from_slice(staged);
        Start::Done(staged.len())
    }

    /// The kind of job the work is, with its shape checked.
    fn kind<'w>(work: &Work<'w>) -> Option<Kind<'w>> {
        let mut kind = None;
        let mut controls = 0;
        for (tag, bytes) in work.items() {
            let this = match tag {
                TAG_CONTROL if bytes.len() == CONTROL_BYTES && controls < MAX_CONTROLS => {
                    controls += 1;
                    continue;
                }
                TAG_SETUP if bytes.len() == SETUP_BYTES => Kind::Setup(bytes),
                TAG_SOURCE if bytes.len() == 8 => {
                    let mut eight = [0u8; 8];
                    eight.copy_from_slice(bytes);
                    Kind::Source(u64::from_le_bytes(eight))
                }
                TAG_DESTINATION if bytes.is_empty() => Kind::Destination,
                TAG_DRAIN if bytes.is_empty() => Kind::Drain,
                TAG_EVENTS if bytes.is_empty() => Kind::Events,
                TAG_ADOPT if bytes.len() == 4 => Kind::Adopt(word(bytes, 0)?),
                TAG_STOP if bytes.is_empty() => Kind::Stop,
                TAG_PRIVATE if bytes.len() >= 4 => Kind::Private(bytes),
                _ => return None,
            };
            if kind.replace(this).is_some() {
                return None;
            }
        }
        let refs = work.ref_count() > 0;
        match kind {
            None if controls > 0 && !refs => Some(Kind::Controls),
            Some(Kind::Setup(bytes)) if !refs => Some(Kind::Setup(bytes)),
            Some(kind @ (Kind::Source(_) | Kind::Destination)) if controls == 0 => Some(kind),
            Some(kind) if controls == 0 && !refs => Some(kind),
            _ => None,
        }
    }
}

impl<D: Device> Block for VpuBlock<D>
where
    D::Node: AsFd,
{
    fn name(&self) -> &str {
        NAME
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: 2048,
            max_refs: MAX_PLANES as u16,
            max_items: 1 + MAX_CONTROLS as u16,
            max_read_back: MAX_PRIVATE_BYTES as u32,
            max_buffers: STREAM_BUFFERS as u32,
        }
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        let Some(kind) = Self::kind(work) else {
            return Start::Refused;
        };
        match kind {
            Kind::Setup(bytes) => self.setup(bytes, work, read_back, now_ns),
            Kind::Source(stamp) => self.hand(work, buffers, Some(stamp)),
            Kind::Destination => self.hand(work, buffers, None),
            Kind::Controls => self.controls(work, read_back),
            Kind::Private(bytes) => self.private(bytes, read_back),
            Kind::Drain => match self.claim().map(|claim| claim.drain()) {
                None => Start::Refused,
                Some(Ok(())) => Start::Done(0),
                Some(Err(_)) => Start::Failed,
            },
            Kind::Events => {
                let Some(claim) = self.claim() else {
                    return Start::Refused;
                };
                match claim.source_change() {
                    Ok(change) => self.destinations(change.map_or(0, |c| c.changes), read_back),
                    Err(_) => Start::Failed,
                }
            }
            Kind::Adopt(count) => {
                let Some(claim) = self.claim() else {
                    return Start::Refused;
                };
                if !(1..=64).contains(&count) {
                    return Start::Refused;
                }
                match claim.adopt_destinations(count) {
                    Ok(()) => {
                        // The destinations the block held came back unfilled.
                        self.fail_pending(|p| !p.source);
                        self.destinations(0, read_back)
                    }
                    Err(_) => Start::Failed,
                }
            }
            Kind::Stop => {
                let Some(claim) = self.claim() else {
                    return Start::Refused;
                };
                let stopped = claim.stop();
                self.fail_pending(|_| true);
                match stopped {
                    Ok(()) => Start::Done(0),
                    Err(_) => Start::Failed,
                }
            }
        }
    }

    fn poll(&mut self, read_back: &mut [u8], _now_ns: u64) -> Option<Finished> {
        // Jobs whose buffers came back without a completion first.
        if let Some(failed) = self.take_failed() {
            return Some(failed);
        }
        loop {
            let claim = self.claim()?;
            let (done, source): (io::Result<Option<Completion>>, bool) =
                match claim.complete_source() {
                    Ok(None) => (claim.complete_destination(), false),
                    other => (other, true),
                };
            let done = match done {
                Ok(Some(done)) => done,
                Ok(None) => return None,
                Err(_) => {
                    // A session whose completions can't be taken is in a state nobody
                    // knows: closed, and what it held fails.
                    self.unclaim();
                    self.fault = Some(CLOSED);
                    return self.take_failed();
                }
            };
            let token = u32::try_from(done.identity).unwrap_or(0);
            if !self
                .pending
                .iter()
                .flatten()
                .any(|p| p.token == token && p.source == source)
            {
                // A buffer of a job already reported: nothing to complete.
                continue;
            }
            self.forget(token);
            let stamp = u64::try_from(done.timestamp.as_nanos()).unwrap_or(u64::MAX);
            let stamp = [stamp as u32, (stamp >> 32) as u32];
            let bytes = if source {
                put(read_back, &[done.flags, done.sequence, stamp[0], stamp[1]])
            } else {
                let mut words = [0u32; 5 + MAX_PLANES];
                words[..5].copy_from_slice(&[
                    done.flags,
                    done.sequence,
                    stamp[0],
                    stamp[1],
                    done.planes as u32,
                ]);
                words[5..].copy_from_slice(&done.bytes);
                put(read_back, &words)
            };
            return Some(match bytes {
                Some(bytes) => Finished {
                    token,
                    status: JobStatus::Done,
                    read_back: bytes,
                },
                None => Finished {
                    token,
                    status: JobStatus::Failed,
                    read_back: 0,
                },
            });
        }
    }

    fn release(&mut self) {
        // The consumer went: its session goes, with every buffer the kernel was given, and
        // the server has forgotten its jobs.
        self.unclaim();
        self.pending = [None; MAX_IN_FLIGHT];
    }

    fn health(&self) -> Health {
        match self.fault {
            None => Health::Ok,
            Some(reason) => Health::Degraded(reason),
        }
    }
}
