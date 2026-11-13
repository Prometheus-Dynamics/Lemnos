//! The engine through the module as `codec.block/1`, through a real host's job stream of
//! dma-bufs, on a session that records what it was asked and answers as the module would. No
//! node is opened.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::{Arc, Mutex};

use lemnos_contract_codec_block::{BufferRef, JobStatus};
use lemnos_core::{CallerId, Health, Lifecycle};
use lemnos_driver_codec_jobs::testing::{Caller, PerBuffer, reference};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::{SimBoard, SimClock};

use super::*;
use crate::codec::{A733Video, A733VideoNeeds};

/// Each stream buffer's size.
const BYTES: u32 = 16384;

/// A job as the session was given it.
#[derive(Debug, PartialEq, Eq)]
struct Ran {
    writes: Vec<Write>,
    addrs: Vec<Addr>,
    reads: Vec<u32>,
    start: u32,
    timeout_ms: u32,
}

#[derive(Default)]
struct Log {
    opens: u32,
    closes: u32,
    fail_open: bool,
    /// The size of each dma-buf imported, in order.
    imports: Vec<u64>,
    runs: u32,
    /// Kept while set: the steady-state check runs with it off, so the log allocates nothing.
    record: bool,
    jobs: Vec<Ran>,
    /// What the module answers, when not a completion.
    answer: Option<i32>,
    status: u32,
}

#[derive(Clone, Default)]
struct Played(Arc<Mutex<Log>>);

impl Played {
    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.0.lock().unwrap()
    }
}

/// One session; closing it is dropping it.
struct Session {
    played: Played,
    next: u32,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.played.log().closes += 1;
    }
}

impl Engine for Session {
    fn import(&mut self, buffer: BorrowedFd<'_>) -> io::Result<u32> {
        let mut log = self.played.log();
        if log.record {
            let fd = buffer.as_raw_fd();
            let size = std::fs::metadata(format!("/proc/self/fd/{fd}")).map_or(0, |m| m.len());
            log.imports.push(size);
        }
        self.next += 1;
        Ok(self.next - 1)
    }

    fn run(&mut self, job: &Job<'_>, values: &mut [u32]) -> io::Result<u32> {
        let mut log = self.played.log();
        log.runs += 1;
        if log.record {
            log.jobs.push(Ran {
                writes: job.writes.to_vec(),
                addrs: job.addrs.to_vec(),
                reads: job.reads.to_vec(),
                start: job.start,
                timeout_ms: job.timeout_ms,
            });
        }
        if let Some(errno) = log.answer {
            return Err(io::Error::from_raw_os_error(errno));
        }
        assert_eq!(values.len(), job.reads.len());
        for (value, reg) in values.iter_mut().zip(job.reads) {
            *value = 0xab00_0000 | reg;
        }
        Ok(log.status)
    }
}

