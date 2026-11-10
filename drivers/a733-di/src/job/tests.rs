use std::os::fd::{AsFd, OwnedFd};

use lemnos_linux_sys::dma_buf;

use super::*;

const W: u32 = 640;
const H: u32 = 480;

fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"di-test", len).expect("memfd")
}

/// A tightly packed layout of `format` at `width` x `height`, and its size in bytes.
fn packed(format: Format, width: u32, height: u32) -> (Layout, u64) {
    let luma = u64::from(width) * u64::from(height);
    let row = format.chroma_row_bytes(width);
    let plane = u64::from(row) * u64::from(format.chroma_rows(height));
    let layout = Layout {
        luma_offset: 0,
        luma_stride: width,
        chroma_offset: luma,
        chroma2_offset: if format.planar() { luma + plane } else { 0 },
        chroma_stride: row,
    };
    let planes = if format.planar() { 2 } else { 1 };
    (layout, luma + planes * plane)
}

const ALL: [Format; 8] = [
    Format::Nv12,
    Format::Nv21,
    Format::Nv16,
    Format::Nv61,
    Format::Yuv420,
    Format::Yvu420,
    Format::Yuv422,
    Format::Yvu422,
];

#[test]
fn a_frame_must_fit_its_buffer_to_the_last_byte() {
    let b = buffer(1);
    let fd = b.as_fd();
    for format in ALL {
        let (layout, len) = packed(format, W, H);
        assert!(
            Frame::with_len(fd, len, format, W, H, layout).is_ok(),
            "{format:?}"
        );
        assert_eq!(
            Frame::with_len(fd, len - 1, format, W, H, layout).unwrap_err(),
            Invalid::OutOfBuffer,
            "{format:?}"
        );
    }
    let (expected, len) = packed(Format::Nv12, W, H);
    assert_eq!(len, u64::from(W * H) * 3 / 2);
    assert_eq!(packed(Format::Yuv422, W, H).1, u64::from(W * H) * 2);
    // Each plane on its own: luma, then chroma, pushed one byte too far.
    let mut layout = expected;
    layout.luma_offset = 1;
    layout.chroma_offset = len - u64::from(W * H / 2) + 1;
    assert_eq!(
        Frame::with_len(fd, len, Format::Nv12, W, H, layout).unwrap_err(),
        Invalid::OutOfBuffer
    );
    let mut layout = expected;
    layout.chroma_offset += 1;
    assert_eq!(
        Frame::with_len(fd, len, Format::Nv12, W, H, layout).unwrap_err(),
        Invalid::OutOfBuffer
    );
    let (planar, len) = packed(Format::Yuv420, W, H);
    let mut layout = planar;
    layout.chroma2_offset += 1;
    assert_eq!(
        Frame::with_len(fd, len, Format::Yuv420, W, H, layout).unwrap_err(),
        Invalid::OutOfBuffer
    );
}

#[test]
fn offsets_and_strides_that_overflow_are_refused() {
    let b = buffer(1);
    let fd = b.as_fd();
    let (layout, _) = packed(Format::Nv12, W, H);
    for (luma, chroma) in [
        (u64::MAX, 1),
        (1, u64::MAX),
        (u64::MAX - 100, u64::MAX - 100),
    ] {
        let l = Layout {
            luma_offset: luma,
            chroma_offset: chroma,
            ..layout
        };
        assert_eq!(
            Frame::with_len(fd, u64::MAX, Format::Nv12, W, H, l).unwrap_err(),
            Invalid::OutOfBuffer
        );
    }
    // The longest stride, at the largest size, still checked against the buffer.
    let tall = Layout {
        luma_offset: 0,
        luma_stride: MAX_STRIDE,
        chroma_offset: u64::from(MAX_STRIDE) * 2048,
        chroma2_offset: 0,
        chroma_stride: MAX_STRIDE,
    };
    let need = u64::from(MAX_STRIDE) * 2048 + u64::from(MAX_STRIDE) * 1023 + 2048;
    assert!(Frame::with_len(fd, need, Format::Nv12, 2048, 2048, tall).is_ok());
    assert_eq!(
        Frame::with_len(fd, need - 1, Format::Nv12, 2048, 2048, tall).unwrap_err(),
        Invalid::OutOfBuffer
    );
}

#[test]
fn strides_cover_their_rows_and_fit_the_pitch_register() {
    let b = buffer(1);
    let fd = b.as_fd();
    let (layout, len) = packed(Format::Nv12, W, H);
    let short = Layout {
        luma_stride: W - 1,
        ..layout
    };
    assert_eq!(
        Frame::with_len(fd, len, Format::Nv12, W, H, short).unwrap_err(),
        Invalid::Stride
    );
    let short_chroma = Layout {
        chroma_stride: W - 1,
        ..layout
    };
    assert_eq!(
        Frame::with_len(fd, len, Format::Nv12, W, H, short_chroma).unwrap_err(),
        Invalid::Stride
    );
    let long = Layout {
        luma_stride: MAX_STRIDE + 1,
        ..layout
    };
    assert_eq!(
        Frame::with_len(fd, u64::MAX, Format::Nv12, W, H, long).unwrap_err(),
        Invalid::Stride
    );
    // A planar format's chroma rows are half as wide.
    let (planar, len) = packed(Format::Yuv420, W, H);
    assert_eq!(planar.chroma_stride, W / 2);
    let narrow = Layout {
        chroma_stride: W / 2 - 1,
        ..planar
    };
    assert_eq!(
        Frame::with_len(fd, len, Format::Yuv420, W, H, narrow).unwrap_err(),
        Invalid::Stride
    );
}

