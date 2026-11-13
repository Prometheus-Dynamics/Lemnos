//! An indicator: named states (`idle`, `receiving`, `writing`, `verifying`, `done`, `failed`)
//! shown on LEDs, as `indicator/1`. A caller sets the state; the board says what each state looks
//! like.
//!
//! # Wiring
//!
//! - `pixels` (required): a `pixels/1` device the LEDs are on: the LED strip driver, or a GPIO LED
//!   driver that provides `pixels/1`. The LED count is the device's `pixel_count`.
//!
//! ```toml
//! [[devices]]
//! id = "indicator"
//! driver = "indicator"
//! uses = { pixels = "strip" }
//! config = { "state.failed" = "blink #ff0000 period=250", brightness = 128 }
//! ```
//!
//! # Configuration
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `state.<name>` | the built-in pattern | The pattern for that state: see [`pattern`]. |
//! | `brightness` | 255 | Scales every pattern that sets no brightness of its own. |
//!
//! The built-in patterns are `idle` off, `receiving` a blue blink, `writing` a green bar over a dim
//! green, `verifying` a yellow chase, `done` solid green and `failed` a fast red blink.
//!
//! # Time
//!
//! The driver has no clock of its own. It is ticked: after it starts, after each method call, and when
//! the time it returned comes round. A state whose pattern never changes (`off`, `solid`, and a bar
//! at a fixed progress) costs nothing after it is drawn: its tick returns `None`. A blink or chase
//! returns the next instant its frame changes.
//!
//! # Failure
//!
//! A failed write to the pixels is never final. The frame stays and is written again after a backoff
//! that starts at 100 ms, doubles, and is capped at 8 s. `health` reports the failure until a write
//! succeeds. Nothing auto-offs.
//!
//! # State
//!
//! Nothing is kept across a restart: the driver starts `idle` and the caller sets the state again.
//!
//! # Allocation
//!
//! The frame (4 bytes per LED) is allocated when the driver starts; steady state allocates nothing.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod pattern;

use alloc::string::String;
use alloc::vec::Vec;

use lemnos_contract_indicator::{
    FULL_PROGRESS, Indicator as IndicatorContract, NO_PROGRESS, STATES,
};
use lemnos_contract_pixels::{MAX_LOCAL_LEDS, Pixels};
use lemnos_contract_self_test::{
    Bench, Flow, Measure, Record, Runner, Scratch, SelfTest, Spec, reason,
};
use lemnos_core::{
    CallerId, Config, Description, Dev, Health, Item, JobId, JobState, Lifecycle, MethodId, Reply,
    Result, Status, driver,
};
use pattern::{Pattern, draw, next_change};

/// `self-test/1`'s `run`, the second contract the driver serves.
const RUN: MethodId = MethodId {
    contract: 1,
    method: 2,
};
/// The checks this driver offers. Its names are stable.
const SPECS: &[Spec] = &[Spec {
    name: "lamp",
    description: "the indicator shows the receiving state for two seconds",
    intrusive: true,
    operator: true,
    fixture: "an operator watching the indicator",
    expect_ms: 2000,
}];
/// The state the `lamp` test shows.
const LAMP_STATE: &str = "receiving";
/// How long the `lamp` test holds its state.
const LAMP_NS: u64 = 2_000 * NS_PER_MS;

const NS_PER_MS: u64 = 1_000_000;
/// The first retry after a failed write, and the longest one.
const BACKOFF_FIRST_NS: u64 = 100 * NS_PER_MS;
const BACKOFF_MAX_NS: u64 = 8_000 * NS_PER_MS;

