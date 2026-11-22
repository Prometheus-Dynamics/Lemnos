//! A long job is bounded by the stall its method declares: progress renews the bound, and a job
//! silent past it ends as `TIMED_OUT` with its driver told to stop. Sim-clock tests: time is moved
//! by the board's clock, never waited for. Each job method of the driver is checked against its own
//! bound.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_core::{
    CallerId, Config, JobId, JobState, Lifecycle, MethodId, Reply, Result, Status, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{Host, JobEvent, JobUpdate, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// Progress the test reports for the job of each method: `done` of [`TOTAL`].
static PROGRESS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
/// Set by the driver when the host tells it to stop a job.
static CANCELLED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
const TOTAL: u64 = 10;
/// The statics above are one set for the file: the tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The bound of the short job: a minute.
const SHORT_SECS: u64 = 60;
/// The bound of the long job: five minutes.
const LONG_SECS: u64 = 300;

#[contract("example.stalls", version = 1)]
pub trait Stalls {
    #[method(1)]
    fn value(&mut self) -> Result<u32>;
    /// A job of a minute's silence.
    #[method(2, job, stall = 60, result = bytes)]
    fn short(&mut self) -> Result<JobId>;
    /// A job of five minutes' silence.
    #[method(3, job, stall = 300, result = bytes)]
    fn long(&mut self) -> Result<JobId>;
}

#[driver("stalls", provides = [Stalls])]
pub struct Stalling {
    /// The open job of each method, if one is.
    open: [Option<JobId>; 2],
}

impl Lifecycle for Stalling {
    type Needs = StallingNeeds;
    const JOBS: bool = true;

    fn start(_: StallingNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self { open: [None, None] })
    }

    fn job(&mut self, job: JobId, _: &mut Reply<'_>) -> JobState {
        for (slot, open) in self.open.iter_mut().enumerate() {
            if *open == Some(job) {
                if CANCELLED[slot].load(Ordering::Acquire) {
                    *open = None;
                    return JobState::Finished(Status::CANCELLED);
                }
                return JobState::Running {
                    done: PROGRESS[slot].load(Ordering::Acquire),
                    total: TOTAL,
                };
            }
        }
        JobState::Finished(Status::NOT_FOUND)
    }

    fn job_cancel(&mut self, job: JobId) {
        for (slot, open) in self.open.iter().enumerate() {
            if *open == Some(job) {
                CANCELLED[slot].store(true, Ordering::Release);
            }
        }
    }
}

impl Stalls for Stalling {
    fn value(&mut self) -> Result<u32> {
        Ok(1)
    }

    fn short(&mut self) -> Result<JobId> {
        self.open[0] = Some(JobId(1));
        Ok(JobId(1))
    }

    fn long(&mut self) -> Result<JobId> {
        self.open[1] = Some(JobId(2));
        Ok(JobId(2))
    }
}

/// The method number and the slot of each job method.
const JOBS: [(u16, usize, u64); 2] = [(2, 0, SHORT_SECS), (3, 1, LONG_SECS)];

fn host() -> (Arc<Host>, SimClock) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"s\"\ndriver = \"stalls\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Stalling>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (Arc::new(host), clock)
}

/// Takes the file's turn and clears the statics for it.
fn reset() -> MutexGuard<'static, ()> {
    let guard = turn();
    for slot in 0..2 {
        PROGRESS[slot].store(0, Ordering::Release);
        CANCELLED[slot].store(false, Ordering::Release);
    }
    guard
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

/// Every update the host has for the job.
fn updates(host: &Host) -> Vec<JobUpdate> {
    let mut all = Vec::new();
    host.take_job_updates(&mut all);
    all
}

fn finished(all: &[JobUpdate]) -> Option<Status> {
    all.iter().find_map(|update| match update.event {
        JobEvent::Finished { status, .. } => Some(status),
        _ => None,
    })
}

