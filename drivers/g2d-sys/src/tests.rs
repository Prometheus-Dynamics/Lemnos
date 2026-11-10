use super::*;
use std::mem::offset_of;
use std::os::fd::{AsFd, OwnedFd};

/// A sealed memfd of `len` bytes: a descriptor with a real, fixed size, and no device behind it.
fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"g2d-test", len).expect("memfd")
}

#[test]
fn the_layouts_match_the_c_compiler() {
    assert_eq!(size_of::<RawRect>(), 16);
    assert_eq!(size_of::<ImageEnh>(), 116);
    assert_eq!(size_of::<BltH>(), 236);
    assert_eq!(size_of::<FillrectH>(), 116);
    assert_eq!(size_of::<Bld>(), 596);
    assert_eq!(size_of::<Ck>(), 12);
    assert_eq!(size_of::<HardwareVersion>(), 8);
    assert_eq!(offset_of!(ImageEnh, format), 8);
    assert_eq!(offset_of!(ImageEnh, laddr), 12);
    assert_eq!(offset_of!(ImageEnh, haddr), 24);
    assert_eq!(offset_of!(ImageEnh, width), 36);
    assert_eq!(offset_of!(ImageEnh, align), 44);
    assert_eq!(offset_of!(ImageEnh, clip_rect), 56);
    assert_eq!(offset_of!(ImageEnh, resize), 72);
    assert_eq!(offset_of!(ImageEnh, coor), 80);
    assert_eq!(offset_of!(ImageEnh, gamut), 88);
    assert_eq!(offset_of!(ImageEnh, bpremul), 92);
    assert_eq!(offset_of!(ImageEnh, alpha), 96);
    assert_eq!(offset_of!(ImageEnh, mode), 100);
    assert_eq!(offset_of!(ImageEnh, fd), 104);
    assert_eq!(offset_of!(ImageEnh, use_phy_addr), 108);
    assert_eq!(offset_of!(ImageEnh, color_range), 112);
    assert_eq!(offset_of!(BltH, src), 4);
    assert_eq!(offset_of!(BltH, dst), 120);
    assert_eq!(offset_of!(Bld, dst), 4);
    assert_eq!(offset_of!(Bld, src), 120);
    assert_eq!(offset_of!(Bld, ck), 584);
    assert_eq!(QUERY_VERSION, 0x8008_479f);
    assert_eq!((BITBLT_H, FILLRECT_H, BLD_H), (0x55, 0x56, 0x57));
}

#[test]
fn a_frame_must_fit_its_dma_buf() {
    let b = buffer(1);
    let fd = b.as_fd();
    assert!(Surface::with_len(fd, 4 * 64 * 64, PixelFormat::Rgba8888, 64, 64).is_ok());
    assert!(Surface::with_len(fd, 4 * 64 * 64 - 1, PixelFormat::Rgba8888, 64, 64).is_err());
    assert!(Surface::with_len(fd, 3 * 64 * 64 - 1, PixelFormat::Rgb888, 64, 64).is_err());
    assert!(Surface::with_len(fd, 2 * 64 * 64 - 1, PixelFormat::Rgb565, 64, 64).is_err());
    assert!(Surface::with_len(fd, 64 * 64 * 3 / 2, PixelFormat::Nv12, 64, 64).is_ok());
    assert!(Surface::with_len(fd, 64 * 64 * 3 / 2 - 1, PixelFormat::I420, 64, 64).is_err());
    assert!(Surface::with_len(fd, 64 * 64 - 1, PixelFormat::Gray8, 64, 64).is_err());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Nv12, 63, 64).is_err());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Gray8, 0, 64).is_err());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Gray8, 64, 0).is_err());
    assert!(Surface::with_len(fd, 1 << 40, PixelFormat::Rgba8888, u32::MAX, u32::MAX).is_err());
}

#[test]
fn a_frame_never_passes_32_bits_whatever_the_dma_buf() {
    let b = buffer(1);
    let fd = b.as_fd();
    // 2^32 bytes exactly is one too many; the largest that fits is accepted.
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Rgba8888, 1 << 15, 1 << 15).is_err());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Gray8, u32::MAX, 1).is_ok());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Gray8, u32::MAX, 2).is_err());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Nv12, 1 << 16, 1 << 16).is_err());
}

