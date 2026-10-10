//! A light's renderer: intents from clients and the service, arbitrated,
//! animated, and written only while something moves.

use lemnos_ipc::{LedRequest, LedShow};
use lemnos_light::{
    Animator, Arbiter, Defaults, Intent, Layer, LookName, LookSpec, Lookup, Rgbw, Show, Transition,
    Winner,
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
    /// The light's gravity settings (its IMU and ring geometry), if the
    /// board gives it any.
    pub gravity: Option<lemnos_board::Gravity>,
    /// The look changed since the last render: the host checks what it
    /// needs for the new look (the gravity of a falling sparkle).
    pub look_changed: bool,
    /// A one-shot IMU read for the falling sparkle is on its way.
    pub waiting_gravity: bool,
    /// Per-owner frames built from single-LED writes (separately for the
    /// test layer).
    frames: Vec<((u32, bool), [Rgbw; MAX_LEDS])>,
    pub count: usize,
    pub next_render_ms: u64,
    /// The frame the ring shows (the last one written), for frame watches.
    pub current: [Rgbw; MAX_LEDS],
    /// `current` is a frame the ring has shown (not yet, on a new light).
    pub has_frame: bool,
    /// Clients watching this light's frames (`Request::WatchFrames`).
    pub watchers: Vec<FrameWatch>,
}

/// A client's watch of a light's frames: the least time between two frames,
/// and the last frame and description sent (a frame goes out only when it
/// differs from the last one).
#[derive(Debug, Clone)]
pub(crate) struct FrameWatch {
    pub client: u32,
    pub period_ms: u64,
    /// When the next frame may go out.
    pub next_ms: u64,
    /// The frame last sent (`None`: none yet).
    pub sent: Option<[Rgbw; MAX_LEDS]>,
    /// The description last sent.
    pub info_sent: Option<lemnos_ipc::LightInfo>,
    /// Frames sent on this watch.
    pub seq: u32,
}

/// When the frame after one due at `due` is due, having rendered at `now`:
/// one frame later on the same cadence, or, if the light was woken a frame
/// or more late, one frame after `now` (no burst of catch-up frames).
pub(crate) fn next_frame_due(due: u64, now: u64) -> u64 {
    let frame = u64::from(lemnos_light::FRAME_MS);
    let base = if now >= due + frame { now } else { due };
    base + frame
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
            gravity: None,
            look_changed: false,
            waiting_gravity: false,
            frames: Vec::new(),
            count: count.min(MAX_LEDS),
            next_render_ms: 0,
            current: [Rgbw::OFF; MAX_LEDS],
            has_frame: false,
            watchers: Vec::new(),
        }
    }

    /// Applies a client's request; returns `false` when every intent slot
    /// is taken, or the request names a look that cannot be a look.
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
            LedShow::Orbit {
                color,
                tail,
                heads,
                base,
            } => Show::Orbit {
                color: color.map(rgbw),
                tail: *tail,
                heads: *heads,
                base: *base,
            },
            LedShow::System(state) => Show::System(*state),
            LedShow::Locate => Show::Locate,
            LedShow::Look { name, progress } => match LookName::new(name) {
                Some(name) => Show::Look {
                    name,
                    progress: *progress,
                },
                None => return false,
            },
            LedShow::Inline { spec, progress } => Show::Inline {
                spec: **spec,
                progress: *progress,
            },
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
    /// changed (`Some(None)`: nobody holds an intent now). `lookup` gives
    /// the look files' looks by name.
    pub fn arbitrate(
        &mut self,
        now_ms: u64,
        lookup: Lookup<'_>,
    ) -> Option<Option<Winner<MAX_LEDS>>> {
        self.arbiter.expire(now_ms);
        let winner = self.arbiter.winner();
        let key = winner.map(|w| (w.owner, w.layer, w.intent));
        if key == self.shown {
            return None;
        }
        let previous = self.shown.map(|(_, _, intent)| intent.show);
        self.shown = key;
        let (look, transition) = match &winner {
            // The ember of a restart hands over to a trial boot's sparkle in
            // a slow cross-fade, not the usual fade.
            Some(w)
                if matches!(
                    previous,
                    Some(Show::System(lemnos_light::SystemState::Rebooting))
                ) && matches!(
                    w.intent.show,
                    Show::System(lemnos_light::SystemState::Booting)
                ) =>
            {
                let (look, _) = w.intent.resolve(&self.defaults, lookup);
                (
                    look,
                    Transition::new(crate::update::TRIAL_CROSSFADE_MS, self.defaults.easing),
                )
            }
            Some(w) => w.intent.resolve(&self.defaults, lookup),
            None => (
                LookSpec::fill(self.defaults.idle),
                Transition::new(self.defaults.fade_ms, self.defaults.easing),
            ),
        };
        self.animator.set(look, transition, now_ms);
        self.look_changed = true;
        Some(winner)
    }

    /// Makes the next arbitration re-resolve the shown intent (after a look
    /// file changed).
    pub fn invalidate(&mut self) {
        self.shown = None;
    }

    /// When the light needs attention next: a frame or an expiry. A frame
    /// is due at the light's own cadence (`next_render_ms`), not a fixed
    /// interval from the wake-up, so a late wake does not stretch the frames.
    /// The winning intent: its owner, layer and intent (`None`: nothing holds
    /// the light).
    pub fn shown_intent(&self) -> Option<(u32, Layer, LightIntent)> {
        self.shown
    }

    pub fn next_ms(&self, now_ms: u64) -> Option<u64> {
        let frame = self
            .animator
            .next_frame_ms(now_ms)
            .map(|_| self.next_render_ms.max(now_ms));
        match (frame, self.arbiter.next_expiry()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_keep_their_cadence_and_a_late_wake_resyncs_without_a_burst() {
        let frame = u64::from(lemnos_light::FRAME_MS);
        // On time, and a millisecond late: the next frame stays on the grid.
        assert_eq!(next_frame_due(100, 100), 100 + frame);
        assert_eq!(next_frame_due(100, 101), 100 + frame);
        assert_eq!(
            next_frame_due(100 + frame, 100 + frame + 1),
            100 + 2 * frame
        );
        // Woken a whole frame late: resync to now, not a catch-up burst.
        assert_eq!(next_frame_due(100, 100 + frame), 100 + 2 * frame);
        assert_eq!(next_frame_due(100, 500), 500 + frame);
    }
}
