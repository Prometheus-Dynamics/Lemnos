//! Soft spots: up to [`MAX_SPOTS`] points of light on the ring, each with a
//! position, a width, a colour and a level, that glide to new places when
//! their inputs change.
//!
//! A spot's position is a fraction of a turn from LED 0's centre (0 to 1000
//! thousandths; the ring's logical index, like a ripple's `origin`, so the
//! board's `offset` and direction apply at output). A position beyond one turn
//! wraps. Its falloff is a raised cosine over `width` LEDs: full at its centre,
//! falling to zero `width / 2` LEDs either side, so a spot between two LEDs
//! lights both of them. Spots in one block combine by `max`, and the block's
//! layer mode (`max`, `add`, `over`) combines it with the layers below.
//!
//! The motion is a critically damped spring per spot, for its position and
//! its level, in fixed point (no float): it starts at zero velocity and never
//! overshoots, and reaches about 95% of a change in `follow_ms`. It moves in
//! whole 1 ms steps from the time it was last rendered, so the same inputs at
//! the same times give the same frames whatever the frame rate. A spot moves
//! the shorter way round the ring. Spots are matched to the spots they replace
//! by nearest position, so a moved spot slides and a new one fades in from
//! nothing at its place; a spot dropped from the list fades out. A look change
//! that is not an input change starts the spots afresh (the look's own fade
//! still applies to the whole look).

use crate::look::{Block, LayerSpec, LookSpec, cos_turn, q16, scale};
use lemnos_device::Rgbw;

/// Spots in one block (at most).
pub const MAX_SPOTS: usize = 8;

/// The default width, thousandths of an LED (1.5 LEDs).
pub const DEFAULT_WIDTH: u16 = 1_500;

/// The default time to follow a change, milliseconds.
pub const DEFAULT_FOLLOW_MS: u32 = 120;

/// The shortest and longest `follow_ms` a block may have. The shortest keeps
/// the spring stable at the 1 ms step.
pub(crate) const MIN_FOLLOW_MS: u32 = 10;
pub(crate) const MAX_FOLLOW_MS: u32 = 10_000;

/// The widest spot, thousandths of an LED (64 LEDs, the most a ring has).
pub(crate) const MAX_WIDTH: u16 = 64_000;

/// One spot's target: where, how bright and, optionally, its own colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Spot {
    /// Thousandths of a turn from LED 0's centre; past 1000 it wraps.
    pub at: u16,
    /// Thousandths of full (0 to 1000).
    pub level: u16,
    /// The spot's colour; `None` takes the block's.
    pub color: Option<Rgbw>,
}

impl Spot {
    /// A spot at `at` (thousandths of a turn) of `level` (thousandths), in the
    /// block's colour.
    pub const fn new(at: u16, level: u16) -> Self {
        Self {
            at,
            level,
            color: None,
        }
    }

    /// This spot in `color`, not the block's.
    pub const fn with_color(mut self, color: Rgbw) -> Self {
        self.color = Some(color);
        self
    }
}

/// A list of spots, at most [`MAX_SPOTS`]: the targets of a block's inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpotList {
    pub len: u8,
    pub spots: [Spot; MAX_SPOTS],
}

impl SpotList {
    /// No spots.
    pub const EMPTY: Self = Self {
        len: 0,
        spots: [Spot {
            at: 0,
            level: 0,
            color: None,
        }; MAX_SPOTS],
    };

    /// The first [`MAX_SPOTS`] of `spots`.
    pub fn new(spots: &[Spot]) -> Self {
        let mut list = Self::EMPTY;
        for spot in spots.iter().take(MAX_SPOTS) {
            list.spots[usize::from(list.len)] = *spot;
            list.len += 1;
        }
        list
    }

    /// The spots, in order.
    pub fn iter(&self) -> impl Iterator<Item = &Spot> {
        self.spots[..usize::from(self.len)].iter()
    }
}

/// Where a block's targets come from: the request's inputs (`"input"`), or
/// fixed in the look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpotSource {
    /// The request's spots (see [`Show::LookSpots`](crate::Show)); none when
    /// the request has none.
    Input,
    Fixed(SpotList),
}

