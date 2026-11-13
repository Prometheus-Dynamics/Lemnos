//! The `k230isp-frames` driver: the ISP run by `lemnos.ko`, frames out into the caller's
//! dma-bufs, configuration in and statistics out as [`K230Isp`](crate::K230Isp) carries them.
//!
//! The kernel module drives the block (it is bound to `lemnos.ko`, listed in its `k230_isp`
//! parameter, not to the vendor driver). This driver holds both of the block's sessions with it:
//! the configuration role, through which a job's configuration is written, and the frames role,
//! through which its frame goes. The module refuses, before any register is written, a write to
//! a register the role's table doesn't let it set and every write to an address register; it
//! writes each plane's address and the size the block wraps within itself, from the dma-buf this
//! driver imported and a range inside it; and it switches the running path to a job's buffers at
//! the end of a frame, in its interrupt handler, reading back what the block took
//! (`docs/kernel-module.md`). This driver adds no check the module has to believe. Holding the
//! configuration role, it can't run beside `k230isp` on the module's way; `k230isp` with
//! `k230-capture` is the other pairing for the same block.
//!
//! # Streams
//!
//! - `config` and `statistics`: as for `k230isp` (the register-list layout, checked against the
//!   same table; the statistics layout).
//! - `output0`: a stream of dma-bufs, each holding one image in the output's layout (luma lines,
//!   then chroma, at a stride of the width).
//!
//! # A job, step by step
//!
//! 1. The job names layout [`LAYOUT_WRITES`], a config buffer this driver holds, no input buffer
//!    and an `output0` buffer it holds and the path isn't writing. Its configuration is read
//!    into the driver's copy once and checked whole; at most [`MAX_CALLER_WRITES`] writes.
//! 2. The output buffer's dma-buf is imported into the module's session the first time it is
//!    named, and again when the stream has put other memory behind the buffer number.
//! 3. The caller's writes go to the module through the configuration session, at once (its bits
//!    of `isp_ctrl` merged by the module with the frames role's). Then the job goes through the
//!    frames session: the input, output window and main-path format from `set_input` and
//!    `set_output`, the update, and the planes' ranges (the raw output's address register is
//!    pointed at the luma plane). The module starts it and the driver returns.
//! 4. Later ticks collect it. It completes at the end of the frame the path was writing when it
//!    came: that frame is in the buffer of the job before, which the block has left, so that
//!    job's image and the statistics read at that frame's end are published, with that job's
//!    identity. The path now writes this job's buffer, and keeps writing it until the next job
//!    completes: a caller keeps at least two output buffers going, and the last job's image
//!    comes out when the next one does.
//!
//! A job that fails step 1 or 2, or whose writes or frame the module refuses, is counted as
//! dropped. A frame that
//! wrapped in its buffer (a plane reached its size) is published empty and counted as an error.
//!
//! # Configuration buffers
//!
//! This driver follows the contract's configuration buffer rules, 2 to 5 (the `# Configuration
//! buffers` section of the `camera.isp/1` contract). A job's configuration buffer is read once,
//! when the job is taken off the ring, into the driver's copy, and only the copy is checked and
//! written. A job is busy from its take until its image is published (at the next job's frame
//! end). While one is busy, a job naming its configuration buffer is refused: it is counted as
//! dropped, nothing is published for it, and the job in flight is not disturbed.
//!
//! # Layouts
//!
//! `input_layout` is `NOT_SUPPORTED`: the frame comes from the receiver, as `input_stream` says.
//! `output_layout` is the layout `set_output` answered, `stride` the width and `size` the planes
//! plus `pad_bytes`, from `set_output` until `clear_output`; `None` before that, `NOT_FOUND` past
//! the one output.
//!
//! # Settings
//!
//! - `block` (string, required): the ISP's platform device name, as the module lists it.
//! - `node` (string, default `/dev/lemnos`): the module's control node.
//!
//! # Failures
//!
//! A frame that doesn't come within [`FRAME_TIMEOUT_MS`] (no input running) fails its job and
//! the one before: the module reset the block, and the next job starts the path again (and
//! writes its configuration again). A
//! session that can't be opened (either role held by another driver) or that the module ends
//! (the block unbound or wedged), or a
//! block that didn't take a job's buffers, is closed (the module resets the block before it lets
//! a buffer go) and opened again on later ticks, with the delay doubling from 250 ms and capped
//! at 8 s; it never gives up and never turns itself off. An output consumer that goes takes its
//! buffers with it: the session is closed, so the block writes none of them again.
//!
//! Nothing on the way allocates.

