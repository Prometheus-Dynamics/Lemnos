//! The front end end to end through the host's own streams, as a client of the daemon holds them.
//! The client writes a configuration into a `config` buffer and posts a job on the `config`
//! stream; it reads output 0, the statistics and output 1 through the descriptors it was handed
//! for the `output0`, `statistics` and `output1` buffers. A stand-in for the block and the module
//! takes the configuration from the memory the module was given for the job, by device address,
//! only when it gets to the job (the module copies it then, not when the job is posted), and
//! writes into each buffer, by device address, data derived from that configuration and the job,
//! as the block's DMA would. The client must read exactly that: right numbers on a ring over
//! memory the block never wrote, or results made from a configuration the client didn't post,
//! are the failures this test is for.
//!
//! Sealed memfds stand in for the streams' dma-bufs and for the module's own memory.

#![cfg(target_os = "linux")]

use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, NO_BUFFER,
};
use lemnos_contract_isp_pisp::LAYOUT_FRONT_END;
use lemnos_core::{CallerId, Encode, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_driver_pisp::fe::FrontEnd;
use lemnos_driver_pisp::fe_hw::{
    Finished, Hardware, Job, Placed, Plan, Registered, Set, Start, slot_bytes,
};
use lemnos_driver_pisp::fe_job::{self, CONFIG_BYTES, Fixed, STATS_BYTES};
use lemnos_driver_pisp::format::formats;
use lemnos_driver_pisp::uapi::{
    BayerOrder as Order, Branch, FeConfig, ImageFormatConfig, Wire, fe_enable as en,
};
use lemnos_driver_pisp::{FeLink, FeOptions, PispFe, PispFeNeeds};
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::{Backing, BufferMemory, HeapAllocator, Lease, StreamAllocator};
use lemnos_host::{DeviceState, Host, Options as HostOptions, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::SimBoard;
use lemnos_wire::ring::{Consumer, Layout, STATE_FAULT};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
/// 16 bits a pixel at 64 by 32, on both outputs.
const OUTPUT_BYTES: u32 = WIDTH * 2 * HEIGHT;
const BUFFERS: u32 = 4;
/// Config buffers, and statistics buffers: a job's statistics go in the one numbered as its
/// configuration.
const CONFIGS: u32 = 2;
/// Where the stand-in puts the configuration slots, what is registered, and output 0's memory.
const CONFIG: u64 = 0x1000_0000;
const REGIONS: u64 = 0x8000_0000;
const REGION_SPAN: u64 = 0x0100_0000;
const MEMORY: u64 = 0x4000_0000;
const MEMORY_SPAN: u64 = 0x0010_0000;
/// The module's room for registered memory, beside the configuration slots.
const MAX_REGIONS: usize = 14;
const CALLER: CallerId = CallerId(5);

const FIXED: Fixed = Fixed {
    width: WIDTH as u16,
    height: HEIGHT as u16,
    output_width: WIDTH as u16,
    output_height: HEIGHT as u16,
    output_stride: WIDTH * 2,
    output_format: formats::BAYER16,
};

/// What the block writes into output `which` (0, 1, or 2 for the statistics) for the job with
/// `identity` under a configuration marked `key`.
fn made(which: u8, key: u8, identity: u64, at: usize) -> u8 {
    (identity as usize)
        .wrapping_mul(31)
        .wrapping_add(at * 7)
        .wrapping_add(usize::from(which) * 101) as u8
        ^ key
        | 1
}

fn expected(which: u8, key: u8, identity: u64, bytes: usize) -> Vec<u8> {
    (0..bytes)
        .map(|at| made(which, key, identity, at))
        .collect()
}

/// The configuration's mark: its red black level, which the block reads like any other setting.
fn key_of(cfg: &FeConfig) -> u8 {
    cfg.bla.black_level_r as u8
}

// ---- The streams' memory, as the daemon makes it: one descriptor per buffer.

struct PerBuffer {
    files: Vec<File>,
    stride: usize,
    bytes: usize,
}

impl PerBuffer {
    fn at(&self, offset: usize, len: usize) -> Option<(&File, u64)> {
        let inner = offset % self.stride;
        let fits = inner.checked_add(len).is_some_and(|end| end <= self.bytes);
        let file = self.files.get(offset / self.stride)?;
        fits.then_some((file, inner as u64))
    }
}

impl BufferMemory for PerBuffer {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        self.at(offset, data.len())
            .is_some_and(|(file, at)| file.write_all_at(data, at).is_ok())
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        self.at(offset, out.len())
            .is_some_and(|(file, at)| file.read_exact_at(out, at).is_ok())
    }
}

struct Handles;

impl StreamAllocator for Handles {
    fn allocate(&self, name: &str, layout: &Layout) -> core::result::Result<Backing, Status> {
        HeapAllocator.allocate(name, layout)
    }

    fn allocate_handles(
        &self,
        name: &str,
        layout: &Layout,
        _heap: &str,
    ) -> core::result::Result<Backing, Status> {
        let plain = HeapAllocator.allocate(name, layout)?;
        let mut files = Vec::new();
        let mut handles = Vec::new();
        for _ in 0..layout.buffers {
            let fd = sealed_memfd(c"fe-buffer", layout.stride()).map_err(|_| Status::IO)?;
            handles.push(fd.try_clone().map_err(|_| Status::IO)?);
            files.push(File::from(fd));
        }
        Ok(Backing {
            buffers: Arc::new(PerBuffer {
                files,
                stride: layout.stride() as usize,
                bytes: layout.buffer_bytes as usize,
            }),
            handles: Some(handles.into()),
            ..plain
        })
    }
}

fn host() -> Host {
    let dma = "memory = \"dma-buf\"";
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"isp\"\ndriver = \"fake-counter\"\n\
         [devices.streams.config]\nbuffers = {CONFIGS}\nbuffer_bytes = {CONFIG_BYTES}\n\
         job_slots = 8\njob_bytes = 128\n\
         [devices.streams.statistics]\nbuffers = {CONFIGS}\nbuffer_bytes = {STATS_BYTES}\n{dma}\n\
         [devices.streams.output0]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n{dma}\n\
         [devices.streams.output1]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n{dma}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        HostOptions::default(),
        Arc::new(Handles),
    )
    .unwrap()
}