/// A spots block's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpotsSpec {
    /// The colour of spots without their own.
    pub color: Rgbw,
    /// The full width of a spot's falloff, thousandths of an LED.
    pub width: u16,
    /// Milliseconds to follow a change (about 95% of the way).
    pub follow_ms: u32,
    /// How many of the targets are shown (1 to 8).
    pub max_spots: u8,
    pub source: SpotSource,
}

impl SpotsSpec {
    /// Spots in `color` from `source`, with the default width, follow and count.
    pub const fn new(color: Rgbw, source: SpotSource) -> Self {
        Self {
            color,
            width: DEFAULT_WIDTH,
            follow_ms: DEFAULT_FOLLOW_MS,
            max_spots: MAX_SPOTS as u8,
            source,
        }
    }
}

impl LayerSpec {
    /// A spots layer (see [`SpotsSpec::new`]) at full brightness, by `max`.
    pub const fn spots(color: Rgbw, source: SpotSource) -> Self {
        Self::new(Block::Spots(SpotsSpec::new(color, source)))
    }
}

impl LookSpec {
    /// The look's spots block, if it has one (at most one).
    pub fn spots_block(&self) -> Option<SpotsSpec> {
        self.iter().find_map(|l| match l.block {
            Block::Spots(s) => Some(s),
            _ => None,
        })
    }

    /// Whether the look takes spots from the request (a `"input"` block).
    pub fn takes_spots(&self) -> bool {
        matches!(
            self.spots_block(),
            Some(SpotsSpec {
                source: SpotSource::Input,
                ..
            })
        )
    }

    /// Whether the look takes a progress from the request (an arc with
    /// `fraction = "input"`).
    pub fn takes_progress(&self) -> bool {
        self.iter().any(|l| {
            matches!(
                l.block,
                Block::Arc {
                    fraction: crate::look::Fraction::Input,
                    ..
                }
            )
        })
    }

    /// Replaces every `Input` spots source with `spots` (the request's).
    pub fn fill_spots(&mut self, spots: &SpotList) {
        for slot in self.layers.iter_mut().flatten() {
            if let Block::Spots(s) = &mut slot.block
                && s.source == SpotSource::Input
            {
                s.source = SpotSource::Fixed(*spots);
            }
        }
    }
}

/// Checks a spots block's ranges. The reason names the first problem.
pub(crate) fn validate(spots: &SpotsSpec) -> Result<(), &'static str> {
    if !(1..=MAX_WIDTH).contains(&spots.width) {
        return Err("a spots block's width must be above 0 and at most 64 LEDs");
    }
    if !(MIN_FOLLOW_MS..=MAX_FOLLOW_MS).contains(&spots.follow_ms) {
        return Err("a spots block's follow_ms must be 10 to 10000");
    }
    if !(1..=MAX_SPOTS as u8).contains(&spots.max_spots) {
        return Err("a spots block's max_spots must be 1 to 8");
    }
    if let SpotSource::Fixed(list) = &spots.source {
        if list.len as usize > MAX_SPOTS {
            return Err("a spots block takes at most 8 spots");
        }
        if list.iter().any(|s| s.at > 1000 || s.level > 1000) {
            return Err("a fixed spot's at and level must be 0 to 1");
        }
    }
    Ok(())
}

/// One turn, in the spring's fixed point (Q32).
const TURN: i64 = 1 << 32;
/// Settled within this (Q32): 1/65536 of a turn, or of full.
const EPS: i64 = 1 << 16;
/// A level below this (Q32, 1/256 of full) is not shown, so a spot there
/// jumps to its new place rather than sliding in unseen.
const HIDDEN: i64 = 1 << 24;
/// A gap this long (ms) since the last render snaps the spots to their
/// targets: they have long settled.
const SNAP_MS: u64 = 2_000;

/// One spot's animated state, in Q32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    pos: i64,
    vel: i64,
    level: i64,
    lvel: i64,
    target_pos: i64,
    target_level: i64,
    color: Rgbw,
}

impl Slot {
    const OFF: Self = Self {
        pos: 0,
        vel: 0,
        level: 0,
        lvel: 0,
        target_pos: 0,
        target_level: 0,
        color: Rgbw::OFF,
    };

    fn is_moving(&self) -> bool {
        self.pos != self.target_pos
            || self.vel != 0
            || self.level != self.target_level
            || self.lvel != 0
    }

