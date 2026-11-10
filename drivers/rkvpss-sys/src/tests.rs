//! The layouts against the C compiler, and every check that keeps a job inside its dma-bufs.
//! Ported from the dev branch's `crates/lemnos-linux-sys/src/rkvpss/tests.rs`, with the output
//! stride, the node check and the fourcc table added.

use super::*;
use std::mem::offset_of;
use std::os::fd::AsFd;

fn file() -> File {
    File::open("/dev/null").expect("/dev/null")
}

const BIG: u64 = 1 << 32;

fn nv12(fd: BorrowedFd<'_>, width: u32, height: u32) -> Surface<'_> {
    Surface::with_len(
        fd,
        BIG,
        PixelFormat::Nv12,
        width,
        height,
        width.next_multiple_of(16),
    )
    .expect("surface")
}

fn out<'a>(channel: Channel, crop: Crop, target: Surface<'a>) -> Output<'a> {
    Output {
        channel,
        crop,
        target,
        flip: false,
    }
}

/// The numbers `gcc` gives for `include/uapi/linux/rk-vpss-config.h` on x86-64 and arm64.
#[test]
fn the_layouts_match_the_c_compiler() {
    assert_eq!(size_of::<ModuleSel>(), 7);
    assert_eq!(size_of::<InputCfg>(), 40);
    assert_eq!(size_of::<OutputCfg>(), 432);
    assert_eq!(size_of::<FrameCfg>(), 2648);
    assert_eq!(offset_of!(InputCfg, dmabuf), 32);
    assert_eq!(offset_of!(OutputCfg, cmsc), 52);
    assert_eq!(offset_of!(OutputCfg, aspt), 392);
    assert_eq!(offset_of!(OutputCfg, dmabuf), 412);
    assert_eq!(offset_of!(OutputCfg, wrap), 420);
    assert_eq!(offset_of!(FrameCfg, input), 12);
    assert_eq!(offset_of!(FrameCfg, output), 52);
    assert_eq!(offset_of!(FrameCfg, sharp), 2644);
    assert_eq!(MODULE_SEL, 0x4007_56f2);
    assert_eq!(FRAME_HANDLE, 0x4a58_56f3);
    assert_eq!(MODULE_GET, 0x8007_56f6);
    assert_eq!(BUF_DEL_BY_FILE, 0x0000_56f9);
}

#[test]
fn the_fourccs_match_videodev2() {
    assert_eq!(PixelFormat::Nv12.fourcc(), 0x3231_564e);
    assert_eq!(PixelFormat::Nv61.fourcc(), 0x3136_564e);
    assert_eq!(PixelFormat::Grey.fourcc(), 0x5945_5247);
    assert_eq!(PixelFormat::Rgb565.fourcc(), 0x5042_4752);
    assert_eq!(PixelFormat::Rgb565x.fourcc(), 0x5242_4752);
    assert_eq!(PixelFormat::Rgb24.fourcc(), 0x3342_4752);
    assert_eq!(PixelFormat::Bgr24.fourcc(), 0x3352_4742);
    assert_eq!(PixelFormat::Xrgb32.fourcc(), 0x3432_5842);
    assert_eq!(PixelFormat::Xbgr32.fourcc(), 0x3432_5258);
    assert_eq!(PixelFormat::Rgbx32.fourcc(), 0x3432_4258);
    assert_eq!(PixelFormat::Bgrx32.fourcc(), 0x3432_5852);
    assert_eq!(PixelFormat::Yvyu.fourcc(), 0x5559_5659);
    for format in PixelFormat::ALL {
        assert_eq!(PixelFormat::from_fourcc(format.fourcc()), Some(format));
    }
    assert_eq!(PixelFormat::from_fourcc(u32::from_le_bytes(*b"FBC0")), None);
}

