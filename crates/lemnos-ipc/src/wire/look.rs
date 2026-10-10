//! The binary form of a [`LookSpec`] (`LedShow::Inline`): a layer count, then
//! each layer as a block code and its fields, then the envelope and the
//! brightness. Colours are `u32` `0xWWRRGGBB`; fractions and lengths are
//! thousandths; no field is optional (the look is complete on the wire).
//! Ranges are checked by the service (`LookSpec::validate`), so a bad value
//! is refused with a reason rather than dropping the connection; only the
//! shape (codes, counts) must decode.

use super::codec::{Decoder, Encoder};
use super::request::{easing_code, easing_from};
use super::{WireError, bad};
use lemnos_device::Rgbw;
use lemnos_light::{
    Block, Effect, Fraction, LayerSpec, LookSpec, MAX_FRAME_LEDS, MAX_LAYERS, MAX_SPARKLE_COLORS,
    Mode, Sparkle,
};

const FILL: u8 = 0;
const COMET: u8 = 1;
const ARC: u8 = 2;
const RIPPLE: u8 = 3;
const FRAME: u8 = 4;
const WASH: u8 = 5;
const SPARKLE: u8 = 6;

const SOLID: u8 = 0;
const BLINK: u8 = 1;
const BREATHE: u8 = 2;
const PULSE: u8 = 3;