/// An indicator on a `pixels/1` device.
#[driver("indicator", provides = [IndicatorContract, SelfTest])]
pub struct Indicator {
    #[needs]
    pixels: Dev<dyn Pixels>,
    count: usize,
    /// One pattern per state, in [`STATES`]'s order.
    patterns: Vec<Pattern>,
    state: usize,
    progress: u16,
    /// A method changed the state since the last tick.
    changed: bool,
    /// When the current state began, in nanoseconds of the host's clock.
    since_ns: u64,
    /// The frame for the current state, 4 bytes per LED.
    frame: Vec<u8>,
    /// When the frame next changes (`None` while it doesn't).
    render_at: Option<u64>,
    /// The frame has not reached the device.
    unsent: bool,
    /// When an unsent frame is next tried.
    retry_at: u64,
    /// The current retry interval after a failure (0 when the last write succeeded).
    backoff: u64,
    /// The last write failed.
    failed: bool,
    /// The self-test checks: their runs and the last result of each.
    runner: Runner,
    /// Who the method being run is for (see [`Lifecycle::caller`]).
    calling: CallerId,
}

impl Indicator {
    /// Sends the frame to the device, in chunks of at most [`MAX_LOCAL_LEDS`], then shows it.
    fn write(&mut self) -> Result<()> {
        let mut bytes = [0u8; 4 * MAX_LOCAL_LEDS];
        for (chunk, leds) in self.frame.chunks(4 * MAX_LOCAL_LEDS).enumerate() {
            bytes[..leds.len()].copy_from_slice(leds);
            // Each chunk starts at an LED index below the strip's count, which fits in u16.
            let offset = (chunk * MAX_LOCAL_LEDS) as u16;
            self.pixels.set(offset, &bytes[..leds.len()])?;
        }
        self.pixels.show()
    }
}

/// The name of a configured state, `state.<name>`.
fn key(state: &str) -> String {
    let mut key = String::from("state.");
    key.push_str(state);
    key
}

