//! The rows of conversions and resizes: strided byte inputs and outputs, so one kernel serves
//! packed and planar pixels alike. Each returns `false`, doing nothing, without V.

use core::arch::asm;

use super::available;

/// Bytes `start`, `start + stride`, `start + 2 stride`, ... of `buf`.
#[derive(Clone, Copy, Debug)]
pub struct Strided<T> {
    buf: T,
    start: usize,
    stride: usize,
}

impl<T: AsRef<[u8]>> Strided<T> {
    /// Element `i` is `buf[start + i * stride]`.
    pub fn new(buf: T, start: usize, stride: usize) -> Self {
        Self { buf, start, stride }
    }

    /// Whether `n` elements lie inside the buffer.
    fn holds(&self, n: usize) -> bool {
        n == 0
            || (n - 1)
                .checked_mul(self.stride)
                .and_then(|o| o.checked_add(self.start))
                .is_some_and(|last| last < self.buf.as_ref().len())
    }
}

/// `out[i] = clamp(((k0 a[i] + k1 b[i] + k2 c[i] + k3) >> 10) + k4, 0, 255)` over `n`
/// elements, with an arithmetic shift: one row of a colour matrix.
pub fn mix3(inputs: [Strided<&[u8]>; 3], k: [i32; 5], out: Strided<&mut [u8]>, n: usize) -> bool {
    assert!(inputs.iter().all(|s| s.holds(n)), "mix3: input too short");
    assert!(out.holds(n), "mix3: output too short");
    if !available() {
        return false;
    }
    if n == 0 {
        return true;
    }
    let [a, b, c] = inputs;
    // SAFETY: V is enabled. Each strided access touches elements 0..n of its `Strided`, which
    // `holds` bounds inside its buffer; the output is borrowed mutably. Only the named vector
    // registers are written. The products fit i32: |k| < 2^12 against bytes, plus k3.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "li {max}, 255",
            "2:",
            "vsetvli {vl}, {n}, e8, mf4, ta, ma",
            "vlse8.v v1, ({a}), {sa}",
            "vlse8.v v2, ({b}), {sb}",
            "vlse8.v v3, ({c}), {sc}",
            "vsetvli zero, {vl}, e32, m1, ta, ma",
            "vzext.vf4 v4, v1",
            "vzext.vf4 v5, v2",
            "vzext.vf4 v6, v3",
            "vmv.v.x v7, {k3}",
            "vmacc.vx v7, {k0}, v4",
            "vmacc.vx v7, {k1}, v5",
            "vmacc.vx v7, {k2}, v6",
            "vsra.vi v7, v7, 10",
            "vadd.vx v7, v7, {k4}",
            "vmax.vx v7, v7, zero",
            "vmin.vx v7, v7, {max}",
            "vsetvli zero, {vl}, e16, mf2, ta, ma",
            "vnsrl.wi v8, v7, 0",
            "vsetvli zero, {vl}, e8, mf4, ta, ma",
            "vnsrl.wi v9, v8, 0",
            "vsse8.v v9, ({o}), {so}",
            "sub {n}, {n}, {vl}",
            "mul {t}, {vl}, {sa}",
            "add {a}, {a}, {t}",
            "mul {t}, {vl}, {sb}",
            "add {b}, {b}, {t}",
            "mul {t}, {vl}, {sc}",
            "add {c}, {c}, {t}",
            "mul {t}, {vl}, {so}",
            "add {o}, {o}, {t}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) n => _,
            a = inout(reg) a.buf[a.start..].as_ptr() => _,
            b = inout(reg) b.buf[b.start..].as_ptr() => _,
            c = inout(reg) c.buf[c.start..].as_ptr() => _,
            o = inout(reg) out.buf[out.start..].as_mut_ptr() => _,
            sa = in(reg) a.stride,
            sb = in(reg) b.stride,
            sc = in(reg) c.stride,
            so = in(reg) out.stride,
            k0 = in(reg) k[0] as isize,
            k1 = in(reg) k[1] as isize,
            k2 = in(reg) k[2] as isize,
            k3 = in(reg) k[3] as isize,
            k4 = in(reg) k[4] as isize,
            vl = out(reg) _,
            t = out(reg) _,
            max = out(reg) _,
            out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
            out("v6") _, out("v7") _, out("v8") _, out("v9") _,
            options(nostack),
        );
    }
    true
}

