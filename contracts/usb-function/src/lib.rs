//! The `usb.function/1` contract: this board as a USB device, served by a caller. The block
//! puts the device on the bus; the caller is the device. It answers the control requests the
//! bus doesn't answer for it and moves the bytes of every endpoint. Lemnos reads none of them.
//!
//! # The data stream
//!
//! A consumer opens the stream named by [`UsbFunction::data_stream`]. Its buffers hold the
//! bytes, and its ring carries the jobs. A job is one [`Job`], encoded with this crate's codec
//! and posted on the stream's job queue. No socket is used per transfer. Every job names one
//! buffer of the stream, and that buffer comes back as the job's completion. A buffer has two
//! parts: its first [`DATA_OFFSET`] bytes are for the completion's [`Completion`] record, and
//! a job's bytes start after them.
//!
//! - [`JobKind::Send`]: `length` bytes of the buffer, from [`DATA_OFFSET`], go to the host on
//!   the IN endpoint `endpoint`, as one transfer. A `length` of 0 sends a zero-length packet.
//!   The completion's count is how many bytes the controller took, which is `length`: they
//!   are the controller's to send, and a reset or a pull before the host asks for them loses
//!   them.
//! - [`JobKind::Receive`]: what the host next sends to the OUT endpoint `endpoint` is written
//!   to the buffer from [`DATA_OFFSET`], at most `length` bytes of it, and the completion's
//!   count is how many came (0 for a zero-length packet). The job waits until something
//!   comes. `length` is at least 1 and should be a multiple of the endpoint's packet size:
//!   with any other, bytes of a packet that did not fit can be lost; with one, a transfer
//!   longer than `length` comes with the next jobs.
//! - [`JobKind::Event`]: the next [`Event`] is written to the buffer from [`DATA_OFFSET`],
//!   encoded with this crate's codec, and the completion's count is its size,
//!   [`EVENT_BYTES`]. The job waits until something happens. `endpoint` and `length` are not
//!   read.
//!
//! Jobs for one endpoint run in the order they were posted, one at a time. A send waits for
//! the endpoint to be free of the one before it, so a host that stops asking holds that
//! endpoint's sends, and only those.
//!
//! On an isochronous endpoint a job is one service interval: a send's `length` is at most
//! the bytes the endpoint moves in an interval (its `wMaxPacketSize` at the speed in force,
//! times its transactions per microframe at high speed), and a receive's `length` is at least
//! that; any other completes as [`JobStatus::Refused`]. The block keeps several sends queued
//! with the controller, so a send completes once queued: bytes the host did not ask for in
//! time are dropped, and nobody is told. An isochronous endpoint has no halt. Its interface
//! has it in an alternate setting the host picks; the bus answers `SET_INTERFACE` or delivers
//! it as a request, as the board's controller does, and may report
//! [`EventKind::Configured`] again when the setting changes.
//!
//! Completion is a frame on the same stream. Its buffer is the job's and its `identity` is
//! the job's. The block writes a [`Completion`] to the start of the buffer first, which says
//! whether the job was done and its count. The frame's length is [`DATA_OFFSET`] plus the
//! bytes a receive or an event job put after it. Every job the block reads that names a
//! buffer the block holds gets exactly one completion. A job that cannot be decoded, or names
//! a buffer the block doesn't hold or one another waiting job has, is a broken consumer: the
//! block faults the stream, as it does for any malformed ring, and counts it in
//! [`Counters::errors`]. (A buffer too small for the record itself completes with a length
//! of 0.)
//!
//! The block holds every buffer the consumer has not been handed: the consumer fills a send's
//! buffer before posting the job and leaves a posted job's buffer alone until its completion.
//! A completion's buffer is released with the usual frame return, after which it can be named
//! again. At most [`FunctionInfo::max_pending`] jobs wait at once; more stay on the job queue
//! until there is room.
//!
//! # Events and control requests
//!
//! Events are kept in order until an event job takes them, [`FunctionInfo::max_events`] at
//! most; when more happen than that with nobody taking them, the oldest are dropped and
//! counted. [`UsbFunction::state`] always says how things stand now.
//!
//! A control request the bus doesn't answer itself arrives as [`EventKind::Setup`], with the
//! request's fields. It is pending from then until the caller answers it:
//!
//! - [`UsbFunction::control_in`] sends the data stage of a request whose data goes to the
//!   host;
//! - [`UsbFunction::control_out`] takes the data stage of one whose data comes from the host,
//!   or accepts one that has none;
//! - [`UsbFunction::control_stall`] refuses it.
//!
//! One request is pending at a time, and nothing else is reported while it is. A request
//! nobody answers within [`FunctionInfo::control_timeout_ms`] is refused with a stall and
//! counted, and so is every request that comes while no consumer has the data stream open:
//! a device nobody serves refuses what it is asked.
//!
//! When the configuration goes ([`EventKind::Deconfigured`]: the host chose none, reset the
//! bus, or the cable was pulled) every send and receive waiting fails with
//! [`JobStatus::NotConfigured`], a pending request is gone, and every halt is cleared. Jobs
//! posted while there is no configuration fail the same way. Event jobs wait on.
//!
//! When the consumer closes the stream, everything waiting is dropped and a pending request
//! is stalled. The next consumer starts with nothing queued but the state.
//!
//! Units: lengths in bytes, an endpoint by its address in the device's descriptors (bit 7 set
//! for an IN endpoint), times in milliseconds.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// Longest stream name [`UsbFunction::data_stream`] answers, in bytes.
pub const MAX_STREAM_NAME: usize = 32;

