//! Sparkle: LEDs twinkle at random. A [`Sparkle`] is the block's parameters
//! (plain data, in a look); a [`SparkleState`] is what the animator keeps
//! while it shows one: a PRNG and the twinkles, all fixed size (no
//! allocation). The state is reset when the look changes, so a look start is
//! reproducible from its seed.
//!
//! A twinkle lasts a random time in `[min_ms, max_ms]`. Its brightness rises
//! linearly over the first fifth and decays as `(1 - x)^2` over the rest. An
//! LED that is not mid-twinkle starts one with probability `density * dt`
//! each render, where `dt` is the time since the last render, so the rate
//! does not depend on the frame rate.
//!
//! With `fall`, a twinkle is a particle instead: it spawns near the top of
//! the ring (the LED opposite `bottom`, see [`Animator::set_bottom`]), slides
//! toward the bottom by the shorter arc, accelerating, and fades out when it
//! lands. Positions are fractional, drawn across the two neighbouring LEDs.

use crate::easing::ONE;
use crate::look::{MAX_FRAME_LEDS, scale};
use lemnos_device::Rgbw;

/// Colours a sparkle chooses from (at most).
pub const MAX_SPARKLE_COLORS: usize = 4;

/// Falling particles at once (at most).
pub const MAX_PARTICLES: usize = 16;

/// The longest a twinkle may run (it must fit the 16-bit stored length).
pub const MAX_TWINKLE_MS: u32 = 60_000;

/// Rates (thousandths of a twinkle per LED a second) are capped here.
pub const MAX_DENSITY: u32 = 64_000;

/// Speeds and accelerations (thousandths of an LED a second, and of an LED a
/// second squared) are capped here.
pub const MAX_MOTION: u32 = 64_000;

/// How long a falling particle takes to fade once it lands.
const LAND_FADE_MS: u64 = 200;

/// A gap this long between two renders (the look was not shown) clears every
/// twinkle, so a stale one cannot look live again.
const RESET_GAP_MS: u64 = 1_000;

/// Sparkle parameters. `density` and `density_end` are in thousandths of a
/// twinkle per LED a second; `fade_ms` ramps the rate from `density` to
/// `density_end` over `fade_ms` from `start_ms`, and fades the `base` to 0
/// by `start_ms + fade_ms`. `base` (thousandths) is a floor in `colors[0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sparkle {
    /// The colours twinkles pick from (`count` of them, 1 to 4).
    pub colors: [Rgbw; MAX_SPARKLE_COLORS],
    pub count: u8,
    pub density: u32,
    pub density_end: u32,
    pub fade_ms: u32,
    pub start_ms: u32,
    pub min_ms: u32,
    pub max_ms: u32,
    pub base: u16,
    /// The PRNG seed; 0 takes the look's start time.
    pub seed: u32,
    /// Twinkles fall (see the module docs) instead of staying on their LED.
    pub fall: bool,
    /// Falling speed at the start, thousandths of an LED a second.
    pub fall_speed: u32,
    /// Falling acceleration, thousandths of an LED a second squared.
    pub fall_accel: u32,
}

impl Sparkle {
    /// The defaults: 1.2 twinkles a LED a second, 350 to 950 ms, no base, no
    /// fall (5 and 6 LEDs a second and squared when it falls).
    pub const DEFAULT: Self = Self {
        colors: [Rgbw::OFF; MAX_SPARKLE_COLORS],
        count: 1,
        density: 1_200,
        density_end: 1_200,
        fade_ms: 0,
        start_ms: 0,
        min_ms: 350,
        max_ms: 950,
        base: 0,
        seed: 0,
        fall: false,
        fall_speed: 5_000,
        fall_accel: 6_000,
    };

    /// Twinkles in one colour.
    pub const fn one(color: Rgbw) -> Self {
        Self {
            colors: [color, Rgbw::OFF, Rgbw::OFF, Rgbw::OFF],
            count: 1,
            ..Self::DEFAULT
        }
    }

    /// Twinkles in `count` of `colors` (at most four), chosen at random.
    pub const fn palette(colors: [Rgbw; MAX_SPARKLE_COLORS], count: u8) -> Self {
        Self {
            colors,
            count,
            ..Self::DEFAULT
        }
    }

