//! What a light shows ([`Look`]) and how it gets there ([`Animator`]).

use crate::easing::{Easing, ONE};
use lemnos_device::Rgbw;

/// How a look changes over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Effect {
    /// Steady.
    #[default]
    Solid,
    /// Hard on/off: on for `duty` thousandths of each `period_ms`.
    Blink { period_ms: u32, duty: u16 },
    /// A soft pulse: brightness eases down by `depth` thousandths and back
    /// up once per `period_ms`.
    Breathe {
        period_ms: u32,
        depth: u16,
        easing: Easing,
    },
}

impl Effect {
    /// Whether the effect changes the output over time.
    pub const fn is_animated(self) -> bool {
        !matches!(self, Self::Solid)
    }

    /// The effect's level (`0..=ONE`) `elapsed_ms` after it started.
    pub fn level(self, elapsed_ms: u64) -> u32 {
        match self {
            Self::Solid => ONE,
            Self::Blink { period_ms, duty } => {
                let period = u64::from(period_ms.max(1));
                let phase = elapsed_ms % period;
                if phase * 1000 < period * u64::from(duty.min(1000)) {
                    ONE
                } else {
                    0
                }
            }
            Self::Breathe {
                period_ms,
                depth,
                easing,
            } => {
                let period = u64::from(period_ms.max(2));
                let phase = elapsed_ms % period;
                // A triangle 1 → 0 → 1 over the period, eased each way.
                let half = period / 2;
                let t = if phase < half {
                    ONE - ((phase << 16) / half.max(1)) as u32
                } else {
                    (((phase - half) << 16) / (period - half).max(1)) as u32
                };
                let eased = easing.apply(t);
                let depth = (u32::from(depth.min(1000)) << 16) / 1000;
                ONE - ((u64::from(depth) * u64::from(ONE - eased)) >> 16) as u32
            }
        }
    }
}

/// What a light shows: a colour for every LED, or a frame, with an effect and
/// a brightness (0-255).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look<const N: usize> {
    pub pixels: Pixels<N>,
    pub effect: Effect,
    pub brightness: u8,
}

/// The colours of a [`Look`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixels<const N: usize> {
    /// Every LED the same colour.
    Fill(Rgbw),
    /// One colour per LED (logical order); `len` are used, the rest are off.
    Frame { pixels: [Rgbw; N], len: usize },
    /// A gauge: the first `fraction` (`0..=ONE`) of the LEDs in `color`, the
    /// leading LED partly lit for sub-LED precision, the rest `background`.
    /// A new fraction is reached by an eased advance, not a cross-fade.
    Progress {
        fraction: u32,
        color: Rgbw,
        background: Rgbw,
    },
    /// A comet going round once per `period_ms`: a head in `color` and a
    /// tail fading over `tail` LEDs, over `background`. The progress of an
    /// unknown amount, booting, a chase.
    Spinner {
        color: Rgbw,
        background: Rgbw,
        period_ms: u32,
        tail: u8,
    },
}

impl<const N: usize> Look<N> {
    /// Everything off.
    pub const OFF: Self = Self::fill(Rgbw::OFF);

    /// Every LED `color`, steady, full brightness.
    pub const fn fill(color: Rgbw) -> Self {
        Self {
            pixels: Pixels::Fill(color),
            effect: Effect::Solid,
            brightness: 255,
        }
    }

    /// A frame from `pixels` (at most `N` used).
    pub fn frame(pixels: &[Rgbw]) -> Self {
        let mut frame = [Rgbw::OFF; N];
        let len = pixels.len().min(N);
        frame[..len].copy_from_slice(&pixels[..len]);
        Self {
            pixels: Pixels::Frame { pixels: frame, len },
            effect: Effect::Solid,
            brightness: 255,
        }
    }

    /// A gauge filled to `fraction` (`0..=ONE`).
    pub const fn progress(fraction: u32, color: Rgbw, background: Rgbw) -> Self {
        Self {
            pixels: Pixels::Progress {
                fraction,
                color,
                background,
            },
            effect: Effect::Solid,
            brightness: 255,
        }
    }

    /// A comet going round.
    pub const fn spinner(color: Rgbw, background: Rgbw, period_ms: u32, tail: u8) -> Self {
        Self {
            pixels: Pixels::Spinner {
                color,
                background,
                period_ms,
                tail,
            },
            effect: Effect::Solid,
            brightness: 255,
        }
    }

