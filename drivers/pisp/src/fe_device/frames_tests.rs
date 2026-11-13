//! The front end's output 0 through the host's own frame stream, end to end: a consumer attaches
//! to the `output0` stream as a client of the daemon does, posts a job on the `config` stream, and
//! reads the frame through the descriptor it was handed for the buffer. A stand-in for the block
//! and the module writes a known pattern by device address into the memory it was given for the
//! job's buffer, as the block's DMA does, and keeps the module's rules: a job on a buffer with no
//! memory is refused, and a buffer's memory can't change while a job has it. The consumer must
//! read that pattern: a frame number on the ring over a buffer the block never wrote is the
//! failure this test exists for.
//!
//! Sealed memfds stand in for the stream's dma-bufs. These sit beside the driver, not under
//! `tests/`, because they build it from its own options.

use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, Counters, IspInput, IspJob, IspOutput, NO_BUFFER,
};
use lemnos_contract_isp_pisp::LAYOUT_FRONT_END;
use lemnos_core::{CallerId, Encode, Health, Lifecycle, Result, Status, StreamHandle, Writer};
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::{Buffers, MemfdAllocator, StreamAllocator};
use lemnos_host::{Host, Options as HostOptions, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_sim::SimBoard;
use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, Layout};

use super::{Link, Options, PispFe, PispFeNeeds};
use crate::fe::FrontEnd;
use crate::fe_hw::{Finished, Hardware, Job, Placed, Plan, Registered, Set, Start};
use crate::fe_job::CONFIG_BYTES;
use crate::format::formats;
use crate::uapi::{Branch, ImageFormatConfig, Wire, fe_enable as en};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
/// 16 bits a pixel at 64 by 32.
const OUTPUT_BYTES: u32 = WIDTH * 2 * HEIGHT;
const BUFFERS: u32 = 4;
const CONFIG: u64 = 0x1000_0000;
/// Where the stand-in puts the first memory it is given, and how far apart the rest.
const MEMORY: u64 = 0x4000_0000;
const MEMORY_SPAN: u64 = 0x0010_0000;
const CALLER: CallerId = CallerId(5);

/// What the block writes into output 0 for the job with `identity`.
fn pattern(identity: u64, at: usize) -> u8 {
    (identity as usize).wrapping_mul(31).wrapping_add(at * 7) as u8 | 1
}

fn image(identity: u64) -> Vec<u8> {
    (0..OUTPUT_BYTES as usize)
        .map(|at| pattern(identity, at))
        .collect()
}

// ---- The stream's memory, as the daemon makes it: one descriptor per buffer.

/// Each buffer of a stream its own sealed memfd, lent as its handle. The plain allocator makes
/// shared memory, as a stream without dma-bufs gets.
struct Handles;

impl StreamAllocator for Handles {
    fn allocate(&self, name: &str, layout: &Layout) -> core::result::Result<Buffers, Status> {
        MemfdAllocator.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        _name: &str,
        layout: &Layout,
        _heap: &str,
        _planes: &Planes,
    ) -> core::result::Result<Buffers, Status> {
        let mut fds = Vec::new();
        for _ in 0..layout.buffers {
            fds.push(sealed_memfd(c"fe-frames", layout.stride()).map_err(|_| Status::IO)?);
        }
        Ok(Buffers {
            fds,
            dma: true,
            sync: None,
        })
    }
}

