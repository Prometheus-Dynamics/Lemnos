//! The built-in named looks: `status.*` and `system.*`, made from the board's
//! [`Defaults`] (its keys shape them). A look file of the same name replaces
//! one entirely.

use crate::animator::Effect;
use crate::easing::Easing;
use crate::intent::{Defaults, EffectKind};
use crate::look::{Block, Fraction, LayerSpec, LookSpec, Mode};
use crate::sparkle::Sparkle;
use lemnos_device::Rgbw;

/// The brightness of a full-ring fill or breathe (0.7 of full): a fill
/// is much brighter than a comet's head, so it is capped lower.
pub const FULL_FILL: u8 = 178;

/// The brightness of the staged breathe: a little above a full fill, so the
/// ring reads as "ready" from across the room.
pub const STAGED_BRIGHTNESS: u8 = 217;
/// The brightness of the booting breathe: a faint white wash under the
/// sparkle (about 9% at its top, 4% at its bottom).
pub const BOOTING_WASH: u8 = 23;

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
            .with_brightness(STAGED_BRIGHTNESS)
            .with_envelope(Effect::Breathe {
                period_ms: d.staged_period_ms,
                depth: d.staged_depth,
                easing: d.easing,
            }),
        "system.booting" => booting(d.booting, d.easing),
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
        "system.confirmed" => confirmed(d.confirmed),
        // PhotonVision's app looks (the board's ring is the vision app's
        // status): blue while targets are seen, green while searching, amber
        // without NetworkTables, red on an error.
        "pv.targets" => LookSpec::fill(Rgbw::rgb(0x28c8ff))
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: 4_000,
                depth: 200,
                easing: d.easing,
            }),
        "pv.searching" => comet(Rgbw::rgb(0x965aff), 1_600, 7_000, 1, 180),
        "pv.no-nt" => comet(Rgbw::rgb(0xff5a00), 2_400, 6_000, 2, 180),
        // A deep-orange twin comet, over a cyan glow at a quarter of full:
        // the head and tail are the comet's colour, the glow shows where the
        // comet is not (`over`, so no hue mixing).
        "pv.no-nt-targets" => {
            let mut look = LookSpec::EMPTY;
            look.push(LayerSpec::fill(Rgbw::rgb(0x28c8ff)).with_brightness(64));
            look.push(
                LayerSpec::comet(Rgbw::rgb(0xff5a00), 2_400, 6_000, 2, 0).with_mode(Mode::Over),
            );
            look
        }
        "pv.error" => LookSpec::fill(Rgbw::rgb(0xff2828))
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
/// The least the ember is shown at, after the ring-wide scale (about 16%).
pub const EMBER_FLOOR: u8 = 40;

/// The booting look (the trial boot, "testing a new image"): a starry
/// sparkle of warm white over a faint white breathe, slow and calm.
pub fn booting(color: Rgbw, easing: Easing) -> LookSpec {
    let mut look = LookSpec::of(
        LayerSpec::new(Block::Wash {
            color: Rgbw::rgb(0xffffff),
            effect: Effect::Breathe {
                period_ms: 3_000,
                depth: 550,
                easing,
            },
        })
        .with_brightness(BOOTING_WASH),
    );
    look.push(LayerSpec::new(Block::Sparkle(Sparkle {
        density: 1_200,
        density_end: 1_200,
        base: 30,
        ..Sparkle::one(color)
    })));
    look
}

/// The confirmed celebration (the update held): a pale green-white flash of
/// the whole ring (attack 80 ms, decaying over 500 ms), a green burst out
/// from the top and back round at 14 LEDs a second, and the green filling
/// the ring from the top (700 ms), holding, then draining to the gravity
/// bottom (from 1.1 s, over 2.1 s, ease-in) with its last LEDs fading out.
/// About 3.2 s; the host holds it a little longer.
pub fn confirmed(color: Rgbw) -> LookSpec {
    const FLASH: Rgbw = Rgbw::rgb(0xc8ffd2);
    let mut look = LookSpec::of(LayerSpec::new(Block::Wash {
        color: FLASH,
        effect: Effect::Pulse {
            attack_ms: 80,
            hold_ms: 0,
            decay_ms: 500,
            repeat: 1,
        },
    }));
    look.push(LayerSpec::new(Block::Drain {
        color,
        fill_ms: 700,
        start_ms: 1_100,
        duration_ms: 2_100,
        easing: Easing::EaseIn,
    }));
    look.push(LayerSpec::new(Block::Ripple {
        origin: 0,
        color,
        speed: 14_000,
        width: 1_600,
        settle_ms: 1,
        glow: 0,
    }));
    look
}

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
        d.spinner_base,
    )
}

/// The spinner's comet as a block (a chase over a fill).
pub fn comet_block(color: Rgbw, d: &Defaults) -> Block {
    Block::Comet {
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