#[test]
fn the_size_is_the_descriptors_own() {
    let b = buffer(4 * 16 * 16);
    let fd = b.as_fd();
    assert!(Surface::new(fd, PixelFormat::Rgba8888, 16, 16).is_ok());
    assert!(Surface::new(fd, PixelFormat::Rgba8888, 16, 17).is_err());
    assert!(Surface::new(fd, PixelFormat::Rgba8888, 17, 16).is_err());
    let empty = buffer(0);
    assert!(Surface::new(empty.as_fd(), PixelFormat::Gray8, 1, 1).is_err());
}

fn surface(fd: BorrowedFd<'_>, format: PixelFormat, w: u32, h: u32) -> Surface<'_> {
    Surface::with_len(fd, u64::MAX, format, w, h).expect("surface")
}

fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

#[test]
fn rectangles_must_fit_their_surface() {
    let b = buffer(1);
    let s = surface(b.as_fd(), PixelFormat::Rgba8888, 100, 50);
    let fill = |r| Job::fill(&s, r, 0, Gamut::Bt601, ColorRange::Full);
    assert!(fill(rect(90, 40, 10, 10)).is_ok());
    assert!(fill(rect(91, 40, 10, 10)).is_err());
    assert!(fill(rect(0, 41, 10, 10)).is_err());
    assert!(fill(rect(u32::MAX, 0, 2, 2)).is_err());
    assert!(fill(rect(0, u32::MAX, 2, 2)).is_err());
    assert!(fill(rect(1, 0, u32::MAX, 2)).is_err());
    assert!(fill(rect(0, 0, 0, 1)).is_err());
    assert!(fill(rect(0, 0, 1, 0)).is_err());
    let big = surface(b.as_fd(), PixelFormat::Gray8, MAX_SIZE + 2, 8);
    let fill = |r| Job::fill(&big, r, 0, Gamut::Bt601, ColorRange::Full);
    assert!(fill(rect(0, 0, MAX_SIZE, 8)).is_ok());
    assert!(fill(rect(0, 0, MAX_SIZE + 1, 8)).is_err());
    let yuv = surface(b.as_fd(), PixelFormat::Nv12, 100, 50);
    let fill = |r| Job::fill(&yuv, r, 0, Gamut::Bt601, ColorRange::Full);
    assert!(fill(rect(2, 2, 10, 10)).is_ok());
    assert!(fill(rect(1, 2, 10, 10)).is_err());
    assert!(fill(rect(2, 2, 11, 10)).is_err());
}

#[test]
fn a_position_past_2_31_is_refused_not_wrapped() {
    let b = buffer(1);
    let wide = surface(b.as_fd(), PixelFormat::Gray8, (1 << 31) + 8, 1);
    let fill = |r| Job::fill(&wide, r, 0, Gamut::Bt601, ColorRange::Full);
    assert!(fill(rect((1 << 31) - 8, 0, 8, 1)).is_ok());
    assert!(fill(rect(1 << 31, 0, 8, 1)).is_err());
}

#[test]
fn every_rectangle_of_a_job_is_checked() {
    let b = buffer(1);
    let s = surface(b.as_fd(), PixelFormat::Rgba8888, 64, 64);
    let d = surface(b.as_fd(), PixelFormat::Rgba8888, 32, 32);
    let out = rect(0, 0, 33, 32);
    let (g, r) = (Gamut::Bt601, ColorRange::Full);
    assert!(Job::blit(&s, s.full(), &d, d.full(), g, r).is_ok());
    assert!(Job::blit(&s, rect(1, 0, 64, 64), &d, d.full(), g, r).is_err());
    assert!(Job::blit(&s, s.full(), &d, out, g, r).is_err());
    assert!(Job::rotate(&d, d.full(), &d, out, Turn::None, false, false).is_err());
    assert!(
        Job::rotate(
            &d,
            rect(0, 1, 32, 32),
            &d,
            d.full(),
            Turn::None,
            false,
            false
        )
        .is_err()
    );
    let small = rect(0, 0, 16, 16);
    let blend = |a, bg, o| Job::blend(&d, a, &d, bg, &s, o, Blend::SrcOver);
    assert!(blend(small, small, small).is_ok());
    assert!(blend(rect(17, 0, 16, 16), small, small).is_err());
    assert!(blend(small, rect(0, 17, 16, 16), small).is_err());
    assert!(blend(small, small, rect(49, 0, 16, 16)).is_err());
}

#[test]
fn a_blit_names_buffers_and_never_a_physical_address() {
    let b = buffer(1);
    let s = surface(b.as_fd(), PixelFormat::Nv12, 640, 480);
    let d = surface(b.as_fd(), PixelFormat::Bgra8888, 320, 240);
    let job = Job::blit(
        &s,
        s.full(),
        &d,
        d.full(),
        Gamut::Bt709,
        ColorRange::Limited,
    )
    .expect("job");
    let Request::Blit(r) = job.request else {
        panic!("a blit");
    };
    assert_eq!(r.flag_h, 0);
    assert_eq!((r.src.bbuff, r.dst.bbuff), (1, 1));
    for image in [r.src, r.dst] {
        assert_eq!(image.use_phy_addr, 0);
        assert_eq!(
            (image.laddr, image.haddr, image.align),
            ([0; 3], [0; 3], [0; 3])
        );
        assert_eq!((image.resize, image.coor), ([0; 2], [0; 2]));
        assert_eq!(image.fd, b.as_raw_fd());
    }
    assert_eq!(r.src.format, 0x28);
    assert_eq!(r.dst.format, 0x03);
    assert_eq!((r.src.width, r.src.height), (640, 480));
    assert_eq!(r.dst.gamut, Gamut::Bt709 as u32);
    assert_eq!(r.src.color_range, 1);
}

#[test]
fn a_fill_is_a_solid_layer() {
    let b = buffer(1);
    let s = surface(b.as_fd(), PixelFormat::Rgba8888, 8, 8);
    let job = Job::fill(&s, s.full(), 0x1234_5678, Gamut::Bt601, ColorRange::Full).expect("job");
    let Request::Fill(r) = job.request else {
        panic!("a fill");
    };
    assert_eq!(r.dst.bbuff, 0);
    assert_eq!(r.dst.color, 0x1234_5678);
    assert_eq!(r.dst.use_phy_addr, 0);
}

#[test]
fn the_rotator_keeps_format_and_size() {
    let b = buffer(1);
    let s = surface(b.as_fd(), PixelFormat::Rgba8888, 64, 32);
    let d = surface(b.as_fd(), PixelFormat::Rgba8888, 32, 64);
    let other = surface(b.as_fd(), PixelFormat::Bgra8888, 32, 64);
    let job = Job::rotate(&s, s.full(), &d, d.full(), Turn::Cw90, true, false).expect("job");
    let Request::Blit(r) = job.request else {
        panic!("a rotate");
    };
    assert_eq!(r.flag_h, 0x1100);
    assert!(Job::rotate(&s, s.full(), &other, other.full(), Turn::Cw90, false, false).is_err());
    assert!(Job::rotate(&s, s.full(), &d, d.full(), Turn::Cw180, false, false).is_err());
    let same = surface(b.as_fd(), PixelFormat::Rgba8888, 64, 32);
    let job = Job::rotate(&s, s.full(), &same, same.full(), Turn::None, false, true).expect("job");
    let Request::Blit(r) = job.request else {
        panic!("a flip");
    };
    assert_eq!(r.flag_h, 0x2400);
}

#[test]
fn a_blend_takes_two_rgb_layers_no_larger_than_its_output() {
    let f = buffer(1);
    let a = surface(f.as_fd(), PixelFormat::Argb8888, 16, 16);
    let b = surface(f.as_fd(), PixelFormat::Xrgb8888, 16, 16);
    let d = surface(f.as_fd(), PixelFormat::Argb8888, 16, 16);
    let small = surface(f.as_fd(), PixelFormat::Argb8888, 8, 8);
    let yuv = surface(f.as_fd(), PixelFormat::Nv12, 16, 16);
    let job = Job::blend(&a, a.full(), &b, b.full(), &d, d.full(), Blend::SrcOver).expect("job");
    let Request::Blend(r) = job.request else {
        panic!("a blend");
    };
    assert_eq!(r.bld_cmd, 4);
    assert_eq!((r.src[0].bbuff, r.src[1].bbuff), (1, 1));
    assert_eq!(
        (
            r.src[0].use_phy_addr,
            r.src[1].use_phy_addr,
            r.dst.use_phy_addr
        ),
        (0, 0, 0)
    );
    assert_eq!(r.src[2], ImageEnh::default());
    assert_eq!(r.src[3], ImageEnh::default());
    assert_eq!(r.ck, Ck::default());
    assert!(Job::blend(&yuv, yuv.full(), &b, b.full(), &d, d.full(), Blend::Xor).is_err());
    assert!(Job::blend(&a, a.full(), &yuv, yuv.full(), &d, d.full(), Blend::Xor).is_err());
    assert!(Job::blend(&a, a.full(), &b, b.full(), &small, small.full(), Blend::Xor).is_err());
}

#[test]
fn a_missing_device_is_an_error() {
    let error = Device::open("/nonexistent/g2d").expect_err("no such device");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}
