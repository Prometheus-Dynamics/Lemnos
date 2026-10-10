//! A look as data: a [`LookSpec`] is up to [`MAX_LAYERS`] layers, each a
//! [`Block`] with its parameters, colour, brightness and compositing
//! [`Mode`], under an optional envelope ([`Effect`]) and a brightness.
//! Everything is fixed size: a spec is copied, never allocated.
//!
//! The blocks: [`Block::Fill`], [`Block::Comet`], [`Block::Arc`] (a progress
//! arc), [`Block::Ripple`] and [`Block::Frame`] (static pixels). A layer's
//! colours are composited with the layers below it by `max` or `add`.

use crate::animator::Effect;
use crate::easing::{ONE, quarter_sin};
use lemnos_device::Rgbw;

/// Layers in one look.
pub const MAX_LAYERS: usize = 4;
/// LEDs in a [`Block::Frame`] (and the most a light may have).
pub const MAX_FRAME_LEDS: usize = 64;
/// The longest look name.
pub const MAX_LOOK_NAME: usize = 40;

/// How a layer's colours combine with the layers below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// Per channel, the brighter of the two.
    #[default]
    Max,
    /// Per channel, the sum, saturating at full.
    Add,
}

impl Mode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Add => "add",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "max" => Some(Self::Max),
            "add" => Some(Self::Add),
            _ => None,
        }
    }
}

/// A gauge's fill: a fixed amount (thousandths), or the progress the request
/// carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fraction {
    Fixed(u16),
    Input,
}

/// What a layer draws. Lengths are thousandths of an LED, brightnesses
/// thousandths of full, periods milliseconds. The frame variant holds its
/// pixels inline (no allocation), so the enum is as large as a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::large_enum_variant)]
pub enum Block {
    /// Every LED `color`.
    Fill { color: Rgbw },
    /// One or two comets going round: a head of `color` with a tail `tail`
    /// LEDs long, floor `base`, one turn per `period_ms`; `reverse` goes the
    /// other way round.
    Comet {
        color: Rgbw,
        period_ms: u32,
        tail: u16,
        heads: u8,
        base: u16,
        reverse: bool,
    },
    /// A progress arc: the first `fraction` of the LEDs in `color`, the rest
    /// `track`. The leading LED is mixed toward white by `head`; `sheen`
    /// moves a soft white over the filled part.
    Arc {
        fraction: Fraction,
        color: Rgbw,
        track: Rgbw,
        head: u16,
        sheen: bool,
    },
    /// A wave from LED `origin` going both ways round at `speed` LEDs per
    /// second, `width` LEDs across, then a glow of `glow` that fades over
    /// `settle_ms`.
    Ripple {
        origin: u16,
        color: Rgbw,
        speed: u16,
        width: u16,
        settle_ms: u32,
        glow: u16,
    },
    /// Static pixels, from LED 0; LEDs past `len` are off.
    Frame {
        pixels: [Rgbw; MAX_FRAME_LEDS],
        len: u8,
    },
}

