//! The job streams: jobs taken from a stream, checked, run on the route `caps` chooses for them,
//! and completed on the same stream.
//!
//! # Buffers
//!
//! The driver holds every buffer of a stream the consumer has not taken or been handed, so a job
//! can name any of them; the consumer fills an input while the driver holds it. A job's output is
//! published as its completion and is the driver's again once the consumer returns it. Nothing is
//! published while a unit works on it: every route has finished with a buffer when its job
//! completes. Buffers numbered [`MAX_BUFFERS`] and up are never used.
//!
//! A stream's memory is made anew for each consumer, so buffers held from before a consumer
//! came have nothing of it behind them. They are given up (published while nobody can receive
//! them) and taken again:
//!
//! - on a stream with handles, when a job names one whose handle is gone (as the dma-buf
//!   drivers do);
//! - on any stream, when a tick finds no consumer attached: from then on the driver holds
//!   nothing until one is, and takes its buffers then.
//!
//! # What is checked, and where
//!
//! Everything in a job is the consumer's, and none of it is trusted.
//!
//! 1. The job alone ([`Jobs`]' plans): formats known, sizes in the limits, strides that hold a
//!    row, distinct buffers, and the sides its contract says match.
//! 2. Every buffer it names must be one the driver holds.
//! 3. Each image's extent (stride times rows, every plane, overflow-free) must fit its stream
//!    buffer: its last byte is read from the stream, which refuses a range outside the buffer.
//!    This comes before any memory is taken for the job.
//! 4. Then per route: the CPU reads each input once into the driver's own memory ([`Frames`])
//!    and works on that copy, never on the shared buffer; the hardware routes check each extent
//!    again against the real size of the dma-buf behind the handle, asked of the kernel, and the
//!    route's own request builders check it once more.
//!
//! # Failures
//!
//! A unit that fails a job is closed, its job runs on the CPU, and the unit is tried again after a
//! wait doubling from 250 ms and capped at 8 s, cleared by a job it completes. The driver never
//! gives up on it and never turns itself off. A job the unit's route can't take (a format or size
//! its device refuses) runs on the CPU without counting against the unit.

use lemnos_contract_image_blend::{self as blend, BlendJob};
use lemnos_contract_image_convert::{self as convert, ConvertJob};
use lemnos_contract_image_scaler::{self as scaler, ScaleJob};
use lemnos_core::{Decode, ErrorKind, Reader};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;

use crate::jobs::{Contract, Frames, Jobs, Plan, Side, sized};

/// Buffers of a stream this driver can hold. A job naming one past these is refused.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken from one stream in one tick; the rest wait for the next, which comes at once.
pub const MAX_JOBS_PER_ROUND: u32 = 8;

/// How often the streams are looked at for jobs.
pub const POLL_NS: u64 = 1_000_000;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a job is taken off the queue and counted, not left at its head.
const LONGEST_JOB: usize = 4096;

/// First wait before a failed unit is tried again.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The wait before retry `attempt` (from 0) of a failed unit: 250 ms doubling, capped at 8 s.
#[must_use]
pub const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// Why the last failed job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job.
    Undecodable,
    /// The job itself was refused: a format, size, stride or pairing its contract doesn't take,
    /// or the same buffer twice.
    Refused,
    /// A buffer the driver doesn't hold.
    Buffers,
    /// An image that doesn't fit its buffer.
    Size,
    /// The consumer went while the job ran.
    NoConsumer,
    /// The stream refused the completion.
    Completion,
}

/// One stream's state.
#[derive(Debug, Default)]
pub(crate) struct Lane {
    /// The buffers this driver holds, one bit each.
    held: u64,
    sequence: u64,
    /// Whether a consumer was attached when last looked.
    attached: bool,
}

