//! A lane is a thread that runs driver calls. Devices on one bus share a lane,
//! because a bus runs one transaction at a time anyway, so a slow bus can't
//! delay another. The caller waits with a deadline: if a driver hangs, the
//! caller gets its answer on time and the lane is abandoned and replaced.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_core::Result;

use crate::host::CallWork;

/// What a lane runs: reads `args`, fills `reply`, and returns the reply's length.
pub(crate) enum Work {
    /// A driver call. The common case, and the one that must not allocate.
    Call(CallWork),
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
