//! The bus workers: one thread per bus, which reads the sensors on that bus.
//!
//! A sensor's device is moved to its bus's thread for one read and comes back
//! with the result. The service thread keeps the client state and every
//! device that is not a plain sensor (fans, lights), and it never waits for a
//! transaction: a slow read on one bus cannot delay a read due on another,
//! and a bus runs one read at a time, so the reads on it keep their order.
//!
//! The service is told a read finished through a channel, and a one-byte
//! write to a socket pair wakes its `poll` loop.

use lemnos_device::{BoxedDevice, MAX_CHANNELS};
use lemnos_hal::ErrorKind;
use lemnos_linux_sys::time::boottime_us;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

/// How long shutdown waits for reads in flight.
const WAIT_DONE: Duration = Duration::from_secs(5);

/// Most samples one read returns (a batch from a FIFO; see
/// `lemnos_drivers_bmi088::MAX_SAMPLES`).
pub(crate) const BATCH: usize = 48;

/// A read to run: the device, moved to the bus's thread.
pub(crate) struct Job {
    /// The sensor's slot.
    pub index: usize,
    pub device: BoxedDevice,
}

/// A finished read, with the device back.
pub(crate) struct Done {
    pub index: usize,
    /// The bus it ran on.
    pub lane: usize,
    pub device: BoxedDevice,
    /// The samples read, oldest first (`count` of them).
    pub samples: [[i32; MAX_CHANNELS]; BATCH],
    pub count: usize,
    /// When the read started (boot clock, microseconds).
    pub started_us: u64,
    pub cost_us: u64,
    pub result: Result<(), ErrorKind>,
    /// The driver's words for a failure.
    pub why: String,
}

struct Lane {
    name: String,
    jobs: Sender<Job>,
    busy: bool,
}

/// The lanes (one per bus), and the channel their results come back on.
pub(crate) struct Workers {
    lanes: Vec<Lane>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    /// The poll loop's end of the wake pair (non-blocking).
    wake_rx: UnixStream,
    /// The template for each thread's wake writer.
    wake_tx: UnixStream,
}

impl Workers {
    pub fn new() -> io::Result<Self> {
        let (wake_rx, wake_tx) = UnixStream::pair()?;
        wake_rx.set_nonblocking(true)?;
        wake_tx.set_nonblocking(true)?;
        let (done_tx, done_rx) = mpsc::channel();
        Ok(Self {
            lanes: Vec::new(),
            done_tx,
            done_rx,
            wake_rx,
            wake_tx,
        })
    }

    /// The fd the poll loop waits on: readable when a read has finished.
    pub fn wake_fd(&self) -> BorrowedFd<'_> {
        self.wake_rx.as_fd()
    }

    /// Empties the wake pair.
    pub fn drain_wake(&mut self) {
        let mut buf = [0u8; 64];
        while matches!(self.wake_rx.read(&mut buf), Ok(n) if n > 0) {}
    }

    /// The lane for bus `name`, starting its thread on first use.
    pub fn lane(&mut self, name: &str) -> io::Result<usize> {
        if let Some(lane) = self.lanes.iter().position(|l| l.name == name) {
            return Ok(lane);
        }
        let (jobs, rx): (Sender<Job>, Receiver<Job>) = mpsc::channel();
        let index = self.lanes.len();
        let done = self.done_tx.clone();
        let mut wake = self.wake_tx.try_clone()?;
        thread::Builder::new()
            .name(format!("lemnosd-bus:{name}"))
            .spawn(move || {
                for mut job in rx {
                    let started_us = boottime_us();
                    let mut why = String::new();
                    let mut samples = [[0i32; MAX_CHANNELS]; BATCH];
                    let read = job.device.read_batch_why(&mut samples, &mut why);
                    let cost_us = boottime_us().saturating_sub(started_us);
                    let (result, count) = match read {
                        Ok(count) => (Ok(()), count.min(BATCH)),
                        Err(kind) => (Err(kind), 0),
                    };
                    let done_msg = Done {
                        index: job.index,
                        lane: index,
                        device: job.device,
                        samples,
                        count,
                        started_us,
                        cost_us,
                        result,
                        why,
                    };
                    if done.send(done_msg).is_err() {
                        break;
                    }
                    // A full wake pair already wakes the loop.
                    let _ = wake.write(&[1]);
                }
            })?;
        self.lanes.push(Lane {
            name: name.to_string(),
            jobs,
            busy: false,
        });
        Ok(index)
    }

    pub fn lanes(&self) -> usize {
        self.lanes.len()
    }

    pub fn is_busy(&self, lane: usize) -> bool {
        self.lanes[lane].busy
    }

    pub fn any_busy(&self) -> bool {
        self.lanes.iter().any(|l| l.busy)
    }

    /// Starts `job` on `lane`. Gives the job back if the lane's thread is gone.
    pub fn submit(&mut self, lane: usize, job: Job) -> Result<(), Job> {
        match self.lanes[lane].jobs.send(job) {
            Ok(()) => {
                self.lanes[lane].busy = true;
                Ok(())
            }
            Err(mpsc::SendError(job)) => Err(job),
        }
    }

    /// The next finished read, if one is waiting; its lane becomes idle.
    pub fn take_done(&mut self) -> Option<Done> {
        match self.done_rx.try_recv() {
            Ok(done) => {
                self.lanes[done.lane].busy = false;
                Some(done)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    /// Waits for the next finished read (used at shutdown). Gives up after
    /// a while: a read that panicked never finishes.
    pub fn wait_done(&mut self) -> Option<Done> {
        let done = self.done_rx.recv_timeout(WAIT_DONE).ok()?;
        self.lanes[done.lane].busy = false;
        Some(done)
    }
}