/// Where a job's bytes start in its buffer. The bytes before are the completion's record.
pub const DATA_OFFSET: usize = 12;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $value:literal),* $(,)? }) => {
        impl Encode for $name {
            const SHAPE: lemnos_core::Shape = lemnos_core::Shape::U8;
            fn encode(&self, out: &mut lemnos_core::Writer<'_>) -> Result<()> {
                (*self as u8).encode(out)
            }
        }

        impl Decode<'_> for $name {
            fn decode(from: &mut lemnos_core::Reader<'_>) -> Result<Self> {
                Self::from_wire(u8::decode(from)?).ok_or(lemnos_core::Status::INVALID_ARGUMENT)
            }
        }

        impl $name {
            /// The value `byte` stands for, if any.
            #[must_use]
            pub const fn from_wire(byte: u8) -> Option<Self> {
                match byte {
                    $($value => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

/// What a job asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum JobKind {
    /// Bytes to the host on an IN endpoint.
    #[default]
    Send = 1,
    /// Bytes from the host on an OUT endpoint.
    Receive = 2,
    /// The next event.
    Event = 3,
}

wire_enum!(JobKind { Send = 1, Receive = 2, Event = 3 });

/// One job, posted on the data stream.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Job {
    /// The caller's name for the job. Carried to the completion, never read.
    pub identity: u64,
    pub kind: JobKind,
    /// The endpoint's address in the descriptors. Not read for an event job.
    pub endpoint: u8,
    /// A buffer on the data stream.
    pub buffer: u32,
    /// Bytes to send, or the most to receive. Not read for an event job.
    pub length: u32,
}

/// Encoded size of a [`Job`], in bytes. The job queue must hold jobs this large.
pub const JOB_BYTES: usize = 4 + 8 + 1 + 1 + 4 + 4;

/// How a job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum JobStatus {
    /// The job was done, and the completion's count says how much.
    #[default]
    Done = 0,
    /// The job was refused and nothing moved: an endpoint the descriptors don't name or one
    /// of the other direction, a `length` over [`FunctionInfo::max_transfer`] or past the
    /// buffer, a receive of nothing, or a buffer too small for an event.
    Refused = 1,
    /// The host has no configuration in force, or it went while the job waited.
    NotConfigured = 2,
    /// [`UsbFunction::cancel`] took the job back.
    Cancelled = 3,
    /// The controller failed the transfer.
    Failed = 4,
}

wire_enum!(JobStatus { Done = 0, Refused = 1, NotConfigured = 2, Cancelled = 3, Failed = 4 });

/// The completion's record, at the start of the job's buffer.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Completion {
    pub status: JobStatus,
    /// Bytes sent, received, or of the event written. 0 unless the job was done.
    pub count: u32,
}

/// Encoded size of a [`Completion`], in bytes. It fits in [`DATA_OFFSET`].
pub const COMPLETION_BYTES: usize = 4 + 1 + 4;

/// What happened on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum EventKind {
    /// The host chose the configuration. Endpoints carry data from now on, none halted.
    #[default]
    Configured = 1,
    /// The configuration is no longer in force.
    Deconfigured = 2,
    /// The bus went idle. Nothing moves until [`Resumed`](Self::Resumed).
    Suspended = 3,
    Resumed = 4,
    /// A control request for the caller to answer.
    Setup = 5,
}

wire_enum!(EventKind {
    Configured = 1,
    Deconfigured = 2,
    Suspended = 3,
    Resumed = 4,
    Setup = 5,
});

/// One event, written to an event job's buffer.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Event {
    pub kind: EventKind,
    /// The request's fields, for [`EventKind::Setup`]. All 0 for any other event.
    pub request: Request,
}

