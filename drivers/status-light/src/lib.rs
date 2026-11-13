//! A status light: a ring of LEDs, drawn on a `pixels/1` device, shown through `light/1`.
//!
//! Whoever calls the contract sends a whole look with each `show`, on a layer and at a priority. The
//! highest priority among the layers with a look is what the ring shows ([`layers`]). The look is
//! animated by [`animator`] and written to the `pixels` device in chunks of at most
//! [`MAX_LOCAL_LEDS`] LEDs, then shown. Nothing is shown from a catalogue of named looks, except the
//! built-in `status.*` ([`builtin`]).
//!
//! # Time
//!
//! The driver has no clock of its own. It is ticked (`Lifecycle::tick`) once after it starts, after
//! each method call, and when the time it returned comes round. A tick can come early, so each one
//! checks what is due: a render when the look is animating, and a write when a frame is unsent. A
//! ring that is not animating returns `None`, so an idle light costs nothing: no thread, no timer,
//! no sleep.
//!
//! # Failure
//!
//! A failed write is never final. The frame stays, and the write is tried again after a backoff
//! that starts at 100 ms, doubles, and is capped at 8 s. Nothing auto-offs. `health` reports the
//! failure until a write succeeds.
//!
//! # State
//!
//! Nothing is kept across a restart. A brightness set at runtime, or a look sent before, is
//! forgotten; the board's settings ([`settings`]) are read again.
//!
//! # `no_std`
//!
//! The crate is `no_std`, and uses `alloc` for the frame buffers only. Those are allocated when the
//! light starts, sized from the ring's `count`, and nothing allocates after that.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod animator;
mod builtin;
mod easing;
mod layers;
mod look;
mod ring;
mod settings;
mod wire;

#[cfg(test)]
mod golden;

pub use builtin::StatusLook;
pub use easing::Easing;
pub use look::{Block, Envelope, Look, Rgbw};
pub use settings::{EffectKind, Settings};
pub use wire::{MAX_LEN, Spec};

use animator::{Animator, Transition};
use layers::{LAYERS, Layers};
use lemnos_contract_light::{Layout, Light as LightContract};
use lemnos_contract_pixels::{MAX_LOCAL_LEDS, Pixels};
use lemnos_core::{
    Config, ConfigError, Description, Dev, Health, Item, Key, Kind, Lifecycle, MethodId, Result,
    Status, Unit, driver,
};

const NS_PER_MS: u64 = 1_000_000;
/// The first retry after a failed write, and the longest one.
const BACKOFF_FIRST_NS: u64 = 100 * NS_PER_MS;
const BACKOFF_MAX_NS: u64 = 8_000 * NS_PER_MS;

/// `layout`: method 1 of the `light/1` contract.
const LAYOUT: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// A status light on a `pixels/1` device.
#[driver("status-light", provides = [LightContract])]
pub struct StatusLight {
    #[needs]
    pixels: Dev<dyn Pixels>,
    settings: Settings,
    layers: Layers,
    animator: Animator,
    /// The look the animator shows, so an unchanged winner does not restart its time.
    shown: Option<Spec>,
    /// A method changed the layers or the runtime brightness since the last tick.
    changed: bool,
    /// When the animator next renders, in nanoseconds (`None` while steady).
    render_at: Option<u64>,
    /// The frame in the animator has not reached the device.
    unsent: bool,
    /// When an unsent frame is next tried.
    retry_at: u64,
    /// The current retry interval after a failure (0 when the last write succeeded).
    backoff: u64,
    /// The last write failed.
    failed: bool,
}

impl StatusLight {
    fn layer(layer: u8) -> Result<usize> {
        let layer = usize::from(layer);
        if layer < LAYERS {
            Ok(layer)
        } else {
            Err(Status::INVALID_ARGUMENT)
        }
    }

    /// Sends the animator's frame to the device, in chunks, then shows it.
    fn write(&mut self) -> Result<()> {
        write_frame(&mut self.pixels, self.animator.frame())
    }
}

