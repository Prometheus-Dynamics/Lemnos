//! The back end end to end through the host's own streams, as a client of the daemon holds them.
//! The client writes a raw frame into an `input` buffer and a configuration into a `config`
//! buffer, posts a job on the `config` stream, and reads the image through the descriptor it was
//! handed for the `output0` buffer. A stand-in for the block reads the input by device address
//! from the memory the driver gave it for the job, and writes an image derived from the input and
//! from the configuration it was given into the output's memory, as the block's DMA would. The
//! client must read exactly that image: right numbers on the ring over memory the block never
//! touched, or an image made from memory the client never wrote, is the failure this test is for.
//!
//! Sealed memfds stand in for the streams' dma-bufs and for the module's pool.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{BayerOrder, CameraIsp, IspInput, IspJob, IspOutput, NO_BUFFER};
use lemnos_contract_isp_pisp::LAYOUT_BACK_END;
use lemnos_core::{CallerId, Encode, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_driver_pisp::be::BackEnd;
use lemnos_driver_pisp::be_hw::{Finished, Hardware, Placed, Plan, Registered};
use lemnos_driver_pisp::be_job::{KernelJob, addr_output};
use lemnos_driver_pisp::format::{compute_stride_align, formats};
use lemnos_driver_pisp::uapi::{BayerOrder as Order, BeTilesConfig, ImageFormatConfig, Wire};
use lemnos_driver_pisp::{BeOptions as Options, PispBe, PispBeNeeds};
use lemnos_driver_pisp_sys::CONFIG_BYTES;
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
/// RAW16 at 64 by 32, lines on 64 bytes.
const INPUT_BYTES: u32 = WIDTH * 2 * HEIGHT;
/// NV12 at 64 by 32: the luma plane, then the chroma plane at half height.
const OUTPUT_BYTES: u32 = WIDTH * HEIGHT * 3 / 2;
const BUFFERS: u32 = 4;
/// Where the stand-in puts the module's pool, and where it puts what is registered.
const POOL: u64 = 0x1000_0000;
const REGIONS: u64 = 0x8000_0000;
const REGION_SPAN: u64 = 0x0100_0000;
const CALLER: CallerId = CallerId(5);

/// The raw frame the client writes for a job.
fn raw(seed: u64, at: usize) -> u8 {
    (seed as usize).wrapping_mul(13).wrapping_add(at * 5) as u8
}

/// What the block makes of `input` under a configuration with `key`: every output byte comes
/// from an input byte, so an image made from the wrong memory or the wrong configuration shows.
fn derived(input: &[u8], key: u8, at: usize) -> u8 {
    input[at % input.len()].wrapping_add(key) | 1
}

/// The configuration's mark: its red gain, which the block reads like any other setting.
fn key_of(config: &BeTilesConfig) -> u8 {
    config.config.wbg.gain_r as u8
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
            let fd = sealed_memfd(c"be-frames", layout.stride()).map_err(|_| Status::IO)?;
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

fn board(input_memory: &str, output_memory: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"isp\"\ndriver = \"fake-counter\"\n\
         [devices.streams.config]\nbuffers = 2\nbuffer_bytes = {CONFIG_BYTES}\n\
         job_slots = 8\njob_bytes = 128\n\
         [devices.streams.input]\nbuffers = {BUFFERS}\nbuffer_bytes = {INPUT_BYTES}\n{input_memory}\n\
         [devices.streams.output0]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n\
         {output_memory}\n"
    )
}

fn host_with(text: &str) -> Host {
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        HostOptions::default(),
        Arc::new(Handles),
    )
    .unwrap()
}

fn host() -> Host {
    let dma = "memory = \"dma-buf\"";
    host_with(&board(dma, dma))
}

// ---- The block: reads and writes where a job says, by device address, from the config it got.

/// Memory the block can reach, by device address.
struct Region {
    addr: u64,
    file: File,
    size: u64,
}

/// What the block is working on: a job and the configuration it was posted with.
struct Pending {
    job: KernelJob,
    config: usize,
}

