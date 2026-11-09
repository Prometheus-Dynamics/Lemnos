//! Renders a [`Look`] onto a ring and fades between looks.
//!
//! The frame buffers are allocated once, when the animator is made, at the ring's size. Rendering
//! into them allocates nothing. [`Animator::render`] returns a frame only when the output changed,
//! and [`Animator::next_frame_ms`] says when to render next (`None` while steady), so a host writes
//! the device only while something moves.

use alloc::vec;
use alloc::vec::Vec;

use crate::easing::Easing;
use crate::look::{Look, Rgbw, scale};

/// The default interval between frames while animating: 50 Hz.
pub const FRAME_MS: u32 = 20;

/// How a look moves to the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    pub duration_ms: u32,
    pub easing: Easing,
}

impl Transition {
    /// No fade.
    pub const CUT: Self = Self {
        duration_ms: 0,
        easing: Easing::Linear,
    };

    pub const fn new(duration_ms: u32, easing: Easing) -> Self {
        Self {
            duration_ms,
            easing,
        }
    }
}

/// Renders a ring: the current look, its envelope, a runtime brightness, and fades between looks.
#[derive(Debug, Clone)]
pub struct Animator {
    /// What each LED showed when the current look was set.
    from: Vec<Rgbw>,
    /// What each LED shows now (the last frame rendered).
    shown: Vec<Rgbw>,
    look: Look,
    /// When the look's own time started (the envelope and the comet count from here).
    look_since_ms: u64,
    transition: Transition,
    transition_since_ms: u64,
    /// Whether the current look has been rendered since it was set.
    written: bool,
    /// The runtime brightness of everything shown, 0..=255.
    ring: u8,
}

fn blend(from: Rgbw, to: Rgbw, t: u32) -> Rgbw {
    let mix = |a: u8, b: u8| {
        let (a, b) = (i64::from(a), i64::from(b));
        (a + (((b - a) * i64::from(t)) >> 16)) as u8
    };
    Rgbw::new(
        mix(from.r, to.r),
        mix(from.g, to.g),
        mix(from.b, to.b),
        mix(from.w, to.w),
    )
}

impl Animator {
    /// An animator for a ring of `count` LEDs, showing nothing.
    pub fn new(count: usize) -> Self {
        Self {
            from: vec![Rgbw::OFF; count],
            shown: vec![Rgbw::OFF; count],
            look: Look::OFF,
            look_since_ms: 0,
            transition: Transition::CUT,
            transition_since_ms: 0,
            written: false,
            ring: 255,
        }
    }

    /// The last frame rendered (what the ring shows).
    pub fn frame(&self) -> &[Rgbw] {
        &self.shown
    }

    /// Scales everything shown, from the next render on.
    pub fn set_ring(&mut self, level: u8) {
        self.ring = level;
    }

    /// Moves to `look` with `transition`, starting from what is shown now.
    pub fn set(&mut self, look: Look, transition: Transition, now_ms: u64) {
        self.from.copy_from_slice(&self.shown);
        if !self.look.same_shape(&look) || look.envelope != self.look.envelope {
            self.look_since_ms = now_ms;
        }
        self.look = look;
        self.transition = transition;
        self.transition_since_ms = now_ms;
        self.written = false;
    }

    /// The transition's eased progress, `None` once it is over.
    fn eased(&self, now_ms: u64) -> Option<u32> {
        self.in_transition(now_ms).then(|| {
            let elapsed = now_ms - self.transition_since_ms;
            let t = ((elapsed << 16) / u64::from(self.transition.duration_ms.max(1))) as u32;
            self.transition.easing.apply(t)
        })
    }

    fn in_transition(&self, now_ms: u64) -> bool {
        now_ms < self.transition_since_ms + u64::from(self.transition.duration_ms)
    }

    /// Whether the output changes over time right now.
    pub fn is_animating(&self, now_ms: u64) -> bool {
        self.in_transition(now_ms)
            || self.look.envelope.is_animated()
            || self.look.block.is_moving()
    }

    /// When to render next: one frame interval on while animating or before the first write, `None`
    /// when steady.
    pub fn next_frame_ms(&self, now_ms: u64) -> Option<u64> {
        (self.is_animating(now_ms) || !self.written).then(|| now_ms + u64::from(FRAME_MS))
    }

    /// Renders the frame for `now_ms`. Returns it when it differs from the last frame, or when the
    /// look was set and not yet rendered; else `None`, and nothing needs writing.
    pub fn render(&mut self, now_ms: u64) -> Option<&[Rgbw]> {
        let elapsed = now_ms.saturating_sub(self.look_since_ms);
        let level = self.look.envelope.level(elapsed);
        let level = (u64::from(level) * u64::from(self.look.brightness) / 255) as u32;
        let level = (u64::from(level) * u64::from(self.ring) / 255) as u32;
        let fade = self.eased(now_ms);
        let count = self.shown.len();
        let mut changed = !self.written;
        for index in 0..count {
            let target = scale(self.look.block.color(index, count, elapsed), level);
            let pixel = match fade {
                Some(t) => blend(self.from[index], target, t),
                None => target,
            };
            if self.shown[index] != pixel {
                self.shown[index] = pixel;
                changed = true;
            }
        }
        if changed {
            self.written = true;
            Some(&self.shown)
        } else {
            None
        }
    }
}