    /// The rate (thousandths of a twinkle a LED a second) `elapsed_ms` after
    /// the look started: none before `start_ms`, then the ramp.
    fn density_at(&self, elapsed_ms: u64) -> u64 {
        let start = u64::from(self.start_ms);
        if elapsed_ms < start {
            return 0;
        }
        let fade = u64::from(self.fade_ms);
        if fade == 0 {
            return u64::from(self.density);
        }
        let t = (elapsed_ms - start).min(fade);
        let (from, to) = (i64::from(self.density), i64::from(self.density_end));
        (from + (to - from) * t as i64 / fade as i64) as u64
    }

    /// The base's level (Q16) `elapsed_ms` after the look started.
    fn base_level(&self, elapsed_ms: u64) -> u64 {
        let fade = u64::from(self.fade_ms);
        if fade == 0 {
            return u64::from(ONE);
        }
        let end = u64::from(self.start_ms) + fade;
        if elapsed_ms >= end {
            0
        } else {
            u64::from(ONE) - ((elapsed_ms << 16) / end)
        }
    }
}

/// A falling twinkle: where it spawned, which way it goes and how far to the
/// bottom, and when it is over.
#[derive(Debug, Clone, Copy, Default)]
struct Particle {
    /// Low 16 bits of the spawn time, its twinkle length, and the length of
    /// its life (the twinkle, or the landing fade, whichever ends first).
    start: u16,
    len: u16,
    life: u16,
    /// The spawn position (thousandths of an LED, mod the ring).
    spawn: u16,
    /// Distance to the bottom along the shorter arc, and when it lands (ms).
    distance: u16,
    land: u16,
    /// +1 or -1 (the direction of the shorter arc).
    dir: i8,
    pick: u8,
    live: bool,
}

/// What the animator keeps for a sparkle while it shows: the PRNG, and the
/// twinkles (per LED, or the falling particles).
#[derive(Debug, Clone)]
pub(crate) struct SparkleState {
    since_ms: u64,
    last_ms: u64,
    rng: u32,
    /// LEDs in the ring, and the bottom's position (thousandths of an LED).
    count: u32,
    bottom: u32,
    /// Per LED: the low 16 bits of the twinkle's start, its length (0: none),
    /// and the colour picked.
    start: [u16; MAX_FRAME_LEDS],
    len: [u16; MAX_FRAME_LEDS],
    pick: [u8; MAX_FRAME_LEDS],
    particles: [Particle; MAX_PARTICLES],
}

impl SparkleState {
    /// A fresh state for a look that starts at `now_ms`.
    pub(crate) fn new(now_ms: u64, sparkle: &Sparkle) -> Self {
        let seed = if sparkle.seed != 0 {
            sparkle.seed
        } else {
            (now_ms as u32) ^ 0x9e37_79b9
        };
        Self {
            since_ms: now_ms,
            last_ms: now_ms,
            rng: if seed == 0 { 0x9e37_79b9 } else { seed },
            count: 0,
            bottom: 0,
            start: [0; MAX_FRAME_LEDS],
            len: [0; MAX_FRAME_LEDS],
            pick: [0; MAX_FRAME_LEDS],
            particles: [Particle::default(); MAX_PARTICLES],
        }
    }