struct Block {
    /// The pool, then what was registered; a slot let go of is `None` and is used again.
    regions: Vec<Option<Region>>,
    pending: Vec<Pending>,
    /// Configuration copies, made once so the block allocates nothing while running.
    configs: Vec<Box<[u8; CONFIG_BYTES]>>,
    next_config: usize,
    finished: Vec<Finished>,
    input: Vec<u8>,
    output: Vec<u8>,
    parsed: Box<BeTilesConfig>,
    /// Jobs with an image in no memory the block was given.
    lost: u32,
    /// The configuration of each job run, by its key.
    keys: Vec<u8>,
    /// Run once, after the job with this identity is posted: the client's part of a race.
    hook: Option<(u64, Box<dyn FnOnce() + Send>)>,
}

impl Default for Block {
    fn default() -> Self {
        Self {
            regions: Vec::with_capacity(16),
            pending: Vec::with_capacity(16),
            configs: (0..16).map(|_| Box::new([0; CONFIG_BYTES])).collect(),
            next_config: 0,
            finished: Vec::with_capacity(16),
            input: Vec::with_capacity(INPUT_BYTES as usize),
            output: Vec::with_capacity(OUTPUT_BYTES as usize),
            parsed: Box::default(),
            lost: 0,
            keys: Vec::with_capacity(1024),
            hook: None,
        }
    }
}

/// The memory `len` bytes at `addr` are in, and where in it.
fn region(regions: &[Option<Region>], addr: u64, len: usize) -> Option<(&File, u64)> {
    regions.iter().flatten().find_map(|r| {
        let end = addr.checked_add(len as u64)?;
        (addr >= r.addr && end <= r.addr + r.size).then(|| (&r.file, addr - r.addr))
    })
}

