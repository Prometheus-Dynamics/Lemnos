//! A status light: a ring of LEDs, drawn on a `pixels/1` device, shown through `light/1`.
//!
//! Each caller that shows something owns a layer (a slot the caller picks), at a priority. A layer
//! holds one look, sent whole with [`Light::show`](lemnos_contract_light::Light::show), or one frame,
//! which sets the LEDs it names ([`Light::show_frame`](lemnos_contract_light::Light::show_frame)). The
//! look is animated by [`animator`] and written to the `pixels` device in chunks of at most
//! [`MAX_LOCAL_LEDS`] LEDs, then shown. Nothing is shown from a catalogue of named looks, except the
//! built-in `status.*` ([`builtin`]).
//!
//! # Precedence
//!
//! Each LED shows the value of the owner with the highest priority that set it. On equal priorities
//! the higher layer wins. A look sets every LED, so the highest look is the base; a frame wins over
//! the base on the LEDs it sets, when its priority beats the base's. Where no owner set an LED, the
//! board's default look (`default_look`, `status.ok` unless the board says otherwise) shows there.
//! Nothing is blended ([`layers`]).
//!
//! `clear(layer)` empties a layer only when the caller owns it. `clear_all` empties every layer.
//!
//! # Callers
//!
//! The driver is told who calls (`TRACKS_CALLERS`). An owner is the caller that showed the layer, and
//! a `show` or `show_frame` on a layer replaces the layer's owner too. When a caller goes (its
//! connection closes, or it dies), the layers it owns are emptied, and the ring shows what it would
//! show without them. Another caller's layers are not touched. A layer the host shows itself (no
//! client) is never ended by a caller going.
//!
//! # Frames and looks
//!
//! A look covers the whole ring and is animated, and fades from what it replaces. A frame is sent as
//! it is and has no fade. A frame's colours are scaled by the runtime brightness, and not by the
//! board's look brightness.
//!
//! The base look is the highest look, whichever layer holds it. It is animated as one, so a change of
//! the base fades as a look does, and frames are drawn over it at once.
//!
//! # Time
//!
//! The driver has no clock of its own. It is ticked (`Lifecycle::tick`) once after it starts, after
//! each method call, and when the time it returned comes round. A tick can come early, so each one
//! checks what is due: a render when the look is animating, and a write when a composed frame is
//! unsent. A ring that is not animating returns `None`, so an idle light costs nothing: no thread,
//! no timer, no sleep.
//!
//! # Failure
//!
//! A failed write is never final. The composed frame stays, and the write is tried again after a backoff
//! that starts at 100 ms, doubles, and is capped at 8 s. Nothing auto-offs. `health` reports the
//! failure until a write succeeds.
//!
//! # State
//!
//! Nothing is kept across a restart. A brightness set at runtime, or a look or frame sent before, is
//! forgotten; the board's settings ([`settings`]) are read again. The layers a caller holds are kept
//! only while it is connected: they end with it (see "Callers").
//!
//! # `no_std`
//!
//! The crate is `no_std`, and uses `alloc` for the frame and composition buffers only. Those are
//! allocated when the light starts, sized from the ring's `count`, and nothing allocates after that.

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

use alloc::vec;
use alloc::vec::Vec;

