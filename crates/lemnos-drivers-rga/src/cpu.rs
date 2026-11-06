//! The CPU route: the per-pixel operations a unit leaves to the CPU (on the K230D, every one of
//! these: its VGLite route serves none), as scalar loops over strided images. On riscv64 Linux
//! with the V extension, each row goes through `lemnos_linux_sys::rvv` instead. Same bytes
//! either way; [`vector`] says which runs.
//!
//! The RVV rows were written from the RVV 1.0 specification for the K230's C908 and checked
//! under qemu (VLEN 128, 256 and 512) against these loops. Untested on board.
//!
//! Each function takes the image's `width` and `height` in pixels and each buffer's stride in
//! bytes, and refuses (`InvalidInput`) a stride shorter than a row or a buffer shorter than the
//! image.

use std::io;

use crate::{Channel, Rop};

/// Whether the rows run as RVV vector code here (riscv64 Linux whose kernel offers V).
pub fn vector() -> bool {
    #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
    return lemnos_linux_sys::rvv::available();
    #[cfg(not(all(target_os = "linux", target_arch = "riscv64")))]
    false
}

/// `dst[x] = src[4 * x + c]`: one channel of RGBA8888 (or RGBX) as a gray byte, exactly.
pub fn channel_to_gray(
    width: usize,
    height: usize,
    channel: Channel,
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
) -> io::Result<()> {
    check(height, 4 * width, src.len(), src_stride)?;
    check(height, width, dst.len(), dst_stride)?;
    let c = match channel {
        Channel::Red => 0,
        Channel::Green => 1,
        Channel::Blue => 2,
    };
    for y in 0..height {
        let s = &src[y * src_stride..][..4 * width];
        let d = &mut dst[y * dst_stride..][..width];
        #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
        if lemnos_linux_sys::rvv::channel(s, c, d) {
            continue;
        }
        for (d, p) in d.iter_mut().zip(s.as_chunks::<4>().0) {
            *d = p[c];
        }
    }
    Ok(())
}

/// `dst[x] = [g, g, g, 255]` for each gray byte `g`: gray to RGBA8888, alpha opaque.
pub fn gray_to_rgba(
    width: usize,
    height: usize,
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
) -> io::Result<()> {
    check(height, width, src.len(), src_stride)?;
    check(height, 4 * width, dst.len(), dst_stride)?;
    for y in 0..height {
        let s = &src[y * src_stride..][..width];
        let d = &mut dst[y * dst_stride..][..4 * width];
        #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
        if lemnos_linux_sys::rvv::gray_to_rgba(s, d) {
            continue;
        }
        for (p, &g) in d.as_chunks_mut::<4>().0.iter_mut().zip(s) {
            *p = [g, g, g, 255];
        }
    }
    Ok(())
}

/// A mask of RGBA8888 (or RGBX): `dst[x]` is 255 when each of R, G and B lies in its
/// `min..=max`, else 0. A channel whose `min` is above its `max` matches nothing.
#[allow(clippy::too_many_arguments)]
pub fn color_key(
    width: usize,
    height: usize,
    src: &[u8],
    src_stride: usize,
    min: [u8; 3],
    max: [u8; 3],
    dst: &mut [u8],
    dst_stride: usize,
) -> io::Result<()> {
    check(height, 4 * width, src.len(), src_stride)?;
    check(height, width, dst.len(), dst_stride)?;
    for y in 0..height {
        let s = &src[y * src_stride..][..4 * width];
        let d = &mut dst[y * dst_stride..][..width];
        #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
        if lemnos_linux_sys::rvv::color_key(s, min, max, d) {
            continue;
        }
        for (d, p) in d.iter_mut().zip(s.as_chunks::<4>().0) {
            let inside = (0..3).all(|c| min[c] <= p[c] && p[c] <= max[c]);
            *d = if inside { 255 } else { 0 };
        }
    }
    Ok(())
}

/// `dst = src op dst` on two 8-bit masks of `width` by `height`.
pub fn rop(
    width: usize,
    height: usize,
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    op: Rop,
) -> io::Result<()> {
    check(height, width, src.len(), src_stride)?;
    check(height, width, dst.len(), dst_stride)?;
    for y in 0..height {
        let s = &src[y * src_stride..][..width];
        let d = &mut dst[y * dst_stride..][..width];
        #[cfg(all(target_os = "linux", target_arch = "riscv64"))]
        {
            use lemnos_linux_sys::rvv::Bitwise;
            let bitwise = match op {
                Rop::And => Bitwise::And,
                Rop::Or => Bitwise::Or,
                Rop::Xor => Bitwise::Xor,
            };
            if lemnos_linux_sys::rvv::rop(s, d, bitwise) {
                continue;
            }
        }
        for (d, &s) in d.iter_mut().zip(s) {
            *d = match op {
                Rop::And => s & *d,
                Rop::Or => s | *d,
                Rop::Xor => s ^ *d,
            };
        }
    }
    Ok(())
}

