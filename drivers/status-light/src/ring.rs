//! Ring geometry: distances going round the ring, and the comet's brightness at each LED.
//!
//! Frames are in logical order, the order the pixels contract takes them. The physical map (which
//! LED is the top, and which way round the wiring runs) belongs to the pixels driver, so nothing
//! here knows the wiring.

use crate::easing::ONE;

/// `x^2.2` for `x = k/32` (Q16), the comet's tail curve.
const POW_2_2: [u32; 33] = [
    0, 32, 147, 359, 676, 1104, 1648, 2314, 3104, 4022, 5072, 6255, 7574, 9033, 10632, 12375,
    14263, 16298, 18482, 20817, 23303, 25944, 28740, 31692, 34803, 38073, 41504, 45097, 48854,
    52775, 56861, 61115, 65536,
];

/// Thousandths as Q16 (rounded).
pub(crate) fn q16(thousandths: u16) -> u32 {
    ((u32::from(thousandths) << 16) + 500) / 1000
}

/// `x^2.2` (both Q16), linear between table entries.
fn tail_curve(x: u32) -> u32 {
    let scaled = x.min(ONE) * 32;
    let index = (scaled >> 16) as usize;
    if index >= 32 {
        return ONE;
    }
    let frac = u64::from(scaled & 0xffff);
    let (a, b) = (POW_2_2[index], POW_2_2[index + 1]);
    a + (((u64::from(b - a)) * frac) >> 16) as u32
}

/// The comet's brightness at LED `index` of `count` (Q16), `elapsed_ms` into the look. For each
/// head, `(1 - d/tail)^2.2` where `d` is how far behind the head the LED is (going round); the
/// brightest head wins, but never below `base`. The LED just ahead of a head is lit by the part of
/// the way the head has got to it, so the head steps smoothly from one LED to the next.
pub(crate) fn comet_level(
    index: usize,
    count: usize,
    elapsed_ms: u64,
    period_ms: u32,
    tail: u32,
    heads: u8,
    base: u32,
) -> u32 {
    let period = u64::from(period_ms.max(1));
    // This turn's position in 1/65536 of a turn.
    let phase = ((elapsed_ms % period) << 16) / period;
    let heads = u64::from(heads.clamp(1, 2));
    let span = (count as u64) << 16;
    let at = (index as u64) << 16;
    let tail = u64::from(tail.max(1));
    let mut best = 0;
    for h in 0..heads {
        let turn = (phase + h * u64::from(ONE) / heads) % u64::from(ONE);
        // A head's position in 1/65536 of an LED.
        let head = turn * count as u64;
        let behind = (head + span - at) % span.max(1);
        if behind < tail {
            let x = ONE - ((behind << 16) / tail) as u32;
            best = best.max(tail_curve(x));
        } else {
            // How far ahead of the head the LED is (in 1/65536 of an LED).
            let ahead = (span - behind) % span.max(1);
            if ahead > 0 && ahead < u64::from(ONE) {
                best = best.max(ONE - ahead as u32);
            }
        }
    }
    best.max(base)
}