impl Block {
    /// Runs the oldest job, as the block would once it got to it.
    fn run(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let Pending { job, config } = self.pending.remove(0);
        let bytes = &self.configs[config];
        self.parsed.read_from(&bytes[..]).unwrap();
        let key = key_of(&self.parsed);
        let input = &self.parsed.config.input_format;
        let input_len = input.stride as usize * usize::from(input.height);
        self.input.resize(input_len, 0);
        let read = match region(&self.regions, job.addr[0], input_len) {
            Some((file, at)) => file.read_exact_at(&mut self.input, at).is_ok(),
            None => false,
        };
        let luma = (WIDTH * HEIGHT) as usize;
        self.output.clear();
        let input = &self.input;
        self.output
            .extend((0..OUTPUT_BYTES as usize).map(|at| derived(input, key, at)));
        let wrote = read
            && [
                (job.addr[addr_output(0, 0)], 0..luma),
                (job.addr[addr_output(0, 1)], luma..OUTPUT_BYTES as usize),
            ]
            .into_iter()
            .all(|(addr, range)| {
                let data = &self.output[range];
                region(&self.regions, addr, data.len())
                    .is_some_and(|(file, at)| file.write_all_at(data, at).is_ok())
            });
        if !wrote {
            self.lost += 1;
        }
        self.keys.push(key);
        self.finished.push(Finished {
            buffer: job.buffer,
            bytes: 4096,
            sequence: job.identity,
            identity: job.identity,
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
        self.with(|b| b.regions.iter().skip(1).flatten().count())
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

    fn start(&mut self, plan: &Plan) -> Result<Placed> {
        assert_eq!(plan.tdn_bytes, None);
        let pool_bytes = 4096 * u64::from(plan.buffers);
        let pool = sealed_memfd(c"be-pool", pool_bytes).map_err(|_| Status::IO)?;
        self.with(|b| {
            b.regions.clear();
            b.regions.push(Some(Region {
                addr: POOL,
                file: File::from(pool),
                size: pool_bytes,
            }));
        });
        Ok(Placed { tdn: None })
    }

    fn stop(&mut self) {
        self.with(|b| b.regions.clear());
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered> {
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|b| {
            let free = b.regions.iter().skip(1).position(Option::is_none);
            let region = match free {
                Some(at) => at + 1,
                None => {
                    b.regions.push(None);
                    b.regions.len() - 1
                }
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
            Some(slot @ Some(_)) if region != 0 => {
                *slot = None;
                Ok(())
            }
            _ => Err(Status::NOT_FOUND),
        })
    }

    fn post(&mut self, job: &KernelJob, config: &[u8; CONFIG_BYTES]) -> Result<()> {
        let hook = self.with(|b| {
            let at = b.next_config;
            b.next_config = (at + 1) % b.configs.len();
            b.configs[at].copy_from_slice(config);
            b.pending.push(Pending {
                job: *job,
                config: at,
            });
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
            if b.finished.is_empty() {
                None
            } else {
                Some(b.finished.remove(0))
            }
        }))
    }

    fn give_back(&mut self, _buffer: u32) -> Result<()> {
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        Ok(())
    }
}

/// A configuration for 64 by 32 RAW16 in, NV12 out, marked with `key`.
fn config_for(key: u8) -> Vec<u8> {
    let mut input = ImageFormatConfig {
        width: WIDTH as u16,
        height: HEIGHT as u16,
        format: formats::BAYER16,
        ..Default::default()
    };
    compute_stride_align(&mut input, 64);
    let mut cfg: BeTilesConfig = BackEnd::simple_bayer(
        input,
        Order::Bggr,
        4096,
        (1.5, 1.0, 1.8),
        None,
        formats::NV12,
        None,
    )
    .prepare()
    .unwrap();
    cfg.config.wbg.gain_r = u16::from(key) | 0x100;
    assert_eq!(key_of(&cfg), key);
    let mut bytes = vec![0; CONFIG_BYTES];
    cfg.write_to(&mut bytes[..]).unwrap();
    bytes
}

fn driver(host: &Host, hw: &Hw) -> PispBe {
    let stream = |name| -> StreamHandle {
        lemnos_core::Held::boxed(Box::new(host.stream_producer(0, name).unwrap()))
    };
    let mut be = PispBe::with_hardware(
        Box::new(hw.clone()),
        PispBeNeeds {
            config: stream("config"),
            input: stream("input"),
            output0: stream("output0"),
            output1: None,
        },
        Options::new(BUFFERS, 4).unwrap(),
    );
    CameraIsp::set_input(
        &mut be,
        IspInput {
            width: WIDTH,
            height: HEIGHT,
            bit_depth: 12,
            bayer: BayerOrder::default(),
        },
    )
    .unwrap();
    let layout = CameraIsp::set_output(
        &mut be,
        0,
        IspOutput {
            width: WIDTH,
            height: HEIGHT,
            fourcc: u32::from_le_bytes(*b"NV12"),
            pad_bytes: 0,
        },
    )
    .unwrap();
    assert_eq!(layout.size, OUTPUT_BYTES);
    be
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

    fn post(&mut self, identity: u64, config: u32, input: u32, output: u32) {
        let job = IspJob {
            identity,
            layout: LAYOUT_BACK_END,
            config_buffer: config,
            input_buffer: input,
            outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
        };
        let mut buf = [0; 128];
        let mut w = Writer::new(&mut buf);
        job.encode(&mut w).unwrap();
        let n = w.len();
        assert!(self.ring.push_job(&buf[..n]).unwrap());
    }

    /// What the descriptor the client was handed for `buffer` holds.
    fn read(&self, buffer: u32, bytes: u32) -> Vec<u8> {
        let handles = self.lease.handles.as_ref().expect("a stream of dma-bufs");
        let fd: BorrowedFd<'_> = handles[buffer as usize].as_fd();
        let file = File::from(fd.try_clone_to_owned().unwrap());
        let mut out = vec![0; bytes as usize];
        file.read_exact_at(&mut out, 0).unwrap();
        out
    }
}

/// The client of all three streams.
struct App {
    config: Client,
    input: Client,
    output: Client,
}

impl App {
    fn attach(host: &Host) -> Self {
        Self {
            config: Client::attach(host, "config"),
            input: Client::attach(host, "input"),
            output: Client::attach(host, "output0"),
        }
    }

    /// The raw frame for `seed`, written into input buffer `buffer`.
    fn give_input(&self, buffer: u32, seed: u64) -> Vec<u8> {
        let frame: Vec<u8> = (0..INPUT_BYTES as usize).map(|at| raw(seed, at)).collect();
        self.input.fill(buffer, &frame);
        frame
    }
}

fn expected(input: &[u8], key: u8) -> Vec<u8> {
    (0..OUTPUT_BYTES as usize)
        .map(|at| derived(input, key, at))
        .collect()
}

#[test]
fn the_client_reads_what_the_block_made_from_its_input_and_config() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();

    let mut now = 1;
    for (identity, config, input, output, key) in [
        (0x51, 0, 1, 2, 0x11),
        (0x52, 1, 3, 0, 0x22),
        (0x53, 0, 0, 3, 0x33),
        (0x54, 1, 1, 1, 0x44),
    ] {
        app.config.fill(config, &config_for(key));
        let frame_in = app.give_input(input, identity);
        app.config.post(identity, config, input, output);
        // One tick posts the job, the next finds it finished.
        now = be.tick(now).unwrap();
        now = be.tick(now).unwrap();
        let frame = app
            .output
            .ring
            .pop_frame()
            .unwrap()
            .expect("a frame for the job");
        assert_eq!(
            (frame.buffer, frame.identity, frame.bytes),
            (output, identity, OUTPUT_BYTES)
        );
        let seen = app.output.read(frame.buffer, frame.bytes);
        let want = expected(&frame_in, key);
        assert!(
            seen == want,
            "job {identity:#x}: the client's output doesn't hold what the block made of its \
             input {input} and config {config} (first bytes {:?}, expected {:?})",
            &seen[..8],
            &want[..8]
        );
        assert!(app.output.ring.release(frame.buffer, frame.fence).unwrap());
        now = be.tick(now).unwrap();
    }
    assert_eq!(
        hw.with(|b| b.lost),
        0,
        "every image was memory the block was given"
    );
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 0);
    assert_eq!(CameraIsp::counters(&mut be).unwrap().jobs, 4);
    assert_eq!(
        hw.registered(),
        0,
        "nothing lent for a job is still registered"
    );
}

#[test]
fn a_config_changed_after_the_driver_read_it_changes_nothing_the_block_runs() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    let posted = config_for(0x5a);
    app.config.fill(0, &posted);
    let frame_in = app.give_input(2, 9);
    app.config.post(9, 0, 2, 1);
    let now = be.tick(1).unwrap();
    // The driver has taken the job and the block not yet run it: the client changes its mind.
    let mut changed = config_for(0xa5);
    changed[200] ^= 0xff;
    app.config.fill(0, &changed);
    be.tick(now).unwrap();
    assert_eq!(
        &hw.with(|b| b.configs[0].to_vec())[..],
        &posted[..],
        "the module got the configuration as it was when the job was taken"
    );
    assert_eq!(hw.with(|b| b.keys.clone()), [0x5a]);
    let frame = app.output.ring.pop_frame().unwrap().unwrap();
    assert!(app.output.read(frame.buffer, frame.bytes) == expected(&frame_in, 0x5a));
}