/// `out[i] = (p + q + r + s + 2) >> 2` for the bytes `p = row0[o]`, `q = row0[o + step]`,
/// `r = row1[o]` and `s = row1[o + step]` at `o = start + i * stride`: a 2x2 block's mean.
pub fn average_2x2(
    row0: &[u8],
    row1: &[u8],
    start: usize,
    step: usize,
    stride: usize,
    out: &mut [u8],
    n: usize,
) -> bool {
    for row in [row0, row1] {
        assert!(
            Strided::new(row, start + step, stride).holds(n),
            "average_2x2: row too short"
        );
    }
    assert!(out.len() >= n, "average_2x2: output too short");
    if !available() {
        return false;
    }
    if n == 0 {
        return true;
    }
    // SAFETY: V is enabled. The four strided loads read elements 0..n at `start` and
    // `start + step`, inside both rows by the asserts; the store writes `n` bytes of `out`.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "2:",
            "vsetvli {vl}, {n}, e8, mf2, ta, ma",
            "vlse8.v v1, ({p}), {st}",
            "vlse8.v v2, ({q}), {st}",
            "vlse8.v v3, ({r}), {st}",
            "vlse8.v v4, ({s}), {st}",
            "vwaddu.vv v6, v1, v2",
            "vwaddu.vv v8, v3, v4",
            "vsetvli zero, {vl}, e16, m1, ta, ma",
            "vadd.vv v6, v6, v8",
            "vadd.vi v6, v6, 2",
            "vsetvli zero, {vl}, e8, mf2, ta, ma",
            "vnsrl.wi v10, v6, 2",
            "vse8.v v10, ({o})",
            "sub {n}, {n}, {vl}",
            "add {o}, {o}, {vl}",
            "mul {t}, {vl}, {st}",
            "add {p}, {p}, {t}",
            "add {q}, {q}, {t}",
            "add {r}, {r}, {t}",
            "add {s}, {s}, {t}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) n => _,
            p = inout(reg) row0[start..].as_ptr() => _,
            q = inout(reg) row0[start + step..].as_ptr() => _,
            r = inout(reg) row1[start..].as_ptr() => _,
            s = inout(reg) row1[start + step..].as_ptr() => _,
            o = inout(reg) out.as_mut_ptr() => _,
            st = in(reg) stride,
            vl = out(reg) _,
            t = out(reg) _,
            out("v1") _, out("v2") _, out("v3") _, out("v4") _,
            out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v10") _,
            options(nostack),
        );
    }
    true
}

/// `acc[i] += input[i]` for each element of `acc` (an area resize's block sums).
pub fn accumulate(acc: &mut [u32], input: Strided<&[u8]>) -> bool {
    let n = acc.len();
    assert!(input.holds(n), "accumulate: input too short");
    if !available() {
        return false;
    }
    if n == 0 {
        return true;
    }
    // SAFETY: V is enabled. The loop reads elements 0..n of `input`, inside by the assert, and
    // reads and writes the `n` words of `acc`.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "2:",
            "vsetvli {vl}, {n}, e8, mf4, ta, ma",
            "vlse8.v v1, ({s}), {st}",
            "vsetvli zero, {vl}, e32, m1, ta, ma",
            "vzext.vf4 v2, v1",
            "vle32.v v3, ({acc})",
            "vadd.vv v3, v3, v2",
            "vse32.v v3, ({acc})",
            "sub {n}, {n}, {vl}",
            "mul {t}, {vl}, {st}",
            "add {s}, {s}, {t}",
            "slli {t}, {vl}, 2",
            "add {acc}, {acc}, {t}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) n => _,
            s = inout(reg) input.buf[input.start..].as_ptr() => _,
            acc = inout(reg) acc.as_mut_ptr() => _,
            st = in(reg) input.stride,
            vl = out(reg) _,
            t = out(reg) _,
            out("v1") _, out("v2") _, out("v3") _,
            options(nostack),
        );
    }
    true
}

/// `out[i] = (acc[i] + divisor / 2) / divisor` for each element of `acc`, which must give bytes
/// (an area resize's means).
pub fn divide(acc: &[u32], divisor: u32, out: Strided<&mut [u8]>) -> bool {
    let n = acc.len();
    assert!(divisor > 0, "divide: by zero");
    assert!(out.holds(n), "divide: output too short");
    if !available() {
        return false;
    }
    if n == 0 {
        return true;
    }
    // SAFETY: V is enabled. The loop reads the `n` words of `acc` and writes elements 0..n of
    // `out`, inside by the assert.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "2:",
            "vsetvli {vl}, {n}, e32, m1, ta, ma",
            "vle32.v v1, ({acc})",
            "vadd.vx v1, v1, {half}",
            "vdivu.vx v1, v1, {div}",
            "vsetvli zero, {vl}, e16, mf2, ta, ma",
            "vnsrl.wi v2, v1, 0",
            "vsetvli zero, {vl}, e8, mf4, ta, ma",
            "vnsrl.wi v3, v2, 0",
            "vsse8.v v3, ({o}), {so}",
            "sub {n}, {n}, {vl}",
            "slli {t}, {vl}, 2",
            "add {acc}, {acc}, {t}",
            "mul {t}, {vl}, {so}",
            "add {o}, {o}, {t}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) n => _,
            acc = inout(reg) acc.as_ptr() => _,
            o = inout(reg) out.buf[out.start..].as_mut_ptr() => _,
            so = in(reg) out.stride,
            half = in(reg) (divisor / 2) as usize,
            div = in(reg) divisor as usize,
            vl = out(reg) _,
            t = out(reg) _,
            out("v1") _, out("v2") _, out("v3") _,
            options(nostack),
        );
    }
    true
}