/// A host whose device 0 has a `config` stream and an `output0` stream, `memory` as given.
fn host_with(memory: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"isp\"\ndriver = \"fake-counter\"\n\
         [devices.streams.config]\nbuffers = 1\nbuffer_bytes = {CONFIG_BYTES}\njob_slots = 4\n\
         job_bytes = 128\n\
         [devices.streams.output0]\nbuffers = {BUFFERS}\nbuffer_bytes = {OUTPUT_BYTES}\n{memory}"
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

fn host() -> Host {
    host_with("memory = \"dma-buf\"\n")
}

// ---- The block and the module: runs jobs by device address, keeping the module's rules.

/// Memory the module pinned for a buffer.
struct Pinned {
    addr: u64,
    file: File,
    size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    /// The module's, free for a job.
    Free,
    /// A job has it: the block is, or will be, writing it.
    Job,
    /// Published, due back.
    Out,
}

struct Buffer {
    memory: Option<Pinned>,
    at: Where,
}

#[derive(Default)]
struct Block {
    buffers: Vec<Buffer>,
    made: u32,
    started: u32,
    output0_bytes: u32,
    /// Jobs taken: identity, buffer, and the address output 0 was armed with.
    queue: VecDeque<(u64, u32, u64)>,
    finished: VecDeque<Finished>,
    /// Memory given, counting swaps.
    given: u32,
    /// Jobs the module would have refused: on a buffer with no memory, or one not free.
    refused: u32,
    /// Jobs whose output 0 address was in no memory the module holds.
    stray: u32,
    bad_returns: u32,
}

impl Block {
    /// The block runs the next job it took: writes output 0 where that job armed it.
    fn run(&mut self) -> Option<u32> {
        let (identity, index, addr) = self.queue.pop_front()?;
        let data = image(identity);
        let wrote = self
            .buffers
            .iter()
            .filter_map(|b| b.memory.as_ref())
            .any(|m| {
                addr == m.addr
                    && data.len() as u64 <= m.size
                    && m.file.write_all_at(&data, 0).is_ok()
            });
        if !wrote {
            self.stray += 1;
        }
        self.buffers[index as usize].at = Where::Out;
        self.finished.push_back(Finished {
            buffer: index,
            bytes: self.output0_bytes,
            sequence: identity,
            identity,
            timestamp_ns: 1,
        });
        Some(index)
    }

    fn with_memory(&self) -> usize {
        self.buffers.iter().filter(|b| b.memory.is_some()).count()
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Block>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Block) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

fn same_file(a: &File, b: &File) -> bool {
    match (a.metadata(), b.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

impl Hardware for Hw {
    fn hw_version(&self) -> u32 {
        0x0225_2700
    }

    fn make(&mut self, plan: &Plan) -> Result<Placed> {
        self.with(|b| {
            b.made += 1;
            b.output0_bytes = plan.output0_bytes;
            b.queue.clear();
            b.finished.clear();
            b.buffers = (0..plan.buffers)
                .map(|_| Buffer {
                    memory: None,
                    at: Where::Free,
                })
                .collect();
        });
        Ok(Placed { config: CONFIG })
    }

    fn set_buffer(&mut self, buffer: u32, memory: BorrowedFd<'_>) -> Result<Set> {
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|b| {
            let need = u64::from(b.output0_bytes);
            let addr = MEMORY + u64::from(b.given) * MEMORY_SPAN;
            let Some(slot) = b.buffers.get_mut(buffer as usize) else {
                return Ok(Set::Refused);
            };
            if size < need {
                return Ok(Set::Refused);
            }
            if slot
                .memory
                .as_ref()
                .is_some_and(|m| same_file(&m.file, &file))
            {
                return Ok(Set::Done);
            }
            if slot.at == Where::Job {
                return Ok(Set::Busy);
            }
            slot.memory = Some(Pinned { addr, file, size });
            b.given += 1;
            Ok(Set::Done)
        })
    }

    fn go(&mut self, _start: &Start) -> Result<()> {
        self.with(|b| b.started += 1);
        Ok(())
    }

    fn stop(&mut self) {
        self.with(|b| {
            b.queue.clear();
            b.buffers.clear();
        });
    }

    fn write_config(&mut self, _slot: u32, _config: &[u8; CONFIG_BYTES]) -> Result<()> {
        Ok(())
    }

    fn register(&mut self, _memory: BorrowedFd<'_>) -> Result<Registered> {
        // Output 0 only here: nothing else is lent.
        Err(Status::NOT_SUPPORTED)
    }

    fn unregister(&mut self, _region: u32) -> Result<()> {
        Err(Status::NOT_FOUND)
    }

    fn post(&mut self, job: &Job) -> Result<()> {
        self.with(|b| {
            let slot = b.buffers.get_mut(job.buffer as usize);
            match slot {
                Some(Buffer {
                    memory: Some(memory),
                    at: at @ Where::Free,
                }) => {
                    *at = Where::Job;
                    b.queue.push_back((job.identity, job.buffer, memory.addr));
                    Ok(())
                }
                // The module ends the stream for a job like this.
                _ => {
                    b.refused += 1;
                    Err(Status::INVALID_ARGUMENT)
                }
            }
        })
    }

    fn next_finished(&mut self) -> Result<Option<Finished>> {
        Ok(self.with(|b| b.finished.pop_front()))
    }

    fn give_back(&mut self, buffer: u32) -> Result<()> {
        self.with(|b| match b.buffers.get_mut(buffer as usize) {
            Some(slot) if slot.at == Where::Out => slot.at = Where::Free,
            _ => b.bad_returns += 1,
        });
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters::default())
    }
}

fn config() -> Vec<u8> {
    let mut fe = FrontEnd::new(WIDTH as u16, HEIGHT as u16, crate::uapi::BayerOrder::Rggb);
    fe.set_output_format(
        Branch::Zero,
        ImageFormatConfig {
            format: formats::BAYER16,
            ..Default::default()
        },
    );
    fe.enable(en::output(0), true);
    let mut bytes = vec![0; CONFIG_BYTES];
    fe.prepare().unwrap().write_to(&mut bytes[..]).unwrap();
    bytes
}

fn driver(host: &Host, hw: &Hw) -> PispFe {
    let stream = |name| -> StreamHandle {
        lemnos_core::Held::boxed(Box::new(host.stream_producer(0, name).unwrap()))
    };
    let link = Link {
        lanes: 2,
        mbps: 800,
        vc: 0,
        data_type: 0x2c,
    };
    let mut fe = PispFe::with_hardware(
        Box::new(hw.clone()),
        PispFeNeeds {
            config: stream("config"),
            statistics: None,
            output0: Some(stream("output0")),
            output1: None,
            _next: None,
        },
        Options::new(link, 4).unwrap(),
    );
    let input = IspInput {
        width: WIDTH,
        height: HEIGHT,
        bit_depth: 12,
        bayer: BayerOrder::default(),
    };
    CameraIsp::set_input(&mut fe, input).unwrap();
    let output = IspOutput {
        width: WIDTH,
        height: HEIGHT,
        fourcc: u32::from_le_bytes(*b"RG16"),
        pad_bytes: 0,
    };
    let layout = CameraIsp::set_output(&mut fe, 2, output).unwrap();
    assert_eq!(layout.size, OUTPUT_BYTES);
    fe
}

/// The consumer's end of a stream, as a client holds it.
struct Client {
    handles: Option<Arc<[OwnedFd]>>,
    ring: Consumer,
}

impl Client {
    fn attach(host: &Host, name: &str) -> Self {
        let lease = host.stream_attach(0, name, CALLER).unwrap();
        let handles = lease.handles.clone();
        let ring = lease.consumer_writable().unwrap();
        Self { handles, ring }
    }

    /// Writes the configuration into config buffer 0 and posts a job naming it.
    fn post(&mut self, identity: u64, output0: u32) {
        assert!(self.ring.write(0, 0, &config()));
        let job = IspJob {
            identity,
            layout: LAYOUT_FRONT_END,
            config_buffer: 0,
            input_buffer: NO_BUFFER,
            outputs: [NO_BUFFER, NO_BUFFER, output0, NO_BUFFER],
        };
        let mut buf = [0; 128];
        let mut w = Writer::new(&mut buf);
        job.encode(&mut w).unwrap();
        let n = w.len();
        assert!(self.ring.jobs().post(&buf[..n]).unwrap());
    }

    /// The descriptor the consumer was handed for `buffer`, as a file of its own.
    fn file(&self, buffer: u32) -> File {
        let handles = self.handles.as_ref().expect("a stream of dma-bufs");
        File::from(
            handles[buffer as usize]
                .as_fd()
                .try_clone_to_owned()
                .unwrap(),
        )
    }

    /// What the descriptor the consumer was handed for `buffer` holds.
    fn read(&self, buffer: u32) -> Vec<u8> {
        read(&self.file(buffer))
    }
}

fn read(file: &File) -> Vec<u8> {
    let mut out = vec![0; OUTPUT_BYTES as usize];
    file.read_exact_at(&mut out, 0).unwrap();
    out
}

#[test]
fn the_consumer_reads_what_the_block_wrote_for_its_job() {
    let host = host();
    let hw = Hw::default();
    let mut frames = Client::attach(&host, "output0");
    let mut jobs = Client::attach(&host, "config");
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    assert_eq!(
        hw.with(|b| b.with_memory()),
        BUFFERS as usize,
        "every buffer of the stream was given to the module"
    );

    let mut now = 1;
    for (identity, output) in [(0x51, 2), (0x52, 0), (0x53, 3), (0x54, 2)] {
        jobs.post(identity, output);
        now = fe.tick(now).unwrap();
        assert_eq!(hw.with(Block::run), Some(output));
        now = fe.tick(now).unwrap();
        let frame = frames
            .ring
            .pop_frame()
            .unwrap()
            .expect("a frame for the job");
        assert_eq!(
            (frame.buffer, frame.identity, frame.bytes),
            (output, identity, OUTPUT_BYTES)
        );
        let seen = frames.read(frame.buffer);
        let expected = image(identity);
        assert!(
            seen == expected,
            "frame {identity:#x} in buffer {output}: the consumer's buffer doesn't hold what the \
             block wrote (first bytes {:?}, expected {:?})",
            &seen[..8],
            &expected[..8]
        );
        assert!(frames.ring.release(frame.buffer));
        now = fe.tick(now).unwrap();
    }
    assert_eq!(hw.with(|b| (b.stray, b.refused, b.bad_returns)), (0, 0, 0));
    assert_eq!(hw.with(|b| b.started), 1);
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 0);
    assert!(matches!(fe.health(), Health::Ok));
}

#[test]
fn a_job_on_a_buffer_the_consumer_holds_or_a_job_has_is_dropped() {
    let host = host();
    let hw = Hw::default();
    let mut frames = Client::attach(&host, "output0");
    let mut jobs = Client::attach(&host, "config");
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    // Two jobs on one buffer, taken in one look: the second has nowhere to go.
    jobs.post(1, 1);
    jobs.post(2, 1);
    let mut now = fe.tick(1).unwrap();
    assert_eq!(hw.with(|b| b.queue.len()), 1);
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 1);
    hw.with(Block::run);
    now = fe.tick(now).unwrap();
    let frame = frames.ring.pop_frame().unwrap().unwrap();
    assert_eq!((frame.buffer, frame.identity), (1, 1));
    // Buffer 1 is the consumer's now: the block must not write it again.
    jobs.post(3, 1);
    now = fe.tick(now).unwrap();
    assert_eq!(hw.with(|b| b.queue.len()), 0);
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 2);
    assert!(frames.read(1) == image(1), "untouched");
    // Given back, it can be a job's again.
    assert!(frames.ring.release(frame.buffer));
    jobs.post(4, 1);
    now = fe.tick(now).unwrap();
    assert_eq!(hw.with(Block::run), Some(1));
    fe.tick(now).unwrap();
    assert!(frames.read(1) == image(4));
    assert_eq!(hw.with(|b| (b.stray, b.refused, b.bad_returns)), (0, 0, 0));
}

