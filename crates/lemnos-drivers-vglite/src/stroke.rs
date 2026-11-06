//! The vendor source is MIT licensed by Vivante; the notice the vendor ships with
//! `vg_lite_stroke.c` (the same notice as in `command.rs`):
//! ```text
//! Copyright 2012 - 2022 Vivante Corporation, Santa Clara, California.
//! All Rights Reserved.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this
//! software and associated documentation files (the 'Software'), to deal in the Software
//! without restriction, including without limitation the rights to use, copy, modify, merge,
//! publish, distribute, sub license, and/or sell copies of the Software, and to permit persons
//! to whom the Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice (including the next paragraph) shall
//! be included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED 'AS IS', WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
//! INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
//! PURPOSE AND NON-INFRINGEMENT. IN NO EVENT SHALL VIVANTE AND/OR ITS SUPPLIERS BE LIABLE FOR
//! ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
//! ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//! ```
//!
//! Stroke outline: a port of the VGLite vendor's stroke conversion
//! (`vg_lite_stroke.c`, `_flatten_path` through `_copy_stroke_path`) for
//! straight-line subpaths.
//!
//! The input is a MOVE, then a LINE for each further point, then END. Whether
//! the vendor closes the subpath depends on `add_end` (see [`Subpath`]). With
//! `add_end` 0 (the default after `vg_lite_init_path`) END joins the last point
//! back to the first, so the outline is that of the closed polygon and no caps
//! are drawn. With `add_end` 1 the subpath is open and gets start and end caps.
//! A cap also matters when the subpath collapses to one point.
//!
//! Not ported: curves and arcs, dashes, ROUND caps and joins (refused as
//! `Unsupported`), and the swing (pie-area) handling. Swing is gated on
//! `need_to_handle_swing`, which is cleared for straight input with no
//! flattened points (BUTT caps included, open or closed), so `swing_handling`
//! stays SWING_NO.
#![cfg_attr(not(test), allow(dead_code))]

use std::io;

/// `FLOAT_EPSILON` (vg_lite_stroke.c:45): the coincident-point test in `_add_point_to_point_list`.
const FLOAT_EPSILON: f32 = 0.001;
/// `FLOAT_ANGLE_EPSILON_COS` (vg_lite_stroke.c:70): a joint whose cosine is beyond this is straight, or a reversal.
const FLOAT_ANGLE_EPSILON_COS: f32 = 0.99999;

/// VLC_OP_END, VLC_OP_MOVE and VLC_OP_LINE (vg_lite.h).
const VLC_OP_END: u8 = 0x00;
const VLC_OP_MOVE: u8 = 0x02;
const VLC_OP_LINE: u8 = 0x04;

/// `vg_lite_cap_style_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cap {
    Butt,
    Round,
    Square,
}

/// `vg_lite_join_style_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Join {
    Miter,
    Round,
    Bevel,
}

/// Whether the subpath is closed, which the vendor chooses by `path->add_end`.
/// `Closed` is add_end 0 (END closes the subpath, no caps). `Open` is add_end 1
/// (start and end caps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Subpath {
    Open,
    Closed,
}

/// The fields of `vg_lite_path_point` that the straight-line case uses: the
/// point, and the length and unit tangent of the segment that starts at it.
/// The last point of a subpath keeps a zero tangent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Node {
    x: f32,
    y: f32,
    length: f32,
    tangent_x: f32,
    tangent_y: f32,
}

