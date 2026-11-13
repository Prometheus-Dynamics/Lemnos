//! The data stream of `usb.function/1`: jobs taken from the stream, checked, carried to the
//! function's endpoints and completed on the same stream; events kept for the jobs that ask
//! for them.
//!
//! # Buffers
//!
//! The driver holds every buffer of the stream the consumer has not been handed, so a job can
//! name any of them. A job's buffer is published as its completion and is the driver's again
//! once the consumer returns it. Buffers numbered [`MAX_BUFFERS`] and up are never used.
//!
//! A stream's memory is made anew for each consumer. A buffer held from a consumer that has
//! gone has nothing behind it, and the stream says so
//! ([`StreamEndpoint::buffer_current`]): every tick asks it of one held buffer before it
//! takes more, so a consumer that went (whether or not another came) is seen by the next
//! tick. Everything held is then given up, published while
//! it reaches no one, and every waiting job and queued event is dropped with it.
//!
//! # What is checked, and where
//!
//! Everything in a job is the consumer's, and none of it is trusted. A job's bytes never
//! become an address: they name a buffer by number, an endpoint by address and a length.
//!
//! 1. The job must decode, to no more than the contract's size.
//! 2. Its buffer must be one the driver holds, and not one another waiting job has.
//! 3. A send's or receive's endpoint must be one the descriptors name, of the job's direction.
//! 4. Its `length` must be at most [`Serve::max_transfer`] (the driver's own memory for one
//!    transfer), and a receive's at least 1.
//! 5. `DATA_OFFSET + length` (an event's size for an event job) must lie inside the buffer:
//!    the last byte of that range is read from the stream, which refuses a range outside the
//!    buffer. So a receive or an event that has come always has room to land.
//! 6. Every copy to or from a buffer goes through the stream's `read` and `write`, which
//!    check the buffer is held and the range is inside it once more, and every copy to or
//!    from the driver's memory is a slice of it no longer than the checked `length`.
//!
//! A job that fails 1 or 2 has no buffer to complete on, and is a broken consumer's: it is
//! counted, the consumer's stream is faulted ([`StreamEndpoint::fault_consumer`]) and
//! everything of its time is dropped. One that fails 3 to 5 completes as refused.

use lemnos_contract_usb_function::{
    COMPLETION_BYTES, Completion, Counters, DATA_OFFSET, EVENT_BYTES, EndpointInfo, Event,
    EventKind, JOB_BYTES, Job, JobKind, JobStatus,
};
use lemnos_core::{Decode, Encode, Reader, Writer};
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::{ErrorKind, StreamEndpoint, UsbFunction};

use core::time::Duration;

use crate::description::MAX_ENDPOINTS;

/// Buffers of the stream this driver can hold. A job naming one past these is a broken
/// consumer's.
pub const MAX_BUFFERS: u32 = 64;
/// Jobs waiting at once. More stay on the stream's job queue.
pub const MAX_PENDING: usize = 16;
/// Events kept for an event job to take.
pub const MAX_EVENTS: usize = 16;
/// Jobs taken from the stream in one tick; the rest wait for the next.
pub const MAX_JOBS_PER_TICK: usize = 16;

/// The most bytes one send or receive carries: the driver's own memory for a transfer. With an
/// allocator it is taken once, at bind, when the board gives the device a data stream.
#[cfg(feature = "alloc")]
pub const TRANSFER_BYTES: usize = 64 * 1024;
/// The most bytes one send or receive carries: the driver's own memory for a transfer, inside
/// the driver where there is no allocator.
#[cfg(not(feature = "alloc"))]
pub const TRANSFER_BYTES: usize = 1024;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a job is taken off the queue and faults its consumer, not left at its head.
const LONGEST_JOB: usize = 4096;

/// The driver's memory for one transfer.
#[cfg(feature = "alloc")]
struct Scratch(alloc::boxed::Box<[u8]>);

#[cfg(not(feature = "alloc"))]
struct Scratch([u8; TRANSFER_BYTES]);

impl Scratch {
    /// Memory for a transfer when `wanted`, none otherwise: without a data stream nothing is
    /// ever carried.
    #[cfg(feature = "alloc")]
    fn new(wanted: bool) -> Self {
        let bytes = if wanted { TRANSFER_BYTES } else { 0 };
        Self(alloc::vec![0; bytes].into_boxed_slice())
    }

    #[cfg(not(feature = "alloc"))]
    fn new(_wanted: bool) -> Self {
        Self([0; TRANSFER_BYTES])
    }
}