impl Block {
    /// The block's name in a look file.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Fill { .. } => "fill",
            Self::Comet { .. } => "comet",
            Self::Arc { .. } => "arc",
            Self::Ripple { .. } => "ripple",
            Self::Frame { .. } => "frame",
        }
    }

    /// Whether the colours move with time (a comet, a ripple, a sheen).
    pub const fn is_moving(&self) -> bool {
        match self {
            Self::Comet { .. } | Self::Ripple { .. } => true,
            Self::Arc {
                fraction, sheen, ..
            } => *sheen && !matches!(fraction, Fraction::Fixed(0)),
            Self::Fill { .. } | Self::Frame { .. } => false,
        }
    }

    /// Whether `other` is the same kind of block (so a change between them
    /// keeps the look's time).
    pub const fn same_kind(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Fill { .. }, Self::Fill { .. })
                | (Self::Comet { .. }, Self::Comet { .. })
                | (Self::Arc { .. }, Self::Arc { .. })
                | (Self::Ripple { .. }, Self::Ripple { .. })
                | (Self::Frame { .. }, Self::Frame { .. })
        )
    }

    /// The colour of LED `index` of `count` at `elapsed_ms` into the look,
    /// before the layer's brightness and the envelope. `fraction` is the
    /// arc's displayed fill (Q16).
    fn color(&self, index: usize, count: usize, elapsed_ms: u64, fraction: u32) -> Rgbw {
        match *self {
            Self::Fill { color } => color,
            Self::Frame { pixels, len } => {
                if index < usize::from(len) {
                    pixels[index]
                } else {
                    Rgbw::OFF
                }
            }
            Self::Arc {
                color,
                track,
                head,
                sheen,
                ..
            } => progress_led(
                index,
                count,
                elapsed_ms,
                fraction,
                color,
                track,
                q16(head),
                sheen,
            ),
            Self::Comet {
                color,
                period_ms,
                tail,
                heads,
                base,
                reverse,
            } => {
                let level = comet_level(
                    index,
                    count,
                    elapsed_ms,
                    period_ms,
                    q16(tail),
                    heads,
                    q16(base),
                    reverse,
                );
                scale(color, level)
            }
            Self::Ripple {
                origin,
                color,
                speed,
                width,
                settle_ms,
                glow,
            } => scale(
                color,
                ripple_level(
                    index,
                    count,
                    elapsed_ms,
                    usize::from(origin),
                    q16(speed),
                    q16(width),
                    settle_ms,
                    q16(glow),
                ),
            ),
        }
    }
}

/// One layer of a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerSpec {
    pub block: Block,
    /// 0..=255.
    pub brightness: u8,
    pub mode: Mode,
}

impl LayerSpec {
    /// A layer at full brightness, composited by `max`.
    pub const fn new(block: Block) -> Self {
        Self {
            block,
            brightness: 255,
            mode: Mode::Max,
        }
    }

    pub const fn with_brightness(mut self, brightness: u8) -> Self {
        self.brightness = brightness;
        self
    }

    pub const fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }
}

/// A whole look: its layers, the envelope over them, and a brightness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LookSpec {
    /// Layers, bottom first; a layer after an empty one is not drawn.
    pub layers: [Option<LayerSpec>; MAX_LAYERS],
    /// A breathe or blink over the whole look.
    pub envelope: Effect,
    /// 0..=255, after the ring-wide brightness.
    pub brightness: u8,
    /// The least brightness the look is shown at, after the ring-wide scale.
    pub floor: u8,
}

impl LayerSpec {
    /// Every LED `color`.
    pub const fn fill(color: Rgbw) -> Self {
        Self::new(Block::Fill { color })
    }

    /// Comets: `tail` and `base` in thousandths of an LED and of full.
    pub const fn comet(color: Rgbw, period_ms: u32, tail: u16, heads: u8, base: u16) -> Self {
        Self::new(Block::Comet {
            color,
            period_ms,
            tail,
            heads,
            base,
            reverse: false,
        })
    }

    /// A progress arc over `track`, with a head and a sheen.
    pub const fn arc(fraction: Fraction, color: Rgbw, track: Rgbw) -> Self {
        Self::new(Block::Arc {
            fraction,
            color,
            track,
            head: 350,
            sheen: true,
        })
    }

    /// The ripple from LED 0, with the settling glow.
    pub const fn ripple(color: Rgbw) -> Self {
        Self::new(Block::Ripple {
            origin: 0,
            color,
            speed: 12_000,
            width: 2_200,
            settle_ms: 1_400,
            glow: 150,
        })
    }

    /// Static pixels, from LED 0 (at most [`MAX_FRAME_LEDS`] used).
    pub fn frame(pixels: &[Rgbw]) -> Self {
        let mut frame = [Rgbw::OFF; MAX_FRAME_LEDS];
        let len = pixels.len().min(MAX_FRAME_LEDS);
        frame[..len].copy_from_slice(&pixels[..len]);
        Self::new(Block::Frame {
            pixels: frame,
            len: len as u8,
        })
    }
}

impl LookSpec {
    /// No layers yet: add them with [`push`](Self::push).
    pub const EMPTY: Self = Self {
        layers: [None; MAX_LAYERS],
        envelope: Effect::Solid,
        brightness: 255,
        floor: 0,
    };