// ---- The block and the module: reads and writes where a job says, by device address.

/// Memory the block can reach, by device address.
struct Region {
    addr: u64,
    file: File,
    size: u64,
}

fn same_file(a: &File, b: &File) -> bool {
    match (a.metadata(), b.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

struct Block {
    /// The configuration slots the driver writes: the module was given this memory at `make`.
    configs: Option<Region>,
    /// What was registered; a slot let go of is `None` and is used again.
    regions: Vec<Option<Region>>,
    /// Each output 0 buffer's pinned memory.
    buffers: Vec<Option<Region>>,
    given: u32,
    output0_bytes: u32,
    /// Jobs posted and not yet run.
    pending: VecDeque<Job>,
    finished: VecDeque<Finished>,
    /// The module's own copy of the configuration of the job it is on.
    copy: Box<[u8; CONFIG_BYTES]>,
    parsed: Box<FeConfig>,
    scratch: Vec<u8>,
    /// Jobs with a range in no memory the block was given, or a configuration the module
    /// would have refused.
    lost: u32,
    /// The configuration of each job run, by its key.
    keys: Vec<u8>,
    /// Run once, after the job with this identity is posted: the client's part of a race.
    hook: Option<(u64, Box<dyn FnOnce() + Send>)>,
}

impl Default for Block {
    fn default() -> Self {
        Self {
            configs: None,
            regions: Vec::with_capacity(MAX_REGIONS),
            buffers: Vec::with_capacity(BUFFERS as usize),
            given: 0,
            output0_bytes: 0,
            pending: VecDeque::with_capacity(16),
            finished: VecDeque::with_capacity(16),
            copy: Box::new([0; CONFIG_BYTES]),
            parsed: Box::default(),
            scratch: Vec::with_capacity(OUTPUT_BYTES as usize),
            lost: 0,
            keys: Vec::with_capacity(1024),
            hook: None,
        }
    }
}

/// The memory `len` bytes at `addr` are in, and where in it.
fn find<'a>(
    mut regions: impl Iterator<Item = &'a Region>,
    addr: u64,
    len: u64,
) -> Option<(&'a File, u64)> {
    regions.find_map(|r| {
        let end = addr.checked_add(len)?;
        (addr >= r.addr && end <= r.addr + r.size).then(|| (&r.file, addr - r.addr))
    })
}

impl Block {
    fn reach(&self, addr: u64, len: u64) -> Option<(&File, u64)> {
        let regions = self
            .configs
            .iter()
            .chain(self.regions.iter().flatten())
            .chain(self.buffers.iter().flatten());
        find(regions, addr, len)
    }

    /// Writes what the block makes for output `which` into `len` bytes at `addr`.
    fn write(&mut self, which: u8, key: u8, identity: u64, addr: u64, len: u64) -> bool {
        let mut scratch = core::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.extend((0..len as usize).map(|at| made(which, key, identity, at)));
        let wrote = self
            .reach(addr, len)
            .is_some_and(|(file, at)| file.write_all_at(&scratch, at).is_ok());
        self.scratch = scratch;
        wrote
    }

    /// Runs the oldest job, as the block would once it got to it: the module takes the
    /// configuration only now.
    fn run(&mut self) {
        let Some(job) = self.pending.pop_front() else {
            return;
        };
        let config = job.range[0];
        let regions = self.configs.iter().chain(self.regions.iter().flatten());
        let copy = &mut self.copy;
        let read = config.len == CONFIG_BYTES as u64
            && find(regions, config.addr, config.len)
                .is_some_and(|(file, at)| file.read_exact_at(&mut copy[..], at).is_ok());
        // The module's checks, on its copy: no address may come from the configuration.
        let shape = if read && self.parsed.read_from(&self.copy[..]).is_ok() {
            fe_job::check(&self.parsed, &FIXED).ok()
        } else {
            None
        };
        let Some(shape) = shape.filter(|s| s.count() == job.count) else {
            self.lost += 1;
            return;
        };
        let key = key_of(&self.parsed);
        let identity = job.identity;
        let output0 = self
            .buffers
            .get(job.buffer as usize)
            .and_then(Option::as_ref)
            .map(|m| m.addr);
        let mut ok =
            output0.is_some_and(|addr| self.write(0, key, identity, addr, u64::from(OUTPUT_BYTES)));
        let mut next = 1;
        if shape.stats {
            let r = job.range[next];
            ok &= r.len == STATS_BYTES as u64 && self.write(2, key, identity, r.addr, r.len);
            next += 1;
        }
        if shape.output1 {
            let r = job.range[next];
            let bytes = fe_job::output_bytes(&self.parsed, 1).unwrap_or(u64::MAX);
            ok &= bytes <= r.len && self.write(1, key, identity, r.addr, bytes);
        }
        if !ok {
            self.lost += 1;
        }
        self.keys.push(key);
        self.finished.push_back(Finished {
            buffer: job.buffer,
            bytes: self.output0_bytes,
            sequence: identity,
            identity,
            timestamp_ns: 1,
        });
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Block>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Block) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    fn registered(&self) -> usize {
        self.with(|b| b.regions.iter().flatten().count())
    }

    /// Makes the client's move in the middle of the driver's pump: `run` runs once, right after
    /// the job with `identity` is posted to the block.
    fn hook_after_post(&self, identity: u64, run: impl FnOnce() + Send + 'static) {
        self.with(|b| b.hook = Some((identity, Box::new(run))));
    }
}

impl Hardware for Hw {
    fn hw_version(&self) -> u32 {
        0x0225_2700
    }

    fn make(&mut self, plan: &Plan) -> Result<Placed> {
        let bytes = u64::from(plan.buffers) * slot_bytes(CONFIG_BYTES as u32);
        let configs = sealed_memfd(c"fe-configs", bytes).map_err(|_| Status::IO)?;
        self.with(|b| {
            b.configs = Some(Region {
                addr: CONFIG,
                file: File::from(configs),
                size: bytes,
            });
            b.regions.clear();
            b.buffers = (0..plan.buffers).map(|_| None).collect();
            b.output0_bytes = plan.output0_bytes;
            b.pending.clear();
        });
        Ok(Placed { config: CONFIG })
    }

    fn set_buffer(&mut self, buffer: u32, memory: BorrowedFd<'_>) -> Result<Set> {
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|b| {
            if size < u64::from(b.output0_bytes) {
                return Ok(Set::Refused);
            }
            let busy = b.pending.iter().any(|j| j.buffer == buffer);
            let addr = MEMORY + u64::from(b.given) * MEMORY_SPAN;
            let Some(slot) = b.buffers.get_mut(buffer as usize) else {
                return Ok(Set::Refused);
            };
            if slot.as_ref().is_some_and(|m| same_file(&m.file, &file)) {
                return Ok(Set::Done);
            }
            if busy {
                return Ok(Set::Busy);
            }
            *slot = Some(Region { addr, file, size });
            b.given += 1;
            Ok(Set::Done)
        })
    }

    fn go(&mut self, _start: &Start) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) {
        self.with(|b| {
            b.pending.clear();
            b.regions.clear();
            b.buffers.clear();
        });
    }

    fn write_config(&mut self, slot: u32, config: &[u8; CONFIG_BYTES]) -> Result<()> {
        self.with(|b| {
            let at = u64::from(slot) * slot_bytes(CONFIG_BYTES as u32);
            let configs = b.configs.as_ref().ok_or(Status::NO_DEVICE)?;
            if at + CONFIG_BYTES as u64 > configs.size {
                return Err(Status::INVALID_ARGUMENT);
            }
            configs
                .file
                .write_all_at(config, at)
                .map_err(|_| Status::IO)
        })
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered> {
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|b| {
            let region = match b.regions.iter().position(Option::is_none) {
                Some(at) => at,
                None if b.regions.len() < MAX_REGIONS => {
                    b.regions.push(None);
                    b.regions.len() - 1
                }
                None => return Err(Status::NO_MEMORY),
            };
            let addr = REGIONS + region as u64 * REGION_SPAN;
            b.regions[region] = Some(Region { addr, file, size });
            Ok(Registered {
                region: region as u32,
                addr,
                size,
            })
        })
    }

    fn unregister(&mut self, region: u32) -> Result<()> {
        self.with(|b| match b.regions.get_mut(region as usize) {
            Some(slot @ Some(_)) => {
                *slot = None;
                Ok(())
            }
            _ => Err(Status::NOT_FOUND),
        })
    }

    fn post(&mut self, job: &Job) -> Result<()> {
        let hook = self.with(|b| {
            b.pending.push_back(*job);
            match b.hook.take() {
                Some((identity, run)) if identity == job.identity => Some(run),
                other => {
                    b.hook = other;
                    None
                }
            }
        });
        // Outside the block's lock: the client may use the streams, not the block.
        if let Some(run) = hook {
            run();
        }
        Ok(())
    }

    fn next_finished(&mut self) -> Result<Option<Finished>> {
        Ok(self.with(|b| {
            b.run();
            b.finished.pop_front()
        }))
    }

    fn give_back(&mut self, _buffer: u32) -> Result<()> {
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters::default())
    }
}