/// A job waiting for its endpoint, or for an event.
#[derive(Debug, Clone, Copy)]
struct Waiting {
    job: Job,
    /// Tells this job from every other the driver has taken.
    ticket: u32,
    /// Bytes of a send the controller has taken so far.
    done: u32,
}

/// What became of one try at a waiting job.
enum Step {
    /// It waits on.
    Wait,
    /// It is over, with this record.
    Over(Completion),
    /// The consumer went: nothing can be completed.
    Gone,
}

/// The bit that stands for an endpoint address: 0 to 15 for OUT, 16 to 31 for IN.
const fn endpoint_bit(address: u8) -> u32 {
    1 << ((address & 0x0f) as u32 + (address >> 7) as u32 * 16)
}

/// The data stream's state.
pub(crate) struct Serve {
    scratch: Scratch,
    /// The ticket of the send whose bytes the scratch holds from `done` on, so a send that
    /// waits is read from its buffer once.
    loaded: Option<u32>,
    /// The buffers this driver holds, one bit each.
    held: u64,
    /// Whether a consumer was attached when last looked.
    attached: bool,
    sequence: u64,
    /// The waiting jobs, oldest first, in `waiting[..len]`.
    waiting: [Option<Waiting>; MAX_PENDING],
    len: usize,
    ticket: u32,
    /// The events nobody has taken, oldest at `head`.
    events: [Event; MAX_EVENTS],
    head: usize,
    count: usize,
    /// How many consumers have gone while their buffers were held. A change says whoever was
    /// being served is no longer the one there.
    pub(crate) gone: u32,
    /// When the control request waiting for its answer is stalled, if one waits.
    pub(crate) request_deadline_ns: Option<u64>,
    pub(crate) counters: Counters,
    endpoints: [EndpointInfo; MAX_ENDPOINTS],
    endpoint_count: usize,
}

impl Serve {
    /// The state for a device with `endpoints`, with memory for transfers when it has a data
    /// stream.
    pub(crate) fn new(endpoints: impl Iterator<Item = EndpointInfo>, stream: bool) -> Self {
        let mut list = [EndpointInfo::default(); MAX_ENDPOINTS];
        let mut endpoint_count = 0;
        for (slot, endpoint) in list.iter_mut().zip(endpoints) {
            *slot = endpoint;
            endpoint_count += 1;
        }
        Self {
            scratch: Scratch::new(stream),
            loaded: None,
            held: 0,
            attached: false,
            sequence: 0,
            waiting: [None; MAX_PENDING],
            len: 0,
            ticket: 0,
            events: [Event::default(); MAX_EVENTS],
            head: 0,
            count: 0,
            gone: 0,
            request_deadline_ns: None,
            counters: Counters::default(),
            endpoints: list,
            endpoint_count,
        }
    }

    /// The endpoints the descriptors name.
    pub(crate) fn endpoints(&self) -> &[EndpointInfo] {
        &self.endpoints[..self.endpoint_count]
    }

    /// The most bytes one send or receive carries.
    pub(crate) fn max_transfer(&self) -> usize {
        self.scratch.0.len()
    }

    fn holds(&self, buffer: u32) -> bool {
        buffer < MAX_BUFFERS && self.held & (1 << buffer) != 0
    }

    fn set_held(&mut self, buffer: u32, held: bool) {
        if buffer < MAX_BUFFERS {
            if held {
                self.held |= 1 << buffer;
            } else {
                self.held &= !(1 << buffer);
            }
        }
    }

    /// Whether a waiting job has `buffer`.
    fn named(&self, buffer: u32) -> bool {
        self.waiting[..self.len]
            .iter()
            .flatten()
            .any(|w| w.job.buffer == buffer)
    }