impl Lane {
    pub(crate) fn holds(&self, buffer: u32) -> bool {
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

    fn any_held(&self) -> Option<u32> {
        (self.held != 0).then(|| self.held.trailing_zeros())
    }
}

/// A job as it came off a stream.
#[derive(Clone, Copy)]
enum Job {
    Scale(ScaleJob),
    Convert(ConvertJob),
    Blend(BlendJob),
}

impl Job {
    fn decode(contract: Contract, bytes: &[u8]) -> Option<Self> {
        let longest = match contract {
            Contract::Scale => scaler::JOB_BYTES,
            Contract::Convert => convert::JOB_BYTES,
            Contract::Blend => blend::JOB_BYTES,
        };
        if bytes.len() > longest {
            return None;
        }
        let mut reader = Reader::new(bytes);
        Some(match contract {
            Contract::Scale => Self::Scale(ScaleJob::decode(&mut reader).ok()?),
            Contract::Convert => Self::Convert(ConvertJob::decode(&mut reader).ok()?),
            Contract::Blend => Self::Blend(BlendJob::decode(&mut reader).ok()?),
        })
    }

    fn identity(&self) -> u64 {
        match self {
            Self::Scale(j) => j.identity,
            Self::Convert(j) => j.identity,
            Self::Blend(j) => j.identity,
        }
    }

    fn output(&self) -> u32 {
        match self {
            Self::Scale(j) => j.output.buffer,
            Self::Convert(j) => j.output.buffer,
            Self::Blend(j) => j.output.buffer,
        }
    }

    /// Every buffer it names.
    fn buffers(&self) -> [u32; 3] {
        match self {
            Self::Scale(j) => [j.input.buffer, j.output.buffer, j.output.buffer],
            Self::Convert(j) => [j.input.buffer, j.output.buffer, j.output.buffer],
            Self::Blend(j) => [j.foreground.buffer, j.background.buffer, j.output.buffer],
        }
    }

    fn plan(&self, jobs: &Jobs) -> Option<Plan> {
        match self {
            Self::Scale(j) => jobs.plan_scale(j),
            Self::Convert(j) => jobs.plan_convert(j),
            Self::Blend(j) => jobs.plan_blend(j),
        }
    }
}

/// What a stream's reads and writes come to.
fn moved(result: Result<bool, ErrorKind>) -> Result<(), Failure> {
    match result {
        Ok(true) => Ok(()),
        Ok(false) => Err(Failure::NoConsumer),
        Err(_) => Err(Failure::Size),
    }
}

/// Copies `buffer`'s first `into.len()` bytes into `into`, once.
fn read_into<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
    stream: &mut S,
    buffer: u32,
    into: &mut [u8],
) -> Result<(), Failure> {
    moved(stream.read(buffer, 0, into))
}

/// Whether `side`'s extent fits its stream buffer: its last byte can be read.
fn fits_stream<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
    stream: &mut S,
    side: Side,
) -> Result<(), Failure> {
    let len = side.layout.frame_len();
    if len == 0 || Jobs::written(side.layout).is_none() {
        return Err(Failure::Size);
    }
    moved(stream.read(side.buffer, len - 1, &mut [0u8; 1]))
}

/// The jobs, the driver's memory for them, and the routes that run them.
pub(crate) struct Server {
    pub(crate) jobs: Jobs,
    frames: Frames,
    routes: Routes,
    last_failure: Option<Failure>,
}

impl Server {
    pub(crate) fn new(jobs: Jobs, routes: Routes) -> Self {
        Self {
            jobs,
            frames: Frames::default(),
            routes,
            last_failure: None,
        }
    }

    pub(crate) fn last_failure(&self) -> Option<Failure> {
        self.last_failure
    }

    pub(crate) fn routes(&self) -> &Routes {
        &self.routes
    }

