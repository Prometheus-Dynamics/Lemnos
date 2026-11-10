//! Lines against Bresenham: the reference itself on the host, and the GPU's lines on a board
//! (ignored by default; run with `--ignored`, `LEMNOS_VGLITE_NODE` naming the node, default
//! `/dev/vg_lite`).
//!
//! The reference is the single-error-term "all octants" Bresenham of the line operation's owner
//! (the caller's reference for its one-pixel line), copied here so this crate does not depend on
//! it. Carried from the dev branch's `tests/lines.rs`. Its variant, which matters for ties:
//! - one error term, `err = dx + dy` with `dx = |to.x - x|` and `dy = -|to.y - y|`;
//! - each step is decided by two inclusive comparisons on `e2 = 2 * err`: `e2 >= dy` steps x,
//!   and `e2 <= dx` steps y, so a tie steps both (a diagonal step);
//! - the walk starts at `from` and ends at `to`, both included; the set is the walk's pixels,
//!   so `from -> to` and `to -> from` can differ (checked by [`reference_is_not_symmetric`]).
//!
//! The device test does not assume the GPU matches it. It reports, per slope class, how many
//! lines match exactly; axes and 45 degrees are asserted, the rest are printed for a board run.
#![cfg(target_os = "linux")]
#![expect(
    clippy::print_stderr,
    reason = "the board check reports the mismatch counts for the board run's record"
)]

use std::collections::BTreeSet;

use lemnos_driver_vglite::{Format, Image, Vglite};

/// The reference: the pixels of the walk from `from` to `to`, inside a `frame` (width, height).
fn bresenham(from: (i32, i32), to: (i32, i32), frame: (i32, i32)) -> Vec<(i32, i32)> {
    let (mut x, mut y) = from;
    let (dx, dy) = ((to.0 - x).abs(), -(to.1 - y).abs());
    let (sx, sy) = (if x < to.0 { 1 } else { -1 }, if y < to.1 { 1 } else { -1 });
    let mut err = dx + dy;
    let mut out = Vec::new();
    loop {
        if x >= 0 && y >= 0 && x < frame.0 && y < frame.1 {
            out.push((x, y));
        }
        if (x, y) == to {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    out
}

fn set(v: &[(i32, i32)]) -> BTreeSet<(i32, i32)> {
    v.iter().copied().collect()
}

#[test]
fn reference_gives_the_textbook_pixels_of_hand_checked_lines() {
    let big = (100, 100);
    // Horizontal: both ends included.
    assert_eq!(
        bresenham((0, 0), (4, 0), big),
        [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0)]
    );
    // Exact diagonal.
    assert_eq!(
        bresenham((0, 0), (3, 3), big),
        [(0, 0), (1, 1), (2, 2), (3, 3)]
    );
    // Tie: from (0,0) to (2,1) the midpoint (1, 0.5) is exactly between two pixels; this variant
    // steps both x and y, so the walk goes through (1,1).
    assert_eq!(bresenham((0, 0), (2, 1), big), [(0, 0), (1, 1), (2, 1)]);
    // Slope 1/2 forward: the first step is a tie again.
    assert_eq!(
        bresenham((0, 0), (4, 2), big),
        [(0, 0), (1, 1), (2, 1), (3, 2), (4, 2)]
    );
    // The same segment backwards is a different pixel set (x = 1 and x = 3 differ).
    assert_eq!(
        bresenham((4, 2), (0, 0), big),
        [(4, 2), (3, 1), (2, 1), (1, 0), (0, 0)]
    );
    // Clipped to the frame: only the pixels inside are kept, the walk is unchanged.
    assert_eq!(bresenham((-2, 0), (2, 0), (3, 1)), [(0, 0), (1, 0), (2, 0)]);
}

#[test]
fn reference_is_not_symmetric_for_some_lines() {
    assert_ne!(
        set(&bresenham((0, 0), (4, 2), (10, 10))),
        set(&bresenham((4, 2), (0, 0), (10, 10)))
    );
}

#[derive(Default)]
struct Class {
    draws: u64,
    exact: u64,
    gpu_differs_from_ref: u64,
    ref_differs_by_direction: u64,
}

fn class_of(from: (i32, i32), to: (i32, i32)) -> usize {
    let (dx, dy) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
    if dx == 0 || dy == 0 {
        0 // axis
    } else if dx == dy {
        1 // 45 degrees
    } else {
        2 // any other slope
    }
}

const NAMES: [&str; 3] = ["axis", "45 degrees", "other slopes"];

/// Every line from the frame's centre to a point on the ring of Chebyshev radius `1..=64` (all
/// eight octants, the axes and 45 degrees, endpoints on the frame edges at radius 64), drawn in
/// both directions on the GPU and compared with the reference walked in the same direction.
#[test]
#[ignore = "needs the GPU: a board run"]
fn gpu_lines_against_bresenham_by_slope_class() {
    let node = std::env::var("LEMNOS_VGLITE_NODE").unwrap_or_else(|_| "/dev/vg_lite".into());
    let mut gpu = Vglite::new(node);
    const SIDE: u32 = 129;
    const C: i32 = 64;
    let image = Image::packed(Format::Rgba8888, SIDE, SIDE);
    let mut buf = vec![0u8; image.frame_len()];
    let mut classes: [Class; 3] = Default::default();
    let mut first_bad: Option<String> = None;

    for radius in 1..=C {
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                if dx.abs().max(dy.abs()) != radius {
                    continue;
                }
                let a = (C, C);
                let b = (C + dx, C + dy);
                let class = class_of(a, b);
                let forward = bresenham(a, b, (SIDE as i32, SIDE as i32));
                let backward = bresenham(b, a, (SIDE as i32, SIDE as i32));
                if set(&forward) != set(&backward) {
                    classes[class].ref_differs_by_direction += 1;
                }
                for (from, to, want) in [(a, b, &forward), (b, a, &backward)] {
                    buf.fill(0);
                    gpu.line_bytes(0, image, &mut buf, from, to, [255; 4])
                        .expect("a line draws");
                    let got: BTreeSet<(i32, i32)> = buf
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .enumerate()
                        .filter(|(_, px)| **px != [0; 4])
                        .map(|(i, _)| ((i as u32 % SIDE) as i32, (i as u32 / SIDE) as i32))
                        .collect();
                    let c = &mut classes[class];
                    c.draws += 1;
                    if got == set(want) {
                        c.exact += 1;
                    } else {
                        c.gpu_differs_from_ref += 1;
                        first_bad.get_or_insert_with(|| format!("{from:?} -> {to:?}"));
                    }
                }
            }
        }
    }

    for (name, c) in NAMES.iter().zip(&classes) {
        eprintln!(
            "{name}: {} draws, {} exact, {} differ from Bresenham; {} lines whose reference differs by direction",
            c.draws, c.exact, c.gpu_differs_from_ref, c.ref_differs_by_direction
        );
    }
    // Axes and 45 degrees must match exactly; other slopes are reported only.
    for c in &classes[..2] {
        assert_eq!(c.gpu_differs_from_ref, 0, "first mismatch: {first_bad:?}");
    }
}
