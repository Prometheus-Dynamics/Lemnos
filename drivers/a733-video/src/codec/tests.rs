//! The engine as `codec.block/1`, through a real host's job stream, on a device that records
//! what it was asked. No node is opened.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_contract_codec_block::{CodecBlock, JobStatus};
use lemnos_core::{Health, Lifecycle};
use lemnos_driver_a733_video_sys as sys;
use lemnos_driver_codec_jobs::testing::{Caller, reference};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

use super::*;
use crate::block::{Completion, Mode};
use crate::uapi;

/// What the engine was asked, in order, and what it does wrong.
#[derive(Default)]
struct Log {
    calls: Vec<String>,
    opens: u32,
    fail_open: bool,
    fail_lock: bool,
}

#[derive(Clone, Default)]
struct Engine(Arc<Mutex<Log>>);

impl Engine {
    fn note(&self, call: String) -> io::Result<()> {
        self.0.lock().unwrap().calls.push(call);
        Ok(())
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().unwrap().calls)
    }
}

impl Device for Engine {
    fn request_engine(&self) -> io::Result<()> {
        self.note("request".into())
    }

    fn release_engine(&self) -> io::Result<()> {
        self.note("release".into())
    }

    fn lock(&self, lock: Lock) -> io::Result<()> {
        if self.0.lock().unwrap().fail_lock {
            return Err(io::Error::from_raw_os_error(5));
        }
        self.note(format!("lock {lock:?}"))
    }

    fn unlock(&self, lock: Lock) -> io::Result<()> {
        self.note(format!("unlock {lock:?}"))
    }

    fn reset(&self) -> io::Result<()> {
        self.note("reset".into())
    }

    fn set_mode(&self, mode: Mode) -> io::Result<()> {
        self.note(format!("mode {mode:?}"))
    }

    fn map(&self, _: BorrowedFd<'_>) -> io::Result<u32> {
        self.note("map".into()).map(|()| 0)
    }

    fn unmap(&self, _: BorrowedFd<'_>) -> io::Result<()> {
        self.note("unmap".into())
    }

    fn clear_completion(&self) -> io::Result<()> {
        self.note("clear".into())
    }

    fn wait(&self, _: Completion, _: u32) -> io::Result<bool> {
        self.note("wait".into()).map(|()| true)
    }

    fn read(&self, offset: usize) -> u32 {
        let _ = self.note(format!("read {offset:#x}"));
        0xc0de_0000 | offset as u32
    }
}

fn board(driver: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"engine\"\ndriver = \"{driver}\"\n\
         config = {{ device = \"/nonexistent/cedar_dev_ve2\" }}\n\
         [devices.streams.jobs]\nbuffers = 4\nbuffer_bytes = 1024\n\
         job_slots = 8\njob_bytes = 20\n"
    )
}

mod nothing {
    use lemnos_core::{Config, Lifecycle, Result, driver};

    /// A driver that leaves its device's stream to the test.
    #[driver("nothing", provides = [])]
    pub struct Nothing {}

    impl Lifecycle for Nothing {
        type Needs = NothingNeeds;

        fn start(_: NothingNeeds, _: &Config<'_>) -> Result<Self> {
            Ok(Self {})
        }
    }
}

fn host(text: &str) -> Host {
    clocked(text).0
}

