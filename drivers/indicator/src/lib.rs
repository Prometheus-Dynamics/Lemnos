//! An indicator: named states (`idle`, `receiving`, `writing`, `verifying`, `done`, `failed`)
//! shown on LEDs, as `indicator/1`. A caller sets the state; the board says what each state looks
//! like.
//!
//! # One writer, in layers
//!
//! The indicator is the only writer of its LEDs, and what it shows is the top one of these, lowest
//! first:
//!
//! 1. `idle`: the board's idle pattern (off unless the board sets one).
//! 2. The application's frames. The driver also serves `pixels/1`: an application drives the
//!    *indicator* as it would a strip (`pixel_count`, `set`, `show`) and never the strip itself.
//!    The first caller to `set` or `show` owns the layer; another caller is refused `BUSY` until
//!    the owner goes, and then the layer is gone and what is under it shows. A frame is taken
//!    whole at `show`, so a frame sent in several `set` calls is never shown half way.
//! 3. A state set through `indicator/1`: `receiving`, `writing`, `verifying`, `failed`, and `done`
//!    for `done_ms` after it is set, when it drops to `idle`. `failed` stays until another state
//!    is set.
//! 4. The `lamp` self-test, for its two seconds.
//! 5. Locate (`indicator/1` `locate`), for the seconds asked, ending by itself.
//!
//! Every layer keeps taking what it is given while another is on top: an application's frames
//! land in its buffer and a state set under a locate is kept. When the top layer ends, the one
//! under it shows at once, the application's latest frame included. Nothing here makes a look:
//! an application computes its own frames, and each state's pattern is the board's.
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
//! | `locate` | `blink #ffffff period=250` | The pattern locate shows. |
//! | `done_ms` | 5000 | How long `done` shows before it drops to `idle`: 0 to 60000. |
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
//! Nothing is kept across a restart: the driver starts `idle` with no application frame, and the
//! callers set theirs again.
//!
//! # Allocation
//!
//! The frame and the application's two buffers (4 bytes per LED each) are allocated when the
//! driver starts; steady state allocates nothing. An application's frame costs two copies of
//! 4 bytes per LED here (into its buffer at `set`, and to the shown frame at `show`), then the
//! same `set` and `show` to the strip that driving the strip itself would have made.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod pattern;

#[cfg(test)]
mod tests;

use alloc::string::String;
use alloc::vec::Vec;

use lemnos_contract_indicator::{
    FULL_PROGRESS, Indicator as IndicatorContract, MAX_LOCATE_SECONDS, NO_PROGRESS, STATES,
};
use lemnos_contract_pixels::{MAX_LOCAL_LEDS, Pixels};
use lemnos_contract_self_test::{
    Bench, Flow, Measure, Record, Runner, Scratch, SelfTest, Spec, reason,
};
use lemnos_core::{
    CallerId, Config, ConfigError, ConfigValue, Description, Dev, Health, Item, JobId, JobState,
    Key, Kind, Lifecycle, MethodId, Reply, Result, Status, driver,
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
/// `indicator/1`'s `locating`, the reading that says locate is showing.
const LOCATING: MethodId = MethodId {
    contract: 0,
    method: 4,
};
/// The state the `lamp` test shows.
const LAMP_STATE: &str = "receiving";
/// `idle` and `done` in [`STATES`].
const IDLE: usize = 0;
const DONE: usize = 4;
/// What locate shows unless the board says.
const LOCATE_DEFAULT: &str = "blink #ffffff period=250";
/// How long `done` shows unless the board says, and the most it may say, in milliseconds.
const DONE_MS_DEFAULT: i64 = 5_000;
const DONE_MS_MAX: i64 = 60_000;
const NS_PER_S: u64 = 1_000_000_000;

/// The layer on top: what the LEDs show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Top {
    Idle,
    App,
    State(usize),
    Lamp,
    Locate,
}
/// How long the `lamp` test holds its state.
const LAMP_NS: u64 = 2_000 * NS_PER_MS;

const NS_PER_MS: u64 = 1_000_000;
/// The first retry after a failed write, and the longest one.
const BACKOFF_FIRST_NS: u64 = 100 * NS_PER_MS;
const BACKOFF_MAX_NS: u64 = 8_000 * NS_PER_MS;

