//! The driver as a host runs it: bound from a board file, its contract called through the
//! host, its jobs posted by a caller on the host's own stream and completed there. Sealed
//! memfds stand in for the stream's dma-bufs and a played kernel for the service node. No
//! device node is opened.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_contract_codec_block::{CodecBlock, JobStatus, MAX_BLOCK_NAME};
use lemnos_core::{Bound, Caller as CoreCaller, Health, MethodId, Result, Status};
use lemnos_driver_codec_jobs::testing::{Caller, PerBuffer, reference};
use lemnos_driver_rk_mpp::codec::{NOT_THERE, TAG_READ, TAG_WRITE};
use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, BoxedDevice, Device, Message, Session};
use lemnos_driver_rk_mpp_codec::{RkMpp, RkMppNeeds};
use lemnos_host::board::parse;
use lemnos_host::stream::HeapAllocator;
use lemnos_host::{Host, Options, entry};
use lemnos_linux_sys::dma_buf;
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::STATE_FAULT;

/// The played kernel, shared with the test.
#[derive(Clone)]
struct Node(Arc<Mutex<Kernel>>);

struct Played(Arc<Mutex<Kernel>>);

impl Session for Played {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.0.lock().unwrap().call(messages)
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        let n = into.len();
        into.copy_from_slice(&self.0.lock().unwrap().areas[slot][..n]);
    }
}

impl Device for Node {
    type Session = Played;

    fn open(&mut self) -> io::Result<Played> {
        let mut kernel = self.0.lock().unwrap();
        if let Some(errno) = kernel.fail_open.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        kernel.opens += 1;
        kernel.client = None;
        Ok(Played(Arc::clone(&self.0)))
    }

    fn confined(&mut self, _block: Block) -> io::Result<bool> {
        Ok(self.0.lock().unwrap().confined)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

/// A board with the driver named `driver` on a job stream, of dma-bufs when `dma_buf`.
fn board(driver: &str, dma_buf: bool) -> String {
    let memory = if dma_buf {
        "memory = \"dma-buf\"\n"
    } else {
        ""
    };
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"codec\"\ndriver = \"{driver}\"\n\
         config = {{ block = \"vepu2\", node = \"/nonexistent/mpp_service\" }}\n\
         [devices.streams.jobs]\n{memory}buffers = 8\nbuffer_bytes = 8192\n\
         job_slots = 8\njob_bytes = 20\n"
    )
}

struct Rig {
    host: Host,
    clock: SimClock,
}

impl Rig {
    fn start(text: &str, dma_buf: bool) -> Self {
        let sim = SimBoard::new();
        let clock = sim.clock();
        let board = parse(text).unwrap();
        let drivers = [entry::<RkMpp>(), entry::<lemnos_fake::Nothing>()];
        let host = if dma_buf {
            Host::start_with_streams(
                &board,
                &drivers,
                Box::new(sim),
                Arc::new(clock.clone()),
                Options::default(),
                Arc::new(PerBuffer),
            )
        } else {
            Host::start_with_streams(
                &board,
                &drivers,
                Box::new(sim),
                Arc::new(clock.clone()),
                Options::default(),
                Arc::new(HeapAllocator),
            )
        }
        .unwrap();
        Self { host, clock }
    }

    fn ticks(&self, count: u32) {
        for _ in 0..count {
            self.clock.advance(Duration::from_millis(1));
            self.host.tick();
            self.host.run_ticks();
        }
    }
}

/// A driver nobody binds: it gives the host a device whose stream the test's own driver
/// takes.
mod lemnos_fake {
    use lemnos_core::{Config, Lifecycle, Result, driver};

    #[driver("nothing", provides = [])]
    pub struct Nothing {}

    impl Lifecycle for Nothing {
        type Needs = NothingNeeds;

        fn start(_: NothingNeeds, _: &Config<'_>) -> Result<Self> {
            Ok(Self {})
        }
    }
}

/// The contract's calls, carried by the host to device 0.
struct Through<'a> {
    host: &'a Host,
    args: [u8; 64],
}

impl CoreCaller for Through<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let id = MethodId {
            contract: 0,
            method,
        };
        self.host.call(0, id, &self.args[..args_len], reply)
    }
}

fn write(first: u32, values: &[u32]) -> Vec<u8> {
    let mut bytes = first.to_le_bytes().to_vec();
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn regs() -> Vec<u32> {
    (0..32).map(|i| 0xa500_0000 | i).collect()
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|four| u32::from_le_bytes(*four))
        .collect()
}