    fn next_random(&mut self) -> u32 {
        // xorshift32.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Whether LED `index`'s twinkle is running at `now16` (the low 16 bits).
    fn running(&self, index: usize, now16: u16) -> bool {
        let len = self.len[index];
        len != 0 && now16.wrapping_sub(self.start[index]) < len
    }

    /// Ends the twinkles that are over, and starts new ones for `count` LEDs
    /// at `now_ms`, the ring's `bottom` at `bottom` (thousandths of an LED).
    /// Called once per render.
    pub(crate) fn advance(&mut self, now_ms: u64, sparkle: &Sparkle, count: usize, bottom: u32) {
        let count = count.min(MAX_FRAME_LEDS);
        let dt = now_ms.saturating_sub(self.last_ms);
        if dt > RESET_GAP_MS {
            self.len = [0; MAX_FRAME_LEDS];
            self.particles = [Particle::default(); MAX_PARTICLES];
        }
        self.last_ms = now_ms;
        self.count = count as u32;
        self.bottom = bottom % (self.count.max(1) * 1_000);
        let now16 = now_ms as u16;
        for index in 0..count {
            if self.len[index] != 0 && !self.running(index, now16) {
                self.len[index] = 0;
            }
        }
        for p in self.particles.iter_mut() {
            if p.live && now16.wrapping_sub(p.start) >= p.life {
                p.live = false;
            }
        }
        let elapsed = now_ms.saturating_sub(self.since_ms);
        let density = sparkle.density_at(elapsed);
        if dt == 0 || density == 0 {
            return;
        }
        // Out of a million: the chance that an idle LED starts a twinkle now.
        let chance = density * dt;
        let colors = u32::from(sparkle.count.clamp(1, MAX_SPARKLE_COLORS as u8));
        let span = sparkle
            .max_ms
            .saturating_sub(sparkle.min_ms)
            .min(MAX_TWINKLE_MS)
            + 1;
        for index in 0..count {
            let roll = self.next_random() % 1_000_000;
            if u64::from(roll) >= chance {
                continue;
            }
            if sparkle.fall {
                self.spawn_particle(now_ms, sparkle, colors, span);
            } else if self.len[index] == 0 {
                let len = sparkle.min_ms.max(1) + self.next_random() % span;
                self.len[index] = len.min(u32::from(u16::MAX)) as u16;
                self.start[index] = now16;
                self.pick[index] = (self.next_random() % colors) as u8;
            }
        }
    }

    /// Spawns a falling twinkle near the top, if a slot is free.
    fn spawn_particle(&mut self, now_ms: u64, sparkle: &Sparkle, colors: u32, span: u32) {
        let Some(slot) = self.particles.iter().position(|p| !p.live) else {
            return;
        };
        let ring = self.count.max(1) * 1_000;
        let top = (self.bottom + self.count.max(1) * 500) % ring;
        // Within three LEDs of the top, either side.
        let spread = self.next_random() % 6_001;
        let spawn = (top + ring + spread).saturating_sub(3_000) % ring;
        // The shorter arc from the spawn to the bottom.
        let to_bottom = (self.bottom + ring - spawn) % ring;
        let (dir, distance) = if to_bottom <= ring / 2 {
            (1, to_bottom)
        } else {
            (-1, ring - to_bottom)
        };
        let land = landing_ms(distance, sparkle.fall_speed, sparkle.fall_accel);
        let len = (sparkle.min_ms.max(1) + self.next_random() % span).min(MAX_TWINKLE_MS);
        let life = u64::from(len).min(land.saturating_add(LAND_FADE_MS));
        self.particles[slot] = Particle {
            start: now_ms as u16,
            len: len as u16,
            life: life.min(u64::from(u16::MAX)) as u16,
            spawn: spawn as u16,
            distance: distance as u16,
            land: land.min(u64::from(u16::MAX)) as u16,
            dir,
            pick: (self.next_random() % colors) as u8,
            live: true,
        };
    }

    /// LED `index`'s colour at `now_ms`, before the layer's brightness: the
    /// base, or the twinkle (or the falling particles that reach it) if
    /// brighter.
    pub(crate) fn pixel(&self, index: usize, now_ms: u64, sparkle: &Sparkle) -> Rgbw {
        let elapsed = now_ms.saturating_sub(self.since_ms);
        let base_q16 = u64::from(sparkle.base.min(1000)) * u64::from(ONE) / 1000;
        let base_level = (base_q16 * sparkle.base_level(elapsed)) >> 16;
        let base = scale(sparkle.colors[0], base_level as u32);
        let now16 = now_ms as u16;
        let mut out = base;
        if sparkle.fall {
            for p in self.particles.iter().filter(|p| p.live) {
                out = max(out, self.particle_pixel(index, now16, p, sparkle));
            }
        } else if index < MAX_FRAME_LEDS && self.running(index, now16) {
            let age = u64::from(now16.wrapping_sub(self.start[index]));
            let level = twinkle_level(age, u64::from(self.len[index]));
            let twinkle = scale(
                sparkle.colors[usize::from(self.pick[index]).min(MAX_SPARKLE_COLORS - 1)],
                level,
            );
            out = max(out, twinkle);
        }
        out
    }

    /// What falling particle `p` lights LED `index` with: its position is
    /// fractional, shared by the two LEDs either side.
    fn particle_pixel(&self, index: usize, now16: u16, p: &Particle, sparkle: &Sparkle) -> Rgbw {
        let age = u64::from(now16.wrapping_sub(p.start));
        let ring = i64::from(self.count.max(1)) * 1_000;
        let travelled =
            travel_milli(age, sparkle.fall_speed, sparkle.fall_accel).min(u64::from(p.distance));
        let at = (i64::from(p.spawn) + i64::from(p.dir) * travelled as i64).rem_euclid(ring);
        let (led, frac) = ((at / 1_000) as usize, (at % 1_000) as u32);
        let count = self.count.max(1) as usize;
        let mut level = twinkle_level(age, u64::from(p.len));
        let land = u64::from(p.land);
        if age >= land {
            let after = (age - land).min(LAND_FADE_MS);
            level = (u64::from(level) * (LAND_FADE_MS - after) / LAND_FADE_MS) as u32;
        }
        let colour = sparkle.colors[usize::from(p.pick).min(MAX_SPARKLE_COLORS - 1)];
        let weight = if index == led % count {
            1000 - frac
        } else if index == (led + 1) % count {
            frac
        } else {
            return Rgbw::OFF;
        };
        scale(colour, (u64::from(level) * u64::from(weight) / 1000) as u32)
    }
}

/// The distance a falling twinkle has gone `age_ms` after it spawned, in
/// thousandths of an LED, at `speed` (thousandths a second) and `accel`.
fn travel_milli(age_ms: u64, speed: u32, accel: u32) -> u64 {
    let speed = u64::from(speed.min(MAX_MOTION));
    let accel = u64::from(accel.min(MAX_MOTION));
    (speed * age_ms) / 1_000 + (accel * age_ms * age_ms) / 2_000_000
}

/// When a falling twinkle covering `distance` (thousandths of an LED) lands,
/// in ms after it spawns (`u64::MAX` if it never does).
fn landing_ms(distance: u32, speed: u32, accel: u32) -> u64 {
    let (v, a, d) = (
        u64::from(speed.min(MAX_MOTION)),
        u64::from(accel.min(MAX_MOTION)),
        u64::from(distance),
    );
    if d == 0 {
        return 0;
    }
    if a == 0 {
        return (1_000 * d).checked_div(v).unwrap_or(u64::MAX);
    }
    // From d = v t + a t^2 / 2 (thousandths and seconds): t = (sqrt(v^2 + 2 a d) - v) / a.
    let root = isqrt(v * v + 2 * a * d);
    1_000 * root.saturating_sub(v) / a
}

/// The integer square root (floor).
fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// The brightness of a twinkle `age` ms into `len` (Q16): a linear rise over
/// the first fifth, then `(1 - x)^2` to 0 at the end.
fn twinkle_level(age: u64, len: u64) -> u32 {
    let len = len.max(1);
    let x = ((age.min(len) << 16) / len) as u32;
    // The rise ends at a fifth of the way (0.2 in Q16).
    const RISE: u32 = ONE / 5;
    if x < RISE {
        ((u64::from(x) << 16) / u64::from(RISE)) as u32
    } else {
        let rest = u64::from(ONE - x);
        let q = (rest * u64::from(ONE) / u64::from(ONE - RISE)).min(u64::from(ONE));
        ((q * q) >> 16) as u32
    }
}

fn max(a: Rgbw, b: Rgbw) -> Rgbw {
    Rgbw::new(a.r.max(b.r), a.g.max(b.g), a.b.max(b.b), a.w.max(b.w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twinkle_rises_then_decays_as_a_square() {
        let len = 1000;
        assert_eq!(twinkle_level(0, len), 0);
        // Halfway up the rise (x = 0.1) is half the peak.
        let half = twinkle_level(100, len);
        assert!((i64::from(half) - i64::from(ONE / 2)).abs() < 200, "{half}");
        // The peak at a fifth.
        assert!(i64::from(twinkle_level(200, len)) - i64::from(ONE) > -200);
        // Halfway down the decay (x = 0.6): (1 - 0.5)^2 = 0.25.
        let mid = twinkle_level(600, len);
        assert!((i64::from(mid) - i64::from(ONE / 4)).abs() < 400, "{mid}");
        assert_eq!(twinkle_level(1000, len), 0);
    }

    #[test]
    fn a_fall_from_eight_leds_at_five_accelerating_lands_in_a_second() {
        // d = 5 t + 3 t^2 = 8 at t = 1 s.
        let land = landing_ms(8_000, 5_000, 6_000);
        assert!((i64::try_from(land).unwrap() - 1_000).abs() <= 1, "{land}");
        assert_eq!(travel_milli(1_000, 5_000, 6_000), 8_000);
        assert_eq!(isqrt(121), 11);
        assert_eq!(isqrt(120), 10);
    }

    #[test]
    fn the_same_seed_gives_the_same_twinkles() {
        let p = Sparkle {
            density: 20_000,
            seed: 7,
            ..Sparkle::one(Rgbw::rgb(0x00ff20))
        };
        let run = || {
            let mut st = SparkleState::new(0, &p);
            let mut out = [0u32; 16];
            let mut seen = [0u32; 16];
            for t in (0..3_000).step_by(20) {
                st.advance(t, &p, 16, 8_000);
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = st.pixel(i, t, &p).g as u32;
                    seen[i] += *slot;
                }
            }
            seen
        };
        assert_eq!(run(), run());
        let (a, b) = {
            let q = Sparkle { seed: 8, ..p };
            let mut st = SparkleState::new(0, &q);
            let mut seen = [0u32; 16];
            for t in (0..3_000).step_by(20) {
                st.advance(t, &q, 16, 8_000);
                for (i, slot) in seen.iter_mut().enumerate() {
                    *slot += st.pixel(i, t, &q).g as u32;
                }
            }
            (seen, run())
        };
        assert_ne!(a, b, "another seed gives other twinkles");
    }

    #[test]
    fn each_led_starts_at_the_density_over_time() {
        // 1 twinkle a LED a second, each 100 ms: a cycle of about 1.1 s, so
        // about 545 starts over 600 s for 16 LEDs at 1 a second: 8727 in all.
        let p = Sparkle {
            density: 1_000,
            min_ms: 100,
            max_ms: 100,
            ..Sparkle::one(Rgbw::rgb(0xffffff))
        };
        let mut st = SparkleState::new(0, &p);
        let mut starts = 0u32;
        let mut prev = [0u16; MAX_FRAME_LEDS];
        for t in (0..600_000u64).step_by(10) {
            st.advance(t, &p, 16, 8_000);
            for (i, last) in prev.iter_mut().enumerate().take(16) {
                if st.len[i] != 0 && *last != st.start[i] {
                    starts += 1;
                }
                *last = st.start[i];
            }
        }
        assert!((8_200..=9_200).contains(&starts), "starts {starts}");
    }

    #[test]
    fn the_average_brightness_does_not_depend_on_the_frame_rate() {
        let p = Sparkle {
            density: 2_000,
            base: 40,
            ..Sparkle::one(Rgbw::rgb(0x00ff20))
        };
        let average = |step: u64| {
            let mut st = SparkleState::new(0, &p);
            let (mut sum, mut n) = (0u64, 0u64);
            let mut t = 0;
            while t < 60_000 {
                st.advance(t, &p, 16, 8_000);
                for i in 0..16 {
                    sum += u64::from(st.pixel(i, t, &p).g);
                }
                n += 16;
                t += step;
            }
            sum / n
        };
        let (fine, coarse) = (average(10), average(20));
        assert!(
            fine.abs_diff(coarse) * 100 <= fine * 8,
            "{fine} vs {coarse}"
        );
    }

    #[test]
    fn a_twinkle_rises_to_a_fifth_of_its_length_then_decays() {
        let p = Sparkle {
            density: 64_000,
            min_ms: 1_000,
            max_ms: 1_000,
            ..Sparkle::one(Rgbw::rgb(0xffffff))
        };
        let mut st = SparkleState::new(0, &p);
        // Start one twinkle on LED 0 (the rolls are random: run until it does).
        let mut t = 0;
        while st.len[0] == 0 {
            st.advance(t, &p, 1, 0);
            t += 1;
        }
        let start = u64::from(st.start[0]);
        let level = |age: u64| u32::from(st.pixel(0, start + age, &p).r);
        assert!(level(0) < 5);
        assert!(level(100) > level(50) && level(200) > level(100));
        assert!(level(200) >= level(199));
        assert!(level(400) < level(200));
        assert!(level(500) > level(700));
        assert!(level(950) < 8);
    }

    #[test]
    fn falling_twinkles_take_the_shorter_arc_to_the_bottom() {
        // Bottom at LED 4 of 16: top at LED 12. Spawns on either side of the
        // top go the way round that is shorter.
        let p = Sparkle {
            density: 64_000,
            fall: true,
            min_ms: 2_000,
            max_ms: 2_000,
            ..Sparkle::one(Rgbw::rgb(0x00ff20))
        };
        let bottom = 4_000;
        let mut st = SparkleState::new(0, &p);
        let mut seen_forward = false;
        let mut seen_back = false;
        for t in 0..400u64 {
            st.advance(t, &p, 16, bottom);
        }
        for q in st.particles.iter().filter(|q| q.live) {
            // The spawn is within three LEDs of the top (LED 12).
            let from_top = (i64::from(q.spawn) - 12_000 + 16_000) % 16_000;
            let from_top = if from_top > 8_000 {
                from_top - 16_000
            } else {
                from_top
            };
            assert!(from_top.abs() <= 3_000, "{from_top}");
            // Towards the bottom the short way: at spawn, dir * distance is the
            // signed shortest step from spawn to the bottom.
            let delta = (i64::from(bottom) - i64::from(q.spawn) + 16_000) % 16_000;
            let short = if delta <= 8_000 {
                delta
            } else {
                delta - 16_000
            };
            assert_eq!(i64::from(q.dir) * i64::from(q.distance), short);
            if q.dir > 0 {
                seen_forward = true;
            } else {
                seen_back = true;
            }
        }
        assert!(seen_forward && seen_back, "both arcs are used");
    }

    #[test]
    fn a_falling_twinkle_lands_on_the_bottom_at_the_computed_time() {
        let p = Sparkle {
            fall: true,
            ..Sparkle::one(Rgbw::rgb(0x00ff20))
        };
        let land = landing_ms(8_000, p.fall_speed, p.fall_accel);
        // At landing it has covered its distance.
        assert_eq!(
            travel_milli(land, p.fall_speed, p.fall_accel).min(8_000),
            8_000
        );
        // Before, it has not.
        assert!(travel_milli(land - 50, p.fall_speed, p.fall_accel) < 8_000);
        // Speed grows: the second half of the fall is quicker than the first.
        let half = landing_ms(4_000, p.fall_speed, p.fall_accel);
        assert!(land - half < half, "{land} {half}");
    }

    #[test]
    fn falling_twinkles_move_steadily_toward_the_bottom_on_either_arc() {
        // The distance to the bottom (the shorter arc, in thousandths) falls
        // every ms a particle is in the air, for a bottom on either side.
        for bottom in [4_000u32, 12_000] {
            let p = Sparkle {
                density: 64_000,
                fall: true,
                min_ms: 3_000,
                max_ms: 3_000,
                ..Sparkle::one(Rgbw::rgb(0x00ff20))
            };
            let mut st = SparkleState::new(0, &p);
            let ring = 16_000i64;
            let shorter = |at: i64| {
                let d = (i64::from(bottom) - at).rem_euclid(ring);
                d.min(ring - d)
            };
            for t in 0..30u64 {
                st.advance(t, &p, 16, bottom);
            }
            let mut checked = 0;
            for q in st
                .particles
                .iter()
                .filter(|q| q.live && u32::from(q.land) > 300)
            {
                let spawn = i64::from(q.spawn);
                let mut last = i64::MAX;
                for age in 0..=u64::from(q.land) {
                    let travelled = travel_milli(age, p.fall_speed, p.fall_accel)
                        .min(u64::from(q.distance)) as i64;
                    let at = (spawn + i64::from(q.dir) * travelled).rem_euclid(ring);
                    let d = shorter(at);
                    assert!(d <= last, "bottom {bottom}, age {age}: {d} after {last}");
                    last = d;
                }
                // Landed on the bottom, to within the rounding of a whole ms (0.02 LED).
                assert!(last <= 20, "bottom {bottom}: landed {last} from it");
                checked += 1;
            }
            assert!(checked > 0, "some particles fell (bottom {bottom})");
        }
    }
}
