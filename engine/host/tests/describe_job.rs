//! A long job does not hold its device: the driver does the work a short step at a time from its
//! tick, and between steps the device answers like any other. Only a step in progress is waited
//! for (see `describe_waits.rs`). Facts, with a job parked inside a step and between steps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lemnos_core::{
    CallerId, Config, JobId, JobState, Lifecycle, MethodId, Reply, Result, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{Host, JobEvent, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

/// The gate is one for the file, so the two tests take turns.
static TURN: Mutex<()> = Mutex::new(());
static ARMED: AtomicBool = AtomicBool::new(false);
static ENTERED: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
static OPEN: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

fn wait_for(pair: &(Mutex<bool>, Condvar), limit: Duration) -> bool {
    let until = Instant::now() + limit;
    let mut flag = pair.0.lock().unwrap();
    while !*flag {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        flag = pair.1.wait_timeout(flag, left).unwrap().0;
    }
    true
}

fn set(pair: &(Mutex<bool>, Condvar)) {
    *pair.0.lock().unwrap() = true;
    pair.1.notify_all();
}

const STEPS: u64 = 4;

#[contract("example.stepper", version = 1)]
pub trait Stepping {
    #[method(1)]
    fn value(&mut self) -> Result<u32>;
    /// Starts a job of a few short steps.
    #[method(2, job)]
    fn begin(&mut self) -> Result<JobId>;
}

#[driver("stepper", provides = [Stepping])]
pub struct Stepper {
    job: Option<(JobId, u64)>,
}

impl Lifecycle for Stepper {
    type Needs = StepperNeeds;
    const TICKS: bool = true;
    const JOBS: bool = true;

    fn start(_: StepperNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self { job: None })
    }

    /// One step. The second one parks when the test armed the gate: a step that takes long.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let (_, done) = self.job.as_mut()?;
        *done += 1;
        if *done == 2 && ARMED.swap(false, Ordering::AcqRel) {
            set(&ENTERED);
            wait_for(&OPEN, Duration::from_secs(10));
        }
        Some(now_ns)
    }

    fn job(&mut self, job: JobId, _: &mut Reply<'_>) -> JobState {
        match self.job {
            Some((id, done)) if id == job && done < STEPS => {
                JobState::Running { done, total: STEPS }
            }
            Some((id, _)) if id == job => {
                self.job = None;
                JobState::Finished(lemnos_core::Status::OK)
            }
            _ => JobState::Finished(lemnos_core::Status::NOT_FOUND),
        }
    }
}

impl Stepping for Stepper {
    fn value(&mut self) -> Result<u32> {
        Ok(7)
    }

    fn begin(&mut self) -> Result<JobId> {
        let id = JobId(1);
        self.job = Some((id, 0));
        Ok(id)
    }
}

fn host() -> (Arc<Host>, SimClock) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"s\"\ndriver = \"stepper\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Stepper>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (Arc::new(host), clock)
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

fn quickly<T>(what: &str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = work();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{what} took {:?} while a job was open",
        started.elapsed()
    );
    out
}

#[test]
fn between_steps_an_open_job_holds_nothing() {
    let _turn = TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (host, _) = host();
    let index = host.device("s").unwrap().index;
    let job = host
        .start_job(CallerId::LOCAL, index, method(2), &[])
        .unwrap();
    assert_eq!(host.open_jobs(), 1, "the job is open and no step has run");

    let mut reply = [0u8; 4];
    quickly("describe", || host.describe(index)).unwrap();
    quickly("value read", || {
        host.call(index, method(1), &[], &mut reply)
    })
    .unwrap();
    assert_eq!(u32::from_le_bytes(reply), 7);
    quickly("list", || host.devices());
    quickly("a second job start", || {
        host.start_job(CallerId::LOCAL, index, method(2), &[])
    })
    .unwrap();

    // The steps run from the tick, and the job ends.
    for _ in 0..16 {
        host.run_ticks();
    }
    let mut updates = Vec::new();
    host.take_job_updates(&mut updates);
    assert!(
        updates
            .iter()
            .any(|u| u.job == job && matches!(u.event, JobEvent::Finished { .. })),
        "{updates:?}"
    );
}

#[test]
fn only_a_step_in_progress_is_waited_for_and_list_never_waits() {
    let _turn = TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (host, _) = host();
    let index = host.device("s").unwrap().index;
    host.start_job(CallerId::LOCAL, index, method(2), &[])
        .unwrap();
    ARMED.store(true, Ordering::Release);
    let ticker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            for _ in 0..8 {
                host.run_ticks();
            }
        })
    };
    assert!(
        wait_for(&ENTERED, Duration::from_secs(10)),
        "no step parked"
    );

    // The driver is inside a step of its job: `list` does not enter it.
    let listed = quickly("list", || host.devices());
    assert_eq!(listed.len(), 1);

    // A describe waits for the step, then answers in full.
    let asker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            let answer = host.describe(index).map(drop);
            (answer, *OPEN.0.lock().unwrap())
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    set(&OPEN);
    let (answer, step_was_over) = asker.join().unwrap();
    ticker.join().unwrap();
    assert_eq!(answer, Ok(()));
    assert!(step_was_over, "answered while the step was still running");
}