    /// Takes every buffer the stream has free or returned, at most one round of the stream.
    fn take_free<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        stream: &mut S,
        lane: &mut Lane,
    ) -> Result<(), ErrorKind> {
        for _ in 0..stream.buffer_count() {
            let Some(buffer) = stream.next_buffer()? else {
                break;
            };
            lane.set_held(buffer, true);
        }
        Ok(())
    }

    /// Gives up `buffer`, empty. Called when nobody can receive it, so it reaches no one.
    fn give_up<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        stream: &mut S,
        lane: &mut Lane,
        buffer: u32,
    ) -> Result<(), ErrorKind> {
        stream.publish(StreamFrame {
            buffer,
            bytes: 0,
            sequence: 0,
            identity: 0,
            timestamp_ns: 0,
        })?;
        lane.set_held(buffer, false);
        Ok(())
    }

    /// Holds the stream's free buffers when a consumer is attached, and nothing when none is.
    /// Whether one is.
    fn refresh<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        stream: &mut S,
        lane: &mut Lane,
    ) -> Result<bool, ErrorKind> {
        // A read of nothing from a held buffer says whether anyone is attached.
        if !lane.attached {
            // A buffer taken now is the present consumer's, if there is one.
            let Some(buffer) = stream.next_buffer()? else {
                return Ok(false);
            };
            lane.set_held(buffer, true);
            if !stream.read(buffer, 0, &mut [])? {
                Self::give_up(stream, lane, buffer)?;
                return Ok(false);
            }
            lane.attached = true;
        }
        Self::take_free(stream, lane)?;
        if let Some(buffer) = lane.any_held()
            && !stream.read(buffer, 0, &mut [])?
        {
            // Gone: everything held is from its time.
            while let Some(buffer) = lane.any_held() {
                Self::give_up(stream, lane, buffer)?;
            }
            lane.attached = false;
            return Ok(false);
        }
        Ok(true)
    }

    /// Gives up a buffer that has nothing behind it, so it can be taken again with something.
    /// Only a stream with handles can say so of one buffer.
    fn renew<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        stream: &mut S,
        lane: &mut Lane,
        buffer: u32,
    ) -> Result<bool, ErrorKind> {
        if !lane.holds(buffer) || !matches!(stream.buffer_handle(buffer), Ok(None)) {
            return Ok(false);
        }
        Self::give_up(stream, lane, buffer)?;
        Ok(true)
    }

    /// Runs the jobs waiting on `stream`, a stream of `contract`'s jobs, at `now_ns` on the
    /// host's clock. Returns how many it read. An error is the stream's, and leaves the jobs not
    /// yet read where they are. Allocates nothing once its memory has held jobs of these sizes.
    pub(crate) fn serve<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        contract: Contract,
        stream: &mut S,
        lane: &mut Lane,
        now_ns: u64,
    ) -> Result<u32, ErrorKind> {
        if !Self::refresh(stream, lane)? {
            return Ok(0);
        }
        let mut encoded = [0u8; LONGEST_JOB];
        let mut read = 0;
        while read < MAX_JOBS_PER_ROUND {
            let Some(len) = stream.next_job(&mut encoded)? else {
                break;
            };
            read += 1;
            let job = encoded
                .get(..len)
                .and_then(|bytes| Job::decode(contract, bytes));
            let Some(job) = job else {
                self.jobs.count(contract, None);
                self.last_failure = Some(Failure::Undecodable);
                continue;
            };
            // Buffers held since before this consumer came have nothing behind them: they are
            // given up and taken again first, so that even a failed job completes to it.
            let mut renewed = false;
            for buffer in job.buffers() {
                renewed |= Self::renew(stream, lane, buffer)?;
            }
            if renewed {
                Self::take_free(stream, lane)?;
            }
            let outcome = match job.plan(&self.jobs) {
                Some(plan) => self.run(stream, lane, &plan, now_ns),
                None => Err(Failure::Refused),
            };
            self.complete(contract, stream, lane, &job, outcome, now_ns)?;
        }
        Ok(read)
    }

    /// Publishes the job's output as its completion: `bytes` long when it ran, empty when it
    /// failed. A job whose output isn't a buffer this driver holds has nothing to complete on.
    fn complete<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        contract: Contract,
        stream: &mut S,
        lane: &mut Lane,
        job: &Job,
        outcome: Result<u32, Failure>,
        now_ns: u64,
    ) -> Result<(), ErrorKind> {
        let buffer = job.output();
        let frame = StreamFrame {
            buffer,
            bytes: 0,
            sequence: lane.sequence,
            identity: job.identity(),
            timestamp_ns: now_ns,
        };
        let mut outcome = outcome;
        let mut published = Ok(());
        if lane.holds(buffer) {
            let delivered = match outcome {
                Ok(bytes) => stream.publish(StreamFrame { bytes, ..frame }).is_ok(),
                Err(_) => false,
            };
            if !delivered {
                // A completion the stream refused leaves the buffer held: it says failed instead.
                outcome = outcome.and(Err(Failure::Completion));
                published = stream.publish(frame);
            }
            if published.is_ok() {
                lane.set_held(buffer, false);
                lane.sequence = lane.sequence.wrapping_add(1);
            }
        }
        match outcome {
            Ok(bytes) => {
                self.jobs.count(contract, Some(bytes));
            }
            Err(failure) => {
                self.jobs.count(contract, None);
                self.last_failure = Some(failure);
            }
        }
        published
    }

    /// Runs one checked job. The bytes written to its output, or why it didn't run.
    fn run<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        stream: &mut S,
        lane: &Lane,
        plan: &Plan,
        now_ns: u64,
    ) -> Result<u32, Failure> {
        if !plan.sides().all(|side| lane.holds(side.buffer)) {
            return Err(Failure::Buffers);
        }
        // Before any memory is taken for the job: every image fits its buffer.
        for side in plan.sides() {
            fits_stream(stream, side)?;
        }
        let Self {
            jobs,
            frames,
            routes,
            ..
        } = self;
        if let Some(served) = routes.run(stream, plan, frames, now_ns) {
            return served;
        }
        // The CPU: each input read once into the driver's memory, and only that copy used.
        let input = sized(&mut frames.input, plan.input.layout.frame_len());
        read_into(stream, plan.input.buffer, input)?;
        let second: &[u8] = match plan.second {
            Some(side) => {
                let second = sized(&mut frames.second, side.layout.frame_len());
                read_into(stream, side.buffer, second)?;
                second
            }
            None => &[],
        };
        let output = sized(&mut frames.output, plan.output.layout.frame_len());
        // Bytes the work leaves alone (a row's padding) are written as zero, never as an
        // earlier job's.
        output.fill(0);
        let written = jobs
            .run_cpu(plan, input, second, output)
            .ok_or(Failure::Refused)?;
        moved(stream.write(plan.output.buffer, 0, output))?;
        Ok(written)
    }
}