/// The stroke outline of `points` as `vg_lite_update_stroke` writes it into
/// `path->stroke_path`: MOVE, LINE, ..., END, each command a 4-byte header
/// (opcode and three zero bytes) followed by little-endian f32 coordinates.
///
/// `width` is the full line width (`half_width = width / 2`), and
/// `miter_limit` is clamped to at least 1, as `vg_lite_set_stroke` does.
pub(crate) fn outline(
    points: &[(f32, f32)],
    width: f32,
    cap: Cap,
    join: Join,
    miter_limit: f32,
    subpath: Subpath,
) -> io::Result<Vec<u8>> {
    // vg_lite_set_stroke: `line_width <= 0` is INVALID_ARGUMENT. NaN and
    // infinity are refused too; the vendor does not define them.
    if !(width.is_finite() && width > 0.0) {
        return Err(invalid_input(
            "stroke width must be a positive, finite number",
        ));
    }
    if cap == Cap::Round {
        return Err(unsupported("round caps are not ported"));
    }
    if join == Join::Round {
        return Err(unsupported("round joins are not ported"));
    }
    if !miter_limit.is_finite() || !points.iter().all(|&(x, y)| x.is_finite() && y.is_finite()) {
        return Err(invalid_input("miter limit and points must be finite"));
    }
    // vg_lite_set_stroke: a miter limit below 1 becomes 1.
    let miter_limit = if miter_limit < 1.0 { 1.0 } else { miter_limit };
    let half_width = width / 2.0;
    let miter_square = miter_limit * miter_limit;

    let (right, left) = match points {
        // A lone MOVE then END: `_flatten_path` deletes the subpath, so nothing is stroked.
        [_] => (Vec::new(), Vec::new()),
        _ => {
            let nodes = build_nodes(points, subpath);
            match nodes.as_slice() {
                [only] => zero_length(*only, cap, half_width),
                _ => match subpath {
                    Subpath::Closed => closed_outline(&nodes, half_width, join, miter_square)?,
                    Subpath::Open => open_outline(&nodes, half_width, cap, join, miter_square)?,
                },
            }
        }
    };
    Ok(encode(&right, &left))
}

/// `_flatten_path` for MOVE, LINE and END: the point list. A closed subpath is
/// joined back to its start.
fn build_nodes(points: &[(f32, f32)], subpath: Subpath) -> Vec<Node> {
    let Some(&(start_x, start_y)) = points.first() else {
        // An END-only stream starts a point list at (0, 0).
        return vec![Node::default()];
    };
    let mut nodes = vec![Node {
        x: start_x,
        y: start_y,
        ..Node::default()
    }];
    for &(x, y) in points.iter().skip(1) {
        push_line(&mut nodes, x, y);
    }
    if subpath == Subpath::Open {
        return nodes;
    }
    // END with add_end == 0: close back to the start, unless the raw last point is already there.
    if let Some(&(last_x, last_y)) = points.last()
        && (last_x != start_x || last_y != start_y)
    {
        push_line(&mut nodes, start_x, start_y);
    }
    // The last point takes the first point's tangent, so the closing joint turns from the last segment to the first.
    if nodes.len() >= 2
        && let Some(first) = nodes.first().copied()
        && let Some(last) = nodes.last_mut()
    {
        last.length = first.length;
        last.tangent_x = first.tangent_x;
        last.tangent_y = first.tangent_y;
    }
    nodes
}

/// `_add_point_to_point_list`: append a LINE, or drop it when it repeats the last point.
fn push_line(nodes: &mut Vec<Node>, x: f32, y: f32) {
    let Some(&last) = nodes.last() else {
        nodes.push(Node {
            x,
            y,
            ..Node::default()
        });
        return;
    };
    let dx = x - last.x;
    let dy = y - last.y;
    let delta_x = dx.abs();
    let delta_y = dy.abs();
    if delta_x == 0.0 && delta_y == 0.0 {
        return;
    }
    if delta_x < FLOAT_EPSILON && delta_y < FLOAT_EPSILON {
        let ratio_x = coincidence_ratio(delta_x, x);
        let ratio_y = coincidence_ratio(delta_y, y);
        if ratio_x < 1.0e-6 && ratio_y < 1.0e-6 {
            return;
        }
    }
    let prev_tangent = nodes
        .len()
        .checked_sub(2)
        .and_then(|index| nodes.get(index))
        .map(|node| (node.tangent_x, node.tangent_y));
    if let Some(last_node) = nodes.last_mut() {
        set_tangent(last_node, prev_tangent, dx, dy);
    }
    nodes.push(Node {
        x,
        y,
        ..Node::default()
    });
}

/// The ratio test in `_add_point_to_point_list`.
fn coincidence_ratio(delta: f32, coord: f32) -> f32 {
    if delta == 0.0 {
        0.0
    } else if coord == 0.0 {
        delta
    } else {
        (delta / coord).abs()
    }
}