/// An indicator on a `pixels/1` device.
#[driver("indicator", provides = [IndicatorContract, SelfTest, Pixels])]
pub struct Indicator {
    #[needs]
    pixels: Dev<dyn Pixels>,
    count: usize,
    /// One pattern per state, in [`STATES`]'s order.
    patterns: Vec<Pattern>,
    /// What locate shows.
    locate_pattern: Pattern,
    /// How long `done` shows.
    done_ns: u64,
    /// The state set through `indicator/1` ([`IDLE`] when none shows).
    state: usize,
    progress: u16,
    /// A method set the state since the last tick.
    changed: bool,
    /// When the state was set.
    state_at_ns: u64,
    /// The application's frame as its `set` calls leave it, and as its last `show` took it.
    app: Vec<u8>,
    app_shown: Vec<u8>,
    /// Who drives the application layer, once someone has.
    app_owner: Option<CallerId>,
    /// The owner has shown a frame: the layer is there.
    app_live: bool,
    /// A frame was shown since the last tick.
    app_dirty: bool,
    /// The `lamp` test is showing its state.
    lamp: bool,
    /// A `locate` asked since the last tick, in seconds (0 ends it), and who asked.
    locate_asked: Option<u16>,
    locate_owner: CallerId,
    /// When the locate that is showing ends.
    locate_until: Option<u64>,
    /// The layer the frame was last drawn for, and when it came on top.
    top: Top,
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

/// The settings this driver reads from its board file config.
const KEYS: &[Key] = &[
    Key::optional("brightness", Kind::Int),
    Key::optional("locate", Kind::Str),
    Key::optional("done_ms", Kind::Int),
    Key::optional("state.idle", Kind::Str),
    Key::optional("state.receiving", Kind::Str),
    Key::optional("state.writing", Kind::Str),
    Key::optional("state.verifying", Kind::Str),
    Key::optional("state.done", Kind::Str),
    Key::optional("state.failed", Kind::Str),
];

impl Lifecycle for Indicator {
    type Needs = IndicatorNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    /// Ticked: a blink or chase, the write retries, and the self-test's steps are the timed work.
    const TICKS: bool = true;
    // The self-test runs its steps from the tick, and tracks who sets the state.
    const JOBS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if let Some(level) = cfg.int("brightness")
            && u8::try_from(level).is_err()
        {
            return Err(ConfigError::new("brightness", "must be 0 to 255"));
        }
        if let Some(ms) = cfg.int("done_ms")
            && !(0..=DONE_MS_MAX).contains(&ms)
        {
            return Err(ConfigError::new("done_ms", "must be 0 to 60000"));
        }
        if let Some(text) = cfg.str("locate")
            && let Err(refused) = pattern::parse(text, 255)
        {
            return Err(ConfigError::new("locate", refused.0));
        }
        // Each state's pattern text must parse. The brightness it falls back to does not change
        // whether it parses, so any value will do here.
        for &(name, value) in cfg.entries() {
            if let ConfigValue::Str(text) = value
                && name.starts_with("state.")
                && let Err(refused) = pattern::parse(text, 255)
            {
                return Err(ConfigError::new(name, refused.0));
            }
        }
        Ok(())
    }