impl Lifecycle for Indicator {
    type Needs = IndicatorNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    /// Ticked: a blink or chase, the write retries, and the self-test's steps are the timed work.
    const TICKS: bool = true;
    // The self-test runs its steps from the tick, and tracks who sets the state.
    const JOBS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn start(mut needs: IndicatorNeeds, cfg: &Config<'_>) -> Result<Self> {
        let count = usize::from(needs.pixels.pixel_count()?);
        let brightness = match cfg.int("brightness") {
            Some(level) => u8::try_from(level).map_err(|_| Status::INVALID_ARGUMENT)?,
            None => 255,
        };
        let defaults = pattern::defaults();
        let mut patterns = Vec::with_capacity(STATES.len());
        for (name, default) in STATES.iter().zip(defaults) {
            let text = cfg.str(&key(name)).unwrap_or(default);
            let parsed = pattern::parse(text, brightness).map_err(|_| Status::INVALID_ARGUMENT)?;
            patterns.push(parsed);
        }
        Ok(Self {
            pixels: needs.pixels,
            count,
            patterns,
            state: 0,
            progress: NO_PROGRESS,
            // The first tick draws idle, so the strip is never left as it was found.
            changed: true,
            since_ns: 0,
            frame: alloc::vec![0; count * 4],
            render_at: None,
            unsent: false,
            retry_at: 0,
            backoff: 0,
            failed: false,
            runner: Runner::new(SPECS),
            calling: CallerId::LOCAL,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("self_test", RUN));
    }

    fn health(&self) -> Health {
        if self.failed {
            return Health::Failed("the last write to the pixels failed; retrying");
        }
        if let Some(why) = self.runner.health() {
            return Health::Degraded(why);
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        // The test's steps first, so the frame they set is drawn in this same tick.
        let test = {
            let mut bench = Lamp {
                state: &mut self.state,
                progress: &mut self.progress,
                changed: &mut self.changed,
            };
            self.runner.tick(now_ns, &mut bench)
        };
        let frame = self.tick_frame(now_ns);
        match (test, frame) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
        self.runner.caller(caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        self.runner.caller_gone(caller);
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        self.runner.job(job, result)
    }

    fn job_cancel(&mut self, job: JobId) {
        let mut bench = Lamp {
            state: &mut self.state,
            progress: &mut self.progress,
            changed: &mut self.changed,
        };
        self.runner.cancel(job, &mut bench);
    }
}

impl SelfTest for Indicator {
    fn tests(&mut self, into: &mut [u8]) -> Result<usize> {
        self.runner.write_tests(into)
    }

    fn run(&mut self, name: &str, confirmed: bool) -> Result<JobId> {
        self.runner.start(name, confirmed, self.calling)
    }

    fn last(&mut self, into: &mut [u8]) -> Result<usize> {
        self.runner.write_last(into)
    }
}

/// The `lamp` test's steps: it shows [`LAMP_STATE`] for [`LAMP_NS`], and puts back the state it
/// found, so the frame returns to what the driver had set.
struct Lamp<'a> {
    state: &'a mut usize,
    progress: &'a mut u16,
    changed: &'a mut bool,
}

impl Bench for Lamp<'_> {
    fn step(&mut self, _test: usize, now_ns: u64, scratch: &mut Scratch) -> Flow {
        if scratch.step == 0 {
            scratch.saved = [*self.state as i64, i64::from(*self.progress), 0, 1];
            scratch.step = 1;
            let lamp = STATES
                .iter()
                .position(|name| *name == LAMP_STATE)
                .unwrap_or(0);
            *self.state = lamp;
            *self.progress = NO_PROGRESS;
            *self.changed = true;
            return Flow::Wait(now_ns + LAMP_NS);
        }
        Flow::Done(
            Record::pass(reason::OPERATOR_CONFIRMED)
                .said("an operator confirmed the indicator showed the state")
                .measure(Measure::new("held", (LAMP_NS / NS_PER_MS) as i64, 0, "ms")),
        )
    }

    fn restore(&mut self, scratch: &mut Scratch) -> core::result::Result<(), Status> {
        if scratch.saved[3] == 0 {
            return Ok(());
        }
        *self.state = scratch.saved[0] as usize;
        *self.progress = scratch.saved[1] as u16;
        *self.changed = true;
        scratch.saved[3] = 0;
        Ok(())
    }
}

impl Indicator {
    /// Draws the current state's frame when it is due, and sends it.
    fn tick_frame(&mut self, now_ns: u64) -> Option<u64> {
        if self.changed {
            self.changed = false;
            self.since_ns = now_ns;
            self.render_at = Some(now_ns);
        }
        if let Some(at) = self.render_at
            && now_ns >= at
        {
            let elapsed_ms = (now_ns - self.since_ns) / NS_PER_MS;
            let pattern = self.patterns[self.state];
            let progress = (self.progress != NO_PROGRESS).then_some(self.progress);
            draw(&pattern, self.count, progress, elapsed_ms, &mut self.frame);
            self.unsent = true;
            self.retry_at = now_ns;
            self.render_at = next_change(&pattern, self.count, elapsed_ms)
                .map(|ms| self.since_ns + ms * NS_PER_MS);
        }
        if self.unsent && now_ns >= self.retry_at {
            match self.write() {
                Ok(()) => {
                    self.unsent = false;
                    self.failed = false;
                    self.backoff = 0;
                }
                Err(_) => {
                    self.failed = true;
                    self.backoff = if self.backoff == 0 {
                        BACKOFF_FIRST_NS
                    } else {
                        (self.backoff * 2).min(BACKOFF_MAX_NS)
                    };
                    self.retry_at = now_ns + self.backoff;
                }
            }
        }
        let retry = self.unsent.then_some(self.retry_at);
        match (self.render_at, retry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

impl IndicatorContract for Indicator {
    fn set(&mut self, state: &str, progress: u16) -> Result<()> {
        let index = STATES
            .iter()
            .position(|name| *name == state)
            .ok_or(Status::INVALID_ARGUMENT)?;
        if progress != NO_PROGRESS && progress > FULL_PROGRESS {
            return Err(Status::INVALID_ARGUMENT);
        }
        // A self-test is showing its own state, and puts this one back when it ends.
        if self.runner.busy() {
            return Err(Status::BUSY);
        }
        self.state = index;
        self.progress = progress;
        self.changed = true;
        Ok(())
    }

    fn states(&mut self, into: &mut [u8]) -> Result<usize> {
        let mut text = String::new();
        for (i, name) in STATES.iter().enumerate() {
            if i > 0 {
                text.push(',');
            }
            text.push_str(name);
        }
        let bytes = text.as_bytes();
        let slot = into
            .get_mut(..bytes.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?;
        slot.copy_from_slice(bytes);
        Ok(bytes.len())
    }
}
