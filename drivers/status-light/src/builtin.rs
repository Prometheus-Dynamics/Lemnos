//! The built-in looks: `status.*`, made from the board's settings. Nothing else is built in; a
//! look is sent in full.

use crate::look::{Block, Envelope, Look, Rgbw, scale_level};
use crate::settings::{EffectKind, Settings};

/// The brightness of a full-ring fill or breathe (0.7 of full): a fill is much brighter than a
/// comet's head, so it is capped lower.
pub const FULL_FILL: u8 = 178;

/// A built-in status look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusLook {
    Ok = 0,
    Warn = 1,
    Error = 2,
    Busy = 3,
    Off = 4,
}

impl StatusLook {
    /// Every status look, in wire order (see [`StatusLook::index`]).
    pub const ALL: [Self; 5] = [Self::Ok, Self::Warn, Self::Error, Self::Busy, Self::Off];

    /// The look's name, `status.ok` and so on.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ok => "status.ok",
            Self::Warn => "status.warn",
            Self::Error => "status.error",
            Self::Busy => "status.busy",
            Self::Off => "status.off",
        }
    }

    /// The look named `name` (`status.ok` and so on).
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|look| look.name() == name)
    }

    /// The look's position in [`StatusLook::ALL`], as a look's bytes carry it.
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// The look at `index` in [`StatusLook::ALL`].
    pub fn from_index(index: u8) -> Option<Self> {
        Self::ALL.get(usize::from(index)).copied()
    }
}

/// The built-in look `status`, from the board's `settings`.
pub fn look(status: StatusLook, s: &Settings) -> Look {
    match status {
        StatusLook::Ok => themed(s.ok, s.status_effect, s),
        StatusLook::Warn => themed(s.warn, s.status_effect, s),
        StatusLook::Error => themed(s.error, s.error_effect, s),
        StatusLook::Busy => themed(s.busy, s.status_effect, s),
        StatusLook::Off => Look {
            block: Block::Fill { color: Rgbw::OFF },
            brightness: scale_level(255, s.look_brightness),
            envelope: Envelope::Solid,
        },
    }
}

/// A status in `color` with `kind`'s envelope. A chase is a spinner: one comet going round.
fn themed(color: Rgbw, kind: EffectKind, s: &Settings) -> Look {
    let fill = |envelope| Look {
        block: Block::Fill { color },
        brightness: scale_level(FULL_FILL, s.look_brightness),
        envelope,
    };
    match kind {
        EffectKind::Solid => fill(Envelope::Solid),
        EffectKind::Chase => Look {
            block: Block::Comet {
                color,
                period_ms: s.spinner_period_ms,
                tail: u16::from(s.spinner_tail) * 1000,
                heads: 1,
                base: s.spinner_base,
            },
            brightness: scale_level(255, s.look_brightness),
            envelope: Envelope::Solid,
        },
        EffectKind::Blink => fill(Envelope::Blink {
            period_ms: s.blink_period_ms,
            duty: s.blink_duty,
        }),
        EffectKind::Breathe => fill(Envelope::Breathe {
            period_ms: s.breathe_period_ms,
            depth: s.breathe_depth,
            easing: s.easing,
        }),
    }
}
