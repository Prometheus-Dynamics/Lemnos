//! A lane is a thread that runs driver calls. Devices on one bus share a lane,
//! because a bus runs one transaction at a time anyway, so a slow bus can't
//! delay another. The caller waits with a deadline: if a driver hangs, the
//! caller gets its answer on time and the lane is abandoned and replaced.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
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

pub(crate) struct Lane {
    shared: Arc<Shared>,
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
        thread::Builder::new()
            .name(name)
            .stack_size(stack)
            .spawn(move || serve(&worker))
            .map_err(|_| LaneError::Spawn)?;
        Ok(Self { shared })
    }

    /// Runs `work` on the lane and copies its reply into `reply`.
    pub(crate) fn run(
        &self,
        args: &[u8],
        reply: &mut [u8],
        deadline: Duration,
        work: Work,
    ) -> core::result::Result<Result<usize>, LaneError> {
        let until = Instant::now() + deadline;
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
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Whatever the driver is doing, this lane never serves again.
                state.stop = true;
                return Err(LaneError::Deadline);
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
    use std::sync::mpsc;

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