    fn snap(&mut self) {
        self.pos = self.target_pos;
        self.vel = 0;
        self.level = self.target_level;
        self.lvel = 0;
    }
}

/// `ω` for a follow time: a critically damped spring reaches 95% at `ωt =
/// 4.74`, so `ω = 4.74 / follow_ms` a millisecond, in Q32.
fn omega_for(follow_ms: u32) -> i64 {
    (474 * TURN) / (100 * i64::from(follow_ms.max(MIN_FOLLOW_MS)))
}

/// One millisecond of a critically damped spring on `x` toward `target`:
/// semi-implicit Euler, `v += -ω²(x - target) - 2ωv`, then `x += v`.
fn spring(x: &mut i64, v: &mut i64, target: i64, omega: i64) {
    let e = i128::from(*x - target);
    let w = i128::from(omega);
    let a = -((w * w * e) >> 64) - ((2 * w * i128::from(*v)) >> 32);
    *v += a as i64;
    *x += *v;
}

/// Lands `x` on `target` once it is within [`EPS`] and still.
fn settle(x: &mut i64, v: &mut i64, target: i64) {
    if (*x - target).abs() < EPS && v.abs() < EPS {
        *x = target;
        *v = 0;
    }
}

/// The circular distance between two positions (Q32 turns).
fn circular(a: i64, b: i64) -> i64 {
    let d = (a - b).rem_euclid(TURN);
    d.min(TURN - d)
}

/// `at` (thousandths of a turn, wrapped) in Q32.
fn turn_of(at: u16) -> i64 {
    (i64::from(at % 1000) << 32) / 1000
}

/// The position `at` (Q32) as the unwrapped one nearest `pos`, so a spring
/// moves the shorter way round.
fn unwrap_near(pos: i64, at: i64) -> i64 {
    let diff = (at - pos.rem_euclid(TURN)).rem_euclid(TURN);
    if diff > TURN / 2 {
        pos + diff - TURN
    } else {
        pos + diff
    }
}

/// The animated spots of a light: what the animator keeps while a look has a
/// spots block (fixed size, no allocation).
#[derive(Debug, Clone)]
pub(crate) struct SpotState {
    slots: [Slot; MAX_SPOTS],
    omega: i64,
    last_ms: u64,
    moving: bool,
}

impl SpotState {
    /// No spots, rendered from `now_ms`.
    pub(crate) fn new(now_ms: u64) -> Self {
        Self {
            slots: [Slot::OFF; MAX_SPOTS],
            omega: omega_for(DEFAULT_FOLLOW_MS),
            last_ms: now_ms,
            moving: false,
        }
    }

    /// Whether any spot is still moving or fading.
    pub(crate) fn moving(&self) -> bool {
        self.moving
    }

