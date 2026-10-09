//! A light's renderer: intents from clients and the service, arbitrated,
//! animated, and written only while something moves.

use lemnos_ipc::{LedRequest, LedShow};
use lemnos_light::{
    Animator, Arbiter, Defaults, Intent, Layer, Look, Rgbw, Show, Transition, Winner,
};

/// The most LEDs a light may have.
pub(crate) const MAX_LEDS: usize = 64;
/// The most intents a light holds at once.
pub(crate) const MAX_INTENTS: usize = 32;
/// The owner id the service itself uses (alerts, system states).
pub(crate) const SERVICE_OWNER: u32 = 0;
/// How long a test-layer intent lasts when the client gives no duration:
/// a lease the client renews by sending it again.
pub const TEST_LEASE_MS: u32 = 10_000;

pub(crate) type LightIntent = Intent<MAX_LEDS>;

/// A light's state.
pub(crate) struct Light {
    pub slot: usize,
    pub defaults: Defaults,
    pub animator: Animator<MAX_LEDS>,
    pub arbiter: Arbiter<MAX_LEDS, MAX_INTENTS>,
    shown: Option<(u32, Layer, LightIntent)>,
    /// Per-owner frames built from single-LED writes (separately for the
    /// test layer).
    frames: Vec<((u32, bool), [Rgbw; MAX_LEDS])>,
    pub count: usize,
    pub next_render_ms: u64,
}

/// `0xWWRRGGBB` as a colour.
pub(crate) fn rgbw(value: u32) -> Rgbw {
    Rgbw::new(
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    )
}

impl Light {
    pub fn new(slot: usize, count: usize, defaults: Defaults) -> Self {
        Self {
            slot,
            defaults,
            animator: Animator::new(count.min(MAX_LEDS)),
            arbiter: Arbiter::new(),
            shown: None,
            frames: Vec::new(),
            count: count.min(MAX_LEDS),
            next_render_ms: 0,
        }
    }

    /// Applies a client's request; returns `false` when every intent slot
    /// is taken.
    pub fn request(&mut self, owner: u32, priority: u8, request: &LedRequest, now_ms: u64) -> bool {
        let key = (owner, request.test);
        let show = match &request.show {
            LedShow::Clear if request.test => {
                self.arbiter.clear(owner, Some(Layer::Test));
                self.frames.retain(|(k, _)| *k != key);
                return true;
            }
            LedShow::Clear => {
                self.forget(owner);
                return true;
            }
            LedShow::Status(status) => Show::Status(*status),
            LedShow::Color(rgb) => Show::Color(rgbw(*rgb & 0xff_ffff)),
            LedShow::Frame(pixels) => {
                let mut frame = [Rgbw::OFF; MAX_LEDS];
                for (slot, value) in frame.iter_mut().zip(pixels) {
                    *slot = rgbw(*value);
                }
                self.store_frame(key, frame);
                Show::Frame {
                    pixels: frame,
                    len: pixels.len().min(MAX_LEDS),
                }
            }
            LedShow::Pixels(pixels) => {
                let mut frame = self.frame_of(key);
                for (index, value) in pixels {
                    if let Some(slot) = frame.get_mut(usize::from(*index)) {
                        *slot = rgbw(*value);
                    }
                }
                self.store_frame(key, frame);
                Show::Frame {
                    pixels: frame,
                    len: self.count,
                }
            }
            LedShow::Progress {
                fraction,
                color,
                background,
            } => Show::Progress {
                fraction: *fraction,
                color: color.map(rgbw),
                background: background.map(rgbw),
            },
            LedShow::Indeterminate { color } => Show::Indeterminate {
                color: color.map(rgbw),
            },
            LedShow::System(state) => Show::System(*state),
            LedShow::Locate => Show::Locate,
        };
        let mut intent = Intent::new(show);
        intent.effect = request.effect;
        intent.period_ms = request.period_ms;
        intent.depth = request.depth;
        intent.brightness = request
            .brightness
            .map(|b| ((u32::from(b.min(1000)) * 255 + 500) / 1000) as u8);
        intent.fade_ms = request.fade_ms;
        intent.easing = request.easing;
        intent.test = request.test;
        let duration = match request.duration_ms {
            None if request.test => Some(TEST_LEASE_MS),
            duration => duration,
        };
        let expires = duration.map(|d| now_ms + u64::from(d));
        self.arbiter.hold(owner, priority, intent, expires)
    }

    fn frame_of(&self, key: (u32, bool)) -> [Rgbw; MAX_LEDS] {
        self.frames
            .iter()
            .find(|(k, _)| *k == key)
            .map_or([Rgbw::OFF; MAX_LEDS], |(_, f)| *f)
    }

    fn store_frame(&mut self, key: (u32, bool), frame: [Rgbw; MAX_LEDS]) {
        match self.frames.iter_mut().find(|(k, _)| *k == key) {
            Some((_, f)) => *f = frame,
            None => self.frames.push((key, frame)),
        }
    }

    /// Holds a service intent (an alert or a system state) in its layer.
    pub fn hold_service(&mut self, intent: LightIntent, expires_ms: Option<u64>) {
        self.arbiter
            .hold(SERVICE_OWNER, u8::MAX, intent, expires_ms);
    }

    pub fn clear_service(&mut self, layer: Layer) {
        self.arbiter.clear(SERVICE_OWNER, Some(layer));
    }

    /// Drops an owner's intents.
    pub fn forget(&mut self, owner: u32) {
        self.arbiter.clear(owner, None);
        self.frames.retain(|((o, _), _)| *o != owner);
    }

    /// Re-arbitrates after changes: expires intents and, when the winner
    /// changed, starts the fade to its look. Returns the new winner when it
    /// changed (`Some(None)`: nobody holds an intent now).
    pub fn arbitrate(&mut self, now_ms: u64) -> Option<Option<Winner<MAX_LEDS>>> {
        self.arbiter.expire(now_ms);
        let winner = self.arbiter.winner();
        let key = winner.map(|w| (w.owner, w.layer, w.intent));
        if key == self.shown {
            return None;
        }
        self.shown = key;
        let (look, transition) = match &winner {
            Some(w) => w.intent.resolve(&self.defaults),
            None => (
                Look::fill(self.defaults.idle),
                Transition::new(self.defaults.fade_ms, self.defaults.easing),
            ),
        };
        self.animator.set(look, transition, now_ms);
        Some(winner)
    }

    /// When the light needs attention next: a frame or an expiry.
    pub fn next_ms(&self, now_ms: u64) -> Option<u64> {
        match (
            self.animator.next_frame_ms(now_ms),
            self.arbiter.next_expiry(),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}