/// One axis of a bilinear resize, made once and used for every row: for each output, the byte
/// offsets of its two source pixels and the second one's weight (0 to 256).
#[derive(Clone, Debug, Default)]
pub struct BilinearTable {
    first: Vec<u32>,
    second: Vec<u32>,
    weight: Vec<u32>,
    /// The largest offset, so a row is checked once per call.
    last: Option<u32>,
}

impl BilinearTable {
    /// The table of `(first offset, second offset, weight)` per output.
    pub fn new(entries: impl IntoIterator<Item = (u32, u32, u32)>) -> Self {
        let mut t = Self::default();
        for (a, b, w) in entries {
            assert!(w <= 256, "BilinearTable: weight {w} over 256");
            t.first.push(a);
            t.second.push(b);
            t.weight.push(w);
            t.last = Some(t.last.unwrap_or(0).max(a).max(b));
        }
        t
    }

    /// Outputs in the table.
    pub fn len(&self) -> usize {
        self.weight.len()
    }

    /// No outputs.
    pub fn is_empty(&self) -> bool {
        self.weight.is_empty()
    }
}

/// `out[i] = ((a (256 - w) + b w) (256 - fy) + (c (256 - w) + d w) fy + 32768) >> 16`, where
/// `a` and `b` are `top` at entry `i`'s two offsets, `c` and `d` the same in `bottom`, and `w`
/// its weight: one output row of one channel.
pub fn bilinear(
    table: &BilinearTable,
    top: &[u8],
    bottom: &[u8],
    fy: u32,
    out: Strided<&mut [u8]>,
) -> bool {
    let n = table.len();
    assert!(fy <= 256, "bilinear: weight {fy} over 256");
    if let Some(last) = table.last {
        let last = last as usize;
        assert!(
            last < top.len() && last < bottom.len(),
            "bilinear: row too short"
        );
    }
    assert!(out.holds(n), "bilinear: output too short");
    if !available() {
        return false;
    }
    if n == 0 {
        return true;
    }
    // SAFETY: V is enabled. The indexed loads read `top` and `bottom` at the table's offsets,
    // each below `last`, inside both rows by the assert; the table's three vectors hold `n`
    // words each, and the store writes elements 0..n of `out`. Sums stay below 2^24.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "li {k256}, 256",
            "sub {fyc}, {k256}, {fy}",
            "li {half}, 32768",
            "2:",
            "vsetvli {vl}, {n}, e32, m1, ta, ma",
            "vle32.v v1, ({o0})",
            "vle32.v v2, ({o1})",
            "vle32.v v3, ({w})",
            "vsetvli zero, {vl}, e8, mf4, ta, ma",
            "vluxei32.v v4, ({r0}), v1",
            "vluxei32.v v5, ({r0}), v2",
            "vluxei32.v v6, ({r1}), v1",
            "vluxei32.v v7, ({r1}), v2",
            "vsetvli zero, {vl}, e32, m1, ta, ma",
            "vzext.vf4 v8, v4",
            "vzext.vf4 v9, v5",
            "vzext.vf4 v10, v6",
            "vzext.vf4 v11, v7",
            "vrsub.vx v12, v3, {k256}",
            "vmul.vv v8, v8, v12",
            "vmacc.vv v8, v3, v9",
            "vmul.vv v10, v10, v12",
            "vmacc.vv v10, v3, v11",
            "vmul.vx v8, v8, {fyc}",
            "vmacc.vx v8, {fy}, v10",
            "vadd.vx v8, v8, {half}",
            "vsrl.vi v8, v8, 16",
            "vsetvli zero, {vl}, e16, mf2, ta, ma",
            "vnsrl.wi v13, v8, 0",
            "vsetvli zero, {vl}, e8, mf4, ta, ma",
            "vnsrl.wi v14, v13, 0",
            "vsse8.v v14, ({o}), {so}",
            "sub {n}, {n}, {vl}",
            "slli {t}, {vl}, 2",
            "add {o0}, {o0}, {t}",
            "add {o1}, {o1}, {t}",
            "add {w}, {w}, {t}",
            "mul {t}, {vl}, {so}",
            "add {o}, {o}, {t}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) n => _,
            o0 = inout(reg) table.first.as_ptr() => _,
            o1 = inout(reg) table.second.as_ptr() => _,
            w = inout(reg) table.weight.as_ptr() => _,
            o = inout(reg) out.buf[out.start..].as_mut_ptr() => _,
            r0 = in(reg) top.as_ptr(),
            r1 = in(reg) bottom.as_ptr(),
            so = in(reg) out.stride,
            fy = in(reg) fy as usize,
            fyc = out(reg) _,
            k256 = out(reg) _,
            half = out(reg) _,
            vl = out(reg) _,
            t = out(reg) _,
            out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _,
            out("v6") _, out("v7") _, out("v8") _, out("v9") _, out("v10") _,
            out("v11") _, out("v12") _, out("v13") _, out("v14") _,
            options(nostack),
        );
    }
    true
}