    /// Nothing at all (off).
    pub const OFF: Self = Self::of(LayerSpec::fill(Rgbw::OFF));

    /// A look with one layer, steady, at full brightness.
    pub const fn of(layer: LayerSpec) -> Self {
        Self {
            layers: [Some(layer), None, None, None],
            envelope: Effect::Solid,
            brightness: 255,
            floor: 0,
        }
    }

    /// Every LED `color`, steady, full brightness.
    pub const fn fill(color: Rgbw) -> Self {
        Self::of(LayerSpec::new(Block::Fill { color }))
    }

    /// A frame from `pixels` (at most [`MAX_FRAME_LEDS`] used).
    pub fn frame(pixels: &[Rgbw]) -> Self {
        let mut frame = [Rgbw::OFF; MAX_FRAME_LEDS];
        let len = pixels.len().min(MAX_FRAME_LEDS);
        frame[..len].copy_from_slice(&pixels[..len]);
        Self::of(LayerSpec::new(Block::Frame {
            pixels: frame,
            len: len as u8,
        }))
    }

    /// A gauge filled to `fraction` (thousandths).
    pub const fn progress(fraction: u16, color: Rgbw, track: Rgbw) -> Self {
        Self::of(LayerSpec::new(Block::Arc {
            fraction: Fraction::Fixed(fraction),
            color,
            track,
            head: 350,
            sheen: true,
        }))
    }

    /// One or two comets going round (`tail` and `base` in thousandths).
    pub const fn comet(color: Rgbw, period_ms: u32, tail: u16, heads: u8, base: u16) -> Self {
        Self::of(LayerSpec::new(Block::Comet {
            color,
            period_ms,
            tail,
            heads,
            base,
            reverse: false,
        }))
    }

    /// The ripple from the top, with the settling glow.
    pub const fn ripple(color: Rgbw) -> Self {
        Self::of(LayerSpec::new(Block::Ripple {
            origin: 0,
            color,
            speed: 12_000,
            width: 2_200,
            settle_ms: 1_400,
            glow: 150,
        }))
    }

    pub const fn with_envelope(mut self, envelope: Effect) -> Self {
        self.envelope = envelope;
        self
    }

    pub const fn with_brightness(mut self, brightness: u8) -> Self {
        self.brightness = brightness;
        self
    }

    pub const fn with_floor(mut self, floor: u8) -> Self {
        self.floor = floor;
        self
    }

