//! Where the simulated chain's wall time went, for a failure record (test only).
//!
//! Every tick of the simulated chain runs inside [`timed`] with the name of its stage. The
//! profile keeps, in fixed-size tables and without allocating (a stage's name is `'static`):
//! each stage's tick count and slowest tick, and the slowest tick of the whole run with its
//! ordinal and the stage that held it. A tick's ordinal counts every timed call of the run, from
//! 1 after [`reset`]. A stage that is still inside a tick (a wait that has not returned) is kept
//! in flight with the thread that runs it, and [`report`] reads that thread's state and wait
//! channel from `/proc/self/task/<tid>` when they can be read.
//!
//! Nothing is printed here. A passing run never calls [`report`]; a failing one asks for
//! [`failure`], which puts the report after the caller's own words.

use std::cell::Cell;
use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How many distinct stage names the profile holds. A stage past this is counted as untracked.
pub const STAGES: usize = 8;

#[derive(Clone, Copy)]
struct Stage {
    name: &'static str,
    ticks: u64,
    max: Duration,
    max_tick: u64,
}

#[derive(Clone, Copy)]
struct Flight {
    tick: u64,
    since: Instant,
    tid: Option<u64>,
}

#[derive(Clone, Copy)]
struct State {
    stages: [Option<Stage>; STAGES],
    in_flight: [Option<Flight>; STAGES],
    calls: u64,
    untracked: u64,
    /// The slowest tick of the run: its duration, its ordinal and its stage's index.
    slowest: Option<(Duration, u64, usize)>,
}

const EMPTY: State = State {
    stages: [None; STAGES],
    in_flight: [None; STAGES],
    calls: 0,
    untracked: 0,
    slowest: None,
};

static STATE: Mutex<State> = Mutex::new(EMPTY);

thread_local! {
    /// This thread's id, read once from `/proc` (None while it cannot be read).
    static TID: Cell<Option<u64>> = const { Cell::new(None) };
}

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// This thread's kernel id, from `/proc/thread-self` (`<pid>/task/<tid>`). Read once per thread.
fn tid() -> Option<u64> {
    TID.with(|cell| {
        if cell.get().is_none() {
            let tid = std::fs::read_link("/proc/thread-self")
                .ok()
                .and_then(|path| path.file_name()?.to_str()?.parse().ok());
            cell.set(tid);
        }
        cell.get()
    })
}

/// Forgets every tick: a new run starts from ordinal 1.
pub fn reset() {
    *state() = EMPTY;
}

/// Runs one tick of `stage` and records its wall time. The tick is in flight until `f` returns
/// (or panics), so a wait that never returns shows in [`report`].
pub fn timed<R>(stage: &'static str, f: impl FnOnce() -> R) -> R {
    let guard = enter(stage);
    let out = f();
    drop(guard);
    out
}

/// The tick in flight, cleared on drop with its duration recorded.
struct Guard {
    slot: Option<usize>,
    tick: u64,
    start: Instant,
}

fn enter(stage: &'static str) -> Guard {
    let start = Instant::now();
    let tid = tid();
    let mut st = state();
    st.calls += 1;
    let tick = st.calls;
    let found = st
        .stages
        .iter()
        .position(|s| s.is_some_and(|s| s.name == stage))
        .or_else(|| st.stages.iter().position(Option::is_none));
    let slot = match found {
        Some(i) => {
            if st.stages[i].is_none() {
                st.stages[i] = Some(Stage {
                    name: stage,
                    ticks: 0,
                    max: Duration::ZERO,
                    max_tick: 0,
                });
            }
            st.in_flight[i] = Some(Flight {
                tick,
                since: start,
                tid,
            });
            Some(i)
        }
        None => {
            st.untracked += 1;
            None
        }
    };
    Guard { slot, tick, start }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        let mut st = state();
        let Some(i) = self.slot else { return };
        st.in_flight[i] = None;
        if let Some(stage) = st.stages[i].as_mut() {
            stage.ticks += 1;
            if elapsed > stage.max {
                stage.max = elapsed;
                stage.max_tick = self.tick;
            }
        }
        if st.slowest.is_none_or(|(max, _, _)| elapsed > max) {
            st.slowest = Some((elapsed, self.tick, i));
        }
    }
}

/// A thread's scheduler state and wait channel, from `/proc/self/task/<tid>`, in words.
fn thread_state(tid: Option<u64>) -> String {
    let Some(tid) = tid else {
        return "thread id unknown".to_string();
    };
    let dir = format!("/proc/self/task/{tid}");
    let state = std::fs::read_to_string(format!("{dir}/stat"))
        .ok()
        .and_then(|stat| {
            // The command name is in parentheses and may hold spaces: the state follows the last ')'.
            let after = stat.get(stat.rfind(')')? + 1..)?;
            after.split_whitespace().next().map(str::to_string)
        });
    let wchan = std::fs::read_to_string(format!("{dir}/wchan"))
        .ok()
        .map(|w| w.trim().to_string());
    format!(
        "thread {tid}, state {}, wait channel {}",
        state.as_deref().unwrap_or("unreadable"),
        match wchan.as_deref() {
            Some("") => "none reported".to_string(),
            Some(w) => w.to_string(),
            None => "unreadable".to_string(),
        }
    )
}

/// The profile as text: the slowest tick of the run, each stage's slowest tick, and every stage
/// still in flight with its thread's state.
#[must_use]
pub fn report() -> String {
    let st = *state();
    let mut out = String::new();
    let _ = writeln!(out, "sim tick profile: {} ticks timed", st.calls);
    match st.slowest {
        Some((max, tick, i)) => {
            let name = st.stages[i].map_or("?", |s| s.name);
            let _ = writeln!(
                out,
                "slowest tick: tick {tick} of stage {name}, {} wall",
                ms(max)
            );
        }
        None => {
            let _ = writeln!(out, "slowest tick: none timed");
        }
    }
    for (i, stage) in st.stages.iter().enumerate() {
        if let Some(stage) = stage {
            let _ = writeln!(
                out,
                "stage {}: {} ticks, slowest {} at tick {}",
                stage.name,
                stage.ticks,
                ms(stage.max),
                stage.max_tick
            );
        }
        if let Some(flight) = st.in_flight[i] {
            let name = stage.map_or("?", |s| s.name);
            let _ = writeln!(
                out,
                "in flight: tick {} of stage {name} for {}; {}",
                flight.tick,
                ms(flight.since.elapsed()),
                thread_state(flight.tid)
            );
        }
    }
    if st.untracked > 0 {
        let _ = writeln!(
            out,
            "untracked ticks: {} (more than {STAGES} stage names)",
            st.untracked
        );
    }
    out
}

/// `context` followed by [`report`]: the text a failing run prints.
#[must_use]
pub fn failure(context: &str) -> String {
    format!("{context}\n{}", report())
}

/// Milliseconds with microseconds.
fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
}