/// A configuration for 64 by 32, output 0 on, statistics and output 1 as asked, marked `key`.
fn config_for(key: u8, stats: bool, output1: bool) -> Vec<u8> {
    let mut fe = FrontEnd::new(WIDTH as u16, HEIGHT as u16, Order::Rggb);
    let raw16 = ImageFormatConfig {
        format: formats::BAYER16,
        ..Default::default()
    };
    fe.set_output_format(Branch::Zero, raw16);
    let mut on = en::output(0);
    if output1 {
        fe.set_output_format(Branch::One, raw16);
        on |= en::output(1);
    }
    if stats {
        on |= en::AWB_STATS;
    }
    fe.enable(on, true);
    let mut cfg = fe.prepare().unwrap();
    cfg.bla.black_level_r = u16::from(key) | 0x100;
    assert_eq!(key_of(&cfg), key);
    let mut bytes = vec![0; CONFIG_BYTES];
    cfg.write_to(&mut bytes[..]).unwrap();
    bytes
}

fn driver(host: &Host, hw: &Hw) -> PispFe {
    let stream = |name| -> StreamHandle {
        lemnos_core::Held::boxed(Box::new(host.stream_producer(0, name).unwrap()))
    };
    let link = FeLink {
        lanes: 2,
        mbps: 800,
        vc: 0,
        data_type: 0x2c,
    };
    let mut fe = PispFe::with_hardware(
        Box::new(hw.clone()),
        PispFeNeeds {
            config: stream("config"),
            statistics: Some(stream("statistics")),
            output0: Some(stream("output0")),
            output1: Some(stream("output1")),
        },
        FeOptions::new(link, 8).unwrap(),
    );
    CameraIsp::set_input(
        &mut fe,
        IspInput {
            width: WIDTH,
            height: HEIGHT,
            bit_depth: 12,
            bayer: BayerOrder::default(),
        },
    )
    .unwrap();
    let image = IspOutput {
        width: WIDTH,
        height: HEIGHT,
        fourcc: u32::from_le_bytes(*b"RG16"),
        pad_bytes: 0,
    };
    for output in [2, 3] {
        let layout = CameraIsp::set_output(&mut fe, output, image).unwrap();
        assert_eq!(layout.size, OUTPUT_BYTES);
    }
    fe
}

