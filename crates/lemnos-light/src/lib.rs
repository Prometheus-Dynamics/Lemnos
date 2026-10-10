//! Light rendering for Lemnos, without `std` or allocation: what an LED or
//! an addressable strip shows, and how it moves between looks.
//!
//! - [`Easing`]: linear, ease-in, ease-out, ease-in-out, sine and CSS-style
//!   cubic Bézier curves, in fixed point (no float code on FPU-less MCUs).
//! - [`Effect`]: `Solid`, `Blink` (hard) and `Breathe` (an eased pulse with a
//!   rate and depth, the soft replacement for blinking).
//! - [`Animator`]: renders a [`LookSpec`] and fades between looks with a
//!   [`Transition`] (`fade_ms` + easing), colour and brightness alike. It returns a frame
//!   only when the output changed and says when to render next (50 Hz while a fade or an
//!   effect runs, never while steady), so a host writes the device only when something
//!   moves. Frames are fixed arrays: no allocation per frame.
//! - Looks as data ([`LookSpec`]): up to four [`LayerSpec`]s, each a [`Block`] (a fill, a
//!   comet with one or two heads, a progress arc with a track, head and sheen, a ripple, or
//!   static pixels), composited by `max` or `add`, under an envelope ([`Effect`]) and a
//!   brightness. The built-in named looks ([`builtin_look`]) are made from a board's
//!   [`Defaults`].
//! - Sparkles (`Block::Sparkle`, see [`Sparkle`]): random twinkles, or falling sparks toward a
//!   bottom the animator is given ([`Animator::set_bottom`]); a wash (`Block::Wash`) is a colour
//!   under its own envelope.
//! - Comets (`Block::Comet`): one or two heads going round with a tail and a floor
//!   brightness, for unknown amounts, booting, searching and chases. A ripple
//!   (`Block::Ripple`) marks a confirmed update.
//! - Built-in system animations ([`SystemState`], named `system.*`): updating (the arc, or a
//!   purple comet while the amount is unknown), booting (a twin comet), staged (a green
//!   breathe), confirmed (the ripple), rebooting (a static [`EMBER`]), update failed and
//!   rolled back (a red breathe).
//! - [`Arbiter`] and [`Intent`]: owners (clients) hold intents in
//!   [`Layer`]s; the light shows `Locate` over `System` over `Alert` over
//!   `Status` over `App`, then by priority and recency, and fades between
//!   owners.
//!   [`Defaults`] (from the board definition) fill in what an intent leaves
//!   out.
//!
//! The renderer works on linear values; apply gamma once, at the output
//! (the Raspberry Pi RP1 `ws2812-pio` kernel driver does).

#![no_std]
#![forbid(unsafe_code)]

mod animator;
mod builtin;
mod easing;
mod intent;
mod look;
mod sparkle;

#[cfg(test)]
mod golden;
#[cfg(test)]
mod tests;

pub use animator::{Animator, Effect, FRAME_MS, Transition};
pub use builtin::{FULL_FILL, NAMES as BUILTIN_LOOK_NAMES, builtin as builtin_look};
pub use easing::{Easing, ONE};
pub use intent::{
    Arbiter, Defaults, EMBER, EffectKind, Intent, Layer, Lookup, Phase, Show, Status, SystemState,
    Winner, named_look,
};
pub use lemnos_device::Rgbw;
pub use look::{
    Block, Fraction, LayerSpec, LookName, LookSpec, MAX_FRAME_LEDS, MAX_LAYERS, MAX_LOOK_NAME,
    Mode, valid_look_name,
};
pub use sparkle::{MAX_PARTICLES, MAX_SPARKLE_COLORS, Sparkle};