#[test]
fn chroma_offsets_are_never_zero_and_match_the_planes() {
    let b = buffer(1);
    let fd = b.as_fd();
    let (layout, len) = packed(Format::Nv12, W, H);
    let zero = Layout {
        chroma_offset: 0,
        ..layout
    };
    assert_eq!(
        Frame::with_len(fd, u64::MAX, Format::Nv12, W, H, zero).unwrap_err(),
        Invalid::ChromaOffset
    );
    let extra = Layout {
        chroma2_offset: 4096,
        ..layout
    };
    assert_eq!(
        Frame::with_len(fd, u64::MAX, Format::Nv12, W, H, extra).unwrap_err(),
        Invalid::ChromaOffset
    );
    let (planar, _) = packed(Format::Yuv420, W, H);
    let missing = Layout {
        chroma2_offset: 0,
        ..planar
    };
    assert_eq!(
        Frame::with_len(fd, u64::MAX, Format::Yuv420, W, H, missing).unwrap_err(),
        Invalid::ChromaOffset
    );
    // Chroma before luma is fine as long as nothing is at 0 but luma.
    let swapped = Layout {
        luma_offset: u64::from(W * H / 2),
        chroma_offset: 0,
        ..layout
    };
    assert_eq!(
        Frame::with_len(fd, len, Format::Nv12, W, H, swapped).unwrap_err(),
        Invalid::ChromaOffset
    );
}

#[test]
fn sizes_are_multiples_of_4_up_to_2048() {
    let b = buffer(1);
    let fd = b.as_fd();
    for (w, h) in [
        (0, 480),
        (640, 0),
        (642, 480),
        (640, 482),
        (2052, 480),
        (640, 2052),
    ] {
        let (layout, _) = packed(Format::Nv12, w.max(4), h.max(4));
        assert_eq!(
            Frame::with_len(fd, u64::MAX, Format::Nv12, w, h, layout).unwrap_err(),
            Invalid::Size,
            "{w}x{h}"
        );
        assert_eq!(Config::new(w, h, Mode::Bob).unwrap_err(), Invalid::Size);
    }
    let (layout, len) = packed(Format::Nv12, 2048, 2048);
    assert!(Frame::with_len(fd, len, Format::Nv12, 2048, 2048, layout).is_ok());
    let (layout, len) = packed(Format::Nv12, 4, 4);
    assert!(Frame::with_len(fd, len, Format::Nv12, 4, 4, layout).is_ok());
}

#[test]
fn the_size_is_the_descriptors_own() {
    let (layout, len) = packed(Format::Nv12, W, H);
    let fits = buffer(len);
    assert!(Frame::new(fits.as_fd(), Format::Nv12, W, H, layout).is_ok());
    let short = buffer(len - 1);
    assert!(matches!(
        Frame::new(short.as_fd(), Format::Nv12, W, H, layout),
        Err(FrameError::Invalid(Invalid::OutOfBuffer))
    ));
}

const TNR: Tnr = Tnr {
    adaptive: true,
    level: Level::Middle,
};

#[test]
fn the_settings_pick_the_kernels_mode() {
    let s = |mode| Config::new(W, H, mode).unwrap().settings();
    let pair = |s: Settings| (s.dit.intp_mode, s.dit.out_frame_mode, s.tnr.mode, s.fmd.en);
    assert_eq!(pair(s(Mode::Weave)), (1, 1, 0, 0));
    assert_eq!(pair(s(Mode::Bob)), (2, 1, 0, 0));
    let motion = Mode::Motion {
        film_detect: true,
        tnr: None,
    };
    assert_eq!(pair(s(motion)), (3, 1, 0, 1));
    let field_rate = Mode::MotionFieldRate {
        film_detect: false,
        tnr: Some(TNR),
    };
    assert_eq!(pair(s(field_rate)), (3, 2, 1, 0));
    assert_eq!(s(field_rate).tnr.level, 1);
    let fixed = Tnr {
        adaptive: false,
        level: Level::Low,
    };
    assert_eq!(pair(s(Mode::NoiseOnly(fixed))), (0, 0, 2, 0));
    let all = s(Mode::Bob);
    assert_eq!((all.size.width, all.size.height), (W, H));
    assert_eq!(
        (all.crop.left, all.crop.top, all.crop.right, all.crop.bottom),
        (0, 0, W, H)
    );
}

/// Frames in `n` buffers of `format`, kept alive by the caller.
fn buffers(n: usize, format: Format) -> (Vec<OwnedFd>, Layout, u64) {
    let (layout, len) = packed(format, W, H);
    ((0..n).map(|_| buffer(len)).collect(), layout, len)
}