/// The client's end of a stream.
struct Client {
    lease: Lease,
    ring: Consumer,
}

impl Client {
    fn attach(host: &Host, name: &str) -> Self {
        let lease = host.stream_attach(0, name, CALLER).unwrap();
        let ring = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
        Self { lease, ring }
    }

    /// Writes `data` at the start of `buffer`, through the memory the client was given.
    fn fill(&self, buffer: u32, data: &[u8]) {
        let at = self.lease.layout.buffer_offset(buffer).unwrap();
        assert!(self.lease.buffers.write(at, data));
    }

    /// What the descriptor the client was handed for `buffer` holds.
    fn read(&self, buffer: u32, bytes: usize) -> Vec<u8> {
        let handles = self.lease.handles.as_ref().expect("a stream of dma-bufs");
        let fd: BorrowedFd<'_> = handles[buffer as usize].as_fd();
        let file = File::from(fd.try_clone_to_owned().unwrap());
        let mut out = vec![0; bytes];
        file.read_exact_at(&mut out, 0).unwrap();
        out
    }
}

/// The client of every stream.
struct App {
    config: Client,
    stats: Client,
    output0: Client,
    output1: Client,
}

impl App {
    fn attach(host: &Host) -> Self {
        Self {
            config: Client::attach(host, "config"),
            stats: Client::attach(host, "statistics"),
            output0: Client::attach(host, "output0"),
            output1: Client::attach(host, "output1"),
        }
    }