use animator::{Animator, Transition};
use layers::{LAYERS, Layers};
use lemnos_contract_light::{FRAME_ENTRY, Layout, Light as LightContract};
use lemnos_contract_pixels::{MAX_LOCAL_LEDS, Pixels};
use lemnos_core::{
    CallerId, Config, ConfigError, Description, Dev, Health, Item, Key, Kind, Lifecycle, MethodId,
    Result, Status, Unit, driver,
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
    /// The look the animator shows, so an unchanged base does not restart its time.
    shown: Option<Spec>,
    /// The base look's `(priority, layer)`: `None` for the default look.
    base_key: Option<(u8, usize)>,
    /// The runtime brightness of everything shown, 0..=255.
    brightness: u8,
    /// What the ring shows: each LED's colour. Sized at start.
    frame: Vec<Rgbw>,
    /// A method changed the layers or the runtime brightness since the last tick.
    changed: bool,
    /// Who the method about to run is for (see [`Lifecycle::caller`]). A look shown now is theirs.
    calling: CallerId,
    /// When the animator next renders, in nanoseconds (`None` while steady).
    render_at: Option<u64>,
    /// The composed frame has not reached the device.
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

    /// Sends the composed frame to the device, in chunks, then shows it.
    fn write(&mut self) -> Result<()> {
        write_frame(&mut self.pixels, &self.frame)
    }

    /// Composes the ring from the base look and the frames, and queues a write when it changed.
    fn compose(&mut self, now_ns: u64) {
        let changed = self.layers.compose(
            self.animator.frame(),
            self.base_key,
            self.brightness,
            &mut self.frame,
        );
        if changed && !self.unsent {
            self.unsent = true;
            self.retry_at = now_ns;
        }
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
        let count = usize::from(settings.count);
        Ok(Self {
            pixels: needs.pixels,
            layers: Layers::new(count),
            animator: Animator::new(count),
            settings,
            shown: None,
            base_key: None,
            brightness: 255,
            frame: vec![Rgbw::OFF; count],
            // The first tick shows the default look, so the ring is never dark at start.
            changed: true,
            calling: CallerId::LOCAL,
            render_at: None,
            unsent: false,
            retry_at: 0,
            backoff: 0,
            failed: false,
        })
    }

    /// The driver is told who calls, so a look is ended when its caller goes (`caller_gone`).
    const TRACKS_CALLERS: bool = true;

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }

    fn caller_gone(&mut self, caller: CallerId) {
        if self.layers.drop_owner(caller) {
            self.changed = true;
        }
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
        let mut due = false;
        if self.changed {
            self.changed = false;
            let (spec, key) = match self.layers.top() {
                Some(top) => (top.spec, Some((top.priority, top.layer))),
                None => (Spec::Status(self.settings.default_look), None),
            };
            self.base_key = key;
            if self.shown != Some(spec) {
                let look = wire::resolve(&spec, &self.settings);
                let transition = Transition::new(self.settings.fade_ms, self.settings.easing);
                self.animator.set(look, transition, now_ms);
                self.shown = Some(spec);
            }
            self.render_at = Some(now_ns);
            due = true;
        }
        if let Some(at) = self.render_at
            && now_ns >= at
        {
            self.animator.render(now_ms);
            self.render_at = self.animator.next_frame_ms(now_ms).map(|ms| ms * NS_PER_MS);
            due = true;
        }
        if due {
            self.compose(now_ns);
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
        self.layers.set_look(layer, priority, spec, self.calling);
        self.changed = true;
        Ok(())
    }

    fn clear(&mut self, layer: u8) -> Result<()> {
        let layer = Self::layer(layer)?;
        if self.layers.clear(layer, self.calling) {
            self.changed = true;
        }
        Ok(())
    }

    fn clear_all(&mut self) -> Result<()> {
        self.layers.clear_all();
        self.changed = true;
        Ok(())
    }

    fn set_brightness(&mut self, level: u8) -> Result<()> {
        self.animator.set_ring(level);
        self.brightness = level;
        self.changed = true;
        Ok(())
    }

    fn show_frame(&mut self, layer: u8, priority: u8, leds: &[u8]) -> Result<()> {
        let layer = Self::layer(layer)?;
        if !leds.len().is_multiple_of(FRAME_ENTRY) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let entries = leds.as_chunks::<FRAME_ENTRY>().0.iter().map(|e| {
            let led = usize::from(u16::from_le_bytes([e[0], e[1]]));
            (led, Rgbw::new(e[2], e[3], e[4], e[5]))
        });
        self.layers
            .set_frame(layer, priority, entries, self.calling);
        self.changed = true;
        Ok(())
    }
}
