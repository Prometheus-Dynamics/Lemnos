//! The built-in named looks: `status.*` and `system.*`, made from the board's
//! [`Defaults`] (its keys shape them). A look file of the same name replaces
//! one entirely.

use crate::animator::Effect;
use crate::intent::{Defaults, EffectKind};
use crate::look::{Block, Fraction, LayerSpec, LookSpec};
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
    "system.verifying",
    "system.writing",
    "system.writing-unknown",
    "system.staged",
    "system.booting",
    "system.rebooting",
    "system.failed",
    "system.rolled-back",
    "system.confirmed",
    "system.locate",
    "pv.targets",
    "pv.searching",
    "pv.no-nt",
    "pv.no-nt-targets",
    "pv.error",
    "pv.vision",
];

/// The built-in look `name`, from the board's `defaults`.
pub fn builtin(name: &str, d: &Defaults) -> Option<LookSpec> {
    Some(match name {
        "status.ok" => themed(d.ok, d.status_effect, d),
        "status.warn" => themed(d.warn, d.status_effect, d),
        "status.error" => themed(d.error, d.error_effect, d),
        "status.busy" => themed(d.busy, d.status_effect, d),
        "status.off" => LookSpec::fill(Rgbw::OFF),
        "system.verifying" => comet(
            d.verifying,
            d.verifying_period_ms,
            d.verifying_tail,
            1,
            d.verifying_base,
        ),
        "system.writing-unknown" => comet(
            d.writing,
            d.verifying_period_ms,
            d.verifying_tail,
            1,
            d.verifying_base,
        ),
        // A gauge: the fraction is the request's progress.
        "system.writing" => LookSpec::of(LayerSpec::new(Block::Arc {
            fraction: Fraction::Input,
            color: d.updating,
            track: d.progress_background,
            head: 350,
            sheen: true,
        })),
        "system.staged" => LookSpec::fill(d.staged)
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: d.staged_period_ms,
                depth: d.staged_depth,
                easing: d.easing,
            }),
        "system.booting" => comet(
            d.booting,
            d.booting_period_ms,
            d.booting_tail,
            2,
            d.booting_base,
        ),
        // The ember: held while power is cut, so it stays visible even when
        // the ring-wide brightness is low.
        "system.rebooting" => LookSpec::fill(d.rebooting)
            .with_brightness(EMBER)
            .with_floor(EMBER_FLOOR),
        "system.failed" | "system.rolled-back" => LookSpec::fill(d.failed)
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: d.failed_period_ms,
                depth: d.failed_depth,
                easing: d.easing,
            }),
        "system.confirmed" => LookSpec::ripple(d.confirmed),
        // PhotonVision's app looks (the board's ring is the vision app's
        // status): blue while targets are seen, green while searching, amber
        // without NetworkTables, red on an error.
        "pv.targets" => LookSpec::fill(Rgbw::rgb(0x2f7bff))
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: 4_000,
                depth: 180,
                easing: d.easing,
            }),
        "pv.searching" => comet(Rgbw::rgb(0x2bd47d), 1_600, 6_000, 1, 60),
        "pv.no-nt" => comet(Rgbw::rgb(0xffa424), 2_400, 5_000, 2, 50),
        "pv.no-nt-targets" => comet(Rgbw::rgb(0x2f7bff), 2_400, 5_000, 2, 80),
        "pv.error" => LookSpec::fill(Rgbw::rgb(0xff3b3b))
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: 2_000,
                depth: 850,
                easing: d.easing,
            }),
        "pv.vision" => LookSpec::fill(Rgbw::rgb(0xffffff)).with_brightness(FULL_FILL),
        "system.locate" => {
            if d.locate_effect == EffectKind::Chase {
                spinner(d.locate, d)
            } else {
                themed(d.locate, d.locate_effect, d)
            }
        }
        _ => return None,
    })
}

/// The brightness of the reboot ember (see [`EMBER`](crate::EMBER)).
pub const EMBER: u8 = crate::intent::EMBER;
/// The least the ember is shown at, after the ring-wide scale (about 6%).
pub const EMBER_FLOOR: u8 = 16;

/// A fill in `color` with `kind`'s envelope; a chase is a spinner.
pub fn themed(color: Rgbw, kind: EffectKind, d: &Defaults) -> LookSpec {
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
        0,
    )
}

/// The spinner's comet as a block (a chase over a fill).
pub fn comet_block(color: Rgbw, d: &Defaults) -> Block {
    Block::Comet {
        color,
        period_ms: d.spinner_period_ms,
        tail: u16::from(d.spinner_tail) * 1000,
        heads: 1,
        base: 0,
        reverse: false,
    }
}

/// Comets in `color`: `tail` and `base` in thousandths.
pub fn comet(color: Rgbw, period_ms: u32, tail: u16, heads: u8, base: u16) -> LookSpec {
    LookSpec::comet(color, period_ms, tail, heads, base)
}
