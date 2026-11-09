use super::*;
use std::mem::offset_of;
use std::os::fd::AsFd;

fn file() -> std::fs::File {
    std::fs::File::open("/dev/null").expect("/dev/null")
}

#[test]
fn the_layouts_match_the_c_compiler() {
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
}

#[test]
fn a_frame_must_fit_its_dma_buf() {
    let f = file();
    let fd = f.as_fd();
    assert!(Surface::with_len(fd, 4 * 64 * 64, PixelFormat::Rgba8888, 64, 64).is_ok());
    assert!(Surface::with_len(fd, 4 * 64 * 64 - 1, PixelFormat::Rgba8888, 64, 64).is_err());
    assert!(Surface::with_len(fd, 64 * 64 * 3 / 2, PixelFormat::Nv12, 64, 64).is_ok());
    assert!(Surface::with_len(fd, 64 * 64 * 3 / 2 - 1, PixelFormat::I420, 64, 64).is_err());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Nv12, 63, 64).is_err());
    assert!(Surface::with_len(fd, u64::MAX, PixelFormat::Gray8, 0, 64).is_err());
    assert!(Surface::with_len(fd, 1 << 40, PixelFormat::Rgba8888, u32::MAX, u32::MAX).is_err());
}

fn surface(fd: BorrowedFd<'_>, format: PixelFormat, w: u32, h: u32) -> Surface<'_> {
    Surface::with_len(fd, u64::MAX, format, w, h).expect("surface")
}

fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

#[test]
fn rectangles_must_fit_their_surface() {
    let f = file();
    let s = surface(f.as_fd(), PixelFormat::Rgba8888, 100, 50);
    let fill = |r| Job::fill(&s, r, 0, Gamut::Bt601, ColorRange::Full);
    assert!(fill(rect(90, 40, 10, 10)).is_ok());
    assert!(fill(rect(91, 40, 10, 10)).is_err());
    assert!(fill(rect(0, 41, 10, 10)).is_err());
    assert!(fill(rect(u32::MAX, 0, 2, 2)).is_err());
    assert!(fill(rect(0, 0, 0, 1)).is_err());
    let big = surface(f.as_fd(), PixelFormat::Gray8, MAX_SIZE + 2, 8);
    let fill = |r| Job::fill(&big, r, 0, Gamut::Bt601, ColorRange::Full);
    assert!(fill(rect(0, 0, MAX_SIZE, 8)).is_ok());
    assert!(fill(rect(0, 0, MAX_SIZE + 1, 8)).is_err());
    let yuv = surface(f.as_fd(), PixelFormat::Nv12, 100, 50);
    let fill = |r| Job::fill(&yuv, r, 0, Gamut::Bt601, ColorRange::Full);
    assert!(fill(rect(2, 2, 10, 10)).is_ok());
    assert!(fill(rect(1, 2, 10, 10)).is_err());
    assert!(fill(rect(2, 2, 11, 10)).is_err());
}

#[test]
fn a_blit_names_buffers_and_never_a_physical_address() {
    let f = file();
    let s = surface(f.as_fd(), PixelFormat::Nv12, 640, 480);
    let d = surface(f.as_fd(), PixelFormat::Bgra8888, 320, 240);
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
    assert_eq!((r.src.use_phy_addr, r.dst.use_phy_addr), (0, 0));
    assert_eq!(r.src.format, 0x28);
    assert_eq!(r.dst.format, 0x03);
    assert_eq!(r.src.align, [0; 3]);
    assert_eq!(r.dst.gamut, Gamut::Bt709 as u32);
    assert_eq!(r.src.color_range, 1);
    assert_eq!(r.src.fd, f.as_raw_fd());
}

#[test]
fn a_fill_is_a_solid_layer() {
    let f = file();
    let s = surface(f.as_fd(), PixelFormat::Rgba8888, 8, 8);
    let job = Job::fill(&s, s.full(), 0x1234_5678, Gamut::Bt601, ColorRange::Full).expect("job");
    let Request::Fill(r) = job.request else {
        panic!("a fill");
    };
    assert_eq!(r.dst.bbuff, 0);
    assert_eq!(r.dst.color, 0x1234_5678);
}

#[test]
fn the_rotator_keeps_format_and_size() {
    let f = file();
    let s = surface(f.as_fd(), PixelFormat::Rgba8888, 64, 32);
    let d = surface(f.as_fd(), PixelFormat::Rgba8888, 32, 64);
    let other = surface(f.as_fd(), PixelFormat::Bgra8888, 32, 64);
    let job = Job::rotate(&s, s.full(), &d, d.full(), Turn::Cw90, true, false).expect("job");
    let Request::Blit(r) = job.request else {
        panic!("a rotate");
    };
    assert_eq!(r.flag_h, 0x1100);
    assert!(Job::rotate(&s, s.full(), &other, other.full(), Turn::Cw90, false, false).is_err());
    assert!(Job::rotate(&s, s.full(), &d, d.full(), Turn::Cw180, false, false).is_err());
    let same = surface(f.as_fd(), PixelFormat::Rgba8888, 64, 32);
    let job = Job::rotate(&s, s.full(), &same, same.full(), Turn::None, false, true).expect("job");
    let Request::Blit(r) = job.request else {
        panic!("a flip");
    };
    assert_eq!(r.flag_h, 0x2400);
}

#[test]
fn a_blend_takes_two_rgb_layers_no_larger_than_its_output() {
    let f = file();
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
    assert_eq!(r.src[2], ImageEnh::default());
    assert!(Job::blend(&yuv, yuv.full(), &b, b.full(), &d, d.full(), Blend::Xor).is_err());
    assert!(Job::blend(&a, a.full(), &b, b.full(), &small, small.full(), Blend::Xor).is_err());
}