/// `_set_point_tangent` (vg_lite_stroke.c:188): the length and unit tangent of
/// the segment (dx, dy) that starts at `node`.
fn set_tangent(node: &mut Node, prev: Option<(f32, f32)>, dx: f32, dy: f32) {
    if dx == 0.0 {
        if dy == 0.0 {
            // Unreachable from `push_line`, which drops zero-length segments. Kept as the vendor has it.
            node.length = 0.0;
            (node.tangent_x, node.tangent_y) = prev.unwrap_or((0.0, 0.0));
        } else {
            node.tangent_x = 0.0;
            if dy > 0.0 {
                node.length = dy;
                node.tangent_y = 1.0;
            } else {
                node.length = -dy;
                node.tangent_y = -1.0;
            }
        }
    } else if dy == 0.0 {
        node.tangent_y = 0.0;
        if dx > 0.0 {
            node.length = dx;
            node.tangent_x = 1.0;
        } else {
            node.length = -dx;
            node.tangent_x = -1.0;
        }
    } else {
        let abs_dx = dx.abs();
        let abs_dy = dy.abs();
        let (length, mut tx, mut ty) = if abs_dx >= abs_dy {
            let t = abs_dy / abs_dx;
            let l = approx_sqrt_1p(t * t);
            let tx = 1.0 / l;
            (l * abs_dx, tx, tx * t)
        } else {
            let t = abs_dx / abs_dy;
            let l = approx_sqrt_1p(t * t);
            let ty = 1.0 / l;
            (l * abs_dy, ty * t, ty)
        };
        if dx < 0.0 {
            tx = -tx;
        }
        if dy < 0.0 {
            ty = -ty;
        }
        node.length = length;
        node.tangent_x = clamp_unit(tx);
        node.tangent_y = clamp_unit(ty);
    }
}

/// `_Sqrt` (vg_lite_stroke.c:168): sqrt(1 + x) for 0 <= x <= 1, as the vendor's
/// fixed polynomial, not the libm sqrt. The bits of the output depend on it.
fn approx_sqrt_1p(big_x: f32) -> f32 {
    let mut x = big_x;
    let mut s = 1.0_f32;
    s += x * 0.5;
    x *= big_x;
    s -= x * 0.124_459_95;
    x *= big_x;
    s += x * 0.058_032_196;
    x *= big_x;
    s -= x * 0.025_314_478;
    x *= big_x;
    s += x * 0.005_958_413_7;
    s
}

/// `CLAMP(v, -1, 1)`.
fn clamp_unit(v: f32) -> f32 {
    v.clamp(-1.0, 1.0)
}

/// The offset to the right of a segment's direction: `dx = uy * half_width`, `dy = -ux * half_width`.
fn normal(node: &Node, half_width: f32) -> (f32, f32) {
    (node.tangent_y * half_width, -node.tangent_x * half_width)
}

/// `_draw_zero_length` and `_add_zero_length_stroke_sub_path`: a subpath of one
/// point. BUTT draws nothing. SQUARE draws a square, and the vendor's
/// zero-tangent branch gives the offset (half_width, 0).
fn zero_length(node: Node, cap: Cap, half_width: f32) -> Sides {
    match cap {
        Cap::Square => {
            let (dx, dy) = if node.tangent_x == 0.0 || node.tangent_y == 0.0 {
                (half_width, 0.0)
            } else {
                normal(&node, half_width)
            };
            let right = vec![
                (node.x + dx + dy, node.y - dx + dy),
                (node.x + dx - dy, node.y + dx + dy),
                (node.x - dx - dy, node.y + dx - dy),
                (node.x - dx + dy, node.y - dx - dy),
            ];
            (right, Vec::new())
        }
        Cap::Butt | Cap::Round => (Vec::new(), Vec::new()),
    }
}

/// The right and left sides of a stroke, each as a list of points.
type Sides = (Vec<(f32, f32)>, Vec<(f32, f32)>);

/// The two sides of the stroke, built as `_add_point_to_right_stroke_point_list_tail`
/// and `_add_point_to_left_point_list_head` build them.
struct Stroker {
    /// The right side in list order. The tail is the last element.
    right: Vec<(f32, f32)>,
    /// The left side in insertion order. The vendor prepends at the head, so
    /// the head is the last element and the first point of the list is element 0.
    left: Vec<(f32, f32)>,
    join: Join,
    miter_square: f32,
}