fn frames<'a>(fds: &'a [OwnedFd], format: Format, layout: Layout) -> Vec<Frame<'a>> {
    fds.iter()
        .map(|fd| Frame::new(fd.as_fd(), format, W, H, layout).unwrap())
        .collect()
}

#[test]
fn every_slot_names_one_of_the_jobs_frames() {
    let (fds, layout, _) = buffers(6, Format::Yuv420);
    let f = frames(&fds, Format::Yuv420, layout);
    let raw: Vec<i32> = fds.iter().map(|fd| fd.as_raw_fd()).collect();
    let cases: [(Mode, usize, usize); 6] = [
        (Mode::Weave, 1, 1),
        (Mode::Bob, 1, 1),
        (
            Mode::Motion {
                film_detect: false,
                tnr: None,
            },
            2,
            1,
        ),
        (
            Mode::Motion {
                film_detect: false,
                tnr: Some(TNR),
            },
            2,
            2,
        ),
        (
            Mode::MotionFieldRate {
                film_detect: true,
                tnr: Some(TNR),
            },
            3,
            3,
        ),
        (Mode::NoiseOnly(TNR), 2, 1),
    ];
    for (mode, n_in, n_out) in cases {
        let config = Config::new(W, H, mode).unwrap();
        let job = Job::new(config, true, Field::Top, &f[..n_in], &f[n_in..n_in + n_out]).unwrap();
        let r = job.request();
        let slots = [
            r.in_fb0,
            r.in_fb1,
            r.in_fb2,
            r.out_dit_fb0,
            r.out_dit_fb1,
            r.out_tnr_fb0,
        ];
        for fb in slots {
            assert!(raw.contains(&fb.dma_buf_fd), "{mode:?}");
            assert_eq!(fb.buf.cb_addr, layout.chroma_offset);
            assert_eq!((fb.size.width, fb.size.height), (W, H));
        }
        assert_eq!((r.is_interlace, r.top_field_first, r.base_field), (1, 1, 0));
    }
    // The slots the kernel uses, mode by mode.
    let motion = Config::new(
        W,
        H,
        Mode::Motion {
            film_detect: false,
            tnr: Some(TNR),
        },
    )
    .unwrap();
    let r = Job::new(motion, false, Field::Bottom, &f[..2], &f[2..4])
        .unwrap()
        .request();
    assert_eq!(
        [r.in_fb0, r.in_fb1, r.out_dit_fb0, r.out_tnr_fb0].map(|fb| fb.dma_buf_fd),
        [raw[0], raw[1], raw[2], raw[3]]
    );
    assert_eq!((r.top_field_first, r.base_field), (0, 1));
    let field_rate = Config::new(
        W,
        H,
        Mode::MotionFieldRate {
            film_detect: false,
            tnr: Some(TNR),
        },
    )
    .unwrap();
    let r = Job::new(field_rate, true, Field::Top, &f[..3], &f[3..6])
        .unwrap()
        .request();
    assert_eq!(
        [
            r.in_fb0,
            r.in_fb1,
            r.in_fb2,
            r.out_dit_fb0,
            r.out_dit_fb1,
            r.out_tnr_fb0
        ]
        .map(|fb| fb.dma_buf_fd),
        [raw[0], raw[1], raw[2], raw[3], raw[4], raw[5]]
    );
    let noise = Config::new(W, H, Mode::NoiseOnly(TNR)).unwrap();
    let r = Job::new(noise, true, Field::Top, &f[..2], &f[2..3])
        .unwrap()
        .request();
    assert_eq!(
        [r.in_fb0, r.in_fb1, r.out_tnr_fb0].map(|fb| fb.dma_buf_fd),
        [raw[0], raw[1], raw[2]]
    );
}

#[test]
fn a_job_takes_the_modes_frames_at_the_configs_size() {
    let (fds, layout, _) = buffers(3, Format::Nv12);
    let f = frames(&fds, Format::Nv12, layout);
    let bob = Config::new(W, H, Mode::Bob).unwrap();
    assert!(Job::new(bob, true, Field::Top, &f[..1], &f[1..2]).is_ok());
    assert_eq!(
        Job::new(bob, true, Field::Top, &f[..2], &f[2..3]).unwrap_err(),
        Invalid::FrameCount
    );
    assert_eq!(
        Job::new(bob, true, Field::Top, &f[..1], &[]).unwrap_err(),
        Invalid::FrameCount
    );
    let smaller = Config::new(W, H - 4, Mode::Bob).unwrap();
    assert_eq!(
        Job::new(smaller, true, Field::Top, &f[..1], &f[1..2]).unwrap_err(),
        Invalid::FrameSize
    );
    // Noise reduction writes planar outputs only.
    let noise = Config::new(W, H, Mode::NoiseOnly(TNR)).unwrap();
    assert_eq!(
        Job::new(noise, true, Field::Top, &f[..2], &f[2..3]).unwrap_err(),
        Invalid::NotPlanar
    );
}
