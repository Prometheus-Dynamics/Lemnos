//! A job the host ends because its driver was lost says why, in its end's result: the reason the
//! device was taken off (here, its driver panicked), as UTF-8. Sim-clock test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lemnos_core::{
    CallerId, Config, JobId, JobState, Lifecycle, MethodId, Reply, Result, Status, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{Host, JobEvent, JobUpdate, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// Set by the test: the driver's next tick panics.
static PANIC: AtomicBool = AtomicBool::new(false);
/// Set while the driver has a job open: its tick then asks to be called at once.
static OPEN: AtomicBool = AtomicBool::new(false);
/// The statics are one set for the file: the tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    let guard = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    PANIC.store(false, Ordering::Release);
    OPEN.store(false, Ordering::Release);
    guard
}

#[contract("example.reasons", version = 1)]
pub trait Reasons {
    /// A job that never finishes on its own.
    #[method(1, job, stall = 300, result = bytes)]
    fn begin(&mut self) -> Result<JobId>;
}

#[driver("reasons", provides = [Reasons])]
pub struct Reasoning {}

impl Lifecycle for Reasoning {
    type Needs = ReasoningNeeds;
    const JOBS: bool = true;
    const TICKS: bool = true;

    fn start(_: ReasoningNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {})
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if PANIC.load(Ordering::Acquire) {
            panic!("the test asks for a panic");
        }
        OPEN.load(Ordering::Acquire).then_some(now_ns)
    }

    fn job(&mut self, _: JobId, _: &mut Reply<'_>) -> JobState {
        JobState::Running { done: 0, total: 10 }
    }
}

impl Reasons for Reasoning {
    fn begin(&mut self) -> Result<JobId> {
        OPEN.store(true, Ordering::Release);
        Ok(JobId(1))
    }
}

fn host() -> (Arc<Host>, SimClock) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"r\"\ndriver = \"reasons\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Reasoning>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (Arc::new(host), clock)
}

#[test]
fn a_job_ended_by_a_panicked_driver_says_why_in_its_result() {
    let _turn = turn();
    let (host, _clock) = host();
    let index = host.device("r").unwrap().index;
    let method = MethodId {
        contract: 0,
        method: 1,
    };
    host.start_job(CallerId::LOCAL, index, method, &[]).unwrap();
    PANIC.store(true, Ordering::Release);
    host.run_ticks();
    let mut all: Vec<JobUpdate> = Vec::new();
    host.take_job_updates(&mut all);
    let end = all
        .iter()
        .find_map(|u| match &u.event {
            JobEvent::Finished { status, result } => Some((*status, result.clone())),
            _ => None,
        })
        .expect("the job ended");
    assert_eq!(end.0, Status::INTERRUPTED);
    assert_eq!(String::from_utf8(end.1).unwrap(), "the driver panicked");
}