#[test]
fn a_job_naming_an_input_buffer_the_driver_does_not_hold_is_dropped() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    app.config.fill(0, &config_for(1));
    app.give_input(1, 1);
    // Not on the stream at all.
    app.config.post(1, 0, BUFFERS, 0);
    // The same input twice in one look: the second while the block has it.
    app.config.post(2, 0, 1, 1);
    app.config.post(3, 0, 1, 2);
    let now = be.tick(1).unwrap();
    be.tick(now).unwrap();
    let frame = app.output.ring.pop_frame().unwrap().unwrap();
    assert_eq!((frame.identity, frame.buffer), (2, 1));
    assert!(app.output.ring.pop_frame().unwrap().is_none());
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 2);
    assert_eq!(hw.with(|b| b.lost), 0);
    assert_eq!(hw.registered(), 0);
}

#[test]
fn bytes_that_are_no_job_fault_the_config_streams_consumer_and_the_next_one_is_served() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    app.config.fill(0, &config_for(1));
    let frame_in = app.give_input(1, 5);
    assert!(app.config.ring.push_job(&[1, 2, 3]).unwrap());
    app.config.post(1, 0, 1, 0);
    let mut now = be.tick(1).unwrap();
    now = be.tick(now).unwrap();
    // The client that posted them sees its config stream faulted, and the job it posted behind
    // them never reached the block.
    assert_eq!(app.config.ring.state(), STATE_FAULT);
    assert!(app.output.ring.pop_frame().unwrap().is_none());
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);
    let stats = host.stream_stats(0, "config").unwrap();
    assert_eq!((stats.faults, stats.attached), (1, false));
    assert_eq!(stats.consumer_held, 0);
    for other in ["input", "output0"] {
        let stats = host.stream_stats(0, other).unwrap();
        assert_eq!((stats.faults, stats.attached), (0, true), "{other}");
    }
    // It attaches again, and its job runs on the configuration it writes now.
    app.config = Client::attach(&host, "config");
    app.config.fill(0, &config_for(3));
    app.config.post(2, 0, 1, 0);
    now = be.tick(now).unwrap();
    be.tick(now).unwrap();
    let frame = app.output.ring.pop_frame().unwrap().expect("a frame");
    assert_eq!(frame.identity, 2);
    assert!(app.output.read(frame.buffer, frame.bytes) == expected(&frame_in, 3));
    assert_eq!(hw.with(|b| b.lost), 0);
}