    /// Moves every spot to `now_ms`, one millisecond at a time.
    pub(crate) fn advance(&mut self, now_ms: u64) {
        let steps = now_ms.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now_ms);
        if steps == 0 || !self.moving {
            return;
        }
        if steps > SNAP_MS {
            for slot in &mut self.slots {
                slot.snap();
            }
        } else {
            for _ in 0..steps {
                for slot in &mut self.slots {
                    spring(&mut slot.pos, &mut slot.vel, slot.target_pos, self.omega);
                    spring(
                        &mut slot.level,
                        &mut slot.lvel,
                        slot.target_level,
                        self.omega,
                    );
                    settle(&mut slot.pos, &mut slot.vel, slot.target_pos);
                    settle(&mut slot.level, &mut slot.lvel, slot.target_level);
                    slot.level = slot.level.clamp(0, TURN);
                }
            }
        }
        self.moving = self.slots.iter().any(Slot::is_moving);
    }

    /// Sets the targets to `list` (at most `max` of them, in `color` unless
    /// a spot has its own) and follow `follow_ms`, from `now_ms`. Spots left
    /// over fade out where they are.
    pub(crate) fn retarget(
        &mut self,
        now_ms: u64,
        follow_ms: u32,
        color: Rgbw,
        max: u8,
        list: &SpotList,
    ) {
        self.advance(now_ms);
        self.omega = omega_for(follow_ms);
        let n = usize::from(list.len).min(usize::from(max)).min(MAX_SPOTS);
        let mut claimed = [false; MAX_SPOTS];
        for spot in list.spots.iter().take(n) {
            let at = turn_of(spot.at);
            // The nearest slot not yet claimed takes this spot.
            let mut best: Option<(i64, usize)> = None;
            for (s, slot) in self.slots.iter().enumerate() {
                if claimed[s] {
                    continue;
                }
                let d = circular(slot.pos, at);
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, s));
                }
            }
            let Some((_, s)) = best else { break };
            claimed[s] = true;
            let slot = &mut self.slots[s];
            slot.target_pos = unwrap_near(slot.pos, at);
            slot.target_level = (i64::from(spot.level.min(1000)) << 32) / 1000;
            slot.color = spot.color.unwrap_or(color);
            if slot.level < HIDDEN {
                slot.pos = slot.target_pos;
                slot.vel = 0;
            }
        }
        for (s, slot) in self.slots.iter_mut().enumerate() {
            if !claimed[s] {
                slot.target_pos = slot.pos;
                slot.target_level = 0;
            }
        }
        self.moving = self.slots.iter().any(Slot::is_moving);
    }

    /// LED `index` of `count`, with the spots' falloff `width` (Q16 LEDs):
    /// the brightest of the spots' contributions.
    pub(crate) fn pixel(&self, index: usize, count: usize, width: u32) -> Rgbw {
        let count = count.max(1) as i64;
        let span = count << 16;
        let support = i64::from(width / 2);
        let mut out = Rgbw::OFF;
        if support == 0 {
            return out;
        }
        for slot in &self.slots {
            if slot.level < HIDDEN {
                continue;
            }
            // The spot's place, in Q16 LEDs, and the LED's distance from it.
            let place = (slot.pos.rem_euclid(TURN) >> 16) * count;
            let d = ((index as i64) << 16) - place;
            let d = d.rem_euclid(span);
            let d = d.min(span - d);
            if d >= support {
                continue;
            }
            // Raised cosine: (1 + cos(πx)) / 2 at x = d / support.
            let x = (d << 16) / support;
            let c = cos_turn(x as u32 / 2);
            let falloff = ((65_536 + c) / 2) as u32;
            let level = (((slot.level >> 16) as u64 * u64::from(falloff)) >> 16) as u32;
            let lit = scale(slot.color, level);
            out = Rgbw::new(
                out.r.max(lit.r),
                out.g.max(lit.g),
                out.b.max(lit.b),
                out.w.max(lit.w),
            );
        }
        out
    }
}

/// The spots' width in Q16 LEDs.
pub(crate) fn width_q16(width: u16) -> u32 {
    q16(width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omega_settles_about_ninety_five_percent_in_follow_ms() {
        // The step response of a critically damped spring at t = follow:
        // 1 - (1 + ωt) e^{-ωt}, here at ωt = 4.74 (about 0.95).
        let mut x = 0i64;
        let mut v = 0i64;
        let target = TURN;
        let omega = omega_for(120);
        for _ in 0..120 {
            spring(&mut x, &mut v, target, omega);
        }
        let fraction = x * 100 / TURN;
        assert!((94..=96).contains(&fraction), "{fraction}");
    }

    #[test]
    fn a_critically_damped_spring_never_overshoots() {
        let mut x = 0i64;
        let mut v = 0i64;
        let target = TURN;
        let omega = omega_for(120);
        for _ in 0..2_000 {
            spring(&mut x, &mut v, target, omega);
            assert!(x <= target + EPS, "overshoot at {x}");
        }
    }

    #[test]
    fn the_nearest_slot_takes_a_spot_and_the_other_way_round_is_the_short_one() {
        let mut state = SpotState::new(0);
        let list = SpotList::new(&[Spot::new(990, 1000)]);
        state.retarget(0, 120, Rgbw::rgb(0xff0000), 8, &list);
        // Placed directly (the slot was hidden), at -0.01 turns: the
        // unwrapped target is the nearest, not 0.99 turns forward.
        assert_eq!(state.slots[0].pos, unwrap_near(0, turn_of(990)));
        assert!(state.slots[0].pos < 0);
        assert_eq!(state.slots[0].pos.rem_euclid(TURN), turn_of(990));
    }
}
