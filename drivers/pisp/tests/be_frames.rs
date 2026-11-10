//! The back end's output 0 through the host's own frame stream, end to end: a consumer attaches
//! to the `frames` stream as a client of the daemon does, posts a job on the `jobs` stream, and
//! reads the frame through the descriptor it was handed for the buffer. A stand-in for the block
//! writes a known pattern into the memory the driver gave it for that job, by device address, as
//! the block's DMA would. The consumer must read that pattern: correct numbers on the ring over a
//! buffer holding nothing the block wrote is the failure this test exists for.
//!
//! Sealed memfds stand in for the stream's dma-bufs, and for the driver's own pool.

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
use lemnos_driver_pisp::{BeOptions as Options, PispBe};
use lemnos_driver_pisp_sys::CONFIG_BYTES;
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::{Backing, BufferMemory, HeapAllocator, Lease, StreamAllocator};
use lemnos_host::{Host, Options as HostOptions, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_sim::SimBoard;
use lemnos_wire::ring::{Consumer, Layout};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
/// NV12 at 64 by 32: the luma plane, then the chroma plane at half height.
const OUTPUT_BYTES: u32 = WIDTH * HEIGHT * 3 / 2;
const BUFFERS: u32 = 4;
/// Where the stand-in puts the driver's own pool, and where it puts what is registered.
const POOL: u64 = 0x1000_0000;
const POOL_STRIDE: u64 = 0x1000;
const INPUT: u64 = 0x2000_0000;
const REGIONS: u64 = 0x8000_0000;
const REGION_SPAN: u64 = 0x0100_0000;
const CALLER: CallerId = CallerId(5);

/// What the block writes into output 0 for the job with `identity`.
fn pattern(identity: u64, at: usize) -> u8 {
    (identity as usize).wrapping_mul(31).wrapping_add(at * 7) as u8 | 1
}

// ---- The stream's memory, as the daemon makes it: one descriptor per buffer.

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

fn host() -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"isp\"\ndriver = \"fake-counter\"\n\
         [devices.streams.jobs]\nbuffers = 1\nbuffer_bytes = 64\njob_slots = 4\njob_bytes = 128\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n\
         memory = \"dma-buf\"\n"
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

// ---- The block: writes what a job says, where the job says, by device address.

/// Memory the block can reach, by device address.
struct Region {
    addr: u64,
    file: File,
    size: u64,
}

#[derive(Default)]
struct Block {
    config: Vec<u8>,
    /// The driver's pool, then what it registered.
    regions: Vec<Option<Region>>,
    finished: Vec<Finished>,
    /// Jobs whose output 0 address was in no memory the block was given.
    lost: u32,
}

impl Block {
    fn write(&self, addr: u64, data: &[u8]) -> bool {
        self.regions.iter().flatten().any(|r| {
            let end = addr.checked_add(data.len() as u64);
            addr >= r.addr
                && end.is_some_and(|end| end <= r.addr + r.size)
                && r.file.write_all_at(data, addr - r.addr).is_ok()
        })
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Block>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Block) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

impl Hardware for Hw {
    fn hw_version(&self) -> u32 {
        0x0225_2700
    }

    fn start(&mut self, plan: &Plan) -> Result<Placed> {
        let pool_bytes = POOL_STRIDE * u64::from(plan.buffers);
        assert!(u64::from(plan.output0_bytes) <= POOL_STRIDE);
        let pool = sealed_memfd(c"be-pool", pool_bytes).map_err(|_| Status::IO)?;
        self.with(|b| {
            b.regions.clear();
            b.regions.push(Some(Region {
                addr: POOL,
                file: File::from(pool),
                size: pool_bytes,
            }));
        });
        Ok(Placed {
            pool: POOL,
            pool_stride: POOL_STRIDE,
            input: INPUT,
            output1: None,
            tdn: None,
        })
    }

    fn stop(&mut self) {
        self.with(|b| b.regions.clear());
    }

    fn read_config(&mut self, _slot: u32, out: &mut [u8; CONFIG_BYTES]) -> Result<()> {
        self.with(|b| out.copy_from_slice(&b.config));
        Ok(())
    }

    fn register(&mut self, memory: BorrowedFd<'_>) -> Result<Registered> {
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|b| {
            let region = b.regions.len() as u32;
            let addr = REGIONS + u64::from(region) * REGION_SPAN;
            b.regions.push(Some(Region { addr, file, size }));
            Ok(Registered { region, addr, size })
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

    fn post(&mut self, job: &KernelJob) -> Result<()> {
        self.with(|b| {
            let luma = (WIDTH * HEIGHT) as usize;
            let image: Vec<u8> = (0..OUTPUT_BYTES as usize)
                .map(|at| pattern(job.identity, at))
                .collect();
            let wrote = b.write(job.addr[addr_output(0, 0)], &image[..luma])
                && b.write(job.addr[addr_output(0, 1)], &image[luma..]);
            if !wrote {
                b.lost += 1;
            }
            b.finished.push(Finished {
                buffer: job.buffer,
                bytes: OUTPUT_BYTES,
                sequence: job.identity,
                identity: job.identity,
                timestamp_ns: 1,
            });
        });
        Ok(())
    }

    fn next_finished(&mut self) -> Result<Option<Finished>> {
        Ok(self.with(|b| {
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

fn config() -> Vec<u8> {
    let mut input = ImageFormatConfig {
        width: WIDTH as u16,
        height: HEIGHT as u16,
        format: formats::BAYER16,
        ..Default::default()
    };
    compute_stride_align(&mut input, 64);
    let cfg: BeTilesConfig = BackEnd::simple_bayer(
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
    let mut bytes = vec![0; CONFIG_BYTES];
    cfg.write_to(&mut bytes[..]).unwrap();
    bytes
}

fn driver(host: &Host, hw: &Hw) -> PispBe {
    let stream = |name| -> StreamHandle { Box::new(host.stream_producer(0, name).unwrap()) };
    let mut be = PispBe::with_hardware(
        Box::new(hw.clone()),
        Some(stream("jobs")),
        Some(stream("frames")),
        Options::new(BUFFERS, 4, "cfg", ["frames", "b"]).unwrap(),
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

/// The consumer's end of a stream, as a client holds it.
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

    fn post(&mut self, identity: u64, output: u32) {
        let job = IspJob {
            identity,
            layout: LAYOUT_BACK_END,
            config_buffer: 0,
            input_buffer: 0,
            outputs: [output, NO_BUFFER, NO_BUFFER, NO_BUFFER],
        };
        let mut buf = [0; 128];
        let mut w = Writer::new(&mut buf);
        job.encode(&mut w).unwrap();
        let n = w.len();
        assert!(self.ring.push_job(&buf[..n]).unwrap());
    }

    /// What the descriptor the consumer was handed for `buffer` holds.
    fn read(&self, buffer: u32, bytes: u32) -> Vec<u8> {
        let handles = self.lease.handles.as_ref().expect("a stream of dma-bufs");
        let fd: BorrowedFd<'_> = handles[buffer as usize].as_fd();
        let file = File::from(fd.try_clone_to_owned().unwrap());
        let mut out = vec![0; bytes as usize];
        file.read_exact_at(&mut out, 0).unwrap();
        out
    }
}

#[test]
fn the_consumer_reads_what_the_block_wrote_for_its_job() {
    let host = host();
    let hw = Hw::default();
    hw.with(|b| b.config = config());
    let mut frames = Client::attach(&host, "frames");
    let mut jobs = Client::attach(&host, "jobs");
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();

    let mut now = 1;
    for (identity, output) in [(0x51, 2), (0x52, 0), (0x53, 3)] {
        jobs.post(identity, output);
        // One tick posts the job, the next finds it finished.
        now = be.tick(now).unwrap();
        now = be.tick(now).unwrap();
        let frame = frames
            .ring
            .pop_frame()
            .unwrap()
            .expect("a frame for the job");
        assert_eq!(
            (frame.buffer, frame.identity, frame.bytes),
            (output, identity, OUTPUT_BYTES)
        );
        let seen = frames.read(frame.buffer, frame.bytes);
        let expected: Vec<u8> = (0..OUTPUT_BYTES as usize)
            .map(|at| pattern(identity, at))
            .collect();
        assert!(
            seen == expected,
            "frame {identity:#x} in buffer {output}: the consumer's buffer doesn't hold what the \
             block wrote (first bytes {:?}, expected {:?})",
            &seen[..8],
            &expected[..8]
        );
        assert!(frames.ring.release(frame.buffer, frame.fence).unwrap());
        now = be.tick(now).unwrap();
    }
    assert_eq!(
        hw.with(|b| b.lost),
        0,
        "every job's output was memory the block was given"
    );
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 0);
    // Nothing the block was lent for a job is still registered once the job is done.
    assert_eq!(hw.with(|b| b.regions.iter().skip(1).flatten().count()), 0);
}

#[test]
fn a_job_on_a_buffer_the_consumer_holds_is_dropped() {
    let host = host();
    let hw = Hw::default();
    hw.with(|b| b.config = config());
    let mut frames = Client::attach(&host, "frames");
    let mut jobs = Client::attach(&host, "jobs");
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    jobs.post(1, 1);
    let mut now = be.tick(1).unwrap();
    now = be.tick(now).unwrap();
    let frame = frames.ring.pop_frame().unwrap().unwrap();
    // Buffer 1 is the consumer's now: the block must not write it again.
    jobs.post(2, 1);
    now = be.tick(now).unwrap();
    be.tick(now).unwrap();
    assert!(frames.ring.pop_frame().unwrap().is_none());
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);
    assert_eq!(frames.read(1, OUTPUT_BYTES)[0], pattern(1, 0), "untouched");
    assert!(frames.ring.release(frame.buffer, frame.fence).unwrap());
}

#[test]
fn a_second_job_on_a_buffer_the_block_is_writing_is_dropped() {
    let host = host();
    let hw = Hw::default();
    hw.with(|b| b.config = config());
    let mut frames = Client::attach(&host, "frames");
    let mut jobs = Client::attach(&host, "jobs");
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    // Both are taken in one look, while the first is still on the block.
    jobs.post(1, 2);
    jobs.post(2, 2);
    let now = be.tick(1).unwrap();
    be.tick(now).unwrap();
    let frame = frames.ring.pop_frame().unwrap().unwrap();
    assert_eq!((frame.buffer, frame.identity), (2, 1));
    assert!(frames.ring.pop_frame().unwrap().is_none());
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);
    let expected: Vec<u8> = (0..OUTPUT_BYTES as usize)
        .map(|at| pattern(1, at))
        .collect();
    assert!(
        frames.read(2, OUTPUT_BYTES) == expected,
        "the first job's image, whole"
    );
}

#[test]
fn a_new_consumer_gets_frames_in_its_own_memory() {
    let host = host();
    let hw = Hw::default();
    hw.with(|b| b.config = config());
    let mut jobs = Client::attach(&host, "jobs");
    let first = Client::attach(&host, "frames");
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    let mut now = be.tick(1).unwrap();
    drop(first);
    host.stream_detach(0, "frames", CALLER).unwrap();
    let mut second = Client::attach(&host, "frames");

    // The driver still holds buffer 3 from the first consumer, with nothing behind it now: the
    // job is dropped and the buffer given up, then taken again as the second consumer's.
    jobs.post(7, 3);
    now = be.tick(now).unwrap();
    now = be.tick(now).unwrap();
    assert!(second.ring.pop_frame().unwrap().is_none());
    assert_eq!(hw.with(|b| b.finished.len() + b.lost as usize), 0);

    jobs.post(8, 3);
    now = be.tick(now).unwrap();
    be.tick(now).unwrap();
    let frame = second
        .ring
        .pop_frame()
        .unwrap()
        .expect("a frame for the job");
    assert_eq!((frame.buffer, frame.identity), (3, 8));
    let expected: Vec<u8> = (0..OUTPUT_BYTES as usize)
        .map(|at| pattern(8, at))
        .collect();
    assert!(second.read(3, OUTPUT_BYTES) == expected);
    assert_eq!(hw.with(|b| b.lost), 0);
}

#[test]
fn a_stream_without_handles_gets_no_job_and_no_garbage() {
    // `frames` declared as plain shared memory: there is nothing to give the block, so the job
    // is dropped rather than run into memory the consumer never sees.
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"isp\"\ndriver = \"fake-counter\"\n\
         [devices.streams.jobs]\nbuffers = 1\nbuffer_bytes = 64\njob_slots = 4\njob_bytes = 128\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        HostOptions::default(),
        Arc::new(Handles),
    )
    .unwrap();
    let hw = Hw::default();
    hw.with(|b| b.config = config());
    let mut frames = Client::attach(&host, "frames");
    let mut jobs = Client::attach(&host, "jobs");
    let mut be = driver(&host, &hw);
    CameraIsp::start(&mut be).unwrap();
    jobs.post(1, 1);
    let now = be.tick(1).unwrap();
    be.tick(now).unwrap();
    assert!(frames.ring.pop_frame().unwrap().is_none());
    assert_eq!(CameraIsp::counters(&mut be).unwrap().dropped, 1);
    assert_eq!(hw.with(|b| b.finished.len() + b.lost as usize), 0);
}