#[test]
fn a_frame_must_fit_its_dma_buf() {
    use PixelFormat::{Nv12, Nv16, Rgb24, Xrgb32};
    let f = file();
    let fd = f.as_fd();
    assert!(Surface::with_len(fd, 64 * 64 * 3 / 2, Nv12, 64, 64, 64).is_ok());
    assert!(Surface::with_len(fd, 64 * 64 * 3 / 2 - 1, Nv12, 64, 64, 64).is_err());
    assert!(Surface::with_len(fd, 64 * 64 * 2 - 1, Nv16, 64, 64, 64).is_err());
    assert!(Surface::with_len(fd, 128 * 64 - 1, Xrgb32, 32, 64, 128).is_err());
    assert!(Surface::with_len(fd, 192 * 64, Rgb24, 64, 64, 192).is_ok());
    // The largest frame taken still fits the driver's `int`.
    let most = Nv16.frame_len(MAX_STRIDE, MAX_HEIGHT);
    assert!(most <= i32::MAX as u64);
    assert!(Surface::with_len(fd, most, Nv16, MAX_WIDTH, MAX_HEIGHT, MAX_STRIDE).is_ok());
    assert!(Surface::with_len(fd, most - 1, Nv16, MAX_WIDTH, MAX_HEIGHT, MAX_STRIDE).is_err());
}

#[test]
fn a_surface_must_be_in_range() {
    use PixelFormat::{Grey, Nv12, Rgb24, Uyvy};
    let f = file();
    let fd = f.as_fd();
    assert!(Surface::with_len(fd, BIG, Nv12, 31, 64, 64).is_err());
    assert!(Surface::with_len(fd, BIG, Nv12, 64, 31, 64).is_err());
    assert!(Surface::with_len(fd, BIG, Nv12, 4097, 64, 8192).is_err());
    assert!(Surface::with_len(fd, BIG, Nv12, 64, 3073, 64).is_err());
    assert!(Surface::with_len(fd, BIG, Nv12, 33, 64, 64).is_err());
    assert!(Surface::with_len(fd, BIG, Nv12, 64, 33, 64).is_err());
    assert!(Surface::with_len(fd, BIG, Uyvy, 33, 64, 68).is_err());
    assert!(Surface::with_len(fd, BIG, Grey, 33, 33, 36).is_ok());
    // A stride that cannot hold a line, is misaligned, or could overflow the driver's `int`.
    assert!(Surface::with_len(fd, BIG, Rgb24, 64, 64, 188).is_err());
    assert!(Surface::with_len(fd, BIG, Nv12, 64, 64, 66).is_err());
    assert!(Surface::with_len(fd, BIG, Nv12, 64, 64, MAX_STRIDE + 4).is_err());
    assert!(Surface::with_len(fd, u64::MAX, Nv12, 64, 64, 0xffff_fffc).is_err());
}

#[test]
fn a_job_fills_in_only_what_was_asked() {
    let (a, b) = (file(), file());
    let (src, dst) = (nv12(a.as_fd(), 640, 480), nv12(b.as_fd(), 320, 240));
    let crop = Crop {
        x: 2,
        y: 4,
        width: 320,
        height: 240,
    };
    let job = Job::new(
        src,
        &[Output {
            flip: true,
            ..out(Channel::Ch2, crop, dst)
        }],
    )
    .expect("job");
    assert_eq!(job.channels(), 1 << 2);
    let cfg = *job.request();
    assert_eq!({ cfg.dev_id }, 0);
    assert_eq!({ cfg.mirror }, 0);
    assert_eq!(cfg.sharp, [0; 4]);
    let input = cfg.input;
    assert_eq!({ input.width }, 640);
    assert_eq!({ input.stride }, 640);
    assert_eq!({ input.ver_stride }, 0);
    assert_eq!({ input.rotate }, 0);
    assert_eq!({ input.rotate_90 }, 0);
    assert_eq!({ input.dmabuf }, 0);
    assert_eq!({ input.buf_fd }, a.as_raw_fd());
    let on = cfg.output[2];
    assert_eq!({ on.enable }, 1);
    assert_eq!({ on.crop_h_offs }, 2);
    assert_eq!({ on.crop_v_offs }, 4);
    assert_eq!({ on.scl_width }, 320);
    assert_eq!({ on.scl_height }, 240);
    assert_eq!({ on.stride }, 320);
    assert_eq!({ on.flip }, 1);
    assert_eq!({ on.avg_scl_down }, 0);
    assert_eq!({ on.dmabuf }, 0);
    assert_eq!({ on.buf_fd }, b.as_raw_fd());
    assert_eq!({ on.alpha }, 0xff);
    assert_eq!(on.cmsc, [0; 340]);
    assert_eq!(on.aspt, [0; 20]);
    assert_eq!(on.wrap, [0; 12]);
    for i in [0, 1, 3, 4, 5] {
        let off = cfg.output[i];
        assert_eq!({ off.enable }, 0);
        assert_eq!({ off.dmabuf }, 0);
    }
}