impl Stroker {
    /// The start of a subpath: the first point's two offsets. `_start_new_stroke_sub_path`.
    fn start(first: &Node, half_width: f32, join: Join, miter_square: f32) -> (Self, (f32, f32)) {
        let d = normal(first, half_width);
        let stroker = Stroker {
            right: vec![(first.x + d.0, first.y + d.1)],
            left: vec![(first.x - d.0, first.y - d.1)],
            join,
            miter_square,
        };
        (stroker, d)
    }

    /// The interior points of a subpath, as `_create_stroke_path` runs them.
    /// Returns the last segment's offset and the point before the last.
    fn sweep(
        &mut self,
        mut d: (f32, f32),
        first: Node,
        middle: &[Node],
        half_width: f32,
    ) -> io::Result<((f32, f32), Node)> {
        let mut prev = first;
        for &cur in middle {
            self.right.push((cur.x + d.0, cur.y + d.1));
            self.left.push((cur.x - d.0, cur.y - d.1));
            d = normal(&cur, half_width);
            self.process_joint(
                prev,
                cur,
                (cur.x + d.0, cur.y + d.1),
                (cur.x - d.0, cur.y - d.1),
            )?;
            prev = cur;
        }
        Ok((d, prev))
    }

    /// `_process_line_joint` for straight segments with no swing. `swing_length`
    /// stays 0 for this input, so the vendor takes its short-line branch, and
    /// the inner side always gets the pivot pair. The miter apex comes from the outer side.
    fn process_joint(
        &mut self,
        prev: Node,
        cur: Node,
        plus: (f32, f32),
        minus: (f32, f32),
    ) -> io::Result<()> {
        let cos_theta = prev.tangent_x * cur.tangent_x + prev.tangent_y * cur.tangent_y;
        if cos_theta > FLOAT_ANGLE_EPSILON_COS {
            // Straight: one point per side.
            self.right.push(plus);
            self.left.push(minus);
            return Ok(());
        }
        let (counter_clockwise, ratio) = if cos_theta < -FLOAT_ANGLE_EPSILON_COS {
            // A reversal: counter-clockwise, with an infinite miter ratio.
            (true, f32::MAX)
        } else {
            let angle_sign = prev.tangent_x * cur.tangent_y - prev.tangent_y * cur.tangent_x;
            (angle_sign >= 0.0, 2.0 / (1.0 + cos_theta))
        };
        let miter_fits = ratio <= self.miter_square;
        if counter_clockwise {
            self.left.push((cur.x, cur.y));
            self.left.push(minus);
            match self.join {
                Join::Miter if miter_fits => {
                    adjust_joint(self.right.last_mut(), cur, plus, ratio)?;
                }
                Join::Miter | Join::Bevel => self.right.push(plus),
                Join::Round => return Err(unsupported("round joins are not ported")),
            }
        } else {
            self.right.push((cur.x, cur.y));
            self.right.push(plus);
            match self.join {
                Join::Miter if miter_fits => {
                    adjust_joint(self.left.last_mut(), cur, minus, ratio)?;
                }
                Join::Miter | Join::Bevel => self.left.push(minus),
                Join::Round => return Err(unsupported("round joins are not ported")),
            }
        }
        Ok(())
    }
}

/// `_adjust_joint_point`: moves a side's joint point toward the miter apex.
fn adjust_joint(
    join_point: Option<&mut (f32, f32)>,
    cur: Node,
    (x, y): (f32, f32),
    ratio: f32,
) -> io::Result<()> {
    let Some(join_point) = join_point else {
        return Err(invalid_data("miter joint has no point to adjust"));
    };
    let mx = (join_point.0 + x) / 2.0;
    let my = (join_point.1 + y) / 2.0;
    let dx = (mx - cur.x) * ratio;
    let dy = (my - cur.y) * ratio;
    *join_point = (cur.x + dx, cur.y + dy);
    Ok(())
}

