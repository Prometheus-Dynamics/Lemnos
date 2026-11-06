//! RISC-V vector (RVV 1.0) kernels for the per-pixel operations the K230's routes leave on the
//! CPU. Each one is a safe function that returns `false`, doing nothing, when the CPU or the
//! kernel does not offer the V extension; the caller then runs its scalar loop.
//!
//! Written from the RVV 1.0 specification for the K230's C908. Run under qemu's RVV (VLEN 128,
//! 256 and 512) against the scalar loops; untested on board.

use core::arch::asm;
use std::sync::OnceLock;

/// `AT_HWCAP`, and its bit for the V extension (`'V' - 'A'`), as the kernel sets it when it
/// enables vector state for user space.
const AT_HWCAP: u64 = 16;
const HWCAP_V: u64 = 1 << (b'V' - b'A');

/// Whether this process may run vector instructions: the kernel's `AT_HWCAP` has V.
pub fn available() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| {
        let Ok(auxv) = std::fs::read("/proc/self/auxv") else {
            return false;
        };
        auxv.as_chunks::<8>()
            .0
            .as_chunks::<2>()
            .0
            .iter()
            .any(|[key, value]| {
                u64::from_ne_bytes(*key) == AT_HWCAP && u64::from_ne_bytes(*value) & HWCAP_V != 0
            })
    })
}

/// `dst[i] = [g, g, g, 255]` for each `g` of `gray`. `dst` must hold `4 * gray.len()` bytes.
pub fn gray_to_rgba(gray: &[u8], dst: &mut [u8]) -> bool {
    assert!(dst.len() >= 4 * gray.len(), "gray_to_rgba: dst too short");
    if !available() {
        return false;
    }
    if gray.is_empty() {
        return true;
    }
    // SAFETY: V is enabled (`available`). The loop reads `gray.len()` bytes from `gray` and
    // writes four times that to `dst`, which the assert above bounds. Only the vector registers
    // named as clobbers are written.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "2:",
            "vsetvli {vl}, {n}, e8, m2, ta, ma",
            "vle8.v v8, ({s})",
            "vmv.v.v v10, v8",
            "vmv.v.v v12, v8",
            "vmv.v.i v14, -1",
            "vsseg4e8.v v8, ({d})",
            "sub {n}, {n}, {vl}",
            "add {s}, {s}, {vl}",
            "slli {vl}, {vl}, 2",
            "add {d}, {d}, {vl}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) gray.len() => _,
            s = inout(reg) gray.as_ptr() => _,
            d = inout(reg) dst.as_mut_ptr() => _,
            vl = out(reg) _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            options(nostack),
        );
    }
    true
}

/// `dst[i] = rgba[4 * i + channel]`, one byte of each four-byte pixel. `rgba` must hold
/// `4 * dst.len()` bytes and `channel` must be below 4.
pub fn channel(rgba: &[u8], channel: usize, dst: &mut [u8]) -> bool {
    assert!(channel < 4, "channel: index {channel}");
    assert!(rgba.len() >= 4 * dst.len(), "channel: rgba too short");
    if !available() {
        return false;
    }
    if dst.is_empty() {
        return true;
    }
    // SAFETY: V is enabled. The strided load reads byte `channel` of each of `dst.len()`
    // four-byte pixels, inside `rgba` by the assert, and the store writes `dst.len()` bytes.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "li {four}, 4",
            "2:",
            "vsetvli {vl}, {n}, e8, m8, ta, ma",
            "vlse8.v v8, ({s}), {four}",
            "vse8.v v8, ({d})",
            "sub {n}, {n}, {vl}",
            "add {d}, {d}, {vl}",
            "slli {vl}, {vl}, 2",
            "add {s}, {s}, {vl}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) dst.len() => _,
            s = inout(reg) rgba[channel..].as_ptr() => _,
            d = inout(reg) dst.as_mut_ptr() => _,
            vl = out(reg) _,
            four = out(reg) _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            options(nostack),
        );
    }
    true
}