use std::io;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, ConfigLayout, Counters, IspInfo, IspInput, IspJob, IspOutput, NO_BUFFER,
    OutputLayout,
};
use lemnos_core::{
    Config, Decode, Description, Health, Item, Lifecycle, MethodId, Reader, Result, Status,
    StreamHandle, driver,
};
use lemnos_driver_k230isp_sys::regs::{
    ACQ_BAYER_SHIFT, ACQ_INPUT_SHIFT, CTRL_ENABLE, CTRL_GEN_CFG_UPD, CTRL_INFORM_ENABLE,
    CTRL_MODE_BAYER_RGB, ISP_ACQ_H_OFFS, ISP_ACQ_H_SIZE, ISP_ACQ_NR_FRAMES, ISP_ACQ_PROP,
    ISP_ACQ_V_OFFS, ISP_ACQ_V_SIZE, ISP_CTRL, ISP_OUT_H_OFFS, ISP_OUT_H_SIZE, ISP_OUT_V_OFFS,
    ISP_OUT_V_SIZE, MI_ALIGN, MI_MP_BUS_ID, MI_MP_CB_BASE, MI_MP_CR_BASE, MI_MP_FMT,
    MI_MP_FRAME_END, MI_MP_RAW_BASE, MI_MP_WRAP, MI_MP_Y_BASE, MI_MP_Y_LLENGTH, MI_MP_Y_PIC_HEIGHT,
    MI_MP_Y_PIC_SIZE, MI_MP_Y_PIC_WIDTH, MI_MP_YCBCR_ENABLE, MP_BUS_ID_ENABLE, MP_FMT_YUV422,
};
use lemnos_driver_regjob_sys::{self as regjob, Addr, Job, STATUS_UNLATCHED, Write};
use lemnos_hal::blocks::StreamFrame;

use crate::config::{self, CONFIG_BYTES, LAYOUT_WRITES, MAX_WRITES};
use crate::driver::{backoff_ns, has, name, statistics_buffer, take_returned, with};
use crate::stats::{self, LAYOUT_STATISTICS, STATISTICS_BYTES, STATISTICS_REGISTERS};

const START: MethodId = MethodId {
    contract: 0,
    method: 10,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 11,
};

/// The stream jobs and configurations come on.
pub const CONFIG_STREAM: &str = "config";
/// The stream statistics go out on.
pub const STATISTICS_STREAM: &str = "statistics";
/// The stream images go out on.
pub const OUTPUT_STREAM: &str = "output0";
/// The module's control node when the board file names none.
pub const NODE: &str = "/dev/lemnos";
/// How long a frame may take to come before its job fails.
pub const FRAME_TIMEOUT_MS: u32 = 2000;
/// The widest and tallest frame passed on; the ISP refuses less than this on its own.
pub const MAX_DIMENSION: u32 = 4096;
/// Writes of the driver's own in every job: the input, the output window, the main path and
/// `isp_ctrl`.
pub const SETUP_WRITES: usize = 17;
/// Writes a job's configuration may hold: all of them go to the module in one request.
pub const MAX_CALLER_WRITES: usize = MAX_WRITES;
/// Most buffers a stream this driver uses may have: one bit each in a `u64`.
pub const MAX_STREAM_BUFFERS: u32 = 64;
/// Bytes of an encoded job at most.
const MAX_JOB_BYTES: usize = 128;
/// Jobs looked at in one tick at most.
const MAX_JOBS_PER_TICK: usize = 64;
/// How often the module and the streams are looked at while running.
const POLL_NS: u64 = 1_000_000;
const NV12: u32 = u32::from_le_bytes(*b"NV12");
const NV16: u32 = u32::from_le_bytes(*b"NV16");

const _: () = assert!(MAX_CALLER_WRITES <= regjob::MAX_WRITES);
const _: () = assert!(SETUP_WRITES <= regjob::MAX_WRITES);
const _: () = assert!(STATISTICS_REGISTERS <= regjob::MAX_READS);