    fn post(&mut self, identity: u64, config: u32, output0: u32, output1: u32) {
        let job = IspJob {
            identity,
            layout: LAYOUT_FRONT_END,
            config_buffer: config,
            input_buffer: NO_BUFFER,
            outputs: [NO_BUFFER, NO_BUFFER, output0, output1],
        };
        let mut buf = [0; 128];
        let mut w = Writer::new(&mut buf);
        job.encode(&mut w).unwrap();
        let n = w.len();
        assert!(self.config.ring.push_job(&buf[..n]).unwrap());
    }
}

/// One tick posts the job, the next runs and finishes it.
fn two_ticks(fe: &mut PispFe, now: u64) -> u64 {
    let now = fe.tick(now).unwrap();
    fe.tick(now).unwrap()
}

#[test]
fn the_client_reads_what_the_block_made_from_its_configuration() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();

    let mut now = 1;
    for (identity, config, output0, output1, key) in [
        (0x51, 0, 2, NO_BUFFER, 0x11),
        (0x52, 1, 0, 3, 0x22),
        (0x53, 0, 3, 1, 0x33),
        (0x54, 1, 1, NO_BUFFER, 0x44),
    ] {
        let with1 = output1 != NO_BUFFER;
        app.config.fill(config, &config_for(key, true, with1));
        app.post(identity, config, output0, output1);
        now = two_ticks(&mut fe, now);

        let frame = app.output0.ring.pop_frame().unwrap().expect("output 0");
        assert_eq!(
            (frame.buffer, frame.identity, frame.bytes),
            (output0, identity, OUTPUT_BYTES)
        );
        let want = expected(0, key, identity, OUTPUT_BYTES as usize);
        assert!(
            app.output0.read(frame.buffer, want.len()) == want,
            "job {identity:#x}: output 0 isn't what the block made of config {config}"
        );
        assert!(app.output0.ring.release(frame.buffer, frame.fence).unwrap());

        let stats = app.stats.ring.pop_frame().unwrap().expect("statistics");
        assert_eq!(
            (stats.buffer, stats.identity, stats.bytes),
            (config, identity, STATS_BYTES as u32),
            "the statistics buffer numbered as the configuration, with the job's identity"
        );
        let want = expected(2, key, identity, STATS_BYTES);
        assert!(
            app.stats.read(stats.buffer, want.len()) == want,
            "job {identity:#x}: the statistics aren't what the block made"
        );
        assert!(app.stats.ring.release(stats.buffer, stats.fence).unwrap());

        let second = app.output1.ring.pop_frame().unwrap();
        if with1 {
            let second = second.expect("output 1");
            assert_eq!(
                (second.buffer, second.identity, second.bytes),
                (output1, identity, OUTPUT_BYTES)
            );
            let want = expected(1, key, identity, OUTPUT_BYTES as usize);
            assert!(
                app.output1.read(second.buffer, want.len()) == want,
                "job {identity:#x}: output 1 isn't what the block made"
            );
            assert!(
                app.output1
                    .ring
                    .release(second.buffer, second.fence)
                    .unwrap()
            );
        } else {
            assert!(second.is_none(), "output 1 was off");
        }
        now = fe.tick(now).unwrap();
    }
    assert_eq!(hw.with(|b| b.lost), 0, "every result was in memory given");
    assert_eq!(hw.with(|b| b.keys.clone()), [0x11, 0x22, 0x33, 0x44]);
    let counters = CameraIsp::counters(&mut fe).unwrap();
    assert_eq!((counters.jobs, counters.dropped), (4, 0));
    assert_eq!(
        hw.registered(),
        0,
        "nothing lent for a job is still registered"
    );
}