#[test]
fn a_new_input_client_gives_frames_from_its_own_memory() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    let mut now = be.tick(1).unwrap();
    // The input's client goes; another takes its place. The driver still holds every input
    // buffer from before, with nothing behind them now.
    drop(app.input);
    host.stream_detach(0, "input", CALLER).unwrap();
    app.input = Client::attach(&host, "input");
    app.config.fill(0, &config_for(3));
    let frame_in = app.give_input(2, 77);

    // Buffer 2 is given up and taken again as the new client's within the job, which runs on
    // the new client's memory: nothing is dropped.
    app.config.post(1, 0, 2, 0);
    now = be.tick(now).unwrap();
    be.tick(now).unwrap();
    let frame = app.output.ring.pop_frame().unwrap().expect("a frame");
    assert_eq!(frame.identity, 1);
    assert!(app.output.read(frame.buffer, frame.bytes) == expected(&frame_in, 3));
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 0);
    assert_eq!(hw.with(|b| b.lost), 0);
}

#[test]
fn a_job_on_an_output_the_client_holds_is_dropped() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    app.config.fill(0, &config_for(1));
    let first = app.give_input(0, 1);
    app.config.post(1, 0, 0, 1);
    let mut now = be.tick(1).unwrap();
    now = be.tick(now).unwrap();
    let frame = app.output.ring.pop_frame().unwrap().unwrap();
    // Output 1 is the client's now: the block must not write it again.
    app.give_input(0, 2);
    app.config.post(2, 0, 0, 1);
    now = be.tick(now).unwrap();
    be.tick(now).unwrap();
    assert!(app.output.ring.pop_frame().unwrap().is_none());
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);
    assert!(
        app.output.read(1, OUTPUT_BYTES) == expected(&first, 1),
        "untouched"
    );
    assert!(app.output.ring.release(frame.buffer, frame.fence).unwrap());
}

#[test]
fn streams_without_handles_are_refused_with_or_without_a_client() {
    let dma = "memory = \"dma-buf\"";
    for text in [board("", dma), board(dma, "")] {
        let host = host_with(&text);
        let hw = Hw::default();
        let mut be = driver(&host, &hw);
        // Nobody is attached yet, and the streams already say what they are.
        assert_eq!(CameraIsp::start(&mut be), Err(Status::NOT_SUPPORTED));
        let mut app = App::attach(&host);
        assert_eq!(CameraIsp::start(&mut be), Err(Status::NOT_SUPPORTED));
        assert_eq!(be.tick(1), None, "not running");
        assert!(app.output.ring.pop_frame().unwrap().is_none());
        assert_eq!(hw.with(|b| b.finished.len() + b.lost as usize), 0);
        assert_eq!(hw.registered(), 0);
    }
}

/// A board whose device is the back end itself, with its streams as `input` and `output` say.
fn bound(input_memory: &str, output_memory: &str) -> Host {
    let text = board(input_memory, output_memory).replace(
        "driver = \"fake-counter\"",
        "driver = \"pisp-be\"\nconfig = { device = \"none\" }",
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<PispBe>()],
        Box::new(sim),
        Arc::new(clock),
        HostOptions::default(),
        Arc::new(Handles),
    )
    .unwrap()
}