    /// Adds a layer above the existing ones. `false` when the look is full.
    pub fn push(&mut self, layer: LayerSpec) -> bool {
        match self.layers.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => {
                *slot = Some(layer);
                true
            }
            None => false,
        }
    }

    /// The layers in order, bottom first.
    pub fn iter(&self) -> impl Iterator<Item = &LayerSpec> {
        self.layers.iter().flatten()
    }

    /// Whether the output changes over time, without an envelope.
    pub fn is_moving(&self) -> bool {
        self.iter().any(|l| l.block.is_moving())
    }

    /// The displayed fill of the first arc (thousandths), if any.
    pub fn arc_fraction(&self) -> Option<u16> {
        self.iter().find_map(|l| match l.block {
            Block::Arc {
                fraction: Fraction::Fixed(f),
                ..
            } => Some(f),
            _ => None,
        })
    }

    /// Replaces every `Input` fraction with `progress` (thousandths; 0 when
    /// the request has none).
    pub fn fill_input(&mut self, progress: u16) {
        for slot in self.layers.iter_mut().flatten() {
            if let Block::Arc { fraction, .. } = &mut slot.block
                && *fraction == Fraction::Input
            {
                *fraction = Fraction::Fixed(progress.min(1000));
            }
        }
    }

    /// Whether the spec can be shown: at least one layer, and every value in
    /// its range. The reason names the first problem.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.iter().next().is_none() {
            return Err("a look needs at least one layer");
        }
        for layer in self.iter() {
            match layer.block {
                Block::Fill { .. } => {}
                Block::Comet {
                    period_ms,
                    tail,
                    heads,
                    base,
                    ..
                } => {
                    if !(1..=2).contains(&heads) {
                        return Err("a comet's heads must be 1 or 2");
                    }
                    if !(100..=600_000).contains(&period_ms) {
                        return Err("a comet's period_ms must be 100 to 600000");
                    }
                    if !(1..=64_000).contains(&tail) {
                        return Err("a comet's tail must be above 0 and at most 64 LEDs");
                    }
                    if base > 1000 {
                        return Err("a comet's base must be 0 to 1");
                    }
                }
                Block::Arc { fraction, head, .. } => {
                    if let Fraction::Fixed(f) = fraction
                        && f > 1000
                    {
                        return Err("an arc's fraction must be 0 to 1");
                    }
                    if head > 1000 {
                        return Err("an arc's head must be 0 to 1");
                    }
                }
                Block::Ripple {
                    origin,
                    speed,
                    width,
                    settle_ms,
                    glow,
                    ..
                } => {
                    if usize::from(origin) >= MAX_FRAME_LEDS {
                        return Err("a ripple's origin must be an LED from 0 to 63");
                    }
                    if !(1..=64_000).contains(&speed) {
                        return Err("a ripple's speed must be above 0 LEDs a second");
                    }
                    if !(1..=64_000).contains(&width) {
                        return Err("a ripple's width must be above 0 and at most 64 LEDs");
                    }
                    if settle_ms > 60_000 {
                        return Err("a ripple's settle_ms must be at most 60000");
                    }
                    if glow > 1000 {
                        return Err("a ripple's glow must be 0 to 1");
                    }
                }
                Block::Frame { len, .. } => {
                    if usize::from(len) > MAX_FRAME_LEDS {
                        return Err("a frame has at most 64 pixels");
                    }
                }
            }
        }
        match self.envelope {
            Effect::Solid => {}
            Effect::Blink { period_ms, duty } => {
                if !(100..=600_000).contains(&period_ms) {
                    return Err("a blink's period_ms must be 100 to 600000");
                }
                if duty > 1000 {
                    return Err("a blink's duty must be 0 to 1");
                }
            }
            Effect::Breathe {
                period_ms, depth, ..
            } => {
                if !(100..=600_000).contains(&period_ms) {
                    return Err("a breathe's period_ms must be 100 to 600000");
                }
                if depth > 1000 {
                    return Err("a breathe's depth must be 0 to 1");
                }
            }
            Effect::Pulse {
                attack_ms,
                hold_ms,
                decay_ms,
                ..
            } => {
                if attack_ms > 60_000 || hold_ms > 60_000 {
                    return Err("a pulse's attack_ms and hold_ms must be 0 to 60000");
                }
                if !(1..=60_000).contains(&decay_ms) {
                    return Err("a pulse's decay_ms must be 1 to 60000");
                }
            }
        }
        Ok(())
    }

    /// LED `index` of `count` at `elapsed_ms` into the look, with the arc at
    /// `fraction` (Q16): the layers combined, before the envelope.
    pub(crate) fn color(&self, index: usize, count: usize, elapsed_ms: u64, fraction: u32) -> Rgbw {
        let mut acc = Rgbw::OFF;
        for layer in self.iter() {
            let c = layer.block.color(index, count, elapsed_ms, fraction);
            let c = scale(c, brightness_level(layer.brightness));
            acc = match layer.mode {
                Mode::Max => max(acc, c),
                Mode::Add => add(acc, c),
            };
        }
        acc
    }
}

/// A look name: 1 to [`MAX_LOOK_NAME`] of `a-z`, `0-9`, `.`, `-` and `_`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LookName {
    len: u8,
    bytes: [u8; MAX_LOOK_NAME],
}

impl LookName {
    /// The name, if it is a valid look name.
    pub fn new(name: &str) -> Option<Self> {
        if !valid_look_name(name) {
            return None;
        }
        let mut bytes = [0; MAX_LOOK_NAME];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Some(Self {
            len: name.len() as u8,
            bytes,
        })
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

impl core::fmt::Debug for LookName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "LookName({:?})", self.as_str())
    }
}

/// Whether `name` is a valid look name (see [`LookName`]).
pub fn valid_look_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_LOOK_NAME
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// Thousandths as Q16 (rounded).
pub(crate) fn q16(thousandths: u16) -> u32 {
    ((u32::from(thousandths) << 16) + 500) / 1000
}