#[test]
fn a_crop_must_be_even_and_inside() {
    let (a, b) = (file(), file());
    let (src, dst) = (nv12(a.as_fd(), 640, 480), nv12(b.as_fd(), 320, 240));
    let at = |x, y, width, height| {
        let crop = Crop {
            x,
            y,
            width,
            height,
        };
        Job::new(src, &[out(Channel::Ch0, crop, dst)])
    };
    assert!(at(320, 240, 320, 240).is_ok());
    assert!(at(322, 240, 320, 240).is_err());
    assert!(at(320, 242, 320, 240).is_err());
    assert!(at(1, 0, 320, 240).is_err());
    assert!(at(0, 0, 321, 240).is_err());
    assert!(at(0, 0, 0, 240).is_err());
    assert!(at(u32::MAX - 1, 0, 2, 240).is_err());
    assert!(at(0, u32::MAX - 1, 320, 2).is_err());
}

#[test]
fn an_output_stride_is_one_the_block_takes_as_given() {
    use PixelFormat::{Grey, Nv12};
    let f = file();
    let fd = f.as_fd();
    let src = nv12(fd, 640, 480);
    let crop = Crop::full(640, 480);
    let target = |stride| Surface::with_len(fd, BIG, Grey, 36, 36, stride).expect("surface");
    let small = Crop::full(36, 36);
    // A multiple of four is a valid surface, and a valid input, but the driver would hand the
    // block a zero pitch for it as an output.
    assert!(Job::new(src, &[out(Channel::Ch0, small, target(36))]).is_err());
    assert!(Job::new(src, &[out(Channel::Ch0, small, target(40))]).is_err());
    assert!(Job::new(src, &[out(Channel::Ch0, small, target(48))]).is_ok());
    let odd_input = Surface::with_len(fd, BIG, Nv12, 640, 480, 644).expect("surface");
    let dst = nv12(fd, 320, 240);
    assert!(Job::new(odd_input, &[out(Channel::Ch0, crop, dst)]).is_ok());
}