#[test]
fn a_healthy_job_never_trips_its_bound() {
    let _turn = reset();
    for (number, _, bound) in JOBS {
        let (host, clock) = host();
        let index = host.device("s").unwrap().index;
        let slot = JOBS.iter().find(|j| j.0 == number).unwrap().1;
        host.start_job(CallerId::LOCAL, index, method(number), &[])
            .unwrap();
        // Progress every half of the bound, for ten steps: the job is always renewed in time.
        for step in 1..=TOTAL {
            clock.advance(Duration::from_secs(bound / 2));
            PROGRESS[slot].store(step, Ordering::Release);
            host.run_ticks();
            let all = updates(&host);
            assert_eq!(
                finished(&all),
                None,
                "method {number} ended while it was making progress at step {step}"
            );
        }
        assert_eq!(
            host.open_jobs(),
            1,
            "method {number} is still open at the end"
        );
        PROGRESS[slot].store(TOTAL, Ordering::Release);
        // The driver answers the end itself once its last step is in.
        clock.advance(Duration::from_secs(1));
        host.run_ticks();
        assert_eq!(
            finished(&updates(&host)),
            None,
            "a healthy job is not timed out"
        );
    }
}

#[test]
fn a_stalled_job_ends_timed_out_at_its_own_bound() {
    for (number, slot, bound) in JOBS {
        let _turn = reset();
        let (host, clock) = host();
        let index = host.device("s").unwrap().index;
        host.start_job(CallerId::LOCAL, index, method(number), &[])
            .unwrap();
        // One second short of the bound: still open, the driver not told anything.
        clock.advance(Duration::from_secs(bound - 1));
        host.run_ticks();
        assert_eq!(
            finished(&updates(&host)),
            None,
            "method {number} ended before its bound"
        );
        assert_eq!(host.open_jobs(), 1);
        // Past the bound, with nothing from the driver: the host ends it.
        clock.advance(Duration::from_secs(2));
        host.run_ticks();
        let all = updates(&host);
        assert_eq!(
            finished(&all),
            Some(Status::TIMED_OUT),
            "method {number} is timed out at its bound"
        );
        let result = all.iter().find_map(|u| match &u.event {
            JobEvent::Finished { result, .. } => Some(result.clone()),
            _ => None,
        });
        assert_eq!(result, Some(Vec::new()), "a timed-out job has no result");
        assert_eq!(host.open_jobs(), 0, "the job is off the books");
        assert!(
            CANCELLED[slot].load(Ordering::Acquire),
            "method {number}'s driver is told to stop it"
        );
    }
}

#[test]
fn progress_renews_the_bound() {
    let _turn = reset();
    let (host, clock) = host();
    let index = host.device("s").unwrap().index;
    host.start_job(CallerId::LOCAL, index, method(2), &[])
        .unwrap();
    // The driver makes progress at 50 s. The host sees it at its next look, the due time at 60 s, and
    // the bound then starts again from that look: due at 120 s, not at 60 s.
    clock.advance(Duration::from_secs(50));
    PROGRESS[0].store(1, Ordering::Release);
    clock.advance(Duration::from_secs(10));
    host.run_ticks();
    assert_eq!(
        finished(&updates(&host)),
        None,
        "progress seen at 60 s renews the minute"
    );
    // At 100 s nothing is due: an unrenewed bound (60 s) would have been looked at here.
    clock.advance(Duration::from_secs(40));
    host.run_ticks();
    assert_eq!(
        finished(&updates(&host)),
        None,
        "renewed at 60 s: not over at 100 s"
    );
    // Silent from 60 s on: over its renewed bound at 120 s.
    clock.advance(Duration::from_secs(21));
    host.run_ticks();
    assert_eq!(finished(&updates(&host)), Some(Status::TIMED_OUT));
}

#[test]
fn a_silent_job_is_ended_by_the_clock_alone() {
    let _turn = reset();
    let (host, clock) = host();
    let index = host.device("s").unwrap().index;
    host.start_job(CallerId::LOCAL, index, method(3), &[])
        .unwrap();
    // The host says when the job is next due: its bound from the start, with no call or tick
    // from anyone in between.
    let due = host
        .next_tick_ns()
        .expect("a job over its bound is a tick due");
    let started = clock.now_ns();
    assert_eq!(due - started, LONG_SECS * 1_000_000_000);
    clock.advance(Duration::from_secs(LONG_SECS + 1));
    host.run_ticks();
    assert_eq!(finished(&updates(&host)), Some(Status::TIMED_OUT));
}