fn clocked(text: &str) -> (Host, SimClock) {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<A733Video>(), entry::<nothing::Nothing>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

struct Rig {
    host: Host,
    driver: A733Video,
    engine: Engine,
    caller: Caller,
    now: u64,
}

impl Rig {
    fn new() -> Self {
        let host = host(&board("nothing"));
        let engine = Engine::default();
        let opened = engine.clone();
        let open = move || {
            let mut log = opened.0.lock().unwrap();
            if log.fail_open {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            log.opens += 1;
            Ok(opened.clone())
        };
        let jobs = Box::new(host.stream_producer(0, "jobs").unwrap());
        let block = EngineBlock::new(open, Lock::Encoder);
        let mut driver = A733Video::on(
            A733VideoNeeds {
                jobs: lemnos_core::Held::boxed(jobs),
            },
            Box::new(block),
        );
        let caller = Caller::attach(&host, 0, "jobs", 1);
        let now = driver.tick(1).unwrap();
        Self {
            host,
            driver,
            engine,
            caller,
            now,
        }
    }

    /// Posts a job with `items` (and `refs`), ticks, and returns how it ended with the
    /// registers it read back.
    fn run(
        &mut self,
        refs: &[lemnos_contract_codec_block::BufferRef],
        items: &[(u32, &[u8])],
    ) -> (JobStatus, Vec<u32>) {
        self.caller.post(9, 0, 1, refs, items);
        self.now = self.driver.tick(self.now).unwrap();
        let (identity, record, back) = self.caller.completion().expect("a completion");
        assert_eq!(identity, 9);
        let back = back
            .as_chunks::<4>()
            .0
            .iter()
            .map(|four| u32::from_le_bytes(*four))
            .collect();
        (record.status, back)
    }

    fn read(&mut self, first: u32, count: u32) -> (JobStatus, Vec<u32>) {
        let item = [first.to_le_bytes(), count.to_le_bytes()].concat();
        self.run(&[], &[(TAG_READ, &item)])
    }
}

#[test]
fn the_two_copies_of_the_kernels_numbers_agree() {
    assert_eq!(sys::WINDOW_BYTES, uapi::WINDOW_BYTES);
    assert_eq!(
        (sys::MAP_DMA_BUF, sys::UNMAP_DMA_BUF),
        (uapi::MAP_DMA_BUF, uapi::UNMAP_DMA_BUF)
    );
    assert_eq!(
        size_of::<sys::DmaBufParam>(),
        size_of::<uapi::DmaBufParam>()
    );
    let numbers = [
        (sys::Plain::WaitDecoder, uapi::WAIT_VE_DE),
        (sys::Plain::WaitEncoder, uapi::WAIT_VE_EN),
        (sys::Plain::Reset, uapi::RESET_VE),
        (sys::Plain::EngineRequest, uapi::ENGINE_REQ),
        (sys::Plain::EngineRelease, uapi::ENGINE_REL),
        (sys::Plain::GetLock, uapi::GET_LOCK),
        (sys::Plain::ReleaseLock, uapi::RELEASE_LOCK),
        (sys::Plain::Mode, uapi::VE_MODE),
        (sys::Plain::WaitVcuEncoder, uapi::WAIT_VCU_ENC),
        (sys::Plain::ClearEncoderInterrupt, uapi::CLEAR_EN_INT_FLAG),
        (sys::Plain::TopRegisterOffset, uapi::GET_VE_TOP_REG_OFFSET),
        (sys::Plain::WaitVcuDecoder, uapi::WAIT_VCU_DEC),
    ];
    for (request, number) in numbers {
        assert_eq!(request as u32, number, "{request:?}");
    }
    // Every lock, mode and wait the block can ask for is a value the request takes.
    for lock in [Lock::Decoder, Lock::Encoder, Lock::Jpeg] {
        assert!(sys::Plain::GetLock.takes(lock.bit()));
    }
    for mode in [
        Mode::EncoderPreprocess,
        Mode::Encoder,
        Mode::Decoder,
        Mode::VcuEncoder,
        Mode::VcuDecoder,
    ] {
        assert!(sys::Plain::Mode.takes(mode.value()));
    }
    assert!(sys::Plain::WaitEncoder.takes(crate::MAX_WAIT_S));
}

#[test]
fn a_job_claims_the_engine_reads_the_registers_it_names_and_lets_it_go() {
    let mut rig = Rig::new();
    let (status, back) = rig.read(0x800, 3);
    assert_eq!(status, JobStatus::Done);
    assert_eq!(back, [0xc0de_0800, 0xc0de_0804, 0xc0de_0808]);
    assert_eq!(
        rig.engine.calls(),
        [
            "request",
            "lock Encoder",
            "read 0x800",
            "read 0x804",
            "read 0x808",
            "unlock Encoder",
            "release"
        ]
    );
    // The last register of the window, and as many as a job reads.
    let (status, back) = rig.read(uapi::WINDOW_BYTES as u32 - 4, 1);
    assert_eq!((status, back.len()), (JobStatus::Done, 1));
    let (status, back) = rig.read(0, MAX_READ);
    assert_eq!((status, back.len()), (JobStatus::Done, MAX_READ as usize));
    assert_eq!(rig.engine.0.lock().unwrap().opens, 1);
    let mut name = [0u8; 48];
    let len = CodecBlock::block(&mut rig.driver, &mut name).unwrap();
    assert_eq!(&name[..len], b"cedar-ve2");
    assert_eq!(CodecBlock::info(&mut rig.driver).unwrap().max_refs, 0);
    assert_eq!(rig.host.stream_stats(0, "jobs").unwrap().faults, 0);
}

#[test]
fn anything_but_a_read_inside_the_window_is_refused_and_the_engine_is_not_claimed() {
    let mut rig = Rig::new();
    let window = uapi::WINDOW_BYTES as u32;
    let reads = [
        ("no registers", 0, 0),
        ("more than a job reads", 0, MAX_READ + 1),
        ("half a register in", 2, 1),
        ("one past the window", window, 1),
        ("running past the window", window - 8, 3),
        ("the largest offset", u32::MAX - 3, 1),
        ("the largest count", 0, u32::MAX),
    ];
    for (what, first, count) in reads {
        assert_eq!(
            rig.read(first, count),
            (JobStatus::Refused, vec![]),
            "{what}"
        );
    }
    let read = [0u32.to_le_bytes(), 1u32.to_le_bytes()].concat();
    type Items<'a> = Vec<(u32, &'a [u8])>;
    let shapes: [(&str, Items<'_>); 5] = [
        ("no item", vec![]),
        ("two reads", vec![(TAG_READ, &read), (TAG_READ, &read)]),
        ("a tag that would be a write", vec![(2, &read)]),
        ("a short read", vec![(TAG_READ, &read[..4])]),
        ("a long read", vec![(TAG_READ, &[0; 12])]),
    ];
    for (what, items) in shapes {
        assert_eq!(rig.run(&[], &items).0, JobStatus::Refused, "{what}");
    }
    // A buffer reference: its address is of use only to a register write.
    assert_eq!(
        rig.run(&[reference(0, 2, 0, 0)], &[(TAG_READ, &read)]).0,
        JobStatus::Refused
    );
    assert!(rig.engine.calls().is_empty(), "nothing reached the engine");
    assert_eq!(rig.engine.0.lock().unwrap().opens, 0);
}

#[test]
fn an_engine_that_fails_fails_the_job_and_is_opened_again_later() {
    let mut rig = Rig::new();
    rig.engine.0.lock().unwrap().fail_open = true;
    assert_eq!(rig.read(0, 1).0, JobStatus::Failed);
    assert_eq!(Lifecycle::health(&rig.driver), Health::Degraded(REOPENING));
    rig.engine.0.lock().unwrap().fail_open = false;
    assert_eq!(rig.read(0, 1).0, JobStatus::Failed, "backing off");
    rig.now += 8_000_000_000;
    assert_eq!(rig.read(0, 1).0, JobStatus::Done);
    assert_eq!(Lifecycle::health(&rig.driver), Health::Ok);
    // A lock the kernel refuses: the use counted for the claim is given back.
    rig.engine.calls();
    rig.engine.0.lock().unwrap().fail_lock = true;
    assert_eq!(rig.read(0, 1).0, JobStatus::Failed);
    assert_eq!(rig.engine.calls(), ["request", "release"]);
}

#[test]
fn a_board_file_binds_it_and_a_node_that_is_not_there_fails_the_job() {
    let (host, clock) = clocked(&board("a733-video"));
    host.tick();
    let device = host.device("engine").unwrap();
    assert_eq!(
        (device.state, device.health),
        (DeviceState::Running, Health::Ok)
    );
    let mut caller = Caller::attach(&host, 0, "jobs", 1);
    let clock_tick = |host: &Host| {
        clock.advance(std::time::Duration::from_millis(1));
        host.tick();
        host.run_ticks();
    };
    clock_tick(&host);
    let read = [0u32.to_le_bytes(), 1u32.to_le_bytes()].concat();
    caller.post(1, 0, 1, &[], &[(TAG_READ, &read)]);
    for _ in 0..3 {
        clock_tick(&host);
    }
    let (_, record, _) = caller.completion().expect("a completion");
    assert_eq!(record.status, JobStatus::Failed);
    assert_eq!(
        host.device("engine").unwrap().health,
        Health::Degraded(REOPENING)
    );
    let bad = host_state(
        &board("a733-video").replace("device = \"/nonexistent/cedar_dev_ve2\"", "lock = \"both\""),
    );
    assert_ne!(bad, DeviceState::Running);
}

fn host_state(text: &str) -> DeviceState {
    let host = host(text);
    host.tick();
    host.device("engine").unwrap().state
}
