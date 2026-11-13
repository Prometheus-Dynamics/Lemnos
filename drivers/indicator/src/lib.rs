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
use lemnos_core::{Config, Description, Dev, Health, Lifecycle, Result, Status, driver};
use pattern::{Pattern, draw, next_change};

const NS_PER_MS: u64 = 1_000_000;
/// The first retry after a failed write, and the longest one.
const BACKOFF_FIRST_NS: u64 = 100 * NS_PER_MS;
const BACKOFF_MAX_NS: u64 = 8_000 * NS_PER_MS;

/// An indicator on a `pixels/1` device.
#[driver("indicator", provides = [IndicatorContract])]
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
    /// Ticked: a blink or chase, and the write retries, are the only timed work.
    const TICKS: bool = true;

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
        })
    }

    fn describe(&self, _out: &mut Description) {}

    fn health(&self) -> Health {
        if self.failed {
            return Health::Failed("the last write to the pixels failed; retrying");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
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