#[test]
fn a_new_consumer_gets_frames_in_its_own_memory() {
    let host = host();
    let hw = Hw::default();
    let mut jobs = Client::attach(&host, "config");
    let first = Client::attach(&host, "output0");
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    // The first consumer leaves with the block in the middle of its job on buffer 3.
    jobs.post(7, 3);
    let mut now = fe.tick(1).unwrap();
    assert_eq!(hw.with(|b| b.queue.len()), 1);
    let old: Vec<File> = (0..BUFFERS).map(|buffer| first.file(buffer)).collect();
    drop(first);
    host.stream_detach(0, "output0", CALLER).unwrap();
    let mut second = Client::attach(&host, "output0");
    now = fe.tick(now).unwrap();
    assert_eq!(
        hw.with(|b| b.given),
        2 * BUFFERS - 1,
        "every buffer but the one the job has is on the second consumer's memory"
    );
    // A job on that buffer now would be written to the first consumer's memory: dropped.
    jobs.post(8, 3);
    now = fe.tick(now).unwrap();
    assert_eq!(hw.with(|b| b.queue.len()), 1);
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 1);

    // The job in flight ends where it started: in the first consumer's memory, which the
    // module still holds. The second consumer is told nothing and its memory is untouched.
    assert_eq!(hw.with(Block::run), Some(3));
    now = fe.tick(now).unwrap();
    assert!(second.ring.pop_frame().unwrap().is_none());
    assert!(read(&old[3]) == image(7));
    assert!(second.read(3).iter().all(|&byte| byte == 0));
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 2);
    assert_eq!(hw.with(|b| b.given), 2 * BUFFERS, "and now that buffer too");

    // From here every job's frame is the second consumer's, in its own memory.
    let before: Vec<Vec<u8>> = old.iter().map(read).collect();
    for (identity, output) in [(9, 3), (10, 0), (11, 3)] {
        jobs.post(identity, output);
        now = fe.tick(now).unwrap();
        assert_eq!(hw.with(Block::run), Some(output));
        now = fe.tick(now).unwrap();
        let frame = second.ring.pop_frame().unwrap().expect("a frame");
        assert_eq!((frame.buffer, frame.identity), (output, identity));
        assert!(
            second.read(output) == image(identity),
            "frame {identity}: the second consumer's buffer {output} doesn't hold what the block \
             wrote"
        );
        assert!(second.ring.release(frame.buffer));
        now = fe.tick(now).unwrap();
    }
    let after: Vec<Vec<u8>> = old.iter().map(read).collect();
    assert!(
        before == after,
        "the first consumer's memory was left alone"
    );
    assert_eq!(hw.with(|b| (b.stray, b.refused, b.bad_returns)), (0, 0, 0));
    assert_eq!(hw.with(|b| b.made), 1, "the block was never stopped for it");
}

