//! The durability barrier off the caller's thread.
//!
//! A slot install writes its bay set unsynced, then needs one flush (fdatasync) before the record
//! that names the set. On a loaded host that flush can take far longer than a driver call's
//! deadline, so it does not run on the driver's tick: [`Barrier::start`] hands the flush to one
//! worker thread and [`Barrier::poll`] asks on later ticks whether it has finished. The worker is
//! started once, on the first barrier, and lives with the process; a barrier costs one channel
//! message, not a thread. A flush that fails, or whose job panics, is an error by name; nothing
//! retries it and nothing else stands in for it.

use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::thread;

/// What a barrier runs: a flush of the device that was written, on the worker thread.
pub type Job = Box<dyn FnOnce() -> io::Result<()> + Send>;

struct Request {
    job: Job,
    reply: Sender<io::Result<()>>,
}

/// The worker's inbox, or why it could not be started (kept, so every barrier names the same cause).
static WORKER: OnceLock<Result<Sender<Request>, String>> = OnceLock::new();

fn inbox() -> io::Result<&'static Sender<Request>> {
    WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Request>();
            thread::Builder::new()
                .name("slot-flush".into())
                .spawn(move || {
                    for Request { job, reply } in rx {
                        let done = std::panic::catch_unwind(AssertUnwindSafe(job))
                            .unwrap_or_else(|_| Err(io::Error::other("the flush panicked")));
                        // The barrier may have been dropped (its install ended): nothing to tell.
                        let _ = reply.send(done);
                    }
                })
                .map(|_| tx)
                .map_err(|e| format!("the flush worker could not start: {e}"))
        })
        .as_ref()
        .map_err(|why| io::Error::other(why.clone()))
}

/// One flush in flight on the worker.
pub struct Barrier {
    reply: mpsc::Receiver<io::Result<()>>,
}

impl Barrier {
    /// Queues `job` on the worker. Returns at once.
    pub fn start(job: Job) -> io::Result<Self> {
        let (reply, answer) = mpsc::channel();
        inbox()?
            .send(Request { job, reply })
            .map_err(|_| io::Error::other("the flush worker is gone"))?;
        Ok(Self { reply: answer })
    }

    /// `Ok(true)` once the flush has finished, `Ok(false)` while it runs, `Err` when it failed.
    /// Never blocks.
    pub fn poll(&mut self) -> io::Result<bool> {
        match self.reply.try_recv() {
            Ok(done) => done.map(|()| true),
            Err(TryRecvError::Empty) => Ok(false),
            Err(TryRecvError::Disconnected) => Err(io::Error::other("the flush worker is gone")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    fn wait_for(barrier: &mut Barrier) -> io::Result<bool> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            match barrier.poll()? {
                true => return Ok(true),
                false if Instant::now() >= until => return Ok(false),
                false => thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    #[test]
    fn a_barrier_is_pending_until_its_job_returns_then_done() {
        let (release, gate): (Sender<()>, Receiver<()>) = mpsc::channel();
        let mut barrier = Barrier::start(Box::new(move || {
            gate.recv().map_err(|_| io::Error::other("released early"))
        }))
        .unwrap();
        // The job waits for the gate: the barrier stays pending on every poll until it is released.
        for _ in 0..3 {
            assert!(!barrier.poll().unwrap(), "pending before release");
        }
        release.send(()).unwrap();
        assert!(wait_for(&mut barrier).unwrap(), "done after release");
    }

    #[test]
    fn a_failed_flush_is_an_error_by_name_and_not_a_pass() {
        let mut barrier =
            Barrier::start(Box::new(|| Err(io::Error::other("EIO on flush")))).unwrap();
        let err = wait_for(&mut barrier).expect_err("a failed flush must not pass");
        assert!(err.to_string().contains("EIO on flush"), "{err}");
    }

    #[test]
    fn a_panicking_flush_is_an_error_and_the_worker_serves_the_next_barrier() {
        let mut bad = Barrier::start(Box::new(|| panic!("boom"))).unwrap();
        assert!(wait_for(&mut bad).is_err(), "a panic must not pass");
        let mut good = Barrier::start(Box::new(|| Ok(()))).unwrap();
        assert!(
            wait_for(&mut good).unwrap(),
            "the worker outlives a panicking job"
        );
    }

    #[test]
    fn a_real_fdatasync_on_a_dup_of_the_file_completes() {
        let dir = std::env::temp_dir().join(format!("lemnos-flush-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("device");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let dup = file.try_clone().unwrap();
        let mut barrier = Barrier::start(Box::new(move || dup.sync_data())).unwrap();
        assert!(wait_for(&mut barrier).unwrap());
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