#[test]
fn a_config_changed_after_the_job_was_posted_changes_nothing_the_block_runs() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    let posted = config_for(0x5a, true, false);
    app.config.fill(0, &posted);
    app.post(9, 0, 1, NO_BUFFER);
    let now = fe.tick(1).unwrap();
    assert_eq!(hw.with(|b| b.pending.len()), 1, "taken, not yet run");
    // The driver has taken the job and the block not yet run it: the client changes its mind,
    // even to a configuration the module would refuse.
    let mut changed = config_for(0xa5, true, false);
    changed[0] = 0x40; // the statistics buffer's address
    app.config.fill(0, &changed);
    fe.tick(now).unwrap();
    assert_eq!(hw.with(|b| b.keys.clone()), [0x5a]);
    assert_eq!(hw.with(|b| b.lost), 0);
    assert_eq!(
        &hw.with(|b| b.copy.to_vec())[..],
        &posted[..],
        "the module copied the configuration as it was when the job was taken"
    );
    let frame = app.output0.ring.pop_frame().unwrap().unwrap();
    assert!(
        app.output0.read(frame.buffer, OUTPUT_BYTES as usize)
            == expected(0, 0x5a, 9, OUTPUT_BYTES as usize)
    );
    let stats = app.stats.ring.pop_frame().unwrap().unwrap();
    assert!(app.stats.read(stats.buffer, STATS_BYTES) == expected(2, 0x5a, 9, STATS_BYTES));
}

#[test]
fn a_configuration_with_an_address_never_reaches_the_module() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    let mut wrong = config_for(1, true, false);
    wrong[4] = 0x01; // the statistics buffer's address, high word
    app.config.fill(0, &wrong);
    app.post(1, 0, 0, NO_BUFFER);
    two_ticks(&mut fe, 1);
    assert_eq!(hw.with(|b| b.keys.len() + b.lost as usize), 0);
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 1);
    assert_eq!(hw.registered(), 0);
    assert!(app.output0.ring.pop_frame().unwrap().is_none());
}

#[test]
fn a_job_naming_a_statistics_buffer_the_driver_does_not_hold_is_dropped() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    app.config.fill(0, &config_for(1, true, false));
    app.post(1, 0, 0, NO_BUFFER);
    let mut now = two_ticks(&mut fe, 1);
    let stats = app.stats.ring.pop_frame().unwrap().expect("statistics");
    assert_eq!((stats.buffer, stats.identity), (0, 1));
    let first = expected(2, 1, 1, STATS_BYTES);

    // Statistics buffer 0 is the client's now: the block must not write it again.
    app.config.fill(0, &config_for(2, true, false));
    app.post(2, 0, 1, NO_BUFFER);
    now = two_ticks(&mut fe, now);
    assert!(app.stats.ring.pop_frame().unwrap().is_none());
    assert!(
        app.output0
            .ring
            .pop_frame()
            .unwrap()
            .is_some_and(|f| f.identity == 1),
        "only the first job's image"
    );
    assert!(app.output0.ring.pop_frame().unwrap().is_none());
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 1);
    assert!(app.stats.read(0, STATS_BYTES) == first, "untouched");
    assert_eq!(hw.with(|b| b.keys.clone()), [1]);

    // Given back, it can be a job's again.
    assert!(app.stats.ring.release(stats.buffer, stats.fence).unwrap());
    app.post(3, 0, 2, NO_BUFFER);
    two_ticks(&mut fe, now);
    let stats = app.stats.ring.pop_frame().unwrap().expect("statistics");
    assert_eq!((stats.buffer, stats.identity), (0, 3));
    assert!(app.stats.read(0, STATS_BYTES) == expected(2, 2, 3, STATS_BYTES));
    assert_eq!(hw.with(|b| b.lost), 0);
    assert_eq!(hw.registered(), 0);
}

#[test]
fn a_job_naming_a_statistics_buffer_another_job_has_is_dropped() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    app.config.fill(0, &config_for(1, true, false));
    // Two jobs on configuration 0, taken in one look: the second's statistics would go where
    // the block is still writing the first's.
    app.post(1, 0, 0, NO_BUFFER);
    app.post(2, 0, 1, NO_BUFFER);
    let now = two_ticks(&mut fe, 1);
    assert_eq!(
        hw.with(|b| b.keys.clone()),
        [1],
        "the second never reached the block"
    );
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 1);
    let stats = app.stats.ring.pop_frame().unwrap().expect("statistics");
    assert_eq!((stats.buffer, stats.identity), (0, 1));
    assert!(app.stats.read(0, STATS_BYTES) == expected(2, 1, 1, STATS_BYTES));
    assert!(app.stats.ring.pop_frame().unwrap().is_none());
    fe.tick(now).unwrap();
    assert_eq!(hw.with(|b| b.lost), 0);
    assert_eq!(hw.registered(), 0);
}