/// The setup packet of a control request.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Request {
    /// `bmRequestType`. Bit 7 set: the data stage goes to the host.
    pub request_type: u8,
    /// `bRequest`.
    pub request: u8,
    /// `wValue`.
    pub value: u16,
    /// `wIndex`.
    pub index: u16,
    /// `wLength`: the most bytes the data stage carries, and 0 when there is none.
    pub length: u16,
}

/// Encoded size of an [`Event`], in bytes. An event job's buffer holds this after
/// [`DATA_OFFSET`].
pub const EVENT_BYTES: usize = 4 + 1 + (4 + 1 + 1 + 2 + 2 + 2);

/// What the block takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FunctionInfo {
    /// How many endpoints [`UsbFunction::endpoint`] can return.
    pub endpoints: u8,
    /// The most bytes one send or receive job carries. A buffer holds [`DATA_OFFSET`] more.
    pub max_transfer: u32,
    /// Jobs waiting at once.
    pub max_pending: u16,
    /// Events kept for an event job to take.
    pub max_events: u16,
    /// How long a control request waits for its answer before it is stalled.
    pub control_timeout_ms: u32,
}

/// One endpoint of the device, as its descriptors have it at full speed.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EndpointInfo {
    /// The address jobs name it by. Bit 7 set: an IN endpoint.
    pub address: u8,
    /// `bmAttributes`: the low two bits are 1 for isochronous, 2 for bulk, 3 for interrupt.
    pub attributes: u8,
    /// `wMaxPacketSize` at full speed. The host may run the device faster, with larger packets.
    pub max_packet: u16,
}

/// How the device stands on the bus.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// The device is on the bus, or waiting for a host to find it.
    pub up: bool,
    /// The host has chosen the configuration: endpoints carry data.
    pub configured: bool,
    /// The bus is idle.
    pub suspended: bool,
    /// A control request is waiting for its answer.
    pub request_pending: bool,
}

/// Counted since the driver started.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Jobs completed with a count.
    pub jobs: u64,
    /// Jobs completed as failed, and consumers faulted for a job that could not be decoded or
    /// named a buffer the block doesn't hold.
    pub errors: u64,
    /// Events dropped because nobody took them.
    pub events_dropped: u64,
    /// Control requests stalled because nobody answered them in time, or nobody was there.
    pub requests_unanswered: u64,
}

#[contract("usb.function", version = 1)]
pub trait UsbFunction {
    #[method(1)]
    fn info(&mut self) -> Result<FunctionInfo>;
    /// An endpoint of the device. `NOT_FOUND` past the last.
    #[method(2)]
    fn endpoint(&mut self, index: u8) -> Result<EndpointInfo>;
    /// The name to open the data stream by, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_STREAM_NAME`] bytes. `NOT_SUPPORTED` when the board gives the
    /// device no data stream: it is on the bus, and nobody can serve it.
    #[method(3)]
    fn data_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    #[method(4)]
    fn state(&mut self) -> Result<State>;
    /// Answers the pending control request, one whose data goes to the host, with `data` as
    /// its data stage. At most the request's `length` bytes are sent, and fewer end the stage
    /// short. Returns how many were sent.
    ///
    /// `INVALID_ARGUMENT` when no request is pending or the pending one goes the other way
    /// (it stays pending). `NO_DEVICE` when the host abandoned the request or the device is
    /// not on the bus.
    #[method(5)]
    fn control_in(&mut self, data: &[u8]) -> Result<u32>;
    /// Takes the data stage of the pending control request, one whose data comes from the
    /// host, into `into`, and so accepts the request. `into` must hold the request's `length`
    /// bytes; a request with a `length` of 0 is accepted with nothing taken. Returns how many
    /// bytes came. Errors are as for [`control_in`](Self::control_in).
    #[method(6)]
    fn control_out(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Refuses the pending control request with a stall. `INVALID_ARGUMENT` when none is
    /// pending.
    #[method(7)]
    fn control_stall(&mut self) -> Result<()>;
    /// Halts `endpoint` (it answers the host with a stall until the halt is cleared) or
    /// clears its halt. The host can clear a halt too, and nobody is told.
    ///
    /// `INVALID_ARGUMENT` for an endpoint the descriptors don't name, `NOT_SUPPORTED` for an
    /// isochronous one, `NO_DEVICE` while there is no configuration.
    #[method(8)]
    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<()>;
    /// Takes back every job waiting on `endpoint`: each completes with
    /// [`JobStatus::Cancelled`]. An `endpoint` of 0 takes back the event jobs. Returns how many
    /// there were. Bytes a send had already handed to the controller still go out.
    #[method(9)]
    fn cancel(&mut self, endpoint: u8) -> Result<u32>;
    #[method(10)]
    fn counters(&mut self) -> Result<Counters>;
}
