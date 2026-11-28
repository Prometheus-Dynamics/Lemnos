//! A lane is a thread that runs driver calls. Devices on one bus share a lane,
//! because a bus runs one transaction at a time anyway, so a slow bus can't
//! delay another. The caller waits with a deadline: if a driver hangs, the
//! caller gets its answer on time and the lane is abandoned and replaced.
//!
//! A call is hung when it has had its deadline of time in which it could have run and has not
//! returned. Time the lane's thread spent runnable but waiting for a CPU does not count: when the
//! wall deadline passes, the caller samples that thread's run-queue wait (the kernel's schedstat)
//! over one short window, and while it grows the deadline is extended by the growth. A thread
//! asleep in its driver (a syscall that never returns, a lock, a device that never answers) and a
//! thread that runs and doesn't return accrue none, and are hung one window after the deadline.
//! Past [`CAP_FACTOR`] times the deadline the call is hung whatever the wait says. Nothing is read
//! while a call is inside its deadline.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_core::Result;

use crate::host::{CallWork, TickWork};

/// What a lane runs: reads `args`, fills `reply`, and returns the reply's length.
pub(crate) enum Work {
    /// A driver call. The common case, and the one that must not allocate.
    Call(CallWork),
    /// A driver's tick. Also must not allocate.
    Tick(TickWork),
    Once(OnceWork),
}

type OnceWork = Box<dyn FnOnce(&[u8], &mut [u8]) -> Result<usize> + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneError {
    /// The work didn't finish in time. The lane is stuck, so replace it.
    Deadline,
    /// Other work held the lane for the whole wait. This work never started.
    Busy,
    /// The thread couldn't be started.
    Spawn,
}

#[derive(Default)]
struct State {
    work: Option<Work>,
    done: Option<Result<usize>>,
    busy: bool,
    stop: bool,
    args: Vec<u8>,
    reply: Vec<u8>,
}

/// A caller's deadline is its timed wait on `changed`, and a timed wait takes `state` again before
/// it returns: the deadline holds only while nothing that can block runs under `state`. So only
/// bookkeeping runs under it (handing work over, storing its result, waking the other side), and
/// the driver's call or tick always runs with it released. Keep it so: anything under `state` that
/// could wait, on a driver, a file or another lock, would make every deadline on the lane wait
/// with it (`a_holder_of_the_lanes_mutex_stops_the_callers_deadline_with_it`).
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A call is hung past this many times its deadline of wall time, however long its thread waited
/// for a CPU: a machine starved to a standstill still reports. 16 s at the default 2 s.
pub(crate) const CAP_FACTOR: u32 = 8;

/// The window a starved call's run-queue wait is sampled over, as a fraction of its deadline: the
/// most a hang is reported after its deadline (250 ms at the default 2 s).
const SAMPLE_DIVISOR: u32 = 8;

/// The least a starved call is extended by, so a thread waiting a little at a time is not sampled
/// in a busy loop.
const LEAST_EXTENSION: Duration = Duration::from_millis(1);

/// How long a lane thread has waited for a CPU, in nanoseconds: `None` when it can't be read.
pub(crate) trait RunWait: Send + Sync {
    fn waited_ns(&self) -> Option<u64>;
}

/// The lane thread's own `/proc/thread-self/schedstat`, opened by that thread when it starts (the
/// open file stays its, whichever thread reads it). Field 2 is the time spent runnable and waiting
/// for a CPU. Read with one pread into a fixed buffer: no path built, nothing allocated.
struct Schedstat(File);

impl Schedstat {
    fn open_own() -> Option<Self> {
        File::open("/proc/thread-self/schedstat").ok().map(Self)
    }
}

impl RunWait for Schedstat {
    fn waited_ns(&self) -> Option<u64> {
        let mut buf = [0u8; 96];
        let n = self.0.read_at(&mut buf, 0).ok()?;
        let text = core::str::from_utf8(buf.get(..n)?).ok()?;
        text.split_ascii_whitespace().nth(1)?.parse().ok()
    }
}