    /// Whether the colours themselves move (a spinner).
    pub const fn is_moving(&self) -> bool {
        matches!(self.pixels, Pixels::Spinner { .. })
    }

    pub const fn with_effect(mut self, effect: Effect) -> Self {
        self.effect = effect;
        self
    }

    pub const fn with_brightness(mut self, brightness: u8) -> Self {
        self.brightness = brightness;
        self
    }

    /// LED `index` of `count`, `elapsed_ms` into the look, with a gauge
    /// filled to `fraction` (the animator's eased value).
    fn color(&self, index: usize, count: usize, elapsed_ms: u64, fraction: u32) -> Rgbw {
        match &self.pixels {
            Pixels::Fill(color) => *color,
            Pixels::Frame { pixels, len } if index < *len => pixels[index],
            Pixels::Frame { .. } => Rgbw::OFF,
            Pixels::Progress {
                color, background, ..
            } => {
                // LEDs lit, in 1/65536 of an LED.
                let lit = u64::from(fraction.min(ONE)) * count as u64;
                let start = (index as u64) << 16;
                let level = lit.saturating_sub(start).min(u64::from(ONE)) as u32;
                blend(*background, *color, level)
            }
            Pixels::Spinner {
                color,
                background,
                period_ms,
                tail,
            } => {
                let period = u64::from((*period_ms).max(1));
                // The head's position in 1/65536 of an LED.
                let head = ((elapsed_ms % period) << 16) * count as u64 / period;
                let at = (index as u64) << 16;
                let span = (count as u64) << 16;
                // How far behind the head this LED is, going round.
                let behind = (head + span - at) % span.max(1);
                let tail = u64::from((*tail).max(1)) << 16;
                let level = if behind < tail {
                    // 1 at the head, 0 at the end of the tail.
                    (u64::from(ONE) * (tail - behind) / tail) as u32
                } else if span - behind < u64::from(ONE) {
                    // The LED just ahead of the head, for sub-LED motion.
                    (u64::from(ONE) - (span - behind)) as u32
                } else {
                    0
                };
                blend(*background, *color, level)
            }
        }
    }
}

/// How a light moves to a new look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    pub duration_ms: u32,
    pub easing: Easing,
}

impl Transition {
    /// No fade.
    pub const CUT: Self = Self {
        duration_ms: 0,
        easing: Easing::Linear,
    };

    pub const fn new(duration_ms: u32, easing: Easing) -> Self {
        Self {
            duration_ms,
            easing,
        }
    }
}

/// Renders a light: the current look, its effect, and fades between looks.
///
/// Frames are rendered into fixed arrays (`N` LEDs at most): no allocation
/// per frame. [`render`](Self::render) returns a frame only when the output
/// changed, so a host writes the device only while a transition or an
/// effect is running, and [`next_frame_ms`](Self::next_frame_ms) says when
/// to render next (`None` while steady).
#[derive(Debug, Clone)]
pub struct Animator<const N: usize> {
    count: usize,
    frame_ms: u32,
    from: [Rgbw; N],
    shown: [Rgbw; N],
    look: Look<N>,
    look_since_ms: u64,
    transition: Transition,
    transition_since_ms: u64,
    /// A gauge's eased advance: from, to, and whether the transition moves
    /// the fraction instead of cross-fading.
    fraction_from: u32,
    fraction_to: u32,
    advancing: bool,
    written: bool,
}

/// The default interval between frames while animating: 50 Hz.
pub const FRAME_MS: u32 = 20;

fn blend(from: Rgbw, to: Rgbw, t: u32) -> Rgbw {
    let mix = |a: u8, b: u8| {
        let (a, b) = (i64::from(a), i64::from(b));
        (a + (((b - a) * i64::from(t)) >> 16)) as u8
    };
    Rgbw::new(
        mix(from.r, to.r),
        mix(from.g, to.g),
        mix(from.b, to.b),
        mix(from.w, to.w),
    )
}

fn scale(color: Rgbw, level: u32) -> Rgbw {
    let s = |c: u8| ((u32::from(c) * level + (ONE / 2)) >> 16) as u8;
    Rgbw::new(s(color.r), s(color.g), s(color.b), s(color.w))
}

impl<const N: usize> Animator<N> {
    /// An animator for `count` LEDs (at most `N`), showing nothing.
    pub fn new(count: usize) -> Self {
        Self {
            count: count.min(N),
            frame_ms: FRAME_MS,
            from: [Rgbw::OFF; N],
            shown: [Rgbw::OFF; N],
            look: Look::OFF,
            look_since_ms: 0,
            transition: Transition::CUT,
            transition_since_ms: 0,
            fraction_from: 0,
            fraction_to: 0,
            advancing: false,
            written: false,
        }
    }