#[cfg(target_os = "linux")]
pub(crate) use hardware::Routes;

/// No unit is reached off Linux: every job runs on the CPU.
#[cfg(not(target_os = "linux"))]
#[derive(Default)]
pub(crate) struct Routes;

#[cfg(not(target_os = "linux"))]
impl Routes {
    pub(crate) fn cpu() -> Self {
        Self
    }

    pub(crate) fn on_system(_soc: Option<crate::caps::Soc>) -> Self {
        Self
    }

    pub(crate) fn failing(&self) -> bool {
        false
    }

    fn run<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        &mut self,
        _stream: &mut S,
        _plan: &Plan,
        _frames: &mut Frames,
        _now_ns: u64,
    ) -> Option<Result<u32, Failure>> {
        None
    }
}

#[cfg(target_os = "linux")]
mod hardware {
    use std::io;

    use lemnos_core::ErrorKind;
    use lemnos_hal::StreamEndpoint;
    use lemnos_hal::blocks::BufferHandle;

    use super::{Failure, backoff_ns, moved, read_into};
    use crate::caps::{self, FixedOp, Op, Route, Soc};
    use crate::cpu::BlendOp;
    use crate::jobs::{Frames, Jobs, Plan, Side, Work, sized};
    use crate::multirga::{Submit, blend_on, copy_on, gray_on};
    use crate::v4l2::M2mRoute;
    use crate::{BlendMode, Image, M2mRga, MultiRga, PixelFormat, RgaFormat};