/// Whether the host has said that a lane's run-queue wait can't be read (said once).
static UNREADABLE_SAID: AtomicBool = AtomicBool::new(false);

fn say_unreadable() {
    if !UNREADABLE_SAID.swap(true, Ordering::Relaxed) {
        crate::host::warn(|| {
            "a lane thread's run-queue wait (/proc/thread-self/schedstat) can't be read: a call is hung at its wall deadline, however long it waited for a CPU".to_string()
        });
    }
}

pub(crate) struct Lane {
    shared: Arc<Shared>,
    wait: Option<Box<dyn RunWait>>,
}

/// Where a call past its wall deadline stands.
enum Past {
    /// Its thread waited for a CPU: wait on, until this.
    Extended(Instant),
    /// It is hung.
    Hung,
}

fn serve(shared: &Shared) {
    let mut state = shared.lock();
    loop {
        while state.work.is_none() {
            if state.stop {
                return;
            }
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let Some(work) = state.work.take() else {
            continue;
        };
        let args = std::mem::take(&mut state.args);
        let mut reply = std::mem::take(&mut state.reply);
        drop(state);

        let result = match work {
            Work::Call(call) => call.run(&args, &mut reply),
            Work::Tick(tick) => tick.run(),
            Work::Once(once) => once(&args, &mut reply),
        };

        state = shared.lock();
        state.args = args;
        state.reply = reply;
        state.done = Some(result);
        shared.changed.notify_all();
        if state.stop {
            return;
        }
    }
}

impl Lane {
    pub(crate) fn spawn(name: String, stack: usize) -> core::result::Result<Self, LaneError> {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let (opened, own) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(name)
            .stack_size(stack)
            .spawn(move || {
                let _ = opened.send(Schedstat::open_own());
                serve(&worker);
            })
            .map_err(|_| LaneError::Spawn)?;
        let wait = own.recv().ok().flatten();
        if wait.is_none() {
            say_unreadable();
        }
        Ok(Self {
            shared,
            wait: wait.map(|w| Box::new(w) as Box<dyn RunWait>),
        })
    }

    /// The call is past its wall deadline at `now`. `sample` is the run-queue wait read at the last
    /// check (`None` at the first). Hung when the wait can't be read, did not grow over the last
    /// window, or the call is past `cap`; else extended by the growth.
    fn past(
        &self,
        sample: &mut Option<u64>,
        now: Instant,
        cap: Instant,
        deadline: Duration,
    ) -> Past {
        if now >= cap {
            return Past::Hung;
        }
        let Some(waited) = self.wait.as_ref().and_then(|w| w.waited_ns()) else {
            if self.wait.is_some() {
                say_unreadable();
            }
            return Past::Hung;
        };
        let more = match sample.replace(waited) {
            // The first check: one window to see whether the thread is waiting for a CPU.
            None => (deadline / SAMPLE_DIVISOR).max(LEAST_EXTENSION),
            Some(before) if waited > before => {
                Duration::from_nanos(waited - before).max(LEAST_EXTENSION)
            }
            Some(_) => return Past::Hung,
        };
        Past::Extended(now.checked_add(more).map_or(cap, |to| to.min(cap)))
    }

    /// Runs `work` on the lane and copies its reply into `reply`.
    pub(crate) fn run(
        &self,
        args: &[u8],
        reply: &mut [u8],
        deadline: Duration,
        work: Work,
    ) -> core::result::Result<Result<usize>, LaneError> {
        let start = Instant::now();
        let mut until = start + deadline;
        let cap = start + deadline.saturating_mul(CAP_FACTOR);
        let mut sample = None;
        let shared = &*self.shared;
        let mut state = shared.lock();

        while state.busy {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LaneError::Busy);
            }
            state = shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }

        state.busy = true;
        state.done = None;
        state.args.clear();
        state.args.extend_from_slice(args);
        state.reply.resize(reply.len(), 0);
        state.work = Some(work);
        shared.changed.notify_all();

        while state.done.is_none() {
            let now = Instant::now();
            let mut left = until.saturating_duration_since(now);
            if left.is_zero() {
                // The read runs with the lane's mutex released (only bookkeeping runs under it).
                drop(state);
                let past = self.past(&mut sample, now, cap, deadline);
                state = shared.lock();
                if state.done.is_some() {
                    break;
                }
                match past {
                    Past::Extended(to) => {
                        until = to;
                        left = until.saturating_duration_since(Instant::now());
                    }
                    Past::Hung => {
                        // Whatever the driver is doing, this lane never serves again.
                        state.stop = true;
                        return Err(LaneError::Deadline);
                    }
                }
                if left.is_zero() {
                    continue;
                }
            }
            state = shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }

        let result = state.done.take().unwrap_or(Err(lemnos_core::Status::IO));
        if let Ok(n) = result
            && let Some(from) = state.reply.get(..n)
        {
            reply[..n].copy_from_slice(from);
        }
        state.busy = false;
        shared.changed.notify_all();
        Ok(result)
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.stop = true;
        self.shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// A stand-in for the kernel's count: each read returns the last plus `step` ns (0: a thread
    /// that never waits for a CPU), and counts the reads. `None` when unreadable.
    struct Fake {
        waited: AtomicU64,
        step: u64,
        readable: bool,
        reads: Arc<AtomicU64>,
    }

    impl RunWait for Fake {
        fn waited_ns(&self) -> Option<u64> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.readable
                .then(|| self.waited.fetch_add(self.step, Ordering::Relaxed) + self.step)
        }
    }

    fn lane_with(step: u64, readable: bool) -> (Lane, Arc<AtomicU64>) {
        let mut lane = Lane::spawn("lane-test".into(), 256 * 1024).unwrap();
        let reads = Arc::new(AtomicU64::new(0));
        lane.wait = Some(Box::new(Fake {
            waited: AtomicU64::new(0),
            step,
            readable,
            reads: Arc::clone(&reads),
        }));
        (lane, reads)
    }

    fn sleeping(d: Duration) -> Work {
        Work::Once(Box::new(move |_: &[u8], _: &mut [u8]| {
            thread::sleep(d);
            Ok(0)
        }))
    }

    const DEADLINE: Duration = Duration::from_millis(80);

    /// A thread asleep in its driver accrues no run-queue wait: hung one sample window after the
    /// deadline, as before but for that window.
    #[test]
    fn a_call_asleep_past_its_deadline_is_hung_one_window_after_it() {
        let (lane, _) = lane_with(0, true);
        let start = Instant::now();
        let got = lane.run(&[], &mut [], DEADLINE, sleeping(Duration::from_secs(5)));
        let took = start.elapsed();
        assert_eq!(got.err(), Some(LaneError::Deadline));
        assert!(took >= DEADLINE, "{took:?}");
        assert!(
            took < DEADLINE + DEADLINE / SAMPLE_DIVISOR + Duration::from_millis(400),
            "hung only after {took:?}"
        );
    }

    /// A thread that waits for a CPU (each window's wait grows by the window) is not hung: its
    /// call is waited for past the wall deadline, and its answer comes back.
    #[test]
    fn a_call_whose_thread_waits_for_a_cpu_is_waited_for() {
        let window = (DEADLINE / SAMPLE_DIVISOR).as_nanos() as u64;
        let (lane, _) = lane_with(window, true);
        let got = lane.run(&[], &mut [], DEADLINE, sleeping(DEADLINE * 3));
        assert_eq!(got, Ok(Ok(0)));
    }

    /// However long the thread waits for a CPU, the call is hung at the cap.
    #[test]
    fn a_starved_call_is_hung_at_the_cap() {
        let window = (DEADLINE / SAMPLE_DIVISOR).as_nanos() as u64;
        let (lane, _) = lane_with(window * 4, true);
        let start = Instant::now();
        let got = lane.run(&[], &mut [], DEADLINE, sleeping(Duration::from_secs(30)));
        let took = start.elapsed();
        assert_eq!(got.err(), Some(LaneError::Deadline));
        let cap = DEADLINE * CAP_FACTOR;
        assert!(took >= cap, "hung at {took:?}, before the cap {cap:?}");
        assert!(took < cap + Duration::from_millis(500), "hung at {took:?}");
    }

    /// When the wait can't be read, the wall deadline holds, as before: never more time unseen.
    #[test]
    fn an_unreadable_wait_keeps_the_wall_deadline() {
        let (lane, _) = lane_with(1_000_000_000, false);
        let start = Instant::now();
        let got = lane.run(&[], &mut [], DEADLINE, sleeping(Duration::from_secs(5)));
        let took = start.elapsed();
        assert_eq!(got.err(), Some(LaneError::Deadline));
        assert!(took < DEADLINE + Duration::from_millis(400), "{took:?}");
    }

    /// Calls inside their deadline read nothing.
    #[test]
    fn calls_inside_their_deadline_read_nothing() {
        let (lane, reads) = lane_with(0, true);
        for _ in 0..200 {
            let work = Work::Once(Box::new(|_: &[u8], _: &mut [u8]| Ok(0)));
            assert_eq!(
                lane.run(&[], &mut [], Duration::from_secs(5), work),
                Ok(Ok(0))
            );
        }
        assert_eq!(reads.load(Ordering::Relaxed), 0);
    }

    /// The real count: a lane opens its own schedstat when it starts, and its field 2 reads as a
    /// number. (That it grows under starvation is the CPU-quota proof's, not a unit test's.)
    #[test]
    fn a_lane_reads_its_own_run_queue_wait() {
        let lane = Lane::spawn("lane-test".into(), 256 * 1024).unwrap();
        let wait = lane.wait.as_ref().expect("the lane opened its schedstat");
        assert!(wait.waited_ns().is_some());
    }

    /// The caller's deadline is its timed wait on the lane's condvar, and that wait begins by
    /// taking the lane's mutex. Only the lane's own bookkeeping runs under that mutex, never a
    /// driver's code, so a stuck driver never holds it. A lane thread stopped from outside at the
    /// instant it holds it (a tracer stopping one thread, as a harness may) is another matter: the
    /// caller then waits on the mutex itself, with no bound, and its deadline never comes. This
    /// pins that down, so a harness that stops lane threads knows to stop them only while they
    /// wait for work.
    #[test]
    fn a_holder_of_the_lanes_mutex_stops_the_callers_deadline_with_it() {
        let lane = Arc::new(Lane::spawn("lane-test".into(), 256 * 1024).unwrap());
        // Stands in for the lane thread stopped while it holds the mutex.
        let stopped = lane.shared.lock();
        let (sent, got) = mpsc::channel();
        let caller = {
            let lane = Arc::clone(&lane);
            thread::spawn(move || {
                let work = Work::Once(Box::new(|_: &[u8], _: &mut [u8]| Ok(0)));
                let answer = lane.run(&[], &mut [], Duration::from_millis(50), work);
                let _ = sent.send(answer);
            })
        };
        assert!(
            got.recv_timeout(Duration::from_millis(500)).is_err(),
            "the call came back while the mutex was held: its 50 ms deadline ran"
        );
        drop(stopped);
        // Let go, the call ends one way or the other: served, or past its deadline.
        let answer = got
            .recv_timeout(Duration::from_secs(10))
            .expect("an answer");
        assert!(
            matches!(answer, Ok(Ok(0)) | Err(LaneError::Deadline)),
            "{answer:?}"
        );
        caller.join().unwrap();
    }
}