#[test]
fn with_no_consumer_no_job_is_sent() {
    let host = host();
    let hw = Hw::default();
    let mut jobs = Client::attach(&host, "config");
    let mut fe = driver(&host, &hw);
    CameraIsp::start(&mut fe).unwrap();
    jobs.post(1, 0);
    let mut now = fe.tick(1).unwrap();
    assert_eq!(
        hw.with(|b| (b.with_memory(), b.queue.len(), b.refused)),
        (0, 0, 0)
    );
    assert_eq!(CameraIsp::counters(&mut fe).unwrap().dropped, 1);

    let mut frames = Client::attach(&host, "output0");
    now = fe.tick(now).unwrap();
    assert_eq!(hw.with(|b| b.with_memory()), BUFFERS as usize);
    jobs.post(2, 0);
    now = fe.tick(now).unwrap();
    assert_eq!(hw.with(Block::run), Some(0));
    fe.tick(now).unwrap();
    let frame = frames.ring.pop_frame().unwrap().expect("a frame");
    assert_eq!(frame.identity, 2);
    assert!(frames.read(0) == image(2));
}

#[test]
fn a_stream_without_handles_is_refused_with_or_without_a_consumer() {
    // `output0` declared as plain shared memory: nothing to give the block. The stream says so
    // before anyone attaches.
    let host = host_with("");
    let hw = Hw::default();
    let mut fe = driver(&host, &hw);
    assert_eq!(CameraIsp::start(&mut fe), Err(Status::NOT_SUPPORTED));
    let mut frames = Client::attach(&host, "output0");
    assert_eq!(CameraIsp::start(&mut fe), Err(Status::NOT_SUPPORTED));
    assert_eq!(fe.tick(1), None, "not running");
    assert!(frames.ring.pop_frame().unwrap().is_none());
    assert_eq!(hw.with(|b| (b.given, b.queue.len(), b.refused)), (0, 0, 0));
}
