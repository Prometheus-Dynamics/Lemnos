//! A light's renderer: intents from clients and the service, arbitrated,
//! animated, and written only while something moves.

use lemnos_ipc::{LedRequest, LedShow};
use lemnos_light::{
    Animator, Arbiter, Defaults, Intent, Layer, LookName, LookSpec, Lookup, Rgbw, Show, SpotList,
    Transition, Winner,
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
    /// Bumped when what the light's description (`LightInfo`) reports may
    /// have changed: the shown intent (`arbitrate`, `invalidate`) or the
    /// ring brightness (`light_setting`).
    pub info_gen: u64,
    /// The description last built, and the `info_gen` it was built at.
    pub info: Option<(u64, lemnos_ipc::LightInfo)>,
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
            info_gen: 0,
            info: None,
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
            LedShow::LookSpots {
                name,
                progress,
                spots,
            } => match LookName::new(name) {
                Some(name) => Show::LookSpots {
                    name,
                    progress: *progress,
                    spots: *spots,
                },
                None => return false,
            },
            LedShow::InlineSpots {
                spec,
                progress,
                spots,
            } => Show::InlineSpots {
                spec: **spec,
                progress: *progress,
                spots: *spots,
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
        self.info_gen += 1;
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
        // An input-only change (a new progress or spots) is not a new look: the
        // light's gravity is read for a look that starts, not for each input.
        if self.animator.set(look, transition, now_ms) {
            self.look_changed = true;
        }
        Some(winner)
    }

    /// Changes the inputs (`progress`, `spots`) of `owner`'s look on this light
    /// and nothing else (see `Request::LedInputs`). Refused, with the reason,
    /// when the owner shows no look here or the look takes no such input. The
    /// caller re-arbitrates: the look's time carries on, and only its input
    /// blocks move.
    pub fn update_inputs(
        &mut self,
        owner: u32,
        progress: Option<u16>,
        spots: Option<SpotList>,
        lookup: Lookup<'_>,
    ) -> Result<(), String> {
        if progress.is_none() && spots.is_none() {
            return Err("give a progress, spots or both".into());
        }
        let defaults = self.defaults;
        let Some(intent) = self.arbiter.look_mut(owner) else {
            return Err("this client shows no look on this light (send a look first)".into());
        };
        let Some(template) = intent.look_template(&defaults, lookup) else {
            return Err("this client's look is not one the service has".into());
        };
        if progress.is_some() && !template.takes_progress() {
            return Err("this look takes no progress (no arc with fraction = \"input\")".into());
        }
        if spots.is_some() && !template.takes_spots() {
            return Err("this look takes no spots (no spots block with spots = \"input\")".into());
        }
        intent.set_inputs(progress, spots);
        Ok(())
    }

    /// Makes the next arbitration re-resolve the shown intent (after a look
    /// file changed).
    pub fn invalidate(&mut self) {
        self.shown = None;
        self.info_gen += 1;
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
    use lemnos_light::{LayerSpec, Spot, SpotSource};

    /// A comet (blue) with a spots block that takes its spots from the request.
    fn comet_and_spots() -> LookSpec {
        let mut look = LookSpec::EMPTY;
        look.push(LayerSpec::comet(Rgbw::rgb(0x0000ff), 1_600, 4_000, 1, 0));
        look.push(LayerSpec::spots(Rgbw::rgb(0xff0000), SpotSource::Input));
        look
    }

    fn spots_request(level: u16) -> LedRequest {
        LedRequest::new(LedShow::InlineSpots {
            spec: Box::new(comet_and_spots()),
            progress: None,
            spots: SpotList::new(&[Spot::new(500, level)]),
        })
    }

    fn no_looks(_: &str) -> Option<LookSpec> {
        None
    }

    /// Arbitrates and renders `light` at `now`, keeping the last frame.
    fn ring(light: &mut Light, now: u64, last: &mut [Rgbw; MAX_LEDS]) {
        light.arbitrate(now, &no_looks);
        if let Some(frame) = light.animator.render(now) {
            let n = frame.len().min(MAX_LEDS);
            last[..n].copy_from_slice(&frame[..n]);
        }
    }

    #[test]
    fn an_input_update_moves_the_spot_and_keeps_the_comet_turn() {
        // LED 8 is the spot's (position 0.5 of 16, and its falloff reaches no
        // other LED): every other LED is the comet, which must be where it
        // would be had the look never changed.
        let mut reference = Light::new(0, 16, Defaults::default());
        assert!(reference.request(1, 0, &spots_request(400), 0));
        let mut a = [Rgbw::OFF; MAX_LEDS];
        ring(&mut reference, 0, &mut a);
        ring(&mut reference, 800, &mut a);

        let mut updated = Light::new(0, 16, Defaults::default());
        assert!(updated.request(1, 0, &spots_request(1_000), 0));
        let mut b = [Rgbw::OFF; MAX_LEDS];
        ring(&mut updated, 0, &mut b);
        ring(&mut updated, 500, &mut b);
        let update = SpotList::new(&[Spot::new(500, 400)]);
        assert_eq!(
            updated.update_inputs(1, None, Some(update), &no_looks),
            Ok(())
        );
        ring(&mut updated, 500, &mut b);
        // Just after the update the spot is still gliding down from full
        // (its level, not a fade of the look): brighter than at rest.
        let mut gliding = [Rgbw::OFF; MAX_LEDS];
        ring(&mut updated, 520, &mut gliding);
        assert!(gliding[8].r > a[8].r, "{} {}", gliding[8].r, a[8].r);
        ring(&mut updated, 800, &mut b);
        for led in (0..16).filter(|&l| l != 8) {
            assert_eq!(b[led], a[led], "LED {led}");
        }

        // Had the look been sent afresh at 500 ms, its comet would be a
        // turn behind: the update is not a restart.
        let mut restarted = Light::new(0, 16, Defaults::default());
        assert!(restarted.request(1, 0, &spots_request(400), 500));
        let mut c = [Rgbw::OFF; MAX_LEDS];
        ring(&mut restarted, 500, &mut c);
        ring(&mut restarted, 800, &mut c);
        assert!((0..16).filter(|&l| l != 8).any(|l| c[l] != a[l]));
    }

    #[test]
    fn an_update_is_refused_with_the_reason_when_there_is_nothing_to_update() {
        let mut light = Light::new(0, 16, Defaults::default());
        // No look from this client yet.
        assert!(
            light
                .update_inputs(1, Some(500), None, &no_looks)
                .unwrap_err()
                .contains("no look")
        );
        // A plain fill takes no inputs.
        assert!(light.request(1, 0, &LedRequest::new(LedShow::Color(0xff0000)), 0));
        assert!(
            light
                .update_inputs(1, Some(500), None, &no_looks)
                .unwrap_err()
                .contains("no look")
        );
        // A look with spots takes spots, not a progress.
        assert!(light.request(2, 0, &spots_request(1_000), 0));
        let refused = light
            .update_inputs(2, Some(500), None, &no_looks)
            .unwrap_err();
        assert!(refused.contains("no progress"), "{refused}");
        assert!(
            light
                .update_inputs(2, None, Some(SpotList::EMPTY), &no_looks)
                .is_ok()
        );
        assert!(light.update_inputs(2, None, None, &no_looks).is_err());
    }

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