/// The outline of the closed subpath: `_create_stroke_path` with `closed` set,
/// then `_close_stroke_sub_path`. No caps are drawn.
fn closed_outline(
    nodes: &[Node],
    half_width: f32,
    join: Join,
    miter_square: f32,
) -> io::Result<Sides> {
    let [first, middle @ .., last] = nodes else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (mut stroker, d) = Stroker::start(first, half_width, join, miter_square);
    let (d, prev) = stroker.sweep(d, *first, middle, half_width)?;
    // The closed branch of `_create_stroke_path` adds the end-of-segment points for the last point.
    stroker.right.push((last.x + d.0, last.y + d.1));
    stroker.left.push((last.x - d.0, last.y - d.1));
    // `_close_stroke_sub_path`: the joint at the last point joins the two sides' first points.
    let (Some(&first_right), Some(&first_left)) = (stroker.right.first(), stroker.left.first())
    else {
        return Err(invalid_data("closed stroke has no sides"));
    };
    stroker.process_joint(prev, *last, first_right, first_left)?;
    // The first point of each side takes the position of that side's end.
    let (Some(&tail), Some(&head)) = (stroker.right.last(), stroker.left.last()) else {
        return Err(invalid_data("closed stroke has no sides"));
    };
    if let Some(first_point) = stroker.right.first_mut() {
        *first_point = tail;
    }
    if let Some(first_point) = stroker.left.first_mut() {
        *first_point = head;
    }
    Ok((stroker.right, stroker.left))
}

/// The outline of the open subpath: `_create_stroke_path` with `add_end_cap`
/// set, `_start_new_stroke_sub_path` and `_end_stroke_sub_path`. BUTT adds
/// nothing at either end; SQUARE extends each end by half the width along the
/// direction of its segment. No closing join.
fn open_outline(
    nodes: &[Node],
    half_width: f32,
    cap: Cap,
    join: Join,
    miter_square: f32,
) -> io::Result<Sides> {
    let [first, middle @ .., last] = nodes else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (mut stroker, d) = Stroker::start(first, half_width, join, miter_square);
    if cap == Cap::Square {
        // `_start_new_stroke_sub_path`: the start moves back along the first segment.
        if let Some(right_start) = stroker.right.first_mut() {
            right_start.0 += d.1;
            right_start.1 -= d.0;
        }
        if let Some(left_start) = stroker.left.first_mut() {
            left_start.0 += d.1;
            left_start.1 -= d.0;
        }
    }
    let (d, _prev) = stroker.sweep(d, *first, middle, half_width)?;
    // `_end_stroke_sub_path`: the end points, then SQUARE extends them forward.
    stroker.right.push((last.x + d.0, last.y + d.1));
    stroker.left.push((last.x - d.0, last.y - d.1));
    if cap == Cap::Square {
        if let Some(right_end) = stroker.right.last_mut() {
            right_end.0 -= d.1;
            right_end.1 += d.0;
        }
        if let Some(left_end) = stroker.left.last_mut() {
            left_end.0 -= d.1;
            left_end.1 += d.0;
        }
    }
    Ok((stroker.right, stroker.left))
}

/// `_copy_stroke_path` and the END fallback in `vg_lite_update_stroke`.
fn encode(right: &[(f32, f32)], left: &[(f32, f32)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev: Option<(f32, f32)> = None;
    // The outline runs along the right side, then back along the left side from its head.
    for &(x, y) in right.iter().chain(left.iter().rev()) {
        match prev {
            None => push_command(&mut out, VLC_OP_MOVE, x, y),
            // A point equal to the one before it is not written.
            Some((px, py)) if px == x && py == y => {}
            Some(_) => push_command(&mut out, VLC_OP_LINE, x, y),
        }
        prev = Some((x, y));
    }
    if out.is_empty() {
        // The vendor writes a lone END byte here, and leaves bytes 1..3 uninitialised. Zeros are written instead.
        return vec![VLC_OP_END, 0, 0, 0];
    }
    out.extend_from_slice(&[VLC_OP_END, 0, 0, 0]);
    out
}

fn push_command(out: &mut Vec<u8>, op: u8, x: f32, y: f32) {
    out.extend_from_slice(&[op, 0, 0, 0]);
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
