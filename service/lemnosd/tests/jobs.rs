//! Long jobs through the real service: progress, cancel, the starter going away, and a restart.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_core::{
    Config as DriverConfig, Encode, JobId, JobState, Lifecycle, Reply, Result, Status, contract,
    driver,
};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

#[contract("copier", version = 1)]
pub trait Copying {
    /// Copies `steps` steps, one per millisecond, and ends with `steps`.
    #[method(1, job)]
    fn copy(&mut self, steps: u32) -> Result<JobId>;
    /// How many cancels the driver has been asked for.
    #[method(2)]
    fn cancels(&mut self) -> Result<u32>;
}

#[derive(Clone, Copy)]
struct Run {
    id: JobId,
    done: u32,
    steps: u32,
    cancelled: bool,
}

#[driver("copier", provides = [Copying])]
pub struct Copier {
    next: u64,
    runs: [Option<Run>; 4],
    cancels: u32,
    due: u64,
}

impl Lifecycle for Copier {
    type Needs = CopierNeeds;
    const TICKS: bool = true;
    const JOBS: bool = true;

    fn start(_: CopierNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {
            next: 0,
            runs: [None; 4],
            cancels: 0,
            due: 0,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let going = || {
            self.runs
                .iter()
                .flatten()
                .any(|r| !r.cancelled && r.done < r.steps)
        };
        if !going() {
            return None;
        }
        if now_ns >= self.due {
            for run in self.runs.iter_mut().flatten() {
                if !run.cancelled && run.done < run.steps {
                    run.done += 1;
                }
            }
            self.due = now_ns + 1_000_000;
        }
        Some(self.due)
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        let Some(slot) = self
            .runs
            .iter_mut()
            .find(|slot| slot.is_some_and(|run| run.id == job))
        else {
            return JobState::Finished(Status::NOT_FOUND);
        };
        let run = slot.unwrap();
        if run.cancelled {
            *slot = None;
            return JobState::Finished(Status::CANCELLED);
        }
        if run.done < run.steps {
            return JobState::Running {
                done: u64::from(run.done),
                total: u64::from(run.steps),
            };
        }
        *slot = None;
        match run.steps.encode(result) {
            Ok(()) => JobState::Finished(Status::OK),
            Err(status) => JobState::Finished(status),
        }
    }

    fn job_cancel(&mut self, job: JobId) {
        self.cancels += 1;
        for run in self.runs.iter_mut().flatten() {
            if run.id == job {
                run.cancelled = true;
            }
        }
    }
}

impl Copying for Copier {
    fn copy(&mut self, steps: u32) -> Result<JobId> {
        let free = self
            .runs
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(Status::BUSY)?;
        self.next += 1;
        let id = JobId(self.next);
        *free = Some(Run {
            id,
            done: 0,
            steps,
            cancelled: false,
        });
        self.due = 0;
        Ok(id)
    }

    fn cancels(&mut self) -> Result<u32> {
        Ok(self.cancels)
    }
}

struct Running {
    service: Option<Service>,
    socket: std::path::PathBuf,
    dir: std::path::PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.service.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn serve(socket: &std::path::Path) -> Service {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"c\"\ndriver = \"copier\"\n";
    Service::start(Config {
        socket: socket.to_path_buf(),
        name: "test-host".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<Copier>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap()
}

fn start() -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnosd-jobs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    Running {
        service: Some(serve(&socket)),
        socket,
        dir,
    }
}

const LONG: Duration = Duration::from_secs(20);

#[test]
fn a_job_reports_progress_and_ends_with_its_result() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut copier = client.open::<dyn Copying>("c").unwrap();
    let job = copier.copy(20).unwrap();
    let mut seen = Vec::new();
    let (status, result) = client
        .wait_job(job.0, LONG, &mut |done, total| seen.push((done, total)))
        .expect("the job ended");
    assert_eq!(status, Status::OK);
    assert_eq!(result, 20u32.to_le_bytes());
    assert!(!seen.is_empty(), "no progress came");
    assert!(seen.iter().all(|&(done, total)| total == 20 && done <= 20));
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0), "{seen:?}");
    // Its end was taken: the number is gone.
    assert_eq!(
        client.wait_job(job.0, Duration::ZERO, &mut |_, _| {}),
        Some((Status::NOT_FOUND, Vec::new()))
    );
}

