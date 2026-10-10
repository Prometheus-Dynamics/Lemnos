//! What a light shows ([`Look`]) and how it gets there ([`Animator`]).

use crate::easing::{Easing, ONE};
use crate::look::{Block, Fraction, LayerSpec, LookSpec, MAX_LAYERS, scale};
use crate::sparkle::{Sparkle, SparkleState};
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
    /// A one-shot flash, then a glow down to off: the level rises over
    /// `attack_ms`, holds for `hold_ms`, and decays (eased) to 0 over
    /// `decay_ms`. It runs `repeat` times (0: keeps repeating); after the
    /// last one the level is 0.
    Pulse {
        attack_ms: u32,
        hold_ms: u32,
        decay_ms: u32,
        repeat: u8,
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
            Self::Pulse {
                attack_ms,
                hold_ms,
                decay_ms,
                repeat,
            } => {
                let attack = u64::from(attack_ms);
                let hold = u64::from(hold_ms);
                let decay = u64::from(decay_ms).max(1);
                let cycle = attack + hold + decay;
                if repeat > 0 && elapsed_ms >= cycle * u64::from(repeat) {
                    return 0;
                }
                let phase = elapsed_ms % cycle;
                if phase < attack {
                    ((phase << 16) / attack.max(1)) as u32
                } else if phase < attack + hold {
                    ONE
                } else {
                    // The glow: a quadratic ease-out to 0, so it lands softly.
                    let left = ONE - (((phase - attack - hold) << 16) / decay) as u32;
                    ((u64::from(left) * u64::from(left)) >> 16) as u32
                }
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

/// Renders a light: the current look, its envelope, and fades between looks.
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
    look: LookSpec,
    look_since_ms: u64,
    transition: Transition,
    transition_since_ms: u64,
    /// An arc's eased advance: from, to, and whether the transition moves
    /// the fill instead of cross-fading (Q16).
    fraction_from: u32,
    fraction_to: u32,
    advancing: bool,
    written: bool,
    /// The sparkle's twinkles (reset when the look changes).
    sparkle: SparkleState,
    /// The LED at the bottom of the ring for a falling sparkle, thousandths
    /// of an LED (see [`set_bottom`](Self::set_bottom)).
    bottom: u32,
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

/// Whether `a` and `b` are the same single arc apart from its fill (colours,
/// track, head, sheen, envelope equal), so the change advances the fill
/// instead of cross-fading.
fn same_arc(a: &LookSpec, b: &LookSpec) -> bool {
    /// The look's one layer with its fill zeroed, if it is a lone arc.
    fn arc_without_fill(look: &LookSpec) -> Option<LayerSpec> {
        let mut layers = look.iter();
        let (Some(layer), None) = (layers.next(), layers.next()) else {
            return None;
        };
        match layer.block {
            Block::Arc {
                color,
                track,
                head,
                sheen,
                ..
            } => Some(LayerSpec {
                block: Block::Arc {
                    fraction: Fraction::Fixed(0),
                    color,
                    track,
                    head,
                    sheen,
                },
                ..*layer
            }),
            _ => None,
        }
    }
    match (arc_without_fill(a), arc_without_fill(b)) {
        (Some(x), Some(y)) => x == y && a.envelope == b.envelope,
        _ => false,
    }
}

fn shape(look: &LookSpec) -> [Option<u8>; MAX_LAYERS] {
    let mut out = [None; MAX_LAYERS];
    for (o, layer) in out.iter_mut().zip(look.layers.iter()) {
        *o = layer.map(|l| match l.block {
            Block::Fill { .. } => 0,
            Block::Comet { .. } => 1,
            Block::Arc { .. } => 2,
            Block::Ripple { .. } => 3,
            Block::Frame { .. } => 4,
            Block::Wash { .. } => 5,
            Block::Sparkle(_) => 6,
        });
    }
    out
}

impl<const N: usize> Animator<N> {
    /// An animator for `count` LEDs (at most `N`), showing nothing.
    pub fn new(count: usize) -> Self {
        Self {
            count: count.min(N),
            frame_ms: FRAME_MS,
            from: [Rgbw::OFF; N],
            shown: [Rgbw::OFF; N],
            look: LookSpec::OFF,
            look_since_ms: 0,
            transition: Transition::CUT,
            transition_since_ms: 0,
            fraction_from: 0,
            fraction_to: 0,
            advancing: false,
            written: false,
            sparkle: SparkleState::new(0, &Sparkle::DEFAULT),
            bottom: (count.min(N) as u32 / 2) * 1_000,
        }
    }

    /// Where the bottom of the ring is for a falling sparkle, in thousandths
    /// of an LED (0 is LED 0's centre). The host sets it from gravity, or
    /// from the default; it takes effect on the next render.
    pub fn set_bottom(&mut self, led_milli: u32) {
        self.bottom = led_milli;
    }

    /// Renders every `frame_ms` while animating (default [`FRAME_MS`]).
    pub fn with_frame_ms(mut self, frame_ms: u32) -> Self {
        self.frame_ms = frame_ms.max(1);
        self
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn look(&self) -> &LookSpec {
        &self.look
    }

    /// Moves to `look` with `transition`, starting from what is shown now. An
    /// arc whose colours stay the same advances its fill instead of
    /// cross-fading.
    pub fn set(&mut self, look: LookSpec, transition: Transition, now_ms: u64) {
        let fraction_now = self.fraction(now_ms);
        self.advancing = same_arc(&self.look, &look);
        let target = look
            .arc_fraction()
            .map_or(0, |f| (u32::from(f.min(1000)) << 16) / 1000);
        if self.advancing {
            self.fraction_from = fraction_now;
        } else {
            self.fraction_from = target;
        }
        self.fraction_to = target;
        self.from = self.shown;
        if !self.advancing
            && (shape(&self.look) != shape(&look) || look.envelope != self.look.envelope)
        {
            self.look_since_ms = now_ms;
        }
        if look != self.look {
            let sparkle = look.sparkle().unwrap_or(Sparkle::DEFAULT);
            self.sparkle = SparkleState::new(now_ms, &sparkle);
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

    /// An arc's displayed fill at `now_ms` (Q16).
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
        self.in_transition(now_ms) || self.look.envelope.is_animated() || self.look.is_moving()
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
            .envelope
            .level(now_ms.saturating_sub(self.look_since_ms));
        let level = (u64::from(level) * u64::from(self.look.brightness) / 255) as u32;
        let fraction = self.fraction(now_ms);
        let fade = if self.advancing {
            None
        } else {
            self.eased(now_ms)
        };
        let elapsed = now_ms.saturating_sub(self.look_since_ms);
        if let Some(sparkle) = self.look.sparkle() {
            self.sparkle
                .advance(now_ms, &sparkle, self.count, self.bottom);
        }
        let mut changed = !self.written;
        for index in 0..self.count {
            let color =
                self.look
                    .color(index, self.count, elapsed, fraction, &self.sparkle, now_ms);
            let target = scale(color, level);
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
