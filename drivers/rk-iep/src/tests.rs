//! Jobs against every check, and the driver against a played device. No device node is opened.

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use crate::device::find_at;
use crate::mpp::{
    CMD_POLL_HW_FINISH, CMD_SET_REG_ADDR_OFFSET, CMD_SET_REG_READ, CMD_SET_REG_WRITE, DEVICE_IEP2,
    FLAG_LAST_MSG, FLAG_MULTI_MSG, FLAG_REG_FD_NO_TRANS, FLAG_REG_NO_OFFSET, FLAG_SCL_FD_NO_TRANS,
    FLAG_SECURE_MODE, OffsetElem,
};
use crate::{
    At, Buffer, Chroma, Data, Device, DilMode, FieldOrder, Frame, Iep, Input, Job, JobError,
    MAX_STRIDE, Mode, Output, PdComp, Results, RunError, Sampling, Slot, State, Swap, Tuning,
    backoff_ns, scratch_len,
};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const W: u32 = 1920;
const H: u32 = 1088;
const LUMA: u64 = W as u64 * H as u64;
const CHROMA_420: u64 = LUMA / 2;

fn memfd() -> OwnedFd {
    dma_buf::memfd(c"rk-iep-test").expect("memfd")
}

fn buf(fd: BorrowedFd<'_>, len: u64) -> Buffer<'_> {
    Buffer::with_len(fd, len).expect("buffer")
}

/// A semi-planar 4:2:0 frame with its planes in one dma-buf of exactly their size.
fn nv12(fd: BorrowedFd<'_>, stride: u32) -> Frame<'_> {
    let luma = u64::from(stride) * u64::from(H);
    let b = buf(fd, luma * 3 / 2);
    Frame {
        luma: At::new(b, 0),
        chroma: Chroma::Semi(At::new(b, luma)),
    }
}

fn input(fd: BorrowedFd<'_>) -> Input<'_> {
    Input {
        sampling: Sampling::Yuv420,
        swap: Swap::SemiUv,
        luma_stride: W,
        chroma_stride: W,
        cur: nv12(fd, W),
        next: nv12(fd, W),
        prev: nv12(fd, W),
    }
}

fn output(fd: BorrowedFd<'_>) -> Output<'_> {
    Output {
        sampling: Sampling::Yuv420,
        swap: Swap::SemiVu,
        stride: W,
        top: nv12(fd, W),
        bottom: nv12(fd, W),
    }
}

fn scratch(fd: BorrowedFd<'_>) -> At<'_> {
    At::new(buf(fd, scratch_len(W, H)), 0)
}

const MODE: Mode = Mode {
    dil: DilMode::I5O2,
    field_order: FieldOrder::BottomFirst,
    pd: PdComp::CurNext,
};

fn job(fd: BorrowedFd<'_>) -> Result<Job<'_>, JobError> {
    Job::new(
        W,
        H,
        &input(fd),
        &output(fd),
        scratch(fd),
        scratch(fd),
        MODE,
        &Tuning::default(),
    )
}

#[test]
fn a_job_fills_in_what_the_kernel_reads() {
    let fd = memfd();
    let raw = fd.as_fd().as_raw_fd_u32();
    let tuning = Tuning {
        md_lambda: 7,
        me_thr_offset: -3,
        osd_area_num: 8,
        mv_tru_list: [-1, 2, -3, 4, 0, 0, 0, 9],
        mtn_tab: Some([5; 16]),
        ..Tuning::default()
    };
    let job = Job::new(
        W,
        H,
        &input(fd.as_fd()),
        &output(fd.as_fd()),
        scratch(fd.as_fd()),
        scratch(fd.as_fd()),
        MODE,
        &tuning,
    )
    .expect("job");
    let p = job.params();
    assert_eq!((p.tile_cols, p.tile_rows), (120, 272));
    assert_eq!(
        (p.src_fmt, p.src_yuv_swap, p.dst_fmt, p.dst_yuv_swap),
        (3, 0, 3, 1)
    );
    assert_eq!(
        (p.src_y_stride, p.src_uv_stride, p.dst_y_stride),
        (480, 480, 480)
    );
    assert_eq!(
        (p.dil_mode, p.dil_out_mode, p.dil_field_order, p.pd_mode),
        (1, 0, 1, 1)
    );
    assert_eq!((p.md_lambda, p.me_thr_offset, p.osd_area_num), (7, -3, 8));
    assert_eq!(p.mv_tru_list, [-1, 2, -3, 4, 0, 0, 0, 9]);
    assert_eq!((p.mtn_en, p.mtn_tab), (1, [5; 16]));
    assert_eq!((p.roi_en, p.roi_layer_num), (0, 0));
    // Every placed word is the descriptor; the third plane of semi-planar frames, and the
    // outputs' third words, stay zero.
    assert_eq!(
        p.address_words(),
        [
            raw, raw, 0, raw, raw, 0, raw, raw, 0, raw, raw, 0, raw, raw, 0, raw, raw
        ]
    );
    let chroma = u32::try_from(LUMA).expect("offset");
    assert_eq!(
        job.offsets(),
        [
            (24, 0),
            (27, chroma),
            (25, 0),
            (29, chroma),
            (26, 0),
            (31, chroma),
            (44, 0),
            (46, chroma),
            (45, 0),
            (47, chroma),
            (33, 0),
            (34, 0),
        ]
        .map(|(index, offset)| OffsetElem { index, offset })
    );
}

trait RawU32 {
    fn as_raw_fd_u32(&self) -> u32;
}

impl RawU32 for BorrowedFd<'_> {
    fn as_raw_fd_u32(&self) -> u32 {
        use std::os::fd::AsRawFd;
        u32::try_from(self.as_raw_fd()).expect("descriptor")
    }
}

#[test]
fn the_call_carries_exact_sizes_and_never_raw_addresses() {
    let fd = memfd();
    let job = job(fd.as_fd()).expect("job");
    let messages = job.messages();
    let cmds = messages.map(|(m, _)| m.cmd);
    assert_eq!(
        cmds,
        [
            CMD_SET_REG_WRITE,
            CMD_SET_REG_ADDR_OFFSET,
            CMD_SET_REG_READ,
            CMD_POLL_HW_FINISH
        ]
    );
    assert_eq!(messages.map(|(m, _)| m.size), [656, 12 * 8, 412, 0]);
    assert_eq!(
        messages.map(|(_, d)| d),
        [Data::Params, Data::Offsets, Data::Results, Data::None]
    );
    for (i, (m, _)) in messages.iter().enumerate() {
        assert_eq!((m.offset, m.data_ptr), (0, 0));
        assert_ne!(m.flags & FLAG_MULTI_MSG, 0);
        assert_ne!(m.flags & FLAG_REG_NO_OFFSET, 0);
        assert_eq!(
            m.flags & (FLAG_REG_FD_NO_TRANS | FLAG_SCL_FD_NO_TRANS | FLAG_SECURE_MODE),
            0
        );
        assert_eq!(m.flags & FLAG_LAST_MSG != 0, i == 3);
    }
}

#[test]
fn a_frame_that_fills_its_buffer_exactly_is_taken() {
    let fd = memfd();
    let b = buf(fd.as_fd(), LUMA + CHROMA_420);
    let frame = Frame {
        luma: At::new(b, 0),
        chroma: Chroma::Semi(At::new(b, LUMA)),
    };
    let mut i = input(fd.as_fd());
    i.cur = frame;
    let o = output(fd.as_fd());
    Job::new(
        W,
        H,
        &i,
        &o,
        scratch(fd.as_fd()),
        scratch(fd.as_fd()),
        MODE,
        &Tuning::default(),
    )
    .expect("exact fit");
}

/// Every plane, one byte short in turn, is refused by name.
#[test]
fn every_plane_one_byte_short_is_refused() {
    let fd = memfd();
    let f = fd.as_fd();
    let short = |len: u64| buf(f, len - 1);
    // The plane alone in a buffer one byte short of it.
    fn luma_short<'a>(f: BorrowedFd<'a>, at: Frame<'a>) -> Frame<'a> {
        Frame {
            luma: At::new(buf(f, LUMA - 1), 0),
            chroma: at.chroma,
        }
    }
    fn chroma_short<'a>(f: BorrowedFd<'a>, at: Frame<'a>) -> Frame<'a> {
        Frame {
            luma: at.luma,
            chroma: Chroma::Semi(At::new(buf(f, CHROMA_420 - 1), 0)),
        }
    }
    let base_in = input(f);
    let base_out = output(f);
    type Case<'a> = (Slot, Input<'a>, Output<'a>, At<'a>, At<'a>);
    let mut cases: Vec<Case<'_>> = Vec::new();
    for slot in [
        Slot::CurY,
        Slot::NextY,
        Slot::PrevY,
        Slot::CurC,
        Slot::NextC,
        Slot::PrevC,
    ] {
        let mut i = base_in;
        let target = match slot {
            Slot::CurY | Slot::CurC => &mut i.cur,
            Slot::NextY | Slot::NextC => &mut i.next,
            _ => &mut i.prev,
        };
        *target = match slot {
            Slot::CurY | Slot::NextY | Slot::PrevY => luma_short(f, *target),
            _ => chroma_short(f, *target),
        };
        cases.push((slot, i, base_out, scratch(f), scratch(f)));
    }
    for slot in [Slot::TopY, Slot::TopC, Slot::BottomY, Slot::BottomC] {
        let mut o = base_out;
        let target = if matches!(slot, Slot::TopY | Slot::TopC) {
            &mut o.top
        } else {
            &mut o.bottom
        };
        *target = if matches!(slot, Slot::TopY | Slot::BottomY) {
            luma_short(f, *target)
        } else {
            chroma_short(f, *target)
        };
        cases.push((slot, base_in, o, scratch(f), scratch(f)));
    }
    let small = At::new(short(scratch_len(W, H)), 0);
    cases.push((Slot::Md, base_in, base_out, small, scratch(f)));
    cases.push((Slot::Mv, base_in, base_out, scratch(f), small));

    assert_eq!(cases.len(), 12);
    for (slot, i, o, md, mv) in cases {
        let refused = Job::new(W, H, &i, &o, md, mv, MODE, &Tuning::default());
        match refused {
            Err(JobError::OutOfBuffer { slot: s, end, len }) => {
                assert_eq!(s, slot);
                assert_eq!(end, len + 1, "{slot:?}");
            }
            other => panic!("{slot:?}: {other:?}"),
        }
    }
}

#[test]
fn planar_input_checks_both_chroma_planes_at_half_width() {
    let fd = memfd();
    let f = fd.as_fd();
    let half = u64::from(W / 2) * u64::from(H / 2);
    let frame = |cr_len: u64| Frame {
        luma: At::new(buf(f, LUMA), 0),
        chroma: Chroma::Planar {
            cb: At::new(buf(f, half), 0),
            cr: At::new(buf(f, cr_len), 0),
        },
    };
    let make = |cr_len: u64, chroma_stride: u32| Input {
        sampling: Sampling::Yuv420,
        swap: Swap::Planar,
        luma_stride: W,
        chroma_stride,
        cur: frame(cr_len),
        next: frame(half),
        prev: frame(half),
    };
    let o = output(f);
    let run = |i: &Input<'_>| {
        Job::new(
            W,
            H,
            i,
            &o,
            scratch(f),
            scratch(f),
            MODE,
            &Tuning::default(),
        )
        .map(|j| *j.params())
    };
    let params = run(&make(half, W / 2)).expect("planar");
    let words = params.address_words();
    assert!(words[..9].iter().all(|&w| w != 0), "{words:?}");
    assert_eq!(params.src_yuv_swap, 3);
    assert!(matches!(
        run(&make(half - 1, W / 2)),
        Err(JobError::OutOfBuffer {
            slot: Slot::CurCr,
            ..
        })
    ));
    assert!(matches!(
        run(&make(half, W / 2 - 4)),
        Err(JobError::Stride {
            slot: Slot::CurC,
            ..
        })
    ));
}

#[test]
fn offsets_past_the_buffer_or_overflowing_are_refused() {
    let fd = memfd();
    let f = fd.as_fd();
    let mut i = input(f);
    let o = output(f);
    let b = buf(f, u64::MAX);
    for (offset, want_end) in [(u64::MAX - 1, u64::MAX), (u64::from(u32::MAX) + 1, 0)] {
        i.cur.luma = At::new(b, offset);
        let refused = Job::new(
            W,
            H,
            &i,
            &o,
            scratch(f),
            scratch(f),
            MODE,
            &Tuning::default(),
        );
        match refused {
            Err(JobError::OutOfBuffer {
                slot: Slot::CurY,
                end,
                ..
            }) => {
                if want_end != 0 {
                    assert_eq!(end, want_end);
                }
            }
            other => panic!("offset {offset}: {other:?}"),
        }
    }
    // An offset that keeps the plane inside is taken and carried.
    let at = u64::from(u32::MAX) - LUMA;
    i.cur.luma = At::new(buf(f, u64::from(u32::MAX)), at);
    let job = Job::new(
        W,
        H,
        &i,
        &o,
        scratch(f),
        scratch(f),
        MODE,
        &Tuning::default(),
    )
    .expect("offset inside");
    assert_eq!(
        job.offsets()[0],
        OffsetElem {
            index: 24,
            offset: u32::try_from(at).expect("offset")
        }
    );
}

#[test]
fn strides_are_checked() {
    let fd = memfd();
    let f = fd.as_fd();
    let o = output(f);
    for (luma, chroma, out, slot) in [
        (W - 4, W, W, Slot::CurY),
        (W + 2, W, W, Slot::CurY),
        (MAX_STRIDE + 4, W, W, Slot::CurY),
        (W, W - 4, W, Slot::CurC),
        (W, W, W - 16, Slot::TopY),
        (W, W, W + 1, Slot::TopY),
    ] {
        let mut i = input(f);
        i.luma_stride = luma;
        i.chroma_stride = chroma;
        let mut o = o;
        o.stride = out;
        assert!(
            matches!(
                Job::new(W, H, &i, &o, scratch(f), scratch(f), MODE, &Tuning::default()),
                Err(JobError::Stride { slot: s, .. }) if s == slot
            ),
            "{luma} {chroma} {out}"
        );
    }
    // A wider stride is checked against the frame it makes: the default buffers hold only W.
    let mut i = input(f);
    i.chroma_stride = W + 64;
    assert!(matches!(
        Job::new(
            W,
            H,
            &i,
            &o,
            scratch(f),
            scratch(f),
            MODE,
            &Tuning::default()
        ),
        Err(JobError::OutOfBuffer {
            slot: Slot::CurC,
            ..
        })
    ));
    let wide = |fd| nv12(fd, W + 64);
    let i = Input {
        luma_stride: W + 64,
        chroma_stride: W + 64,
        cur: wide(f),
        next: wide(f),
        prev: wide(f),
        ..input(f)
    };
    let job = Job::new(
        W,
        H,
        &i,
        &o,
        scratch(f),
        scratch(f),
        MODE,
        &Tuning::default(),
    )
    .expect("wide stride");
    assert_eq!(job.params().src_y_stride, (W + 64) / 4);
}

#[test]
fn sizes_are_whole_tiles_inside_the_block() {
    let fd = memfd();
    let f = fd.as_fd();
    for (w, h) in [
        (0, H),
        (W, 0),
        (W + 16, H),
        (W, H + 4),
        (W - 8, H),
        (W, H - 2),
    ] {
        assert_eq!(
            Job::new(
                w,
                h,
                &input(f),
                &output(f),
                scratch(f),
                scratch(f),
                MODE,
                &Tuning::default()
            )
            .map(|_| ()),
            Err(JobError::Size {
                width: w,
                height: h
            })
        );
    }
    let small = Job::new(
        16,
        4,
        &input(f),
        &output(f),
        scratch(f),
        scratch(f),
        MODE,
        &Tuning::default(),
    )
    .expect("one tile");
    assert_eq!((small.params().tile_cols, small.params().tile_rows), (1, 1));
}

#[test]
fn layouts_must_match_and_the_output_is_semi_planar() {
    let fd = memfd();
    let f = fd.as_fd();
    let mut i = input(f);
    i.swap = Swap::Planar;
    i.chroma_stride = W / 2;
    assert_eq!(
        Job::new(
            W,
            H,
            &i,
            &output(f),
            scratch(f),
            scratch(f),
            MODE,
            &Tuning::default()
        )
        .map(|_| ()),
        Err(JobError::Layout { slot: Slot::CurY })
    );
    let mut o = output(f);
    o.swap = Swap::Planar;
    assert_eq!(
        Job::new(
            W,
            H,
            &input(f),
            &o,
            scratch(f),
            scratch(f),
            MODE,
            &Tuning::default()
        )
        .map(|_| ()),
        Err(JobError::Layout { slot: Slot::TopY })
    );
    let mut o = output(f);
    o.bottom.chroma = Chroma::Planar {
        cb: o.top.luma,
        cr: o.top.luma,
    };
    assert_eq!(
        Job::new(
            W,
            H,
            &input(f),
            &o,
            scratch(f),
            scratch(f),
            MODE,
            &Tuning::default()
        )
        .map(|_| ()),
        Err(JobError::Layout {
            slot: Slot::BottomY
        })
    );
}

#[test]
fn osd_areas_stop_at_eight() {
    let fd = memfd();
    let f = fd.as_fd();
    let tuning = Tuning {
        osd_area_num: 9,
        ..Tuning::default()
    };
    assert_eq!(
        Job::new(
            W,
            H,
            &input(f),
            &output(f),
            scratch(f),
            scratch(f),
            MODE,
            &tuning
        )
        .map(|_| ()),
        Err(JobError::OsdAreas(9))
    );
}

#[test]
fn descriptor_zero_is_refused() {
    let stdin = io::stdin();
    let error = Buffer::with_len(stdin.as_fd(), 1 << 30).expect_err("descriptor 0");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn a_buffer_is_measured_by_the_kernel() {
    let fd = dma_buf::sealed_memfd(c"rk-iep-measured", 12_288).expect("memfd");
    let b = Buffer::new(fd.as_fd()).expect("buffer");
    assert_eq!(b.len(), 12_288);
}

#[test]
fn building_and_running_jobs_allocates_nothing() {
    let fd = memfd();
    let (mut iep, log) = iep();
    let mut results = Results::default();
    iep.run(0, &job(fd.as_fd()).expect("job"), &mut results)
        .expect("first run opens");
    let ((), allocated) = allocations(|| {
        for now in 1..=1_000 {
            let job = job(fd.as_fd()).expect("job");
            iep.run(now, &job, &mut results).expect("run");
        }
    });
    assert_eq!(allocated, 0, "building and running a job");
    assert_eq!(log.runs.get(), 1_001);
    assert_eq!(results.dect_osd_cnt, 1_001);
}

/// What the played device saw, and what it is to fail next.
#[derive(Default)]
struct Log {
    opens: Cell<u32>,
    clients: RefCell<Vec<u32>>,
    runs: Cell<u32>,
    releases: Cell<u32>,
    fail_open: Cell<Option<i32>>,
    fail_init: Cell<Option<i32>>,
    fail_run: Cell<Option<i32>>,
    fail_release: Cell<Option<i32>>,
}

struct Played(Rc<Log>);

fn failed(errno: &Cell<Option<i32>>) -> io::Result<()> {
    errno
        .take()
        .map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
}

impl Device for Played {
    fn init_client(&mut self, device_type: u32) -> io::Result<()> {
        failed(&self.0.fail_init)?;
        self.0.clients.borrow_mut().push(device_type);
        Ok(())
    }

    fn run(&mut self, job: &Job<'_>, results: &mut Results) -> io::Result<()> {
        failed(&self.0.fail_run)?;
        assert_eq!(job.messages()[2].0.size as usize, size_of::<Results>());
        self.0.runs.set(self.0.runs.get() + 1);
        results.dect_osd_cnt = self.0.runs.get();
        Ok(())
    }

    fn release_buffers(&mut self) -> io::Result<()> {
        failed(&self.0.fail_release)?;
        self.0.releases.set(self.0.releases.get() + 1);
        Ok(())
    }
}

type PlayedIep = Iep<Played, Box<dyn FnMut() -> io::Result<Played>>>;

fn iep() -> (PlayedIep, Rc<Log>) {
    let log = Rc::new(Log::default());
    let held = Rc::clone(&log);
    let open: Box<dyn FnMut() -> io::Result<Played>> = Box::new(move || {
        failed(&held.fail_open)?;
        held.opens.set(held.opens.get() + 1);
        Ok(Played(Rc::clone(&held)))
    });
    (Iep::new(open), log)
}

#[test]
fn the_service_opens_on_the_first_job_and_binds_to_the_iep() {
    let fd = memfd();
    let (mut iep, log) = iep();
    assert_eq!(iep.state(), State::Closed);
    assert_eq!(log.opens.get(), 0);
    let job = job(fd.as_fd()).expect("job");
    iep.run(5, &job, &mut Results::default()).expect("run");
    iep.run(6, &job, &mut Results::default()).expect("run");
    assert_eq!(iep.state(), State::Running);
    assert_eq!(log.opens.get(), 1);
    assert_eq!(*log.clients.borrow(), [DEVICE_IEP2]);
    iep.release_buffers(7).expect("release");
    assert_eq!(log.releases.get(), 1);
}

#[test]
fn a_refused_job_keeps_the_file_open() {
    let fd = memfd();
    let (mut iep, log) = iep();
    let job = job(fd.as_fd()).expect("job");
    log.fail_run.set(Some(libc::EINVAL));
    assert!(matches!(
        iep.run(1, &job, &mut Results::default()),
        Err(RunError::Refused(_))
    ));
    assert_eq!(iep.state(), State::Running);
    iep.run(2, &job, &mut Results::default()).expect("run");
    assert_eq!(log.opens.get(), 1);
}

#[test]
fn failures_back_off_doubling_to_eight_seconds_and_never_give_up() {
    let fd = memfd();
    let (mut iep, log) = iep();
    let job = job(fd.as_fd()).expect("job");
    let mut now = 1_000;
    for attempt in 0..10u32 {
        log.fail_open.set(Some(libc::ENODEV));
        let Err(RunError::Device { retry_at_ns, .. }) = iep.run(now, &job, &mut Results::default())
        else {
            panic!("attempt {attempt} did not fail");
        };
        assert_eq!(retry_at_ns - now, backoff_ns(attempt));
        assert!(retry_at_ns - now <= 8_000_000_000);
        assert!(matches!(
            iep.run(retry_at_ns - 1, &job, &mut Results::default()),
            Err(RunError::BackingOff { .. })
        ));
        now = retry_at_ns;
    }
    assert_eq!(backoff_ns(9), 8_000_000_000);
    assert_eq!(log.opens.get(), 0);
    iep.run(now, &job, &mut Results::default())
        .expect("recovers");
    assert_eq!(iep.state(), State::Running);
    // A job that runs clears the backoff.
    log.fail_run.set(Some(libc::ETIMEDOUT));
    let Err(RunError::Device { retry_at_ns, .. }) = iep.run(now, &job, &mut Results::default())
    else {
        panic!("run did not fail");
    };
    assert_eq!(retry_at_ns - now, backoff_ns(0));
}

#[test]
fn a_failed_bind_or_release_closes_and_backs_off() {
    let fd = memfd();
    let (mut iep, log) = iep();
    let job = job(fd.as_fd()).expect("job");
    log.fail_init.set(Some(libc::EINVAL));
    assert!(matches!(
        iep.run(0, &job, &mut Results::default()),
        Err(RunError::Device { .. })
    ));
    assert!(matches!(iep.state(), State::BackingOff { failures: 1, .. }));
    iep.run(backoff_ns(0), &job, &mut Results::default())
        .expect("reopens");
    log.fail_release.set(Some(libc::EIO));
    assert!(iep.release_buffers(1 << 40).is_err());
    assert!(matches!(iep.state(), State::BackingOff { .. }));
    iep.stop();
    assert_eq!(iep.state(), State::Closed);
    assert!(iep.release_buffers(0).is_ok());
    assert_eq!(log.opens.get(), 2);
}

#[test]
fn the_node_is_found_when_present() {
    assert_eq!(find_at(Path::new("/nonexistent/mpp_service")), None);
    let here = std::env::current_dir().expect("cwd");
    assert_eq!(find_at(&here), Some(PathBuf::from(&here)));
}