/// `dst[i] = 255` when each of the first three bytes of pixel `i` of `rgbx` lies in
/// `min..=max` for its channel, else 0. `rgbx` must hold `4 * dst.len()` bytes. A channel whose
/// `min` is above its `max` matches nothing.
pub fn color_key(rgbx: &[u8], min: [u8; 3], max: [u8; 3], dst: &mut [u8]) -> bool {
    assert!(rgbx.len() >= 4 * dst.len(), "color_key: rgbx too short");
    if !available() {
        return false;
    }
    if (0..3).any(|c| min[c] > max[c]) {
        dst.fill(0);
        return true;
    }
    if dst.is_empty() {
        return true;
    }
    let span = |c: usize| usize::from(max[c] - min[c]);
    // SAFETY: V is enabled. The segment load reads `dst.len()` four-byte pixels, inside `rgbx`
    // by the assert, and the store writes `dst.len()` bytes. A byte is in range when
    // `byte - min`, wrapping, is at most `max - min`.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +v",
            "2:",
            "vsetvli {vl}, {n}, e8, m2, ta, ma",
            "vlseg4e8.v v8, ({s})",
            "vsub.vx v8, v8, {r0}",
            "vmsleu.vx v1, v8, {r1}",
            "vsub.vx v10, v10, {g0}",
            "vmsleu.vx v2, v10, {g1}",
            "vsub.vx v12, v12, {b0}",
            "vmsleu.vx v3, v12, {b1}",
            "vmand.mm v0, v1, v2",
            "vmand.mm v0, v0, v3",
            "vmv.v.i v16, 0",
            "vmerge.vim v16, v16, -1, v0",
            "vse8.v v16, ({d})",
            "sub {n}, {n}, {vl}",
            "add {d}, {d}, {vl}",
            "slli {vl}, {vl}, 2",
            "add {s}, {s}, {vl}",
            "bnez {n}, 2b",
            ".option pop",
            n = inout(reg) dst.len() => _,
            s = inout(reg) rgbx.as_ptr() => _,
            d = inout(reg) dst.as_mut_ptr() => _,
            vl = out(reg) _,
            r0 = in(reg) usize::from(min[0]),
            r1 = in(reg) span(0),
            g0 = in(reg) usize::from(min[1]),
            g1 = in(reg) span(1),
            b0 = in(reg) usize::from(min[2]),
            b1 = in(reg) span(2),
            out("v0") _, out("v1") _, out("v2") _, out("v3") _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            out("v16") _, out("v17") _,
            options(nostack),
        );
    }
    true
}

/// A bitwise operation of [`rop`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bitwise {
    /// `src & dst`.
    And,
    /// `src | dst`.
    Or,
    /// `src ^ dst`.
    Xor,
}

/// `dst[i] = src[i] op dst[i]` over `dst.len()` bytes. `src` must hold as many.
pub fn rop(src: &[u8], dst: &mut [u8], op: Bitwise) -> bool {
    assert!(src.len() >= dst.len(), "rop: src too short");
    if !available() {
        return false;
    }
    if dst.is_empty() {
        return true;
    }
    macro_rules! run {
        ($op:literal) => {
            // SAFETY: V is enabled. Each pass reads `vl` bytes of `src` and of `dst` and writes
            // `vl` bytes of `dst`, over `dst.len()` bytes in all, inside both by the assert.
            unsafe {
                asm!(
                    ".option push",
                    ".option arch, +v",
                    "2:",
                    "vsetvli {vl}, {n}, e8, m8, ta, ma",
                    "vle8.v v8, ({s})",
                    "vle8.v v16, ({d})",
                    concat!($op, ".vv v8, v8, v16"),
                    "vse8.v v8, ({d})",
                    "sub {n}, {n}, {vl}",
                    "add {s}, {s}, {vl}",
                    "add {d}, {d}, {vl}",
                    "bnez {n}, 2b",
                    ".option pop",
                    n = inout(reg) dst.len() => _,
                    s = inout(reg) src.as_ptr() => _,
                    d = inout(reg) dst.as_mut_ptr() => _,
                    vl = out(reg) _,
                    out("v8") _, out("v9") _, out("v10") _, out("v11") _,
                    out("v12") _, out("v13") _, out("v14") _, out("v15") _,
                    out("v16") _, out("v17") _, out("v18") _, out("v19") _,
                    out("v20") _, out("v21") _, out("v22") _, out("v23") _,
                    options(nostack),
                )
            }
        };
    }
    match op {
        Bitwise::And => run!("vand"),
        Bitwise::Or => run!("vor"),
        Bitwise::Xor => run!("vxor"),
    }
    true
}
