//! The example look set's definitions as they were built in, for the tests
//! only: `system.*`, `detect.*` and `link.*` at the default board settings.
//! The golden frames (`golden.txt`) and the engine tests render these. The
//! shipped set is the look files in `examples/looks`, which `lemnosd`'s
//! `example_looks` test checks frame for frame against the same golden
//! frames. Nothing here is compiled into a release build.

use crate::animator::{Effect, Transition};
use crate::builtin::{FULL_FILL, comet};
use crate::easing::Easing;
use crate::intent::{Defaults, EMBER, Intent, Lookup};
use crate::look::{Block, Fraction, LayerSpec, LookSpec, Mode};
use crate::sparkle::Sparkle;
use lemnos_device::Rgbw;

/// The reboot ember's floor (see `EMBER`).
const EMBER_FLOOR: u8 = 40;
/// The staged breathe's brightness: a little above a full fill.
const STAGED_BRIGHTNESS: u8 = 217;
/// The booting look's faint white wash.
const BOOTING_WASH: u8 = 23;

/// The look `name` as the example set defined it at the default settings.
pub(crate) fn look(name: &str, d: &Defaults) -> Option<LookSpec> {
    Some(match name {
        "system.verifying" => comet(Rgbw::rgb(0x8a5cff), 1_200, 8_000, 1, 180),
        "system.writing-unknown" => comet(Rgbw::rgb(0x2f7bff), 1_200, 8_000, 1, 180),
        "system.writing" => LookSpec::of(LayerSpec::new(Block::Arc {
            fraction: Fraction::Input,
            color: Rgbw::rgb(0x2f7bff),
            track: d.progress_background,
            head: 350,
            sheen: true,
        })),
        "system.staged" => LookSpec::fill(Rgbw::rgb(0x00ff20))
            .with_brightness(STAGED_BRIGHTNESS)
            .with_envelope(Effect::Breathe {
                period_ms: 2_200,
                depth: 450,
                easing: Easing::EaseInOut,
            }),
        "system.booting" => booting(Rgbw::rgb(0xfff4e6), Easing::EaseInOut),
        "system.rebooting" => LookSpec::fill(Rgbw::rgb(0xff8000))
            .with_brightness(EMBER)
            .with_floor(EMBER_FLOOR),
        "system.failed" | "system.rolled-back" => LookSpec::fill(Rgbw::rgb(0xff3b3b))
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: 2_400,
                depth: 900,
                easing: Easing::EaseInOut,
            }),
        "system.confirmed" => confirmed(Rgbw::rgb(0x00ff20)),
        "system.locate" => LookSpec::fill(Rgbw::rgb(0x00ffff))
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: d.breathe_period_ms,
                depth: d.breathe_depth,
                easing: Easing::EaseInOut,
            }),
        "detect.targets" => LookSpec::fill(Rgbw::rgb(0x28c8ff))
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: 4_000,
                depth: 200,
                easing: Easing::EaseInOut,
            }),
        "detect.searching" => comet(Rgbw::rgb(0x965aff), 1_600, 7_000, 1, 180),
        "link.waiting" => comet(Rgbw::rgb(0xff5a00), 2_400, 6_000, 2, 180),
        "link.waiting-targets" => {
            let mut look = LookSpec::EMPTY;
            look.push(LayerSpec::fill(Rgbw::rgb(0x28c8ff)).with_brightness(64));
            look.push(
                LayerSpec::comet(Rgbw::rgb(0xff5a00), 2_400, 6_000, 2, 0).with_mode(Mode::Over),
            );
            look
        }
        "detect.error" => LookSpec::fill(Rgbw::rgb(0xff2828))
            .with_brightness(FULL_FILL)
            .with_envelope(Effect::Breathe {
                period_ms: 2_000,
                depth: 850,
                easing: Easing::EaseInOut,
            }),
        "detect.live" => LookSpec::fill(Rgbw::rgb(0xffffff)).with_brightness(FULL_FILL),
        _ => return None,
    })
}

/// The booting look: a starry sparkle over a faint white breathe.
fn booting(color: Rgbw, easing: Easing) -> LookSpec {
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

/// The confirmed celebration: a flash, a burst and a drain (see `example`).
fn confirmed(color: Rgbw) -> LookSpec {
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

/// Resolves an intent with the reference looks, as a light with the example
/// set loaded would (a name the reference does not hold is the built-in one).
pub(crate) trait ResolveReference {
    fn resolve_reference(&self, d: &Defaults) -> (LookSpec, Transition);
}

impl<const N: usize> ResolveReference for Intent<N> {
    fn resolve_reference(&self, d: &Defaults) -> (LookSpec, Transition) {
        let lookup = |name: &str| look(name, d);
        let lookup: Lookup<'_> = &lookup;
        self.resolve(d, lookup)
    }
}