fn board(driver: &str, config: &str) -> String {
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"engine\"\ndriver = \"{driver}\"\n\
         config = {{ {config} }}\n\
         [devices.streams.jobs]\nmemory = \"dma-buf\"\nbuffers = 8\nbuffer_bytes = {BYTES}\n\
         job_slots = 8\njob_bytes = 24\n"
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

fn clocked(text: &str) -> (Host, SimClock) {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start_with_streams(
        &parse(text).unwrap(),
        &[entry::<A733Video>(), entry::<nothing::Nothing>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
        Arc::new(PerBuffer),
    )
    .unwrap();
    (host, clock)
}

struct Rig {
    _host: Host,
    driver: A733Video,
    played: Played,
    caller: Caller,
    now: u64,
}

/// Writes as the item carries them.
fn writes_item(writes: &[(u32, u32)]) -> Vec<u8> {
    writes
        .iter()
        .flat_map(|(offset, value)| [offset.to_le_bytes(), value.to_le_bytes()])
        .flatten()
        .collect()
}

fn pair(a: u32, b: u32) -> Vec<u8> {
    [a.to_le_bytes(), b.to_le_bytes()].concat()
}

const WRITES: [(u32, u32); 3] = [(0x800, 0x80), (0xa00, 0x0001_0001), (0xb04, 0x8000_0000)];

/// The usual job's references: the input planes in buffer 2, the bitstream in buffer 3.
fn usual_refs() -> [BufferRef; 3] {
    [
        reference(0xa78, 2, 0, 4096),
        reference(0xb80, 3, 256, 1024),
        reference(0xa7c, 2, 4096, 2048),
    ]
}

fn usual_items() -> Vec<(u32, Vec<u8>)> {
    vec![
        (TAG_WRITES, writes_item(&WRITES)),
        (TAG_START, pair(8, 100)),
        (TAG_READ, pair(0xb1c, 2)),
    ]
}

impl Rig {
    fn new() -> Self {
        let (host, _) = clocked(&board("nothing", ""));
        let played = Played::default();
        played.log().record = true;
        played.log().status = 1;
        let opened = played.clone();
        let open = move || {
            let mut log = opened.log();
            if log.fail_open {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            log.opens += 1;
            drop(log);
            Ok(Session {
                played: opened.clone(),
                next: 0,
            })
        };
        let jobs = Box::new(host.stream_producer(0, "jobs").unwrap());
        let mut driver = A733Video::on(
            A733VideoNeeds {
                jobs: lemnos_core::Held::boxed(jobs),
            },
            Box::new(ModuleBlock::new(open)),
        );
        let caller = Caller::attach(&host, 0, "jobs", 1);
        let now = driver.tick(1).unwrap();
        Self {
            _host: host,
            driver,
            played,
            caller,
            now,
        }
    }

    fn post(&mut self, refs: &[BufferRef], items: &[(u32, Vec<u8>)]) {
        let items: Vec<(u32, &[u8])> = items.iter().map(|(t, b)| (*t, b.as_slice())).collect();
        self.caller.post(9, 0, 1, refs, &items);
    }

    fn done(&mut self) -> (JobStatus, Vec<u32>) {
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

    fn run(&mut self, refs: &[BufferRef], items: &[(u32, Vec<u8>)]) -> (JobStatus, Vec<u32>) {
        self.post(refs, items);
        self.now = self.driver.tick(self.now).unwrap();
        self.done()
    }

    fn usual(&mut self) -> (JobStatus, Vec<u32>) {
        self.run(&usual_refs(), &usual_items())
    }

    fn health(&self) -> Health {
        Lifecycle::health(&self.driver)
    }
}

#[test]
fn a_job_reaches_the_module_as_its_writes_buffers_and_reads() {
    let mut rig = Rig::new();
    let (status, back) = rig.usual();
    assert_eq!(status, JobStatus::Done);
    assert_eq!(back, [1, 0xab00_0b1c, 0xab00_0b20]);
    let ran = rig.played.log().jobs.pop().unwrap();
    let addr = |reg, buffer, offset, bytes| Addr {
        reg,
        buffer,
        offset,
        bytes,
    };
    assert_eq!(
        ran,
        Ran {
            writes: WRITES
                .iter()
                .map(|&(offset, value)| Write { offset, value })
                .collect(),
            // Buffer 2 is imported once and named by both its planes.
            addrs: vec![
                addr(0xa78, 0, 0, 4096),
                addr(0xb80, 1, 256, 1024),
                addr(0xa7c, 0, 4096, 2048),
            ],
            reads: vec![0xb1c, 0xb20],
            start: 8,
            timeout_ms: 100,
        }
    );
    // Each import was the stream buffer's dma-buf.
    assert_eq!(rig.played.log().imports, [u64::from(BYTES); 2]);
    // A second job imports nothing again; one with no reads gives back the status alone.
    let mut items = usual_items();
    items.pop();
    let (status, back) = rig.run(&usual_refs(), &items);
    assert_eq!((status, back), (JobStatus::Done, vec![1]));
    let log = rig.played.log();
    assert_eq!(
        (log.opens, log.imports.len(), log.runs, log.closes),
        (1, 2, 2, 0)
    );
    drop(log);
    assert_eq!(rig.health(), Health::Ok);
}

/// A named job: its buffers and its items.
type Case = (&'static str, Vec<BufferRef>, Vec<(u32, Vec<u8>)>);

#[test]
fn work_of_the_wrong_shape_never_reaches_the_module() {
    let mut rig = Rig::new();
    let refs = usual_refs();
    let base = usual_items();
    let with = |tag: u32, bytes: Vec<u8>| -> Vec<(u32, Vec<u8>)> {
        base.iter()
            .map(|(t, b)| {
                if *t == tag {
                    (tag, bytes.clone())
                } else {
                    (*t, b.clone())
                }
            })
            .collect()
    };
    let without = |tag: u32| -> Vec<(u32, Vec<u8>)> {
        base.iter().filter(|(t, _)| *t != tag).cloned().collect()
    };
    let twice = |tag: u32| -> Vec<(u32, Vec<u8>)> {
        let mut items = base.clone();
        items.extend(base.iter().filter(|(t, _)| *t == tag).cloned());
        items
    };
    let cases: Vec<Case> = vec![
        ("no writes", refs.to_vec(), without(TAG_WRITES)),
        ("no start", refs.to_vec(), without(TAG_START)),
        ("no buffer", vec![], base.clone()),
        ("writes twice", refs.to_vec(), twice(TAG_WRITES)),
        ("start twice", refs.to_vec(), twice(TAG_START)),
        ("reads twice", refs.to_vec(), twice(TAG_READ)),
        ("empty writes", refs.to_vec(), with(TAG_WRITES, vec![])),
        (
            "a write cut short",
            refs.to_vec(),
            with(TAG_WRITES, vec![0; 12]),
        ),
        (
            "more writes than a job has",
            refs.to_vec(),
            with(TAG_WRITES, vec![0; 8 * (MAX_WRITES + 1)]),
        ),
        ("no time", refs.to_vec(), with(TAG_START, pair(8, 0))),
        (
            "more time than a job has",
            refs.to_vec(),
            with(TAG_START, pair(8, MAX_TIMEOUT_MS + 1)),
        ),
        (
            "a start cut short",
            refs.to_vec(),
            with(TAG_START, vec![8, 0, 0, 0]),
        ),
        (
            "no registers read",
            refs.to_vec(),
            with(TAG_READ, pair(0, 0)),
        ),
        (
            "more registers read than a job has",
            refs.to_vec(),
            with(TAG_READ, pair(0, MAX_READ + 1)),
        ),
        (
            "reads past 4 GiB",
            refs.to_vec(),
            with(TAG_READ, pair(u32::MAX - 3, 2)),
        ),
        (
            "an unknown item",
            refs.to_vec(),
            [base.clone(), vec![(9, vec![0; 4])]].concat(),
        ),
        (
            "a buffer range of 0",
            vec![reference(0xa78, 2, 0, 0)],
            base.clone(),
        ),
        (
            "a buffer the stream doesn't have",
            vec![reference(0xa78, 99, 0, 64)],
            base.clone(),
        ),
    ];
    for (what, refs, items) in cases {
        assert_eq!(rig.run(&refs, &items).0, JobStatus::Refused, "{what}");
    }
    assert_eq!(rig.played.log().runs, 0);
    assert_eq!(rig.usual().0, JobStatus::Done);
}

#[test]
fn what_the_module_refuses_is_refused_and_the_session_kept() {
    let mut rig = Rig::new();
    for errno in [
        libc::EPERM,
        libc::EINVAL,
        libc::ENOENT,
        libc::EFAULT,
        libc::ENOSPC,
    ] {
        rig.played.log().answer = Some(errno);
        assert_eq!(rig.usual().0, JobStatus::Refused, "errno {errno}");
        assert_eq!(rig.health(), Health::Ok);
    }
    rig.played.log().answer = None;
    assert_eq!(rig.usual().0, JobStatus::Done);
    let log = rig.played.log();
    assert_eq!((log.opens, log.closes, log.imports.len()), (1, 0, 2));
}

#[test]
fn a_job_that_times_out_fails_and_the_reset_engine_runs_the_next() {
    let mut rig = Rig::new();
    rig.played.log().answer = Some(libc::ETIMEDOUT);
    assert_eq!(rig.usual().0, JobStatus::Failed);
    assert_eq!(rig.health(), Health::Ok);
    rig.played.log().answer = None;
    assert_eq!(rig.usual().0, JobStatus::Done);
    let log = rig.played.log();
    assert_eq!((log.opens, log.closes), (1, 0));
}

#[test]
fn a_session_the_module_ends_is_opened_again_after_a_backoff() {
    let mut rig = Rig::new();
    assert_eq!(rig.usual().0, JobStatus::Done);
    rig.played.log().answer = Some(libc::ENODEV);
    assert_eq!(rig.usual().0, JobStatus::Failed);
    assert_eq!(rig.health(), Health::Degraded(REOPENING));
    assert_eq!(rig.played.log().closes, 1);
    rig.played.log().answer = None;
    assert_eq!(rig.usual().0, JobStatus::Failed, "backing off");
    rig.now += 8_000_000_000;
    assert_eq!(rig.usual().0, JobStatus::Done);
    assert_eq!(rig.health(), Health::Ok);
    // The new session imported its buffers again.
    let log = rig.played.log();
    assert_eq!((log.opens, log.imports.len()), (2, 4));
    drop(log);
    // A wedged engine says so until a job runs again.
    rig.played.log().answer = Some(libc::EIO);
    assert_eq!(rig.usual().0, JobStatus::Failed);
    assert_eq!(rig.health(), Health::Degraded(WEDGED));
    // A session that can't be opened is tried again, never given up on.
    rig.played.log().answer = None;
    rig.played.log().fail_open = true;
    for _ in 0..3 {
        rig.now += 8_000_000_000;
        assert_eq!(rig.usual().0, JobStatus::Failed);
    }
    rig.played.log().fail_open = false;
    rig.now += 8_000_000_000;
    assert_eq!(rig.usual().0, JobStatus::Done);
    assert_eq!(rig.health(), Health::Ok);
}

#[test]
fn the_session_closes_when_the_caller_goes() {
    let mut rig = Rig::new();
    assert_eq!(rig.usual().0, JobStatus::Done);
    let Rig {
        _host: host,
        mut driver,
        played,
        caller,
        now,
    } = rig;
    drop(caller);
    host.stream_detach(0, "jobs", CallerId(1)).unwrap();
    driver.tick(now + 1).unwrap();
    assert_eq!(played.log().closes, 1);
    // A new caller's first job opens a new session.
    let mut rig = Rig {
        caller: Caller::attach(&host, 0, "jobs", 2),
        _host: host,
        driver,
        played,
        now: now + 2,
    };
    rig.now = rig.driver.tick(rig.now).unwrap();
    assert_eq!(rig.usual().0, JobStatus::Done);
    assert_eq!(rig.played.log().opens, 2);
}

#[test]
fn a_job_allocates_nothing_once_its_buffers_are_imported() {
    let mut rig = Rig::new();
    assert_eq!(rig.usual().0, JobStatus::Done);
    rig.played.log().record = false;
    for _ in 0..3 {
        rig.post(&usual_refs(), &usual_items());
        let now = rig.now;
        let (next, count) = allocations(|| rig.driver.tick(now).unwrap());
        rig.now = next;
        assert_eq!(count, 0);
        assert_eq!(rig.done().0, JobStatus::Done);
    }
}

#[test]
fn a_board_file_picks_the_route_and_a_module_not_there_fails_the_job() {
    // A bad route, or a module route without a block, is refused by the device's settings: the
    // board starts, and the device is listed failed with the reason, and no device is made.
    let refused = |config: &str| {
        let sim = SimBoard::new();
        let clock = sim.clock();
        let host = Host::start_with_streams(
            &parse(&board("a733-video", config)).unwrap(),
            &[entry::<A733Video>(), entry::<nothing::Nothing>()],
            Box::new(sim),
            Arc::new(clock),
            Options::default(),
            Arc::new(PerBuffer),
        )
        .unwrap();
        matches!(
            &host.device("engine").unwrap().state,
            DeviceState::Backoff { reason, .. } if reason.starts_with("device \"engine\": setting \"")
        )
    };
    assert!(refused("route = \"module\""));
    assert!(refused("route = \"module\", block = \"\""));
    assert!(refused("route = \"both\""));
    let (host, clock) = clocked(&board(
        "a733-video",
        "route = \"module\", block = \"ve\", node = \"/nonexistent/lemnos\"",
    ));
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
    let items: Vec<(u32, Vec<u8>)> = usual_items();
    let items: Vec<(u32, &[u8])> = items.iter().map(|(t, b)| (*t, b.as_slice())).collect();
    caller.post(1, 0, 1, &usual_refs(), &items);
    for _ in 0..3 {
        clock_tick(&host);
    }
    let (_, record, _) = caller.completion().expect("a completion");
    assert_eq!(record.status, JobStatus::Failed);
    assert_eq!(
        host.device("engine").unwrap().health,
        Health::Degraded(REOPENING)
    );
}