#[test]
fn bytes_that_are_no_job_fault_the_config_streams_consumer_and_the_next_one_is_served() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    app.config.fill(0, &config_for(1, true, false));
    assert!(app.config.ring.push_job(&[1, 2, 3]).unwrap());
    app.post(1, 0, 0, NO_BUFFER);
    let now = two_ticks(&mut fe, 1);
    // The client that posted them sees its config stream faulted, and the job it posted behind
    // them never reached the block.
    assert_eq!(app.config.ring.state(), STATE_FAULT);
    assert!(hw.with(|b| b.keys.is_empty()));
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 1);
    let stats = host.stream_stats(0, "config").unwrap();
    assert_eq!((stats.faults, stats.attached), (1, false));
    assert_eq!(stats.consumer_held, 0);
    let stats = host.stream_stats(0, "statistics").unwrap();
    assert_eq!((stats.faults, stats.attached), (0, true));
    // It attaches again, and its job runs on the configuration it writes now.
    app.config = Client::attach(&host, "config");
    app.config.fill(0, &config_for(4, true, false));
    app.post(2, 0, 0, NO_BUFFER);
    let now = two_ticks(&mut fe, now);
    assert_eq!(hw.with(|b| b.keys.clone()), [4]);
    let stats = app.stats.ring.pop_frame().unwrap().expect("statistics");
    assert_eq!((stats.buffer, stats.identity), (0, 2));
    fe.tick(now).unwrap();
    assert_eq!(hw.with(|b| b.lost), 0);
}

#[test]
fn a_new_statistics_client_gets_statistics_in_its_own_memory() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    let now = fe.tick(1).unwrap();
    // The statistics client goes; another takes its place. The driver still holds every
    // statistics buffer from before, with nothing behind them now.
    drop(app.stats);
    host.stream_detach(0, "statistics", CALLER).unwrap();
    app.stats = Client::attach(&host, "statistics");
    app.config.fill(1, &config_for(7, true, false));

    // Buffer 1 is given up and taken again as the new client's within the job, which runs and
    // writes the new client's memory: nothing is dropped.
    app.post(1, 1, 0, NO_BUFFER);
    two_ticks(&mut fe, now);
    let stats = app.stats.ring.pop_frame().unwrap().expect("statistics");
    assert_eq!((stats.buffer, stats.identity), (1, 1));
    assert!(app.stats.read(1, STATS_BYTES) == expected(2, 7, 1, STATS_BYTES));
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 0);
    assert_eq!(hw.with(|b| b.lost), 0);
}

#[test]
fn jobs_in_steady_state_allocate_nothing() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    app.config.fill(0, &config_for(7, true, true));
    app.config.fill(1, &config_for(8, true, true));
    let mut now = 1;
    let mut allocated = 0;
    for identity in 0..64u64 {
        let buffer = (identity % u64::from(BUFFERS)) as u32;
        let config = buffer % CONFIGS;
        app.post(identity, config, buffer, 3 - buffer);
        for _ in 0..2 {
            let (next, n) = allocations(|| fe.tick(now));
            now = next.unwrap();
            // The first jobs size what the stand-in and the host keep.
            if identity >= 8 {
                allocated += n;
            }
        }
        for client in [&mut app.output0, &mut app.stats, &mut app.output1] {
            let frame = client.ring.pop_frame().unwrap().expect("a frame");
            assert_eq!(frame.identity, identity);
            assert!(client.ring.release(frame.buffer, frame.fence).unwrap());
        }
    }
    assert_eq!(allocated, 0, "taking, running and publishing jobs");
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().jobs, 64);
    assert_eq!(hw.with(|b| b.lost), 0);
    assert_eq!(hw.registered(), 0);
}

/// A board whose device is the front end itself, with each image stream as its entry says.
fn bound(statistics: &str, output0: &str, output1: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"isp\"\ndriver = \"pisp-fe\"\n\
         config = {{ device = \"none\", lanes = 2, mbps = 900, data_type = 43 }}\n\
         [devices.streams.config]\nbuffers = {CONFIGS}\nbuffer_bytes = {CONFIG_BYTES}\n\
         job_slots = 8\njob_bytes = 128\n\
         [devices.streams.statistics]\nbuffers = {CONFIGS}\nbuffer_bytes = {STATS_BYTES}\n\
         {statistics}\n\
         [devices.streams.output0]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n{output0}\n\
         [devices.streams.output1]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n{output1}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<PispFe>()],
        Box::new(sim),
        Arc::new(clock),
        HostOptions::default(),
        Arc::new(Handles),
    )
    .unwrap()
}