#[test]
fn a_plain_call_to_a_job_method_is_refused() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut handle = client.open_tag("c", "copier/1").unwrap();
    let mut reply = [0; 16];
    let err = client
        .call(&mut handle, 1, &5u32.to_le_bytes(), &mut reply)
        .unwrap_err();
    assert_eq!(Status::from(err), Status::NOT_SUPPORTED);
    // And a method that starts no job can't be started as one.
    let err = client.start_job(&mut handle, 2, &[]).unwrap_err();
    assert_eq!(Status::from(err), Status::INVALID_ARGUMENT);
}

#[test]
fn a_job_cancelled_midway_ends_as_cancelled() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut copier = client.open::<dyn Copying>("c").unwrap();
    let job = copier.copy(1_000_000).unwrap();
    // Wait until it is under way.
    let deadline = Instant::now() + LONG;
    let mut under_way = false;
    while !under_way {
        assert!(Instant::now() < deadline, "no progress came");
        let end = client.wait_job(job.0, Duration::from_millis(5), &mut |done, _| {
            under_way |= done > 0;
        });
        assert_eq!(end, None, "it ended early");
    }
    client.cancel_job(job.0).unwrap();
    let (status, result) = client
        .wait_job(job.0, LONG, &mut |_, _| {})
        .expect("the job ended");
    assert_eq!(status, Status::CANCELLED);
    assert!(result.is_empty());
    assert_eq!(copier.cancels().unwrap(), 1);
    // Cancelling it again: it is over.
    assert_eq!(
        client.cancel_job(job.0).map_err(Status::from),
        Err(Status::NOT_FOUND)
    );
}

#[test]
fn only_the_starter_cancels_a_job() {
    let running = start();
    let owner = Client::connect(&running.socket, "owner").unwrap();
    let other = Client::connect(&running.socket, "other").unwrap();
    let job = owner
        .open::<dyn Copying>("c")
        .unwrap()
        .copy(1_000_000)
        .unwrap();
    assert_eq!(
        other.cancel_job(job.0).map_err(Status::from),
        Err(Status::NOT_PERMITTED)
    );
    owner.cancel_job(job.0).unwrap();
    let (status, _) = owner.wait_job(job.0, LONG, &mut |_, _| {}).unwrap();
    assert_eq!(status, Status::CANCELLED);
}

#[test]
fn a_starter_that_goes_away_has_its_jobs_cancelled() {
    let running = start();
    let watcher = Client::connect(&running.socket, "watcher").unwrap();
    let mut watch = watcher.open::<dyn Copying>("c").unwrap();
    {
        let leaving = Client::connect(&running.socket, "leaving").unwrap();
        let mut copier = leaving.open::<dyn Copying>("c").unwrap();
        copier.copy(1_000_000).unwrap();
        copier.copy(1_000_000).unwrap();
    }
    let deadline = Instant::now() + LONG;
    while watch.cancels().unwrap() != 2 {
        assert!(Instant::now() < deadline, "the jobs were never cancelled");
        std::thread::sleep(Duration::from_millis(2));
    }
    // And they are over: the driver has room for four again.
    let jobs: Vec<_> = (0..4).map(|_| watch.copy(1).unwrap()).collect();
    for job in jobs {
        let (status, _) = watcher.wait_job(job.0, LONG, &mut |_, _| {}).unwrap();
        assert_eq!(status, Status::OK);
    }
}

#[test]
fn a_restart_reports_a_running_job_as_interrupted() {
    let mut running = start();
    let socket = running.socket.clone();
    let client = Client::connect(&socket, "t").unwrap();
    let mut copier = client.open::<dyn Copying>("c").unwrap();
    let job = copier.copy(1_000_000).unwrap();
    running.service.take().unwrap().stop();
    let (status, result) = client
        .wait_job(job.0, LONG, &mut |_, _| {})
        .expect("the client noticed the service going");
    assert_eq!(status, Status::INTERRUPTED);
    assert!(result.is_empty());

    // The new service keeps nothing of it, and says the number belonged to a run before it.
    running.service = Some(serve(&socket));
    let deadline = Instant::now() + LONG;
    let refused = loop {
        match client.cancel_job(job.0).map_err(Status::from) {
            Err(Status::INTERRUPTED) => break true,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            other => panic!("{other:?}"),
        }
    };
    assert!(refused);
    // A new job on the new service runs to its end.
    let job = copier.copy(3).unwrap();
    let (status, _) = client.wait_job(job.0, LONG, &mut |_, _| {}).unwrap();
    assert_eq!(status, Status::OK);
}