    /// Renders every `frame_ms` while animating (default [`FRAME_MS`]).
    pub fn with_frame_ms(mut self, frame_ms: u32) -> Self {
        self.frame_ms = frame_ms.max(1);
        self
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn look(&self) -> &Look<N> {
        &self.look
    }

    /// Moves to `look` with `transition`, starting from what is shown now. A
    /// gauge whose colours stay the same advances its fraction instead of
    /// cross-fading.
    pub fn set(&mut self, look: Look<N>, transition: Transition, now_ms: u64) {
        let fraction_now = self.fraction(now_ms);
        self.advancing = false;
        if let (
            Pixels::Progress {
                color: c1,
                background: b1,
                ..
            },
            Pixels::Progress {
                fraction,
                color: c2,
                background: b2,
            },
        ) = (self.look.pixels, look.pixels)
            && c1 == c2
            && b1 == b2
        {
            self.advancing = true;
            self.fraction_from = fraction_now;
            self.fraction_to = fraction;
        } else if let Pixels::Progress { fraction, .. } = look.pixels {
            self.fraction_from = fraction;
            self.fraction_to = fraction;
        }
        self.from = self.shown;
        if !self.advancing
            && (core::mem::discriminant(&look.pixels) != core::mem::discriminant(&self.look.pixels)
                || look.effect != self.look.effect)
        {
            self.look_since_ms = now_ms;
        }
        self.look = look;
        self.transition = transition;
        self.transition_since_ms = now_ms;
        self.written = false;
    }

    /// The transition's eased progress, `None` once it is over.
    fn eased(&self, now_ms: u64) -> Option<u32> {
        self.in_transition(now_ms).then(|| {
            let elapsed = now_ms - self.transition_since_ms;
            let t = ((elapsed << 16) / u64::from(self.transition.duration_ms.max(1))) as u32;
            self.transition.easing.apply(t)
        })
    }

    /// A gauge's displayed fraction at `now_ms`.
    fn fraction(&self, now_ms: u64) -> u32 {
        match (self.advancing, self.eased(now_ms)) {
            (true, Some(t)) => {
                let (a, b) = (i64::from(self.fraction_from), i64::from(self.fraction_to));
                (a + (((b - a) * i64::from(t)) >> 16)) as u32
            }
            _ => self.fraction_to,
        }
    }

    fn in_transition(&self, now_ms: u64) -> bool {
        now_ms < self.transition_since_ms + u64::from(self.transition.duration_ms)
    }

    /// Whether a new look was set and its first frame not rendered yet.
    pub fn is_pending(&self) -> bool {
        !self.written
    }

    /// Whether the output changes over time right now.
    pub fn is_animating(&self, now_ms: u64) -> bool {
        self.in_transition(now_ms) || self.look.effect.is_animated() || self.look.is_moving()
    }

    /// When to render next: in `frame_ms` while animating or before the
    /// first write, `None` when steady.
    pub fn next_frame_ms(&self, now_ms: u64) -> Option<u64> {
        (self.is_animating(now_ms) || !self.written).then(|| now_ms + u64::from(self.frame_ms))
    }

    /// Renders the frame for `now_ms`. Returns it when it differs from the
    /// last one returned (or nothing was returned since the last `set`), else
    /// `None`: nothing needs writing.
    pub fn render(&mut self, now_ms: u64) -> Option<&[Rgbw]> {
        let level = self
            .look
            .effect
            .level(now_ms.saturating_sub(self.look_since_ms));
        let level = (u64::from(level) * u64::from(self.look.brightness) / 255) as u32;
        let fraction = self.fraction(now_ms);
        let fade = if self.advancing {
            None
        } else {
            self.eased(now_ms)
        };
        let elapsed = now_ms.saturating_sub(self.look_since_ms);
        let mut changed = !self.written;
        for index in 0..self.count {
            let target = scale(self.look.color(index, self.count, elapsed, fraction), level);
            let pixel = match fade {
                Some(t) => blend(self.from[index], target, t),
                None => target,
            };
            if self.shown[index] != pixel {
                self.shown[index] = pixel;
                changed = true;
            }
        }
        if changed {
            self.written = true;
            Some(&self.shown[..self.count])
        } else {
            None
        }
    }
}