/// A layer's brightness as a Q16 level (255 is exactly one).
fn brightness_level(brightness: u8) -> u32 {
    (u32::from(brightness) * ONE) / 255
}

fn max(a: Rgbw, b: Rgbw) -> Rgbw {
    Rgbw::new(a.r.max(b.r), a.g.max(b.g), a.b.max(b.b), a.w.max(b.w))
}

fn add(a: Rgbw, b: Rgbw) -> Rgbw {
    Rgbw::new(
        a.r.saturating_add(b.r),
        a.g.saturating_add(b.g),
        a.b.saturating_add(b.b),
        a.w.saturating_add(b.w),
    )
}

/// `color` at `level` (Q16).
pub(crate) fn scale(color: Rgbw, level: u32) -> Rgbw {
    let s = |c: u8| ((u32::from(c) * level + (ONE / 2)) >> 16) as u8;
    Rgbw::new(s(color.r), s(color.g), s(color.b), s(color.w))
}

/// `x^2.2` for `x = k/32` (Q16), the comet's tail curve.
const POW_2_2: [u32; 33] = [
    0, 32, 147, 359, 676, 1104, 1648, 2314, 3104, 4022, 5072, 6255, 7574, 9033, 10632, 12375,
    14263, 16298, 18482, 20817, 23303, 25944, 28740, 31692, 34803, 38073, 41504, 45097, 48854,
    52775, 56861, 61115, 65536,
];

/// `x^2.2` (both Q16), linear between table entries.
fn tail_curve(x: u32) -> u32 {
    let scaled = x.min(ONE) * 32;
    let index = (scaled >> 16) as usize;
    if index >= 32 {
        return ONE;
    }
    let frac = u64::from(scaled & 0xffff);
    let (a, b) = (POW_2_2[index], POW_2_2[index + 1]);
    a + (((u64::from(b - a)) * frac) >> 16) as u32
}

/// The comets' brightness at LED `index` (Q16): for each head, `(1 - d/tail)^2.2`
/// where `d` is how far behind the head the LED is (going round), and the
/// brightest head wins, but never below `base`.
///
/// The leading edge is anti-aliased: the LED just ahead of a head (less than
/// one LED ahead) is lit by the fraction of the way the head has got to it,
/// so the head steps smoothly from one LED to the next rather than jumping
/// a whole LED at once.
#[allow(clippy::too_many_arguments)]
fn comet_level(
    index: usize,
    count: usize,
    elapsed_ms: u64,
    period_ms: u32,
    tail: u32,
    heads: u8,
    base: u32,
    reverse: bool,
) -> u32 {
    let period = u64::from(period_ms.max(1));
    // This turn's position in 1/65536 of a turn.
    let phase = ((elapsed_ms % period) << 16) / period;
    let heads = u64::from(heads.clamp(1, 2));
    let span = (count as u64) << 16;
    let at = (index as u64) << 16;
    let tail = u64::from(tail.max(1));
    let mut best = 0;
    for h in 0..heads {
        let turn = (phase + h * u64::from(ONE) / heads) % u64::from(ONE);
        // A head's position in 1/65536 of an LED.
        let head = turn * count as u64;
        let behind = if reverse {
            (at + span - head) % span.max(1)
        } else {
            (head + span - at) % span.max(1)
        };
        if behind < tail {
            let x = ONE - ((behind << 16) / tail) as u32;
            best = best.max(tail_curve(x));
        } else {
            // How far ahead of the head the LED is (in 1/65536 of an LED).
            let ahead = (span - behind) % span.max(1);
            if ahead > 0 && ahead < u64::from(ONE) {
                best = best.max(ONE - ahead as u32);
            }
        }
    }
    best.max(base)
}