/// The two sessions with the module, one request each. The Linux one is [`LinuxModule`]; tests
/// play one back.
pub trait Module: Send {
    /// Opens the configuration session and the frames session.
    fn open(&mut self) -> io::Result<()>;
    /// Closes both: the module resets the block if it may be using a buffer, then lets every
    /// one go.
    fn close(&mut self);
    /// Writes `writes` through the configuration session, at once.
    fn configure(&mut self, writes: &[Write]) -> io::Result<()>;
    /// Imports `buffer`; the index a job's planes name.
    fn import(&mut self, buffer: BorrowedFd<'_>) -> io::Result<u32>;
    /// Lets an imported buffer go.
    fn release(&mut self, index: u32) -> io::Result<()>;
    /// Starts `job`.
    fn start(&mut self, job: &Job<'_>) -> io::Result<()>;
    /// The started job's end: `None` while it runs, else its status, its reads in `values`.
    fn collect(&mut self, values: &mut [u32]) -> io::Result<Option<u32>>;
}

/// The block's number behind the control node at `node`.
fn find(node: &std::path::Path, block: &str) -> io::Result<(regjob::Control, u32)> {
    let control = regjob::Control::open(node)?;
    let id = control.find_block(block)?;
    Ok((control, id))
}

/// The module's session on Linux.
#[derive(Debug)]
pub struct LinuxModule {
    node: PathBuf,
    block: String,
    config: Option<regjob::Session>,
    session: Option<regjob::Session>,
}

impl LinuxModule {
    /// The block whose device name is `block`, through the control node at `node`.
    #[must_use]
    pub fn new(node: PathBuf, block: String) -> Self {
        Self {
            node,
            block,
            config: None,
            session: None,
        }
    }

    fn session(&self) -> io::Result<&regjob::Session> {
        self.session
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))
    }
}

impl Module for LinuxModule {
    fn open(&mut self) -> io::Result<()> {
        self.close();
        let (control, id) = find(&self.node, &self.block)?;
        let config = control.open_config(id)?;
        let session = control.open_session(id)?;
        if session.profile() != regjob::PROFILE_K230_ISP {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.config = Some(config);
        self.session = Some(session);
        Ok(())
    }

    fn close(&mut self) {
        self.session = None;
        self.config = None;
    }

    fn configure(&mut self, writes: &[Write]) -> io::Result<()> {
        self.config
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))?
            .write(writes, &[], &mut [])
    }

    fn import(&mut self, buffer: BorrowedFd<'_>) -> io::Result<u32> {
        self.session()?
            .import(buffer)
            .map(|imported| imported.index)
    }

    fn release(&mut self, index: u32) -> io::Result<()> {
        self.session()?.release(index)
    }

    fn start(&mut self, job: &Job<'_>) -> io::Result<()> {
        self.session()?.start(job)
    }

    fn collect(&mut self, values: &mut [u32]) -> io::Result<Option<u32>> {
        Ok(self.session()?.collect(values)?.map(|done| done.status))
    }
}

/// What the module answers for a job it refuses.
fn refusal(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::EPERM | libc::EINVAL | libc::ENOENT | libc::EFAULT | libc::ENOSPC)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Running,
    Recovering { attempt: u32, due: Option<u64> },
}

/// A frame job: whose, the output buffer its image goes into, and the config buffer its
/// configuration was read from. Both buffers stay busy until the job completes (its image is
/// published).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    identity: u64,
    buffer: u32,
    config: u32,
}

/// An output buffer's dma-buf as imported: the module's index and the memory's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Imported {
    index: u32,
    memory: (u64, u64),
}

/// The output as agreed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Output {
    image: IspOutput,
    luma: u32,
    chroma: u32,
    /// What `set_output` answered, and what `output_layout` gives until the output is cleared.
    layout: OutputLayout,
}