    /// Opens a unit's device.
    pub(crate) type Opener<D> = Box<dyn FnMut() -> io::Result<D> + Send>;

    /// One unit's device: opened when a job first wants it, closed when it fails, and opened
    /// again after the wait.
    pub(crate) struct Slot<D> {
        device: Option<D>,
        open: Opener<D>,
        attempt: u32,
        due_ns: u64,
        /// It failed a job and none has run on it since.
        failing: bool,
    }

    impl<D> Slot<D> {
        pub(crate) fn new(open: Opener<D>) -> Self {
            Self {
                device: None,
                open,
                attempt: 0,
                due_ns: 0,
                failing: false,
            }
        }

        /// The device, opened if it isn't and its wait is over.
        fn get(&mut self, now_ns: u64) -> Option<&mut D> {
            if self.device.is_none() {
                if now_ns < self.due_ns {
                    return None;
                }
                match (self.open)() {
                    Ok(device) => self.device = Some(device),
                    Err(_) => {
                        // Not there (yet): looked for again after the wait.
                        self.wait(now_ns);
                        return None;
                    }
                }
            }
            self.device.as_mut()
        }

        fn wait(&mut self, now_ns: u64) {
            self.device = None;
            self.due_ns = now_ns.saturating_add(backoff_ns(self.attempt));
            self.attempt = self.attempt.saturating_add(1);
        }

        fn failed(&mut self, now_ns: u64) {
            self.failing = true;
            self.wait(now_ns);
        }

        fn worked(&mut self) {
            self.attempt = 0;
            self.failing = false;
        }

        /// Whether it is open now.
        pub(crate) fn is_open(&self) -> bool {
            self.device.is_some()
        }

        /// When it may be opened next, while closed.
        #[cfg(test)]
        pub(crate) fn due_ns(&self) -> u64 {
            self.due_ns
        }

        pub(crate) fn is_failing(&self) -> bool {
            self.failing
        }
    }

    /// What became of a job a unit was given.
    enum Ran {
        Done,
        /// The route can't take this job: the CPU runs it, and the unit is not at fault.
        NotTaken,
        Failed,
    }