    fn start(mut needs: IndicatorNeeds, cfg: &Config<'_>) -> Result<Self> {
        let count = usize::from(needs.pixels.pixel_count()?);
        let brightness = match cfg.int("brightness") {
            // The range is checked by check_config.
            Some(level) => u8::try_from(level).map_err(|_| Status::INVALID_ARGUMENT)?,
            None => 255,
        };
        let defaults = pattern::defaults();
        let mut patterns = Vec::with_capacity(STATES.len());
        for (name, default) in STATES.iter().zip(defaults) {
            let text = cfg.str(&key(name)).unwrap_or(default);
            // The text is checked by check_config; this builds the pattern from it.
            let parsed = pattern::parse(text, brightness).map_err(|_| Status::INVALID_ARGUMENT)?;
            patterns.push(parsed);
        }
        let locate_pattern =
            pattern::parse(cfg.str("locate").unwrap_or(LOCATE_DEFAULT), brightness)
                .map_err(|_| Status::INVALID_ARGUMENT)?;
        // The range is checked by check_config.
        let done_ms = u64::try_from(cfg.int("done_ms").unwrap_or(DONE_MS_DEFAULT))
            .map_err(|_| Status::INVALID_ARGUMENT)?;
        Ok(Self {
            pixels: needs.pixels,
            count,
            patterns,
            locate_pattern,
            done_ns: done_ms * NS_PER_MS,
            state: IDLE,
            progress: NO_PROGRESS,
            // The first tick draws idle, so the strip is never left as it was found.
            changed: true,
            state_at_ns: 0,
            app: alloc::vec![0; count * 4],
            app_shown: alloc::vec![0; count * 4],
            app_owner: None,
            app_live: false,
            app_dirty: false,
            lamp: false,
            locate_asked: None,
            locate_owner: CallerId::LOCAL,
            locate_until: None,
            top: Top::Idle,
            since_ns: 0,
            frame: alloc::vec![0; count * 4],
            // Drawn at the first tick, whatever is on top then.
            render_at: Some(0),
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
        out.add(Item::value("locating", LOCATING).dimensionless());
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
                lamp: &mut self.lamp,
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
        // The application's layer goes with its owner, and what is under it shows.
        if self.app_owner == Some(caller) {
            self.app_owner = None;
            self.app_live = false;
            self.app.fill(0);
            self.app_shown.fill(0);
        }
        // So does a locate with the caller that asked for it.
        if self.locate_owner == caller
            && (self.locate_until.is_some() || self.locate_asked.is_some())
        {
            self.locate_asked = Some(0);
        }
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        self.runner.job(job, result)
    }

    fn job_cancel(&mut self, job: JobId) {
        let mut bench = Lamp {
            lamp: &mut self.lamp,
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

/// The `lamp` test's steps: it shows [`LAMP_STATE`] for [`LAMP_NS`] as a layer of its own, over the
/// state and the application, and then is gone: what was under it shows again as it now stands.
struct Lamp<'a> {
    lamp: &'a mut bool,
}

impl Bench for Lamp<'_> {
    fn step(&mut self, _test: usize, now_ns: u64, scratch: &mut Scratch) -> Flow {
        if scratch.step == 0 {
            scratch.step = 1;
            *self.lamp = true;
            return Flow::Wait(now_ns + LAMP_NS);
        }
        Flow::Done(
            Record::pass(reason::OPERATOR_CONFIRMED)
                .said("an operator confirmed the indicator showed the state")
                .measure(Measure::new("held", (LAMP_NS / NS_PER_MS) as i64, 0, "ms")),
        )
    }

    fn restore(&mut self, _scratch: &mut Scratch) -> core::result::Result<(), Status> {
        *self.lamp = false;
        Ok(())
    }
}

impl Indicator {
    /// The layer on top as things stand.
    fn top(&self) -> Top {
        if self.locate_until.is_some() {
            Top::Locate
        } else if self.lamp {
            Top::Lamp
        } else if self.state != IDLE {
            Top::State(self.state)
        } else if self.app_live {
            Top::App
        } else {
            Top::Idle
        }
    }

    /// The pattern a layer draws, and its progress. `None` for the application's layer, whose
    /// frame is its own.
    fn pattern_of(&self, top: Top) -> Option<(Pattern, Option<u16>)> {
        let lamp = || {
            STATES
                .iter()
                .position(|name| *name == LAMP_STATE)
                .unwrap_or(IDLE)
        };
        match top {
            Top::App => None,
            Top::Idle => Some((self.patterns[IDLE], None)),
            Top::State(state) => Some((
                self.patterns[state],
                (self.progress != NO_PROGRESS).then_some(self.progress),
            )),
            Top::Lamp => Some((self.patterns[lamp()], None)),
            Top::Locate => Some((self.locate_pattern, None)),
        }
    }

    /// Takes what the methods asked since the last tick, ends what has run its time, draws the top
    /// layer's frame when it is due, and sends it.
    fn tick_frame(&mut self, now_ns: u64) -> Option<u64> {
        let set = core::mem::take(&mut self.changed);
        if set {
            self.state_at_ns = now_ns;
        }
        // `done` is not a lasting layer: after its time whatever is under it shows.
        let done_at = (self.state == DONE).then(|| self.state_at_ns.saturating_add(self.done_ns));
        if done_at.is_some_and(|at| now_ns >= at) {
            self.state = IDLE;
            self.progress = NO_PROGRESS;
        }
        let done_at = done_at.filter(|_| self.state == DONE);
        if let Some(seconds) = self.locate_asked.take() {
            self.locate_until =
                (seconds != 0).then(|| now_ns.saturating_add(u64::from(seconds) * NS_PER_S));
        }
        if self.locate_until.is_some_and(|until| now_ns >= until) {
            self.locate_until = None;
        }
        let shown = core::mem::take(&mut self.app_dirty);

        let top = self.top();
        // A layer that came on top starts its pattern from the beginning. So does a state set
        // again while it is on top, and an application's new frame is drawn when it is on top.
        // What happens under the top layer changes nothing that is shown.
        if top != self.top || (set && matches!(top, Top::State(_))) {
            self.top = top;
            self.since_ns = now_ns;
            self.render_at = Some(now_ns);
        } else if shown && top == Top::App {
            self.render_at = Some(now_ns);
        }
        if let Some(at) = self.render_at
            && now_ns >= at
        {
            match self.pattern_of(top) {
                Some((pattern, progress)) => {
                    let elapsed_ms = (now_ns - self.since_ns) / NS_PER_MS;
                    draw(&pattern, self.count, progress, elapsed_ms, &mut self.frame);
                    self.render_at = next_change(&pattern, self.count, elapsed_ms)
                        .map(|ms| self.since_ns + ms * NS_PER_MS);
                }
                None => {
                    self.frame.copy_from_slice(&self.app_shown);
                    self.render_at = None;
                }
            }
            self.unsent = true;
            self.retry_at = now_ns;
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
        // The next thing with a time: a frame change, a retry, `done` dropping, locate ending.
        [
            self.render_at,
            self.unsent.then_some(self.retry_at),
            done_at,
            self.locate_until,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Whether the method being run may drive the application's layer: its owner, or the first
    /// caller to try while it has none.
    fn app_caller(&mut self) -> Result<()> {
        match self.app_owner {
            Some(owner) if owner != self.calling => Err(Status::BUSY),
            Some(_) => Ok(()),
            None => {
                self.app_owner = Some(self.calling);
                Ok(())
            }
        }
    }
}

/// The application's layer: the indicator as a strip. See the crate notes.
impl Pixels for Indicator {
    fn pixel_count(&mut self) -> Result<u16> {
        u16::try_from(self.count).map_err(|_| Status::IO)
    }

    fn set(&mut self, offset: u16, pixels: &[u8]) -> Result<()> {
        self.app_caller()?;
        let at = usize::from(offset) * 4;
        // LEDs past the end are ignored, and a partial LED at the end is dropped, as a strip does.
        let room = self.app.len().saturating_sub(at);
        let len = (pixels.len() / 4 * 4).min(room);
        if len > 0 {
            self.app[at..at + len].copy_from_slice(&pixels[..len]);
        }
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        self.app_caller()?;
        // The frame is taken whole here: what is shown never has half of a later one in it.
        self.app_shown.copy_from_slice(&self.app);
        self.app_live = true;
        self.app_dirty = true;
        Ok(())
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
        // Taken whatever is on top: under a locate or the lamp test it shows when they end.
        self.state = index;
        self.progress = progress;
        self.changed = true;
        Ok(())
    }

    fn locate(&mut self, seconds: u16) -> Result<()> {
        if seconds > MAX_LOCATE_SECONDS {
            return Err(Status::INVALID_ARGUMENT);
        }
        // The lamp test is an operator watching the LEDs for what it shows.
        if seconds != 0 && self.runner.busy() {
            return Err(Status::BUSY);
        }
        self.locate_asked = Some(seconds);
        self.locate_owner = self.calling;
        Ok(())
    }

    fn locating(&mut self) -> Result<bool> {
        Ok(match self.locate_asked {
            Some(seconds) => seconds != 0,
            None => self.locate_until.is_some(),
        })
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