/// The ISP through `lemnos.ko`, frames into the caller's dma-bufs, as `camera.isp/1`.
#[driver("k230isp-frames", provides = [CameraIsp])]
pub struct K230IspFrames {
    #[needs(job)]
    config: StreamHandle,
    #[needs(job)]
    statistics: StreamHandle,
    #[needs(job)]
    output0: StreamHandle,
    module: Box<dyn Module>,
    state: State,
    /// The session is open.
    open: bool,
    counters: Counters,
    input: Option<IspInput>,
    output: Option<Output>,
    config_buffers: u32,
    held_config: u64,
    held_statistics: u64,
    held_output: u64,
    imported: [Option<Imported>; MAX_STREAM_BUFFERS as usize],
    /// The job with the module, not yet collected.
    started: Option<Frame>,
    /// The job whose buffer the path is writing.
    writing: Option<Frame>,
    /// Frames published.
    sequence: u64,
    copy: Box<[u8]>,
    writes: Box<[Write]>,
    reads: [u32; STATISTICS_REGISTERS],
    values: [u32; STATISTICS_REGISTERS],
    stats: [u8; STATISTICS_BYTES],
}

impl Lifecycle for K230IspFrames {
    type Needs = K230IspFramesNeeds;
    const TICKS: bool = true;

    fn start(needs: K230IspFramesNeeds, cfg: &Config<'_>) -> Result<Self> {
        let Some(block) = cfg.str("block").filter(|block| !block.is_empty()) else {
            return Err(Status::INVALID_ARGUMENT);
        };
        let node = PathBuf::from(cfg.str("node").unwrap_or(NODE));
        Ok(Self::with_module(
            Box::new(LinuxModule::new(node, block.to_owned())),
            needs,
        ))
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } => Health::Degraded(
                "the ISP's session with the kernel module failed: opening it again",
            ),
            _ => Health::Ok,
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Running => {
                if self.pump(now_ns).is_err() {
                    self.fail();
                    return self.tick(now_ns);
                }
                Some(now_ns.saturating_add(POLL_NS))
            }
            State::Recovering { attempt, due: None } => {
                let at = now_ns.saturating_add(backoff_ns(attempt));
                self.state = State::Recovering {
                    attempt,
                    due: Some(at),
                };
                Some(at)
            }
            State::Recovering { due: Some(at), .. } if now_ns < at => Some(at),
            State::Recovering { attempt, .. } => {
                if self.begin().is_ok() {
                    self.state = State::Running;
                    return Some(now_ns.saturating_add(POLL_NS));
                }
                let attempt = attempt.saturating_add(1);
                let at = now_ns.saturating_add(backoff_ns(attempt));
                self.state = State::Recovering {
                    attempt,
                    due: Some(at),
                };
                Some(at)
            }
        }
    }
}

impl K230IspFrames {
    /// A driver over `module` with the streams in `needs`, for a test.
    #[must_use]
    pub fn with_module(module: Box<dyn Module>, needs: K230IspFramesNeeds) -> Self {
        let mut reads = [0; STATISTICS_REGISTERS];
        for (into, addr) in reads.iter_mut().zip(stats::registers()) {
            *into = addr;
        }
        Self {
            config: needs.config,
            statistics: needs.statistics,
            output0: needs.output0,
            module,
            state: State::Stopped,
            open: false,
            counters: Counters::default(),
            input: None,
            output: None,
            config_buffers: 0,
            held_config: 0,
            held_statistics: 0,
            held_output: 0,
            imported: [None; MAX_STREAM_BUFFERS as usize],
            started: None,
            writing: None,
            sequence: 0,
            copy: vec![0; CONFIG_BYTES].into_boxed_slice(),
            writes: vec![Write::default(); regjob::MAX_WRITES].into_boxed_slice(),
            reads,
            values: [0; STATISTICS_REGISTERS],
            stats: [0; STATISTICS_BYTES],
        }
    }

    fn error(&mut self) {
        self.counters.errors = self.counters.errors.saturating_add(1);
    }

    fn dropped(&mut self) {
        self.counters.dropped = self.counters.dropped.saturating_add(1);
    }

    /// Closes the session: the module resets the block if it may still write a buffer, and lets
    /// every one go. The jobs with it are errors; their buffers stay with the driver.
    fn close(&mut self) {
        self.module.close();
        self.open = false;
        self.imported = [None; MAX_STREAM_BUFFERS as usize];
        if self.started.take().is_some() {
            self.error();
        }
        if self.writing.take().is_some() {
            self.error();
        }
    }