#[test]
fn a_board_with_an_image_or_statistics_stream_of_plain_memory_is_refused_at_bind() {
    let dma = "memory = \"dma-buf\"";
    for plain in 0..3 {
        let memory = |stream: usize| if stream == plain { "" } else { dma };
        let host = bound(memory(0), memory(1), memory(2));
        host.tick();
        let DeviceState::Backoff { reason, .. } = host.device("isp").unwrap().state else {
            panic!("the driver bound to a stream it cannot use");
        };
        assert!(
            reason.contains(&format!("{:?}", Status::NOT_SUPPORTED)),
            "{reason}"
        );
        // Nobody had to attach for the driver to know.
        assert_eq!(host.stream_stats(0, "output0").unwrap().attachments, 0);
    }
    // With all three as dma-bufs it binds; nothing is opened until `start`.
    let host = bound(dma, dma, dma);
    host.tick();
    assert_eq!(host.device("isp").unwrap().state, DeviceState::Running);
}

#[test]
fn an_output_1_buffer_given_back_and_named_again_within_one_pump_completes_its_job() {
    let host = host();
    let hw = Hw::default();
    let app = Arc::new(Mutex::new(App::attach(&host)));
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    {
        let mut app = app.lock().unwrap();
        app.config.fill(0, &config_for(0x11, false, true));
        app.config.fill(1, &config_for(0x22, false, false));
        app.post(1, 0, 1, 0);
    }
    // Job A runs and publishes its output 1 frame on buffer 0. The consumer has not popped it.
    let now = two_ticks(&mut fe, 1);

    // Job X is queued before the pump. When the pump posts X to the block, the consumer pops
    // A's output 1 frame, gives buffer 0 back and names it in job B, all before the pump reads B.
    app.lock().unwrap().post(2, 1, 3, NO_BUFFER);
    let shared = Arc::clone(&app);
    hw.hook_after_post(2, move || {
        let mut app = shared.lock().unwrap();
        let frame = app
            .output1
            .ring
            .pop_frame()
            .unwrap()
            .expect("job A's output 1");
        assert_eq!((frame.buffer, frame.identity), (0, 1));
        assert!(
            app.output1.read(frame.buffer, OUTPUT_BYTES as usize)
                == expected(1, 0x11, 1, OUTPUT_BYTES as usize)
        );
        assert!(app.output1.ring.release(frame.buffer, frame.fence).unwrap());
        app.post(3, 0, 2, 0);
    });
    let now = fe.tick(now).unwrap();
    fe.tick(now).unwrap();

    let mut app = app.lock().unwrap();
    let a = app
        .output0
        .ring
        .pop_frame()
        .unwrap()
        .expect("job A's output 0");
    assert_eq!((a.buffer, a.identity), (1, 1));
    let x = app
        .output0
        .ring
        .pop_frame()
        .unwrap()
        .expect("job X's output 0");
    assert_eq!((x.buffer, x.identity), (3, 2));
    assert!(
        app.output0.read(x.buffer, OUTPUT_BYTES as usize)
            == expected(0, 0x22, 2, OUTPUT_BYTES as usize)
    );
    let b = app
        .output0
        .ring
        .pop_frame()
        .unwrap()
        .expect("job B's output 0");
    assert_eq!((b.buffer, b.identity), (2, 3));
    assert!(
        app.output0.read(b.buffer, OUTPUT_BYTES as usize)
            == expected(0, 0x11, 3, OUTPUT_BYTES as usize)
    );
    let second = app
        .output1
        .ring
        .pop_frame()
        .unwrap()
        .expect("job B's output 1");
    assert_eq!((second.buffer, second.identity), (0, 3));
    assert!(
        app.output1.read(second.buffer, OUTPUT_BYTES as usize)
            == expected(1, 0x11, 3, OUTPUT_BYTES as usize)
    );
    assert_eq!(hw.with(|b| b.keys.clone()), [0x11, 0x22, 0x11]);
    assert_eq!(hw.with(|b| b.lost), 0, "every result was in memory given");
    let counters = CameraIsp::counters(&mut fe).unwrap();
    assert_eq!((counters.jobs, counters.dropped), (3, 0));
    assert_eq!(hw.registered(), 0);
}