#[test]
fn a_board_with_an_image_stream_of_plain_memory_is_refused_at_bind() {
    let dma = "memory = \"dma-buf\"";
    for (input, output) in [("", dma), (dma, "")] {
        let host = bound(input, output);
        host.tick();
        let DeviceState::Backoff { reason, .. } = host.device("isp").unwrap().state else {
            panic!("the driver bound to a stream it cannot use");
        };
        assert!(
            reason.contains(&format!("{:?}", Status::NOT_SUPPORTED)),
            "{reason}"
        );
        // Nobody had to attach for the driver to know.
        assert_eq!(host.stream_stats(0, "input").unwrap().attachments, 0);
    }
    // With both as dma-bufs it binds; nothing is opened until `start`.
    let host = bound(dma, dma);
    host.tick();
    assert_eq!(host.device("isp").unwrap().state, DeviceState::Running);
}

#[test]
fn jobs_in_steady_state_allocate_nothing() {
    let host = host();
    let hw = Hw::default();
    let mut app = App::attach(&host);
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    app.config.fill(0, &config_for(7));
    app.config.fill(1, &config_for(8));
    for input in 0..BUFFERS {
        app.give_input(input, u64::from(input));
    }
    let mut now = 1;
    let mut allocated = 0;
    for identity in 0..64u64 {
        let buffer = (identity % u64::from(BUFFERS)) as u32;
        app.config.post(identity, buffer % 2, buffer, buffer);
        for _ in 0..2 {
            let (next, n) = allocations(|| be.tick(now));
            now = next.unwrap();
            // The first jobs size what the stand-in and the host keep.
            if identity >= 8 {
                allocated += n;
            }
        }
        let frame = app.output.ring.pop_frame().unwrap().expect("a frame");
        assert_eq!(frame.identity, identity);
        assert!(app.output.ring.release(frame.buffer, frame.fence).unwrap());
    }
    assert_eq!(allocated, 0, "taking, running and publishing jobs");
    assert_eq!(CameraIsp::counters(&mut be).unwrap().jobs, 64);
    assert_eq!(hw.with(|b| b.lost), 0);
}

#[test]
fn a_buffer_given_back_and_named_again_within_one_pump_completes_its_job() {
    let host = host();
    let hw = Hw::default();
    let app = Arc::new(Mutex::new(App::attach(&host)));
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    app.lock().unwrap().config.fill(0, &config_for(0x11));
    let frame_a = app.lock().unwrap().give_input(0, 1);
    let frame_b = app.lock().unwrap().give_input(1, 2);
    let frame_x = app.lock().unwrap().give_input(2, 3);

    // Job A runs and publishes its frame on output buffer 0. The consumer has not popped it.
    app.lock().unwrap().config.post(1, 0, 0, 0);
    let now = be.tick(1).unwrap();
    let now = be.tick(now).unwrap();

    // Job X is queued before the pump. When the pump posts X to the block, the consumer pops
    // A's frame, gives buffer 0 back and names it in job B, all before the pump reads B.
    app.lock().unwrap().config.post(2, 0, 2, 1);
    let shared = Arc::clone(&app);
    hw.hook_after_post(2, move || {
        let mut app = shared.lock().unwrap();
        let frame = app.output.ring.pop_frame().unwrap().expect("job A's frame");
        assert_eq!((frame.buffer, frame.identity), (0, 1));
        assert!(app.output.read(frame.buffer, frame.bytes) == expected(&frame_a, 0x11));
        assert!(app.output.ring.release(frame.buffer, frame.fence).unwrap());
        app.config.post(3, 0, 1, 0);
    });
    let now = be.tick(now).unwrap();
    be.tick(now).unwrap();

    let mut app = app.lock().unwrap();
    let x = app.output.ring.pop_frame().unwrap().expect("job X's frame");
    assert_eq!((x.buffer, x.identity), (1, 2));
    assert!(app.output.read(x.buffer, x.bytes) == expected(&frame_x, 0x11));
    let b = app.output.ring.pop_frame().unwrap().expect("job B's frame");
    assert_eq!((b.buffer, b.identity), (0, 3));
    assert!(app.output.read(b.buffer, b.bytes) == expected(&frame_b, 0x11));
    assert_eq!(hw.with(|b| b.keys.clone()), [0x11, 0x11, 0x11]);
    assert_eq!(
        hw.with(|b| b.lost),
        0,
        "every image was memory the block was given"
    );
    let counters = CameraIsp::counters(&mut be).unwrap();
    assert_eq!((counters.jobs, counters.dropped), (3, 0));
    assert_eq!(hw.registered(), 0);
}