    fn fail(&mut self) {
        self.close();
        self.state = State::Recovering {
            attempt: 0,
            due: None,
        };
    }

    fn begin(&mut self) -> Result<()> {
        self.close();
        self.module.open().map_err(|_| Status::IO)?;
        self.open = true;
        Ok(())
    }

    /// Gives up an output buffer whose consumer has gone: publishing it reaches no one.
    fn give_up(&mut self, buffer: u32) -> Result<()> {
        self.output0
            .publish(StreamFrame {
                buffer,
                bytes: 0,
                sequence: 0,
                identity: 0,
                timestamp_ns: 0,
            })
            .map_err(|_| Status::IO)?;
        self.held_output = with(self.held_output, buffer, false);
        Ok(())
    }

    /// Whether the output consumer the path's buffers were taken under is still there.
    fn consumer_there(&self) -> Result<bool> {
        for frame in [self.started, self.writing].into_iter().flatten() {
            if !self
                .output0
                .buffer_current(frame.buffer)
                .map_err(|_| Status::IO)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Stream buffers back, the started job collected, the next job in. An error ends the
    /// session.
    fn pump(&mut self, now_ns: u64) -> Result<()> {
        take_returned(&mut self.config, &mut self.held_config)?;
        take_returned(&mut self.statistics, &mut self.held_statistics)?;
        take_returned(&mut self.output0, &mut self.held_output)?;
        if !self.consumer_there()? {
            // The block must write none of the gone consumer's memory again.
            let gone = [self.started, self.writing];
            self.close();
            for frame in gone.into_iter().flatten() {
                self.give_up(frame.buffer)?;
            }
            self.module.open().map_err(|_| Status::IO)?;
            self.open = true;
        }
        if self.started.is_some() {
            self.collect(now_ns)?;
        }
        if self.started.is_none() {
            self.take_jobs()?;
        }
        Ok(())
    }

    fn collect(&mut self, now_ns: u64) -> Result<()> {
        let status = match self.module.collect(&mut self.values) {
            Ok(None) => return Ok(()),
            Ok(Some(status)) => status,
            Err(err) if err.raw_os_error() == Some(libc::ETIMEDOUT) => {
                // The module reset the block: neither buffer is written any more, and neither
                // holds a frame. The next job starts the path again.
                if self.started.take().is_some() {
                    self.error();
                }
                if self.writing.take().is_some() {
                    self.error();
                }
                return Ok(());
            }
            Err(_) => return Err(Status::IO),
        };
        if status & STATUS_UNLATCHED != 0 {
            // The block may be writing either buffer: only a reset says which it isn't.
            return Err(Status::IO);
        }
        let done = self.writing.take();
        self.writing = self.started.take();
        if let Some(frame) = done {
            self.deliver(frame, status, now_ns)?;
        }
        Ok(())
    }

    /// Publishes the image in `frame`'s buffer, which the block has left, and the statistics
    /// read at its end.
    fn deliver(&mut self, frame: Frame, status: u32, now_ns: u64) -> Result<()> {
        let Some(output) = self.output else {
            return Err(Status::IO);
        };
        let whole = status & MI_MP_FRAME_END != 0 && status & MI_MP_WRAP == 0;
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        let image = StreamFrame {
            buffer: frame.buffer,
            bytes: if whole {
                output.luma + output.chroma
            } else {
                0
            },
            sequence,
            identity: frame.identity,
            timestamp_ns: now_ns,
        };
        self.output0.publish(image).map_err(|_| Status::IO)?;
        self.held_output = with(self.held_output, frame.buffer, false);
        if !whole {
            self.error();
            return Ok(());
        }
        let Some(buffer) = statistics_buffer(&mut self.statistics, &mut self.held_statistics)?
        else {
            self.error();
            return Ok(());
        };
        for (into, value) in self
            .stats
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&self.values)
        {
            *into = value.to_le_bytes();
        }
        let reached = matches!(self.statistics.write(buffer, 0, &self.stats), Ok(true));
        let stats = StreamFrame {
            buffer,
            bytes: if reached { STATISTICS_BYTES as u32 } else { 0 },
            sequence,
            identity: frame.identity,
            timestamp_ns: now_ns,
        };
        self.statistics.publish(stats).map_err(|_| Status::IO)?;
        self.held_statistics = with(self.held_statistics, buffer, false);
        if reached {
            self.counters.jobs = self.counters.jobs.saturating_add(1);
        } else {
            self.error();
        }
        Ok(())
    }

    /// Takes posted jobs until one goes to the module, counting the others as dropped.
    fn take_jobs(&mut self) -> Result<()> {
        let mut encoded = [0u8; MAX_JOB_BYTES];
        for _ in 0..MAX_JOBS_PER_TICK {
            let Some(len) = self.config.next_job(&mut encoded).map_err(|_| Status::IO)? else {
                break;
            };
            let job = encoded
                .get(..len)
                .and_then(|bytes| IspJob::decode(&mut Reader::new(bytes)).ok());
            take_returned(&mut self.config, &mut self.held_config)?;
            take_returned(&mut self.output0, &mut self.held_output)?;
            let sent = match job {
                Some(job) => self.submit(&job)?,
                None => false,
            };
            if sent {
                break;
            }
            self.dropped();
        }
        Ok(())
    }

    /// The module's index for output buffer `buffer`, importing its memory when the session
    /// doesn't hold it yet. `None` when nothing is behind the buffer (its consumer has gone).
    fn import(&mut self, buffer: u32) -> Result<Option<u32>> {
        let slot = buffer as usize;
        let handle = match self.output0.buffer_handle(buffer) {
            Ok(Some(handle)) => handle,
            Ok(None) => return Ok(None),
            Err(_) => return Err(Status::IO),
        };
        let memory = lemnos_linux_sys::dma_buf::identity(handle.fd()).map_err(|_| Status::IO)?;
        match self.imported.get(slot).copied().flatten() {
            Some(held) if held.memory == memory => return Ok(Some(held.index)),
            // Other memory behind the number now: the old memory is not the block's any more.
            Some(held) => self.module.release(held.index).map_err(|_| Status::IO)?,
            None => {}
        }
        let index = match self.module.import(handle.fd()) {
            Ok(index) => index,
            Err(err) if refusal(&err) => return Ok(None),
            Err(_) => return Err(Status::IO),
        };
        if let Some(into) = self.imported.get_mut(slot) {
            *into = Some(Imported { index, memory });
        }
        Ok(Some(index))
    }

    /// Fills the job's writes: the driver's own, then `isp_ctrl`.
    fn setup(&mut self, input: IspInput, output: Output) -> usize {
        let (width, height) = (input.width, input.height);
        let bayer = match input.bayer {
            BayerOrder::Grbg => 1,
            BayerOrder::Gbrg => 2,
            BayerOrder::Bggr => 3,
            BayerOrder::Rggb | BayerOrder::Mono => 0,
        };
        let depth = match input.bit_depth {
            8 => 3,
            10 => 1,
            _ => 0,
        };
        let format = if output.image.fourcc == NV16 {
            MP_FMT_YUV422
        } else {
            0
        };
        let own: [(u32, u32); SETUP_WRITES] = [
            (
                ISP_ACQ_PROP,
                bayer << ACQ_BAYER_SHIFT | depth << ACQ_INPUT_SHIFT,
            ),
            (ISP_ACQ_H_OFFS, 0),
            (ISP_ACQ_V_OFFS, 0),
            (ISP_ACQ_H_SIZE, width),
            (ISP_ACQ_V_SIZE, height),
            (ISP_ACQ_NR_FRAMES, 0),
            (ISP_OUT_H_OFFS, 0),
            (ISP_OUT_V_OFFS, 0),
            (ISP_OUT_H_SIZE, width),
            (ISP_OUT_V_SIZE, height),
            (MI_MP_FMT, format),
            (MI_MP_BUS_ID, MP_BUS_ID_ENABLE),
            (MI_MP_Y_LLENGTH, width),
            (MI_MP_Y_PIC_WIDTH, width),
            (MI_MP_Y_PIC_HEIGHT, height),
            (MI_MP_Y_PIC_SIZE, width * height),
            // The frames role's bits; the module keeps the configuration's (gamma) as they are.
            (
                ISP_CTRL,
                CTRL_ENABLE | CTRL_MODE_BAYER_RGB | CTRL_INFORM_ENABLE | CTRL_GEN_CFG_UPD,
            ),
        ];
        for (into, (offset, value)) in self.writes.iter_mut().zip(own) {
            *into = Write { offset, value };
        }
        SETUP_WRITES
    }

    /// Sends one job to the module. `false` for a job that can't go; an error ends the session.
    fn submit(&mut self, job: &IspJob) -> Result<bool> {
        let (Some(input), Some(output)) = (self.input, self.output) else {
            return Ok(false);
        };
        let buffer = job.outputs[0];
        let config = job.config_buffer;
        // A job still in flight (started, or the path writing its image) holds both its buffers.
        let busy_output = |frame: Option<Frame>| frame.is_some_and(|f| f.buffer == buffer);
        let busy_config = |frame: Option<Frame>| frame.is_some_and(|f| f.config == config);
        if job.layout != LAYOUT_WRITES
            || job.config_buffer >= self.config_buffers
            || !has(self.held_config, job.config_buffer)
            || job.input_buffer != NO_BUFFER
            || job.outputs[1..].iter().any(|&b| b != NO_BUFFER)
            || !has(self.held_output, buffer)
            || busy_output(self.writing)
            || busy_config(self.started)
            || busy_config(self.writing)
        {
            return Ok(false);
        }
        if !self
            .output0
            .buffer_current(buffer)
            .map_err(|_| Status::IO)?
        {
            self.give_up(buffer)?;
            return Ok(false);
        }
        // The one read of the caller's buffer. Everything after uses the copy.
        match self.config.read(job.config_buffer, 0, &mut self.copy) {
            Ok(true) => {}
            Ok(false) | Err(_) => return Ok(false),
        }
        let Some(writes) = config::check(&self.copy) else {
            return Ok(false);
        };
        if writes.len() > MAX_CALLER_WRITES {
            return Ok(false);
        }
        let mut count = 0;
        for ((offset, value), into) in writes.zip(self.writes.iter_mut()) {
            *into = Write { offset, value };
            count += 1;
        }
        let Some(index) = self.import(buffer)? else {
            return Ok(false);
        };
        match self.module.configure(&self.writes[..count]) {
            Ok(()) => {}
            Err(err) if refusal(&err) => return Ok(false),
            Err(_) => return Err(Status::IO),
        }
        let count = self.setup(input, output);
        let addrs = [
            Addr {
                reg: MI_MP_Y_BASE,
                buffer: index,
                offset: 0,
                bytes: u64::from(output.luma),
            },
            Addr {
                reg: MI_MP_CB_BASE,
                buffer: index,
                offset: u64::from(output.luma),
                bytes: u64::from(output.chroma),
            },
            // Semi-planar formats have no Cr plane; the module writes the register all the same.
            Addr {
                reg: MI_MP_CR_BASE,
                buffer: index,
                offset: u64::from(output.luma),
                bytes: u64::from(output.chroma),
            },
            // The raw output stays off; its register names the luma plane all the same.
            Addr {
                reg: MI_MP_RAW_BASE,
                buffer: index,
                offset: 0,
                bytes: u64::from(output.luma),
            },
        ];
        let request = Job {
            writes: &self.writes[..count],
            addrs: &addrs,
            reads: &self.reads,
            start: MI_MP_YCBCR_ENABLE,
            timeout_ms: FRAME_TIMEOUT_MS,
        };
        match self.module.start(&request) {
            Ok(()) => {}
            Err(err) if refusal(&err) => return Ok(false),
            Err(_) => return Err(Status::IO),
        }
        self.started = Some(Frame {
            identity: job.identity,
            buffer,
            config: job.config_buffer,
        });
        Ok(true)
    }

    fn stopped(&self) -> Result<()> {
        if self.state == State::Stopped {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
    }
}

/// The bytes of each plane for `image`, or `None` for one the main path can't write.
fn planes(image: &IspOutput) -> Option<(u32, u32)> {
    let (width, height) = (image.width, image.height);
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || !width.is_multiple_of(MI_ALIGN)
        || !height.is_multiple_of(2)
    {
        return None;
    }
    let luma = width * height;
    let chroma = match image.fourcc {
        NV12 => luma / 2,
        NV16 => luma,
        _ => return None,
    };
    Some((luma, chroma))
}

const _: () = {
    assert!(CONFIG_STREAM.len() <= lemnos_contract_camera_isp::MAX_STREAM_NAME);
    assert!(STATISTICS_STREAM.len() <= lemnos_contract_camera_isp::MAX_STREAM_NAME);
    assert!(OUTPUT_STREAM.len() <= lemnos_contract_camera_isp::MAX_STREAM_NAME);
    // A plane's size fits the module's register: 4096 * 4096 bytes.
    assert!(MAX_DIMENSION as u64 * MAX_DIMENSION as u64 <= 0x1fff_fff0);
};

impl CameraIsp for K230IspFrames {
    fn info(&mut self) -> Result<IspInfo> {
        Ok(IspInfo {
            max_outputs: 1,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
            config_layouts: 1,
            statistics_layout: LAYOUT_STATISTICS,
            statistics_bytes: STATISTICS_BYTES as u32,
        })
    }

    fn config_layout(&mut self, index: u8) -> Result<ConfigLayout> {
        if index != 0 {
            return Err(Status::NOT_FOUND);
        }
        Ok(ConfigLayout {
            layout: LAYOUT_WRITES,
            bytes: CONFIG_BYTES as u32,
        })
    }

    /// The raw frames coming in: 8, 10 or 12 bits, a colour filter, the size the output is.
    fn set_input(&mut self, input: IspInput) -> Result<()> {
        self.stopped()?;
        if input.width == 0
            || input.height == 0
            || input.width > MAX_DIMENSION
            || input.height > MAX_DIMENSION
            || !matches!(input.bit_depth, 8 | 10 | 12)
            || input.bayer == BayerOrder::Mono
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.input = Some(input);
        Ok(())
    }

    /// NV12 or NV16 at the input's size (the main path doesn't scale here), a width a multiple
    /// of 16; the stride is the width.
    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout> {
        if output != 0 {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        let (luma, chroma) = planes(&image).ok_or(Status::INVALID_ARGUMENT)?;
        let stream_bytes = (luma + chroma)
            .checked_add(image.pad_bytes)
            .ok_or(Status::INVALID_ARGUMENT)?;
        let layout = OutputLayout {
            stride: image.width,
            size: stream_bytes,
        };
        self.output = Some(Output {
            image,
            luma,
            chroma,
            layout,
        });
        Ok(layout)
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        if output != 0 {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        self.output = None;
        Ok(())
    }

    /// `NOT_SUPPORTED`: the frame comes from the receiver, not from a buffer this driver's
    /// consumer writes.
    fn input_layout(&mut self) -> Result<Option<OutputLayout>> {
        Err(Status::NOT_SUPPORTED)
    }

    fn output_layout(&mut self, output: u8) -> Result<Option<OutputLayout>> {
        if output != 0 {
            return Err(Status::NOT_FOUND);
        }
        Ok(self.output.map(|o| o.layout))
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(CONFIG_STREAM, into)
    }

    fn statistics_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(STATISTICS_STREAM, into)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // Frames come from the receiver that feeds the ISP, not through memory.
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, output: u8, into: &mut [u8]) -> Result<usize> {
        if output != 0 {
            return Err(Status::NOT_FOUND);
        }
        name(OUTPUT_STREAM, into)
    }

    fn start(&mut self) -> Result<()> {
        self.stopped()?;
        let (Some(input), Some(output)) = (self.input, self.output) else {
            return Err(Status::INVALID_ARGUMENT);
        };
        if input.width != output.image.width || input.height != output.image.height {
            return Err(Status::INVALID_ARGUMENT);
        }
        let count = |n: u32| {
            if n > MAX_STREAM_BUFFERS {
                Err(Status::INVALID_ARGUMENT)
            } else {
                Ok(n)
            }
        };
        let config = count(self.config.buffer_count())?;
        count(self.statistics.buffer_count())?;
        count(self.output0.buffer_count())?;
        if self.output0.buffer_kind() != lemnos_hal::blocks::BufferKind::Handles {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.counters = Counters::default();
        self.begin()?;
        self.config_buffers = config;
        self.state = State::Running;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.state = State::Stopped;
        self.close();
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }
}
