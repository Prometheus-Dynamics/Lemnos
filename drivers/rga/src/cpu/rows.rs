//! The rows the conversions are made of, each a scalar loop that RVV replaces where it can.
//! Both compute `clamp(((c0 a + c1 b + c2 c + bias) >> 10) + add)` (see `super::convert`).

use super::convert::{Chroma, Coefficients, mix};

/// `d[x] = mix(k . (R, G, B) of pixel x, add)` over a packed row.
pub(super) fn mix3_packed(
    s: &[u8],
    bpp: usize,
    rgb: [usize; 3],
    k: [i32; 3],
    add: i32,
    d: &mut [u8],
) {
    let n = d.len();
    #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
    {
        use lemnos_driver_rga_sys::rvv::{Strided, mix3};
        let inputs = rgb.map(|c| Strided::new(s, c, bpp));
        if mix3(
            inputs,
            [k[0], k[1], k[2], 512, add],
            Strided::new(d, 0, 1),
            n,
        ) {
            return;
        }
    }
    for (x, d) in d.iter_mut().enumerate().take(n) {
        let p = &s[x * bpp..];
        *d = mix(
            k[0] * i32::from(p[rgb[0]]) + k[1] * i32::from(p[rgb[1]]) + k[2] * i32::from(p[rgb[2]]),
            add,
        );
    }
}

/// One row of 4:2:0 chroma (chroma row `cy`) from two RGB rows: each block's R, G and B
/// averaged, then the matrix's U and V rows.
#[allow(clippy::too_many_arguments)]
pub(super) fn rgb_to_chroma(
    r0: &[u8],
    r1: &[u8],
    bpp: usize,
    rgb: [usize; 3],
    k: &Coefficients,
    dst: &mut [u8],
    ch: Chroma,
    cy: usize,
    w: usize,
    #[cfg_attr(
        not(all(target_os = "linux", target_arch = "riscv64")),
        allow(unused_variables)
    )]
    avg: &mut [Vec<u8>; 3],
) {
    let cw = w.div_ceil(2);
    let (u0, v0) = (ch.u + cy * ch.stride, ch.v + cy * ch.stride);
    // Whole blocks: both columns inside the row.
    #[cfg_attr(
        not(all(target_os = "linux", target_arch = "riscv64")),
        allow(unused_mut)
    )]
    let mut done = 0;
    #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
    {
        use lemnos_driver_rga_sys::rvv::{Strided, average_2x2, mix3};
        let full = w / 2;
        for row in avg.iter_mut() {
            // Grows only for a wider frame than any before: the steady state reuses it.
            row.resize(full, 0);
        }
        let ok = (0..3).all(|c| average_2x2(r0, r1, rgb[c], bpp, 2 * bpp, &mut avg[c], full));
        if ok && full > 0 {
            let [r, g, b] = &*avg;
            let planar = [
                Strided::new(r.as_slice(), 0, 1),
                Strided::new(g.as_slice(), 0, 1),
                Strided::new(b.as_slice(), 0, 1),
            ];
            let to = |row: [i32; 3]| [row[0], row[1], row[2], 512, 128];
            let u = mix3(
                planar,
                to(k.to_u),
                Strided::new(&mut *dst, u0, ch.step),
                full,
            );
            let v = mix3(
                planar,
                to(k.to_v),
                Strided::new(&mut *dst, v0, ch.step),
                full,
            );
            if u && v {
                done = full;
            }
        }
    }
    for cx in done..cw {
        let (xa, xb) = (2 * cx, (2 * cx + 1).min(w - 1));
        let avg = |c: usize| {
            let sum = u32::from(r0[xa * bpp + rgb[c]])
                + u32::from(r0[xb * bpp + rgb[c]])
                + u32::from(r1[xa * bpp + rgb[c]])
                + u32::from(r1[xb * bpp + rgb[c]]);
            ((sum + 2) >> 2) as i32
        };
        let (r, g, b) = (avg(0), avg(1), avg(2));
        let dot = |row: [i32; 3]| row[0] * r + row[1] * g + row[2] * b;
        dst[u0 + cx * ch.step] = mix(dot(k.to_u), 128);
        dst[v0 + cx * ch.step] = mix(dot(k.to_v), 128);
    }
}

/// One RGB row from luma row `y_start` and chroma row `cy`: each pixel takes its block's chroma.
#[allow(clippy::too_many_arguments)]
pub(super) fn yuv_to_rgb(
    src: &[u8],
    y_start: usize,
    ch: Chroma,
    cy: usize,
    k: &Coefficients,
    row: &mut [u8],
    bpp: usize,
    rgb: [usize; 3],
    fourth: Option<usize>,
    w: usize,
) {
    let (u0, v0) = (ch.u + cy * ch.stride, ch.v + cy * ch.stride);
    // R from Y and V, G from all three, B from Y and U: as `mix3` rows over (Y, U, V), with
    // the offsets folded into the bias.
    let rows: [[i32; 3]; 3] = [[k.y, 0, k.vr], [k.y, k.ug, k.vg], [k.y, k.ub, 0]];
    let bias = |c: [i32; 3]| 512 - c[0] * k.y_offset - 128 * (c[1] + c[2]);
    #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
    {
        use lemnos_driver_rga_sys::rvv::{Strided, mix3};
        let mut ok = true;
        // Even pixels, then odd: both read chroma column x / 2 with the chroma's own step.
        for parity in 0..2 {
            let n = (w + 1 - parity) / 2;
            if n == 0 {
                continue;
            }
            let inputs = [
                Strided::new(src, y_start + parity, 2),
                Strided::new(src, u0, ch.step),
                Strided::new(src, v0, ch.step),
            ];
            for c in 0..3 {
                let co = rows[c];
                let out = Strided::new(&mut *row, parity * bpp + rgb[c], 2 * bpp);
                ok &= mix3(inputs, [co[0], co[1], co[2], bias(co), 0], out, n);
            }
            if let Some(f) = fourth {
                let out = Strided::new(&mut *row, parity * bpp + f, 2 * bpp);
                ok &= mix3(inputs, [0, 0, 0, 0, 255], out, n);
            }
        }
        if ok {
            return;
        }
    }
    for x in 0..w {
        let yv = i32::from(src[y_start + x]);
        let uv = i32::from(src[u0 + (x / 2) * ch.step]);
        let vv = i32::from(src[v0 + (x / 2) * ch.step]);
        let p = &mut row[x * bpp..][..bpp];
        for c in 0..3 {
            let co = rows[c];
            // Same sum as the vector rows: (c0 Y + c1 U + c2 V + bias) >> 10.
            p[rgb[c]] = mix(co[0] * yv + co[1] * uv + co[2] * vv + bias(co) - 512, 0);
        }
        if let Some(f) = fourth {
            p[f] = 255;
        }
    }
}
