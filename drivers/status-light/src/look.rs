//! A look as data: one block (a fill or a comet) at a brightness, under an envelope (solid, a blink
//! or a breathe). The animator renders a [`Look`] onto a ring.

use crate::easing::{Easing, ONE};
use crate::ring;

/// An LED's colour: red, green, blue and white.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgbw {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub w: u8,
}

impl Rgbw {
    /// Off.
    pub const OFF: Self = Self::new(0, 0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8, w: u8) -> Self {
        Self { r, g, b, w }
    }

    /// A colour from `0xRRGGBB` (white 0).
    pub const fn rgb(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0)
    }
}

/// How a look's brightness changes over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Envelope {
    /// Steady.
    Solid,
    /// Hard on/off: on for `duty` thousandths of each `period_ms`.
    Blink { period_ms: u32, duty: u16 },
    /// A soft pulse: brightness eases down by `depth` thousandths and back up once per `period_ms`.
    Breathe {
        period_ms: u32,
        depth: u16,
        easing: Easing,
    },
}

impl Envelope {
    /// Whether the envelope changes the output over time.
    pub const fn is_animated(self) -> bool {
        !matches!(self, Self::Solid)
    }

    /// The envelope's level (`0..=ONE`) `elapsed_ms` after the look started.
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
                // A triangle 1 -> 0 -> 1 over the period, eased each way.
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

/// What a look draws on the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    /// Every LED `color`.
    Fill { color: Rgbw },
    /// One or two comets going round: a head of `color` with a tail `tail` LEDs long (thousandths),
    /// floor `base` (thousandths of full), one turn per `period_ms`.
    Comet {
        color: Rgbw,
        period_ms: u32,
        tail: u16,
        heads: u8,
        base: u16,
    },
}

impl Block {
    /// Whether the colours move with time.
    pub const fn is_moving(self) -> bool {
        matches!(self, Self::Comet { .. })
    }

    /// The colour of LED `index` of `count`, `elapsed_ms` into the look, before the envelope.
    pub(crate) fn color(self, index: usize, count: usize, elapsed_ms: u64) -> Rgbw {
        match self {
            Self::Fill { color } => color,
            Self::Comet {
                color,
                period_ms,
                tail,
                heads,
                base,
            } => scale(
                color,
                ring::comet_level(
                    index,
                    count,
                    elapsed_ms,
                    period_ms,
                    ring::q16(tail),
                    heads,
                    ring::q16(base),
                ),
            ),
        }
    }
}

/// A whole look: its block, the brightness it is shown at (0..=255, after the board's
/// `look_brightness`), and the envelope over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub block: Block,
    pub brightness: u8,
    pub envelope: Envelope,
}

impl Look {
    /// Nothing at all (off).
    pub const OFF: Self = Self {
        block: Block::Fill { color: Rgbw::OFF },
        brightness: 255,
        envelope: Envelope::Solid,
    };

    /// Whether `other` is the same kind of block (so a change between them keeps the look's time).
    pub(crate) fn same_shape(&self, other: &Self) -> bool {
        matches!(
            (self.block, other.block),
            (Block::Fill { .. }, Block::Fill { .. }) | (Block::Comet { .. }, Block::Comet { .. })
        )
    }
}

/// `color` at `level` (Q16).
pub(crate) fn scale(color: Rgbw, level: u32) -> Rgbw {
    let s = |c: u8| ((u32::from(c) * level + (ONE / 2)) >> 16) as u8;
    Rgbw::new(s(color.r), s(color.g), s(color.b), s(color.w))
}

/// `brightness` scaled by the ring-wide `ring` (255 is all of it).
pub(crate) fn scale_level(brightness: u8, ring: u8) -> u8 {
    ((u32::from(brightness) * u32::from(ring) + 127) / 255) as u8
}