    fn ran(result: io::Result<()>) -> Ran {
        match result {
            Ok(()) => Ran::Done,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
                ) =>
            {
                Ran::NotTaken
            }
            Err(_) => Ran::Failed,
        }
    }

    /// `side`'s handle, when the stream has one for it now.
    fn handle<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
        stream: &S,
        side: Side,
    ) -> Option<BufferHandle<'_>> {
        stream.buffer_handle(side.buffer).ok().flatten()
    }

    /// The existing check on the real size of the dma-buf behind `handle`: `side`'s extent fits
    /// it.
    fn fits_handle(side: Side, handle: BufferHandle<'_>) -> bool {
        lemnos_linux_sys::dma_buf::size(handle.fd())
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .is_some_and(|n| Jobs::fits(side.layout, n))
    }

    /// The units a SoC has, by route.
    pub(crate) struct Routes {
        soc: Option<Soc>,
        m2m: Option<Slot<Box<dyn M2mRoute>>>,
        /// Whether the mainline device took the stream's dma-bufs: cleared when it refused them
        /// and took bytes, and set again when it is opened afresh.
        imports: bool,
        multi: Option<Slot<Box<dyn Submit>>>,
    }

    impl Routes {
        /// No unit: every job runs on the CPU.
        pub(crate) fn cpu() -> Self {
            Self::new(None, None, None)
        }

        /// The units of `soc` (none when `None`), each a slot over its opener.
        pub(crate) fn new(
            soc: Option<Soc>,
            m2m: Option<Opener<Box<dyn M2mRoute>>>,
            multi: Option<Opener<Box<dyn Submit>>>,
        ) -> Self {
            Self {
                soc,
                m2m: m2m.map(Slot::new),
                imports: true,
                multi: multi.map(Slot::new),
            }
        }

        /// The units of `soc` on this system: the `rockchip-rga` mem2mem node and `/dev/rga`,
        /// each opened when a job first wants it. Nothing is opened for `None`.
        pub(crate) fn on_system(soc: Option<Soc>) -> Self {
            if soc.is_none() {
                return Self::cpu();
            }
            let missing = || io::Error::from(io::ErrorKind::NotFound);
            Self::new(
                soc,
                Some(Box::new(move || {
                    M2mRga::find()
                        .map(|m| Box::new(m) as Box<dyn M2mRoute>)
                        .ok_or_else(missing)
                })),
                Some(Box::new(move || {
                    MultiRga::find()
                        .map(|m| Box::new(m) as Box<dyn Submit>)
                        .ok_or_else(missing)
                })),
            )
        }

        /// Whether a unit failed a job and none has run on it since.
        pub(crate) fn failing(&self) -> bool {
            self.m2m.as_ref().is_some_and(Slot::is_failing)
                || self.multi.as_ref().is_some_and(Slot::is_failing)
        }

        #[cfg(test)]
        pub(crate) fn m2m(&self) -> Option<&Slot<Box<dyn M2mRoute>>> {
            self.m2m.as_ref()
        }

        #[cfg(test)]
        pub(crate) fn multi(&self) -> Option<&Slot<Box<dyn Submit>>> {
            self.multi.as_ref()
        }

        /// The route `caps` chooses for `plan`, `None` for the CPU.
        pub(crate) fn choose(&self, plan: &Plan) -> Option<Route> {
            let soc = self.soc?;
            let op = match plan.work {
                Work::Scale(filter) => Op::Resize(filter),
                Work::Convert => Op::Convert,
                Work::Blend(_) => Op::Fixed(FixedOp::Blend),
            };
            caps::route_for(soc, op, plan.input.layout, plan.output.layout)
        }

        /// Runs `plan` on the unit `caps` chooses, when that unit is there and can take it.
        /// `None` sends the job to the CPU.
        pub(super) fn run<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
            &mut self,
            stream: &mut S,
            plan: &Plan,
            frames: &mut Frames,
            now_ns: u64,
        ) -> Option<Result<u32, Failure>> {
            let written = Jobs::written(plan.output.layout)?;
            match self.choose(plan)? {
                Route::V4l2 => self.run_m2m(stream, plan, frames, now_ns),
                Route::MultiRga => self.run_multi(stream, plan, now_ns),
                _ => None,
            }
            .map(|served| served.map(|()| written))
        }

        /// The mainline route: the stream's dma-bufs where the device takes them, else the
        /// images copied through the driver's memory.
        fn run_m2m<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
            &mut self,
            stream: &mut S,
            plan: &Plan,
            frames: &mut Frames,
            now_ns: u64,
        ) -> Option<Result<(), Failure>> {
            let filter = match plan.work {
                Work::Scale(filter) => Some(filter),
                Work::Convert => None,
                Work::Blend(_) => return None,
            };
            let slot = self.m2m.as_mut()?;
            let reopened = !slot.is_open();
            let device = slot.get(now_ns)?;
            if reopened {
                self.imports = true;
            }
            let (src, dst) = (plan.input, plan.output);
            let want = dst.layout.frame_len();
            let mut tried = false;
            if self.imports
                && let (Some(i), Some(o)) = (handle(stream, src), handle(stream, dst))
            {
                if !(fits_handle(src, i) && fits_handle(dst, o)) {
                    return Some(Err(Failure::Size));
                }
                let result = match filter {
                    Some(f) => device.resize(src.layout, i.fd(), dst.layout, o.fd(), f),
                    None => device.blit(src.layout, i.fd(), dst.layout, o.fd()),
                };
                match ran(short(result, want)) {
                    Ran::Done => {
                        slot.worked();
                        return Some(Ok(()));
                    }
                    Ran::Failed => {
                        slot.failed(now_ns);
                        return None;
                    }
                    // The device doesn't take the dma-bufs: bytes, below.
                    Ran::NotTaken => tried = true,
                }
            }
            // Bytes: the input read once into the driver's memory, the output written back.
            let input = sized(&mut frames.input, src.layout.frame_len());
            if let Err(failure) = read_into(stream, src.buffer, input) {
                return Some(Err(failure));
            }
            let output = sized(&mut frames.output, want);
            output.fill(0);
            let result = match filter {
                Some(f) => device.resize_bytes(src.layout, input, dst.layout, output, f),
                None => device.convert(src.layout, input, dst.layout, output),
            };
            match ran(short(result, want)) {
                Ran::Done => {
                    if tried {
                        // It had the dma-bufs and didn't take them: bytes until it is reopened.
                        self.imports = false;
                    }
                    slot.worked();
                    Some(moved(stream.write(dst.buffer, 0, output)))
                }
                Ran::NotTaken => None,
                Ran::Failed => {
                    slot.failed(now_ns);
                    None
                }
            }
        }

        /// The `/dev/rga` route: dma-bufs only, so only a stream with handles reaches it.
        fn run_multi<S: StreamEndpoint<Error = ErrorKind> + ?Sized>(
            &mut self,
            stream: &mut S,
            plan: &Plan,
            now_ns: u64,
        ) -> Option<Result<(), Failure>> {
            let stream = &*stream;
            let mut handles = [None; 3];
            for (slot, side) in handles.iter_mut().zip(plan.sides()) {
                let h = handle(stream, side)?;
                // Every check, on the real size of the dma-buf behind the handle.
                if !fits_handle(side, h) {
                    return Some(Err(Failure::Size));
                }
                *slot = Some(h);
            }
            let slot = self.multi.as_mut()?;
            let (src, dst) = (plan.input.layout, plan.output.layout);
            let result = match (plan.work, handles) {
                (Work::Convert, [Some(i), Some(o), None]) => {
                    // The node's gray input is full-range BT.601, which repeats the byte as the
                    // CPU does, whatever the driver's matrix; its images are packed.
                    let packed = src.stride == src.width && dst.stride == src.width * 4;
                    let gray = src.format == PixelFormat::Grey
                        && matches!(dst.format, PixelFormat::Abgr32 | PixelFormat::Xbgr32);
                    if !packed || !gray {
                        return None;
                    }
                    let device = slot.get(now_ns)?;
                    gray_on(&**device, src.width, src.height, i.fd(), o.fd())
                }
                (Work::Blend(op), [Some(fg), Some(bg), Some(out)]) => {
                    let second = plan.second?.layout;
                    let strides = src.stride == second.stride && src.stride == dst.stride;
                    if !strides || !src.stride.is_multiple_of(4) {
                        return None;
                    }
                    let image = Image {
                        format: RgaFormat::Bgra8888,
                        width: src.width,
                        height: src.height,
                        wstride: src.stride / 4,
                        hstride: src.height,
                    };
                    let mode = match op {
                        BlendOp::Src => BlendMode::Src,
                        BlendOp::SrcOver => BlendMode::SrcOver,
                        BlendOp::DstOver => BlendMode::DstOver,
                    };
                    let device = slot.get(now_ns)?;
                    // The background into the output, then the foreground onto it.
                    copy_on(&**device, image, bg.fd(), out.fd())
                        .and_then(|()| blend_on(&**device, image, fg.fd(), out.fd(), mode, true))
                }
                _ => return None,
            };
            match ran(result) {
                Ran::Done => {
                    slot.worked();
                    Some(Ok(()))
                }
                Ran::NotTaken => None,
                Ran::Failed => {
                    slot.failed(now_ns);
                    None
                }
            }
        }
    }

    /// A device that wrote less than the frame did not do the job.
    fn short(result: io::Result<usize>, want: usize) -> io::Result<()> {
        match result {
            Ok(n) if n >= want => Ok(()),
            Ok(n) => Err(io::Error::other(format!(
                "the device wrote {n} of {want} bytes"
            ))),
            Err(e) => Err(e),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