#[test]
fn a_job_keeps_to_the_blocks_rules() {
    use PixelFormat::{Grey, Nv12, Nv21, Rgb24, Yuyv};
    let f = file();
    let fd = f.as_fd();
    let src = nv12(fd, 3840, 2160);
    let full = Crop::full(3840, 2160);
    let surface = |format, width, height, stride| {
        Surface::with_len(fd, BIG, format, width, height, stride).expect("surface")
    };
    let small = surface(Nv12, 640, 480, 640);

    assert!(Job::new(src, &[]).is_err());
    let twice = [
        out(Channel::Ch0, full, small),
        out(Channel::Ch0, full, small),
    ];
    assert!(Job::new(src, &twice).is_err());

    // Grey cannot be read; YUYV cannot be written; RGB only comes from CH1.
    assert!(
        Job::new(
            surface(Grey, 640, 480, 640),
            &[out(Channel::Ch1, full, small)]
        )
        .is_err()
    );
    assert!(
        Job::new(
            src,
            &[out(Channel::Ch0, full, surface(Yuyv, 640, 480, 1280))]
        )
        .is_err()
    );
    let rgb = surface(Rgb24, 640, 480, 1920);
    assert!(Job::new(src, &[out(Channel::Ch0, full, rgb)]).is_err());
    assert!(Job::new(src, &[out(Channel::Ch1, full, rgb)]).is_ok());

    // The output width must be a multiple of four.
    assert!(
        Job::new(
            src,
            &[out(Channel::Ch0, full, surface(Grey, 642, 480, 656))]
        )
        .is_err()
    );

    // One chroma order for every output.
    let swapped = surface(Nv21, 640, 480, 640);
    let mixed = [
        out(Channel::Ch0, full, small),
        out(Channel::Ch1, full, swapped),
    ];
    assert!(Job::new(src, &mixed).is_err());

    // At most 1:32 either way.
    let tiny = surface(Nv12, 64, 64, 64);
    assert!(Job::new(src, &[out(Channel::Ch0, full, tiny)]).is_err());
    let upscale = Crop::full(32, 32);
    let big = surface(Nv12, 1024, 1024, 1024);
    assert!(Job::new(src, &[out(Channel::Ch0, upscale, big)]).is_ok());
    let beyond = surface(Nv12, 1040, 1040, 1040);
    assert!(Job::new(src, &[out(Channel::Ch0, upscale, beyond)]).is_err());

    // From CH2 up, a scaled output is at most 1080p; an unscaled one may be larger.
    let uhd = surface(Nv12, 2560, 1440, 2560);
    assert!(Job::new(src, &[out(Channel::Ch1, full, uhd)]).is_ok());
    assert!(Job::new(src, &[out(Channel::Ch2, full, uhd)]).is_err());
    let exact = Crop::full(2560, 1440);
    assert!(Job::new(src, &[out(Channel::Ch2, exact, uhd)]).is_ok());
}

#[test]
fn every_channel_can_run_at_once() {
    let f = file();
    let src = nv12(f.as_fd(), 1920, 1080);
    let dst = nv12(f.as_fd(), 640, 360);
    let crop = Crop::full(1920, 1080);
    let outputs = Channel::ALL.map(|channel| out(channel, crop, dst));
    assert_eq!(Job::new(src, &outputs).expect("job").channels(), 0b11_1111);
}

/// A scratch `/sys` with one character device named `name` at `major:minor`.
fn sys_with(name: &str, major: u32, minor: u32) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rkvpss-sys-{}-{name}", std::process::id()));
    let dir = root.join(format!("dev/char/{major}:{minor}"));
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(dir.join("name"), format!("{name}\n")).expect("write");
    root
}

#[test]
fn only_the_offline_node_is_opened() {
    // `/dev/null` is character device 1:3. Named as the node, it opens; named as another video
    // node, or not a character device at all, it is refused before any request is made.
    let null = File::open("/dev/null").expect("/dev/null").metadata();
    let rdev = null.expect("stat").rdev();
    let (major, minor) = (libc::major(rdev), libc::minor(rdev));

    let sys = sys_with(NODE_NAME, major, minor);
    let opened = Node::open_in(&sys, Path::new("/dev/null"));
    let other = sys_with("rkisp_mainpath", major, minor);
    let refused = Node::open_in(&other, Path::new("/dev/null"));
    let plain = sys.join(format!("dev/char/{major}:{minor}/name"));
    let not_a_device = Node::open_in(&sys, &plain);
    let empty = std::env::temp_dir().join("rkvpss-sys-nothing-here");
    let unnamed = Node::open_in(&empty, Path::new("/dev/null"));
    fs::remove_dir_all(&sys).expect("clean up");
    fs::remove_dir_all(&other).expect("clean up");

    assert!(opened.is_ok());
    for refused in [refused, not_a_device, unnamed] {
        let error = refused.expect_err("not the node");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
    let missing = Node::open("/nonexistent/video9").expect_err("no such node");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}