/// Ripple level at LED `index` (Q16): a wave front moving out from `origin`
/// both ways round, then a glow that settles.
#[allow(clippy::too_many_arguments)]
fn ripple_level(
    index: usize,
    count: usize,
    elapsed_ms: u64,
    origin: usize,
    speed: u32,
    width: u32,
    settle_ms: u32,
    glow: u32,
) -> u32 {
    let count = count.max(1);
    let d = index.abs_diff(origin.min(count - 1));
    let distance = d.min(count - d) as u64;
    // The front, in 1/65536 of an LED.
    let front = (elapsed_ms * u64::from(speed)) / 1000;
    let apart = (distance << 16).abs_diff(front);
    let width = u64::from(width.max(1));
    let wave = if apart >= width {
        0
    } else {
        ONE - (apart * u64::from(ONE) / width) as u32
    };
    let settle = u64::from(settle_ms.max(1));
    let settled = if elapsed_ms >= settle {
        0
    } else {
        ONE - (elapsed_ms * u64::from(ONE) / settle) as u32
    };
    let glow = ((u64::from(glow) * u64::from(settled)) >> 16) as u32;
    wave.max(glow)
}

/// The sheen's strength over the filled part (35%), and its speed (0.35
/// turns per second).
const SHEEN_STRENGTH: u64 = 22_938;
const SHEEN_TURNS_PER_MS_Q16: u64 = 35 * ONE as u64 / 100_000;

/// `t` mixed toward white by `amount` (Q16), in the red, green and blue
/// (the white channel is kept).
fn whiten(t: Rgbw, amount: u32) -> Rgbw {
    let mix = |c: u8| (u32::from(c) + (((255 - u32::from(c)) * amount) >> 16)) as u8;
    Rgbw::new(mix(t.r), mix(t.g), mix(t.b), t.w)
}

/// `t` with `amount` (Q16) of white added to the red, green and blue.
fn add_white(t: Rgbw, amount: u32) -> Rgbw {
    let add = |c: u8| (u32::from(c) + ((255 * amount) >> 16)).min(255) as u8;
    Rgbw::new(add(t.r), add(t.g), add(t.b), t.w)
}

/// `cos(2π x)` for `x` in turns (Q16), from the quarter-wave table, as a
/// signed Q16 value.
fn cos_turn(x: u32) -> i64 {
    let quarters = u64::from(x % ONE) * 4;
    let quadrant = quarters >> 16;
    let f = (quarters & 0xffff) as u32;
    let s = |t: u32| i64::from(quarter_sin(t));
    match quadrant {
        0 => s(ONE - f),
        1 => -s(f),
        2 => -s(ONE - f),
        _ => s(f),
    }
}

/// The gauge's LED `index` at `elapsed_ms` with `fraction` lit: the track
/// unfilled, else the colour shaded 25% to full along the filled part (the
/// leading LED mixed toward white by `head`), and the sheen if on.
#[allow(clippy::too_many_arguments)]
fn progress_led(
    index: usize,
    count: usize,
    elapsed_ms: u64,
    fraction: u32,
    color: Rgbw,
    track: Rgbw,
    head: u32,
    sheen: bool,
) -> Rgbw {
    // LEDs lit, in 1/65536 of an LED.
    let lit = u64::from(fraction.min(ONE)) * count as u64;
    let span = (count as u64) << 16;
    let k = lit.saturating_sub((index as u64) << 16).min(u64::from(ONE)) as u32;
    if k == 0 {
        return track;
    }
    // The leading LED: the last one lit, while the gauge is not full.
    let is_head = lit < span && index as u64 == ((lit + u64::from(ONE) - 1) >> 16) - 1;
    let base = if is_head { whiten(color, head) } else { color };
    let bright = ONE / 4 + (3 * k) / 4;
    let shaded = scale(base, bright);
    if !sheen {
        return shaded;
    }
    // A sheen of white, moving over the filled part.
    let position = (index as u64 * u64::from(ONE)) / count as u64;
    let phase = (elapsed_ms * SHEEN_TURNS_PER_MS_Q16) % u64::from(ONE);
    let along = ((position + u64::from(ONE) - phase) % u64::from(ONE)) as u32;
    let c = cos_turn(along).max(0) as u64;
    let c2 = (c * c) >> 16;
    let c4 = (c2 * c2) >> 16;
    let c8 = (c4 * c4) >> 16;
    let sheen = (((c8 * SHEEN_STRENGTH) >> 16) * u64::from(k)) >> 16;
    add_white(shaded, sheen as u32)
}