/// `height` rows of `row` bytes at `stride` fit in `len` bytes.
fn check(height: usize, row: usize, len: usize, stride: usize) -> io::Result<()> {
    let fits = stride >= row
        && (height == 0
            || (height - 1)
                .checked_mul(stride)
                .and_then(|n| n.checked_add(row))
                .is_some_and(|n| n <= len));
    if fits {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{height} rows of {row} bytes at stride {stride} need more than {len} bytes"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (s >> 33) as u8
            })
            .collect()
    }

    // Widths around the vector lengths (16, 32 and 64 bytes a register at VLEN 128 to 512),
    // strides with padding, and the padding left untouched.
    const SIZES: [(usize, usize); 6] = [(1, 1), (15, 3), (16, 2), (17, 5), (65, 4), (640, 3)];

    #[test]
    fn channel_to_gray_takes_one_byte_of_each_pixel() {
        for (w, h) in SIZES {
            let (ss, ds) = (4 * w + 12, w + 7);
            let src = bytes(ss * h, 1);
            for (channel, c) in [(Channel::Red, 0), (Channel::Green, 1), (Channel::Blue, 2)] {
                let mut dst = vec![0xAA; ds * h];
                channel_to_gray(w, h, channel, &src, ss, &mut dst, ds).unwrap();
                for y in 0..h {
                    for x in 0..w {
                        assert_eq!(dst[y * ds + x], src[y * ss + 4 * x + c]);
                    }
                    assert!(dst[y * ds + w..(y + 1) * ds].iter().all(|&b| b == 0xAA));
                }
            }
        }
    }

    #[test]
    fn gray_to_rgba_repeats_the_byte_with_alpha_opaque() {
        for (w, h) in SIZES {
            let (ss, ds) = (w + 3, 4 * w + 8);
            let src = bytes(ss * h, 2);
            let mut dst = vec![0xAA; ds * h];
            gray_to_rgba(w, h, &src, ss, &mut dst, ds).unwrap();
            for y in 0..h {
                for x in 0..w {
                    let g = src[y * ss + x];
                    assert_eq!(dst[y * ds + 4 * x..][..4], [g, g, g, 255]);
                }
                assert!(dst[y * ds + 4 * w..(y + 1) * ds].iter().all(|&b| b == 0xAA));
            }
        }
    }

    #[test]
    fn color_key_marks_pixels_inside_every_range() {
        let ranges = [
            ([0, 0, 0], [255, 255, 255]),
            ([40, 0, 100], [200, 128, 220]),
            ([10, 10, 10], [10, 10, 10]),
            ([50, 0, 0], [40, 255, 255]),
            ([0, 128, 0], [127, 255, 63]),
        ];
        for (w, h) in SIZES {
            let (ss, ds) = (4 * w, w + 1);
            let src = bytes(ss * h, 3);
            for (min, max) in ranges {
                let mut dst = vec![0xAA; ds * h];
                color_key(w, h, &src, ss, min, max, &mut dst, ds).unwrap();
                for y in 0..h {
                    for x in 0..w {
                        let p = &src[y * ss + 4 * x..][..3];
                        let inside = (0..3).all(|c| (min[c]..=max[c]).contains(&p[c]));
                        assert_eq!(dst[y * ds + x], if inside { 255 } else { 0 });
                    }
                    assert_eq!(dst[y * ds + w], 0xAA);
                }
            }
        }
    }

    #[test]
    fn rop_combines_two_masks() {
        for (w, h) in SIZES {
            let (ss, ds) = (w + 5, w + 2);
            let src = bytes(ss * h, 4);
            let before = bytes(ds * h, 5);
            for op in [Rop::And, Rop::Or, Rop::Xor] {
                let mut dst = before.clone();
                rop(w, h, &src, ss, &mut dst, ds, op).unwrap();
                for y in 0..h {
                    for x in 0..w {
                        let (s, d) = (src[y * ss + x], before[y * ds + x]);
                        let want = match op {
                            Rop::And => s & d,
                            Rop::Or => s | d,
                            Rop::Xor => s ^ d,
                        };
                        assert_eq!(dst[y * ds + x], want);
                    }
                    assert_eq!(
                        dst[y * ds + w..(y + 1) * ds],
                        before[y * ds + w..(y + 1) * ds]
                    );
                }
            }
        }
    }

    /// `LEMNOS_EXPECT_VECTOR=1` (or `0`) checks which path the other tests took: set it when
    /// running under an emulator with the V extension on (or off).
    #[test]
    fn the_expected_path_runs() {
        if let Some(want) = std::env::var_os("LEMNOS_EXPECT_VECTOR") {
            assert_eq!(vector(), want == "1");
        }
    }

    #[test]
    fn short_strides_and_buffers_are_refused() {
        let mut dst = [0u8; 8];
        assert!(gray_to_rgba(2, 1, &[0; 2], 2, &mut dst, 7).is_err());
        assert!(gray_to_rgba(2, 2, &[0; 3], 2, &mut dst, 8).is_err());
        assert!(gray_to_rgba(2, 1, &[0; 2], 1, &mut dst, 8).is_err());
        gray_to_rgba(2, 1, &[0; 2], 2, &mut dst, 8).unwrap();
        gray_to_rgba(0, 0, &[], 0, &mut [], 0).unwrap();
    }
}