    /// Takes every buffer the stream has free or returned, at most one round of the stream.
    fn take_free<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        stream: &mut S,
    ) -> Result<(), ErrorKind> {
        for _ in 0..stream.buffer_count() {
            let Some(buffer) = stream.next_buffer()? else {
                break;
            };
            self.set_held(buffer, true);
        }
        Ok(())
    }

    /// Gives up everything held, published while it reaches no one, and drops every waiting
    /// job and queued event: they were a consumer's that has gone.
    fn forget<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        stream: &mut S,
    ) -> Result<(), ErrorKind> {
        self.waiting = [None; MAX_PENDING];
        self.len = 0;
        self.count = 0;
        self.loaded = None;
        self.attached = false;
        self.gone = self.gone.wrapping_add(1);
        while self.held != 0 {
            let buffer = self.held.trailing_zeros();
            stream.publish(StreamFrame {
                buffer,
                bytes: 0,
                sequence: 0,
                identity: 0,
                timestamp_ns: 0,
            })?;
            self.set_held(buffer, false);
        }
        Ok(())
    }

    /// Holds the stream's free buffers when a consumer is attached, and nothing when none is.
    /// Whether one is. When the consumer the driver was serving has gone, everything of its
    /// time is dropped first.
    pub(crate) fn refresh<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        stream: &mut S,
    ) -> Result<bool, ErrorKind> {
        // One held buffer says whether the consumer it was taken under is still the one
        // there. It is asked before any buffer is taken, so everything held is of one
        // consumer's time.
        if self.held != 0 && !stream.buffer_current(self.held.trailing_zeros())? {
            self.forget(stream)?;
        }
        if !self.attached {
            // A buffer taken now is the present consumer's, if there is one.
            let Some(buffer) = stream.next_buffer()? else {
                return Ok(false);
            };
            self.set_held(buffer, true);
            if !stream.buffer_current(buffer)? {
                self.forget(stream)?;
                return Ok(false);
            }
            self.attached = true;
        }
        self.take_free(stream)?;
        Ok(true)
    }

    /// Queues `event` for an event job, dropping the oldest when the queue is full. Nothing is
    /// queued while nobody is attached: the next consumer asks for the state.
    pub(crate) fn note(&mut self, event: Event) {
        if !self.attached {
            return;
        }
        if self.count == MAX_EVENTS {
            self.head = (self.head + 1) % MAX_EVENTS;
            self.count -= 1;
            self.counters.events_dropped += 1;
        }
        self.events[(self.head + self.count) % MAX_EVENTS] = event;
        self.count += 1;
    }

    /// Publishes `job`'s buffer as its completion, with `record` at its start.
    fn complete<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &Job,
        record: Completion,
        now_ns: u64,
    ) -> Result<(), ErrorKind> {
        let mut bytes = [0u8; COMPLETION_BYTES];
        let mut writer = Writer::new(&mut bytes);
        let encoded = record.encode(&mut writer).is_ok();
        let extra = match (record.status, job.kind) {
            (JobStatus::Done, JobKind::Receive | JobKind::Event) => record.count,
            _ => 0,
        };
        // A buffer too small for the record completes empty.
        let recorded = encoded && matches!(stream.write(job.buffer, 0, &bytes), Ok(true));
        let length = if recorded {
            DATA_OFFSET as u32 + extra
        } else {
            0
        };
        stream.publish(StreamFrame {
            buffer: job.buffer,
            bytes: length,
            sequence: self.sequence,
            identity: job.identity,
            timestamp_ns: now_ns,
        })?;
        self.set_held(job.buffer, false);
        self.sequence = self.sequence.wrapping_add(1);
        if record.status == JobStatus::Done {
            self.counters.jobs += 1;
        } else {
            self.counters.errors += 1;
        }
        Ok(())
    }

    /// Whether `DATA_OFFSET + length` lies inside `buffer`: its last byte can be read.
    /// `None` when the consumer has gone.
    fn fits<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        stream: &mut S,
        buffer: u32,
        length: usize,
    ) -> Option<bool> {
        let Some(last) = (DATA_OFFSET + length).checked_sub(1) else {
            return Some(false);
        };
        match stream.read(buffer, last, &mut [0u8; 1]) {
            Ok(true) => Some(true),
            Ok(false) => None,
            Err(_) => Some(false),
        }
    }

    /// Whether the descriptors name `address`, an endpoint that sends to the host when
    /// `to_host`.
    fn endpoint(&self, address: u8, to_host: bool) -> bool {
        (address & 0x80 != 0) == to_host && self.endpoints().iter().any(|e| e.address == address)
    }

    /// Takes jobs off the stream while there is room for them to wait: each is checked and
    /// then waits, or completes as refused, or is dropped.
    fn intake<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        stream: &mut S,
        now_ns: u64,
    ) -> Result<(), ErrorKind> {
        let mut encoded = [0u8; LONGEST_JOB];
        for _ in 0..MAX_JOBS_PER_TICK {
            if self.len == MAX_PENDING {
                break;
            }
            let Some(len) = stream.next_job(&mut encoded)? else {
                break;
            };
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look.
            self.take_free(stream)?;
            let job = encoded
                .get(..len)
                .filter(|bytes| bytes.len() <= JOB_BYTES)
                .and_then(|bytes| Job::decode(&mut Reader::new(bytes)).ok());
            // Bytes that are no job, or a job with no buffer to complete on: a broken consumer.
            // It loses the stream, and what was held and waiting under it goes with it.
            let job = job.filter(|job| self.holds(job.buffer) && !self.named(job.buffer));
            let Some(job) = job else {
                self.counters.errors += 1;
                stream.fault_consumer()?;
                return self.forget(stream);
            };
            let length = job.length as usize;
            let (known, room) = match job.kind {
                JobKind::Send => (
                    self.endpoint(job.endpoint, true) && length <= self.max_transfer(),
                    length,
                ),
                JobKind::Receive => (
                    self.endpoint(job.endpoint, false)
                        && (1..=self.max_transfer()).contains(&length),
                    length,
                ),
                JobKind::Event => (true, EVENT_BYTES),
            };
            let fits = if known {
                Self::fits(stream, job.buffer, room)
            } else {
                Some(false)
            };
            match fits {
                // The consumer went: the next tick drops what is left of it.
                None => return Ok(()),
                Some(false) => {
                    let refused = Completion {
                        status: JobStatus::Refused,
                        count: 0,
                    };
                    self.complete(stream, &job, refused, now_ns)?;
                }
                Some(true) => {
                    self.ticket = self.ticket.wrapping_add(1);
                    self.waiting[self.len] = Some(Waiting {
                        job,
                        ticket: self.ticket,
                        done: 0,
                    });
                    self.len += 1;
                }
            }
        }
        Ok(())
    }

    /// Takes the waiting job at `at` out of the line.
    fn remove(&mut self, at: usize) {
        self.waiting.copy_within(at + 1..self.len, at);
        self.len -= 1;
        self.waiting[self.len] = None;
    }

    /// Completes every waiting job `which` picks with `status`. Returns how many there were.
    pub(crate) fn end<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        stream: &mut S,
        status: JobStatus,
        now_ns: u64,
        which: impl Fn(&Job) -> bool,
    ) -> Result<u32, ErrorKind> {
        let mut ended = 0;
        let mut at = 0;
        while at < self.len {
            match self.waiting[at] {
                Some(waiting) if which(&waiting.job) => {
                    self.remove(at);
                    if self.loaded == Some(waiting.ticket) {
                        self.loaded = None;
                    }
                    self.complete(
                        stream,
                        &waiting.job,
                        Completion { status, count: 0 },
                        now_ns,
                    )?;
                    ended += 1;
                }
                _ => at += 1,
            }
        }
        Ok(ended)
    }

    /// One try at the send `waiting`.
    fn send<S, F>(&mut self, stream: &mut S, function: &mut F, waiting: &mut Waiting) -> Step
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
        F: UsbFunction<Error = ErrorKind> + ?Sized,
    {
        let job = waiting.job;
        // A send of no bytes is done without a transfer. A zero-length write on a FunctionFS
        // endpoint is a zero-length packet on the wire, and the byte stream on the lent endpoints
        // gives packet boundaries and short packets no meaning, so this driver never emits one.
        if job.length == 0 {
            return Step::Over(Completion {
                status: JobStatus::Done,
                count: 0,
            });
        }
        let done = waiting.done as usize;
        // Checked at intake to be within the scratch.
        let Some(rest) = (job.length as usize)
            .checked_sub(done)
            .and_then(|rest| self.scratch.0.get_mut(..rest))
        else {
            return Step::Over(Completion {
                status: JobStatus::Refused,
                count: 0,
            });
        };
        if self.loaded != Some(waiting.ticket) {
            self.loaded = None;
            match stream.read(job.buffer, DATA_OFFSET + done, rest) {
                Ok(true) => self.loaded = Some(waiting.ticket),
                Ok(false) => return Step::Gone,
                Err(_) => {
                    return Step::Over(Completion {
                        status: JobStatus::Refused,
                        count: 0,
                    });
                }
            }
        }
        match function.write(job.endpoint, rest, Duration::ZERO) {
            Ok(taken) => {
                // The scratch holds the bytes from `done`: after a part is taken it no longer
                // matches, and the rest is read again.
                waiting.done += taken.min(rest.len()) as u32;
                if waiting.done == job.length {
                    self.loaded = None;
                    return Step::Over(Completion {
                        status: JobStatus::Done,
                        count: job.length,
                    });
                }
                if taken != 0 {
                    self.loaded = None;
                }
                Step::Wait
            }
            Err(ErrorKind::Timeout) => Step::Wait,
            Err(kind) => {
                self.loaded = None;
                Step::Over(Completion {
                    status: transfer_status(kind),
                    count: 0,
                })
            }
        }
    }

    /// One try at the receive `waiting`.
    fn receive<S, F>(&mut self, stream: &mut S, function: &mut F, waiting: &Waiting) -> Step
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
        F: UsbFunction<Error = ErrorKind> + ?Sized,
    {
        let job = waiting.job;
        // Checked at intake to be within the scratch.
        let Some(into) = self.scratch.0.get_mut(..job.length as usize) else {
            return Step::Over(Completion {
                status: JobStatus::Refused,
                count: 0,
            });
        };
        self.loaded = None;
        let came = match function.read(job.endpoint, into, Duration::ZERO) {
            Ok(came) => came.min(into.len()),
            Err(ErrorKind::Timeout) => return Step::Wait,
            Err(kind) => {
                return Step::Over(Completion {
                    status: transfer_status(kind),
                    count: 0,
                });
            }
        };
        // The room was checked at intake, so this lands unless the consumer went.
        match stream.write(job.buffer, DATA_OFFSET, &into[..came]) {
            Ok(true) => Step::Over(Completion {
                status: JobStatus::Done,
                count: came as u32,
            }),
            Ok(false) => Step::Gone,
            Err(_) => Step::Over(Completion {
                status: JobStatus::Failed,
                count: 0,
            }),
        }
    }

    /// One try at the event job `waiting`: the oldest queued event, if there is one.
    fn event<S>(&mut self, stream: &mut S, waiting: &Waiting) -> Step
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
    {
        if self.count == 0 {
            return Step::Wait;
        }
        let mut bytes = [0u8; EVENT_BYTES];
        let mut writer = Writer::new(&mut bytes);
        if self.events[self.head].encode(&mut writer).is_err() {
            return Step::Over(Completion {
                status: JobStatus::Failed,
                count: 0,
            });
        }
        let written = writer.len();
        match stream.write(waiting.job.buffer, DATA_OFFSET, &bytes[..written]) {
            Ok(true) => {
                self.head = (self.head + 1) % MAX_EVENTS;
                self.count -= 1;
                Step::Over(Completion {
                    status: JobStatus::Done,
                    count: written as u32,
                })
            }
            Ok(false) => Step::Gone,
            // The room was checked at intake.
            Err(_) => Step::Over(Completion {
                status: JobStatus::Failed,
                count: 0,
            }),
        }
    }

    /// Takes the stream's new jobs and tries every waiting job once, in the order they came,
    /// the first for each endpoint only. Whether a send or receive moved and more wait, so
    /// that looking again at once is worth it. Never waits.
    pub(crate) fn run<S, F>(
        &mut self,
        stream: &mut S,
        function: &mut F,
        now_ns: u64,
    ) -> Result<bool, ErrorKind>
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
        F: UsbFunction<Error = ErrorKind> + ?Sized,
    {
        self.intake(stream, now_ns)?;
        // Endpoints whose first job waits on: the jobs behind it keep their place.
        let mut blocked = 0u32;
        let mut moved = false;
        let mut at = 0;
        while at < self.len {
            let Some(mut waiting) = self.waiting[at] else {
                break;
            };
            // Endpoint 0 is no job's, so its bit stands for the event jobs.
            let bit = match waiting.job.kind {
                JobKind::Event => endpoint_bit(0),
                JobKind::Send | JobKind::Receive => endpoint_bit(waiting.job.endpoint),
            };
            if blocked & bit != 0 {
                at += 1;
                continue;
            }
            let step = match waiting.job.kind {
                JobKind::Send => self.send(stream, function, &mut waiting),
                JobKind::Receive => self.receive(stream, function, &waiting),
                JobKind::Event => self.event(stream, &waiting),
            };
            match step {
                Step::Wait => {
                    moved |= self.waiting[at].is_some_and(|w| w.done != waiting.done);
                    self.waiting[at] = Some(waiting);
                    blocked |= bit;
                    at += 1;
                }
                Step::Over(record) => {
                    self.remove(at);
                    self.complete(stream, &waiting.job, record, now_ns)?;
                    moved |= waiting.job.kind != JobKind::Event;
                }
                // The next tick's `refresh` drops what is left of the consumer.
                Step::Gone => return Ok(false),
            }
        }
        let more = self.waiting[..self.len]
            .iter()
            .flatten()
            .any(|w| w.job.kind != JobKind::Event);
        Ok(moved && more)
    }

    /// What the next event job would be given, for tests.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.count
    }

    /// How many jobs wait, for tests.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.len
    }
}

/// How a transfer the function failed ends its job.
fn transfer_status(kind: ErrorKind) -> JobStatus {
    match kind {
        ErrorKind::Unavailable => JobStatus::NotConfigured,
        ErrorKind::InvalidInput => JobStatus::Refused,
        _ => JobStatus::Failed,
    }
}

/// An event with no request.
pub(crate) fn plain(kind: EventKind) -> Event {
    Event {
        kind,
        request: lemnos_contract_usb_function::Request::default(),
    }
}
