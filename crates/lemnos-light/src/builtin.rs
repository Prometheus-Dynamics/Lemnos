//! The built-in named looks: `status.*`, made from the board's [`Defaults`]
//! (its keys shape them). Every other named look (the system states, the
//! detection and link looks) is a look file: the example set is in
//! `examples/looks`, and a look file of the same name replaces a built-in
//! entirely.

use crate::animator::Effect;
use crate::intent::{Defaults, EffectKind};
use crate::look::LookSpec;
use lemnos_device::Rgbw;

/// The brightness of a full-ring fill or breathe (0.7 of full): a fill
/// is much brighter than a comet's head, so it is capped lower.
pub const FULL_FILL: u8 = 178;

/// The names of every built-in look.
pub const NAMES: &[&str] = &[
    "status.ok",
    "status.warn",
    "status.error",
    "status.busy",
    "status.off",
];

/// The built-in look `name`, from the board's `defaults`.
pub fn builtin(name: &str, d: &Defaults) -> Option<LookSpec> {
    Some(match name {
        "status.ok" => themed(d.ok, d.status_effect, d),
        "status.warn" => themed(d.warn, d.status_effect, d),
        "status.error" => themed(d.error, d.error_effect, d),
        "status.busy" => themed(d.busy, d.status_effect, d),
        "status.off" => LookSpec::fill(Rgbw::OFF),
        _ => return None,
    })
}

/// A fill in `color` with `kind`'s envelope; a chase is a spinner.
pub(crate) fn themed(color: Rgbw, kind: EffectKind, d: &Defaults) -> LookSpec {
    let fill = LookSpec::fill(color).with_brightness(FULL_FILL);
    match kind {
        EffectKind::Solid => fill,
        EffectKind::Chase => spinner(color, d),
        EffectKind::Blink => fill.with_envelope(Effect::Blink {
            period_ms: d.blink_period_ms,
            duty: d.blink_duty,
        }),
        EffectKind::Breathe => fill.with_envelope(Effect::Breathe {
            period_ms: d.breathe_period_ms,
            depth: d.breathe_depth,
            easing: d.easing,
        }),
    }
}

/// One comet, no floor, at the spinner's period and tail.
pub fn spinner(color: Rgbw, d: &Defaults) -> LookSpec {
    comet(
        color,
        d.spinner_period_ms,
        u16::from(d.spinner_tail) * 1000,
        1,
        d.spinner_base,
    )
}

/// The spinner's comet as a block (a chase over a fill).
pub(crate) fn comet_block(color: Rgbw, d: &Defaults) -> crate::look::Block {
    crate::look::Block::Comet {
        color,
        period_ms: d.spinner_period_ms,
        tail: u16::from(d.spinner_tail) * 1000,
        heads: 1,
        base: d.spinner_base,
        reverse: false,
    }
}

/// Comets in `color`: `tail` and `base` in thousandths.
pub fn comet(color: Rgbw, period_ms: u32, tail: u16, heads: u8, base: u16) -> LookSpec {
    LookSpec::comet(color, period_ms, tail, heads, base)
}