#[test]
fn a_board_file_binds_it_and_what_it_names_is_checked() {
    // The real node isn't there: the device binds (nothing is opened at bind), says what it
    // is, and a job fails with the reason in its health.
    let rig = Rig::start(&board("rk-mpp", true), true);
    rig.ticks(1);
    assert_eq!(rig.host.device("codec").unwrap().health, Health::Ok);
    let mut device: Bound<dyn CodecBlock, _> = Bound::new(Through {
        host: &rig.host,
        args: [0; 64],
    });
    let mut name = [0u8; MAX_BLOCK_NAME];
    let len = device.block(&mut name).unwrap();
    assert_eq!(&name[..len], b"rk-mpp:vepu2");
    let len = device.job_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"jobs");
    let info = device.info().unwrap();
    assert_eq!(
        (info.max_refs, info.max_items, info.max_buffers),
        (64, 2, 32)
    );
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    rig.ticks(1);
    caller.post(
        1,
        0,
        1,
        &[reference(8, 2, 0, 0)],
        &[(TAG_WRITE, &write(0, &regs()))],
    );
    rig.ticks(1);
    let (identity, record, _) = caller.completion().unwrap();
    assert_eq!((identity, record.status), (1, JobStatus::Failed));
    assert_eq!(
        rig.host.device("codec").unwrap().health,
        Health::Degraded(NOT_THERE)
    );
    assert_eq!(device.counters().unwrap().failed, 1);

    // A stream of plain memory, a block nobody knows, no block: the bind is refused.
    for (text, dma_buf) in [
        (board("rk-mpp", false), false),
        (board("rk-mpp", true).replace("vepu2", "vepu9"), true),
        (
            board("rk-mpp", true).replace("block = \"vepu2\", ", ""),
            true,
        ),
    ] {
        let rig = Rig::start(&text, dma_buf);
        rig.ticks(1);
        assert_ne!(
            rig.host.device("codec").unwrap().state,
            lemnos_host::DeviceState::Running,
            "{text}"
        );
    }
}

/// The driver on the played kernel, on the stream of a device whose own driver doesn't use
/// it: the test plays the host's part in ticking it.
fn driver_on(rig: &Rig, kernel: &Arc<Mutex<Kernel>>) -> RkMpp {
    let jobs = Box::new(rig.host.stream_producer(0, "jobs").unwrap());
    RkMpp::on(
        RkMppNeeds {
            jobs: lemnos_core::Held::boxed(jobs),
        },
        BoxedDevice::new(Node(Arc::clone(kernel))),
        Block::Vepu2,
    )
}

fn all_blocks() -> u32 {
    Block::ALL
        .into_iter()
        .fold(0, |bits, b| bits | 1 << b.facts().client)
}

#[test]
fn a_callers_job_runs_on_the_block_with_the_streams_buffers_and_comes_back() {
    use lemnos_core::Lifecycle;
    let rig = Rig::start(&board("nothing", true), true);
    let kernel = Arc::new(Mutex::new(Kernel::new(all_blocks())));
    let mut driver = driver_on(&rig, &kernel);
    let mut caller = Caller::attach(&rig.host, 0, "jobs", 1);
    let mut now = driver.tick(1).unwrap();

    // Registers 0 to 31, a buffer in 8 and one in 9, all of them read back.
    let refs = [reference(8, 2, 0, 0), reference(9, 3, 4095, 1)];
    let read = [0u32.to_le_bytes(), 32u32.to_le_bytes()].concat();
    caller.post(
        7,
        0,
        1,
        &refs,
        &[(TAG_WRITE, &write(0, &regs())), (TAG_READ, &read)],
    );
    now = driver.tick(now).unwrap();
    let (identity, record, back) = caller.completion().unwrap();
    assert_eq!((identity, record.status), (7, JobStatus::Done));
    let back = words(&back);
    let fds: Vec<u32> = kernel.lock().unwrap().translated[..2].to_vec();
    for (i, (&got, &wrote)) in back.iter().zip(&regs()).enumerate() {
        match i {
            8 => assert_eq!(got, iova(fds[0])),
            9 => assert_eq!(got, iova(fds[1]) + 4095),
            _ => assert_eq!(got, wrote, "register {i}"),
        }
    }
    // The descriptors the kernel was given are the driver's own holds on the stream's
    // buffers, which are the caller's: same size, different numbers.
    assert_eq!(dma_buf::size(caller.handle(2).as_fd()).unwrap(), 8192);

    // An offset past the buffer the stream really has is refused, and nothing is queued.
    caller.post(
        8,
        0,
        1,
        &[reference(8, 2, 8192, 0)],
        &[(TAG_WRITE, &write(0, &regs()))],
    );
    now = driver.tick(now).unwrap();
    assert_eq!(caller.completion().unwrap().1.status, JobStatus::Refused);
    assert_eq!(kernel.lock().unwrap().queued, 0);

    // Bytes that are no job: the caller loses the stream and the session is reset.
    caller.post_bytes(&[1, 2, 3]);
    now = driver.tick(now).unwrap();
    assert_eq!(caller.state(), STATE_FAULT);
    assert_eq!(kernel.lock().unwrap().resets, 1);

    // The next caller is served on a session of its own.
    let mut next = Caller::attach(&rig.host, 0, "jobs", 2);
    now = driver.tick(now).unwrap();
    next.post(
        9,
        0,
        1,
        &[reference(8, 4, 0, 0)],
        &[(TAG_WRITE, &write(0, &regs()))],
    );
    driver.tick(now).unwrap();
    assert_eq!(next.completion().unwrap().1.status, JobStatus::Done);
    assert_eq!(kernel.lock().unwrap().opens, 2);
    let counters = CodecBlock::counters(&mut driver).unwrap();
    assert_eq!(
        (counters.jobs, counters.refused, counters.failed),
        (2, 2, 0)
    );
    assert_eq!(Lifecycle::health(&driver), Health::Ok);
    let _ = Status::OK;
}