fn color_code(c: Rgbw) -> u32 {
    (u32::from(c.w) << 24) | (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

fn color_from(v: u32) -> Rgbw {
    Rgbw::new((v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8)
}

pub(super) fn encode(e: &mut Encoder, spec: &LookSpec) {
    let count = spec.iter().count().min(MAX_LAYERS);
    e.u8(count as u8);
    for layer in spec.iter().take(count) {
        match layer.block {
            Block::Fill { color } => {
                e.u8(FILL).u32(color_code(color));
            }
            Block::Comet {
                color,
                period_ms,
                tail,
                heads,
                base,
                reverse,
            } => {
                e.u8(COMET)
                    .u32(color_code(color))
                    .u32(period_ms)
                    .u16(tail)
                    .u8(heads)
                    .u16(base)
                    .u8(u8::from(reverse));
            }
            Block::Arc {
                fraction,
                color,
                track,
                head,
                sheen,
            } => {
                let (kind, value) = match fraction {
                    Fraction::Fixed(f) => (0, f),
                    Fraction::Input => (1, 0),
                };
                e.u8(ARC)
                    .u8(kind)
                    .u16(value)
                    .u32(color_code(color))
                    .u32(color_code(track))
                    .u16(head)
                    .u8(u8::from(sheen));
            }
            Block::Ripple {
                origin,
                color,
                speed,
                width,
                settle_ms,
                glow,
            } => {
                e.u8(RIPPLE)
                    .u16(origin)
                    .u32(color_code(color))
                    .u16(speed)
                    .u16(width)
                    .u32(settle_ms)
                    .u16(glow);
            }
            Block::Frame { pixels, len } => {
                let len = usize::from(len).min(MAX_FRAME_LEDS);
                e.u8(FRAME).u8(len as u8);
                for p in &pixels[..len] {
                    e.u32(color_code(*p));
                }
            }
            Block::Wash { color, effect } => {
                e.u8(WASH).u32(color_code(color));
                encode_effect(e, effect);
            }
            Block::Sparkle(sp) => {
                let count = usize::from(sp.count).clamp(1, MAX_SPARKLE_COLORS);
                e.u8(SPARKLE).u8(count as u8);
                for c in &sp.colors[..count] {
                    e.u32(color_code(*c));
                }
                e.u32(sp.density)
                    .u32(sp.density_end)
                    .u32(sp.fade_ms)
                    .u32(sp.start_ms)
                    .u32(sp.min_ms)
                    .u32(sp.max_ms)
                    .u16(sp.base)
                    .u32(sp.seed)
                    .u8(u8::from(sp.fall))
                    .u32(sp.fall_speed)
                    .u32(sp.fall_accel);
            }
        }
        e.u8(layer.brightness).u8(match layer.mode {
            Mode::Max => 0,
            Mode::Add => 1,
        });
    }
    encode_effect(e, spec.envelope);
    e.u8(spec.brightness).u8(spec.floor);
}

/// An envelope: its code, then its fields.
fn encode_effect(e: &mut Encoder, effect: Effect) {
    match effect {
        Effect::Solid => {
            e.u8(SOLID);
        }
        Effect::Blink { period_ms, duty } => {
            e.u8(BLINK).u32(period_ms).u16(duty);
        }
        Effect::Breathe {
            period_ms,
            depth,
            easing,
        } => {
            let (code, params) = easing_code(Some(easing));
            e.u8(BREATHE).u32(period_ms).u16(depth).u8(code);
            for p in params {
                e.u16(p);
            }
        }
        Effect::Pulse {
            attack_ms,
            hold_ms,
            decay_ms,
            repeat,
        } => {
            e.u8(PULSE)
                .u32(attack_ms)
                .u32(hold_ms)
                .u32(decay_ms)
                .u8(repeat);
        }
    }
}

/// An envelope, as [`encode_effect`] wrote it.
fn decode_effect(d: &mut Decoder<'_>) -> Result<Effect, WireError> {
    Ok(match d.u8()? {
        SOLID => Effect::Solid,
        BLINK => Effect::Blink {
            period_ms: d.u32()?,
            duty: d.u16()?,
        },
        BREATHE => {
            let period_ms = d.u32()?;
            let depth = d.u16()?;
            let code = d.u8()?;
            let params = [d.u16()?, d.u16()?, d.u16()?, d.u16()?];
            let easing =
                easing_from(code, params).ok_or_else(|| bad(format!("breathe easing {code}")))?;
            Effect::Breathe {
                period_ms,
                depth,
                easing,
            }
        }
        PULSE => Effect::Pulse {
            attack_ms: d.u32()?,
            hold_ms: d.u32()?,
            decay_ms: d.u32()?,
            repeat: d.u8()?,
        },
        other => return Err(bad(format!("look envelope {other}"))),
    })
}

pub(super) fn decode(d: &mut Decoder<'_>) -> Result<LookSpec, WireError> {
    let count = usize::from(d.u8()?);
    if !(1..=MAX_LAYERS).contains(&count) {
        return Err(bad(format!("look with {count} layers")));
    }
    let mut spec = LookSpec::EMPTY;
    for _ in 0..count {
        let block = match d.u8()? {
            FILL => Block::Fill {
                color: color_from(d.u32()?),
            },
            COMET => Block::Comet {
                color: color_from(d.u32()?),
                period_ms: d.u32()?,
                tail: d.u16()?,
                heads: d.u8()?,
                base: d.u16()?,
                reverse: d.u8()? != 0,
            },
            ARC => {
                let kind = d.u8()?;
                let value = d.u16()?;
                let fraction = match kind {
                    0 => Fraction::Fixed(value),
                    1 => Fraction::Input,
                    other => return Err(bad(format!("arc fraction kind {other}"))),
                };
                Block::Arc {
                    fraction,
                    color: color_from(d.u32()?),
                    track: color_from(d.u32()?),
                    head: d.u16()?,
                    sheen: d.u8()? != 0,
                }
            }
            RIPPLE => Block::Ripple {
                origin: d.u16()?,
                color: color_from(d.u32()?),
                speed: d.u16()?,
                width: d.u16()?,
                settle_ms: d.u32()?,
                glow: d.u16()?,
            },
            FRAME => {
                let len = usize::from(d.u8()?);
                if len > MAX_FRAME_LEDS {
                    return Err(bad(format!("frame of {len} pixels")));
                }
                let mut pixels = [Rgbw::OFF; MAX_FRAME_LEDS];
                for slot in pixels.iter_mut().take(len) {
                    *slot = color_from(d.u32()?);
                }
                Block::Frame {
                    pixels,
                    len: len as u8,
                }
            }
            WASH => Block::Wash {
                color: color_from(d.u32()?),
                effect: decode_effect(d)?,
            },
            SPARKLE => {
                let count = usize::from(d.u8()?);
                if !(1..=MAX_SPARKLE_COLORS).contains(&count) {
                    return Err(bad(format!("sparkle of {count} colours")));
                }
                let mut colors = [Rgbw::OFF; MAX_SPARKLE_COLORS];
                for slot in colors.iter_mut().take(count) {
                    *slot = color_from(d.u32()?);
                }
                Block::Sparkle(Sparkle {
                    colors,
                    count: count as u8,
                    density: d.u32()?,
                    density_end: d.u32()?,
                    fade_ms: d.u32()?,
                    start_ms: d.u32()?,
                    min_ms: d.u32()?,
                    max_ms: d.u32()?,
                    base: d.u16()?,
                    seed: d.u32()?,
                    fall: d.u8()? != 0,
                    fall_speed: d.u32()?,
                    fall_accel: d.u32()?,
                })
            }
            other => return Err(bad(format!("look block {other}"))),
        };
        let brightness = d.u8()?;
        let mode = match d.u8()? {
            0 => Mode::Max,
            1 => Mode::Add,
            other => return Err(bad(format!("look mode {other}"))),
        };
        spec.push(LayerSpec {
            block,
            brightness,
            mode,
        });
    }
    spec.envelope = decode_effect(d)?;
    spec.brightness = d.u8()?;
    spec.floor = d.u8()?;
    Ok(spec)
}