/// Sends `frame` to a `pixels/1` device: `set` for each run of at most [`MAX_LOCAL_LEDS`] LEDs, then
/// `show`. The byte buffer lives on the stack, so this allocates nothing.
fn write_frame(pixels: &mut Dev<dyn Pixels>, frame: &[look::Rgbw]) -> Result<()> {
    let mut bytes = [0u8; 4 * MAX_LOCAL_LEDS];
    for (chunk, leds) in frame.chunks(MAX_LOCAL_LEDS).enumerate() {
        for (slot, led) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(leds) {
            slot.copy_from_slice(&[led.r, led.g, led.b, led.w]);
        }
        // Each chunk starts at an LED index below the ring's count, which fits in u16.
        let offset = (chunk * MAX_LOCAL_LEDS) as u16;
        pixels.set(offset, &bytes[..leds.len() * 4])?;
    }
    pixels.show()
}

/// Every setting the driver reads (see `settings`). `count` is required; the rest have defaults.
const KEYS: &[Key] = &[
    Key::required("count", Kind::Int),
    Key::optional("fade_ms", Kind::Int),
    Key::optional("easing", Kind::Str),
    Key::optional("status_effect", Kind::Str),
    Key::optional("error_effect", Kind::Str),
    Key::optional("breathe_period_ms", Kind::Int),
    Key::optional("breathe_depth", Kind::Float),
    Key::optional("blink_period_ms", Kind::Int),
    Key::optional("blink_duty", Kind::Float),
    Key::optional("look_brightness", Kind::Float),
    Key::optional("ok", Kind::Int),
    Key::optional("warn", Kind::Int),
    Key::optional("error", Kind::Int),
    Key::optional("busy", Kind::Int),
    Key::optional("default_look", Kind::Str),
    Key::optional("spinner_period_ms", Kind::Int),
    Key::optional("spinner_tail", Kind::Int),
    Key::optional("spinner_base", Kind::Float),
];

impl Lifecycle for StatusLight {
    type Needs = StatusLightNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        Settings::from_config(cfg).map(|_| ())
    }

    /// Ticked: the animator's frame interval and the write retries are the only timed work, and
    /// both are returned from [`Lifecycle::tick`].
    const TICKS: bool = true;

    fn start(needs: StatusLightNeeds, cfg: &Config<'_>) -> Result<Self> {
        let settings = Settings::from_config(cfg).map_err(|_| Status::INVALID_ARGUMENT)?;
        Ok(Self {
            pixels: needs.pixels,
            layers: Layers::default(),
            animator: Animator::new(usize::from(settings.count)),
            settings,
            shown: None,
            // The first tick shows the default look, so the ring is never dark at start.
            changed: true,
            render_at: None,
            unsent: false,
            retry_at: 0,
            backoff: 0,
            failed: false,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("layout", LAYOUT).unit(Unit::One, 0));
    }

    fn health(&self) -> Health {
        if self.failed {
            return Health::Failed("the last write to the pixels failed; retrying");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let now_ms = now_ns / NS_PER_MS;
        if self.changed {
            self.changed = false;
            let spec = self
                .layers
                .winner()
                .unwrap_or(Spec::Status(self.settings.default_look));
            if self.shown != Some(spec) {
                let look = wire::resolve(&spec, &self.settings);
                let transition = Transition::new(self.settings.fade_ms, self.settings.easing);
                self.animator.set(look, transition, now_ms);
                self.shown = Some(spec);
            }
            self.render_at = Some(now_ns);
        }
        if let Some(at) = self.render_at
            && now_ns >= at
        {
            if self.animator.render(now_ms).is_some() && !self.unsent {
                self.unsent = true;
                self.retry_at = now_ns;
            }
            self.render_at = self.animator.next_frame_ms(now_ms).map(|ms| ms * NS_PER_MS);
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

impl LightContract for StatusLight {
    fn layout(&mut self) -> Result<Layout> {
        Ok(Layout {
            pixels: self.settings.count,
            layers: LAYERS as u8,
        })
    }

    fn show(&mut self, layer: u8, priority: u8, look: &[u8]) -> Result<()> {
        let layer = Self::layer(layer)?;
        let spec = Spec::decode(look)?;
        self.layers.set(layer, priority, spec);
        self.changed = true;
        Ok(())
    }

    fn clear(&mut self, layer: u8) -> Result<()> {
        self.layers.clear(Self::layer(layer)?);
        self.changed = true;
        Ok(())
    }

    fn clear_all(&mut self) -> Result<()> {
        self.layers.clear_all();
        self.changed = true;
        Ok(())
    }

    fn set_brightness(&mut self, level: u8) -> Result<()> {
        self.animator.set_ring(level);
        self.changed = true;
        Ok(())
    }
}
