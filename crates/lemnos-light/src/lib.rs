//! Light rendering for Lemnos, without `std` or allocation: what an LED or
//! an addressable strip shows, and how it moves between looks.
//!
//! - [`Easing`]: linear, ease-in, ease-out, ease-in-out, sine and CSS-style
//!   cubic Bézier curves, in fixed point (no float code on FPU-less MCUs).
//! - [`Effect`]: `Solid`, `Blink` (hard) and `Breathe` (an eased pulse with a
//!   rate and depth, the soft replacement for blinking).
//! - [`Animator`]: renders a [`Look`] (a colour or a frame, an effect, a
//!   brightness) and fades between looks with a [`Transition`]
//!   (`fade_ms` + easing), colour and brightness alike. It returns a frame
//!   only when the output changed and says when to render next (50 Hz while
//!   a fade or an effect runs, never while steady), so a host writes the
//!   device only when something moves. Frames are fixed arrays: no
//!   allocation per frame.
//! - Gauges and spinners: a progress fill with sub-LED precision and an
//!   eased advance, and a comet for unknown amounts, booting and chases.
//! - Built-in system animations ([`SystemState`]): updating (a fill over the
//!   phase's colour), booting and rebooting (spinners), update failed and
//!   rolled back (a red pulse).
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
mod easing;
mod intent;

#[cfg(test)]
mod tests;

pub use animator::{Animator, Effect, FRAME_MS, Look, Pixels, Transition};
pub use easing::{Easing, ONE};
pub use intent::{
    Arbiter, Defaults, EffectKind, Intent, Layer, Phase, Show, Status, SystemState, Winner,
};
pub use lemnos_device::Rgbw;
