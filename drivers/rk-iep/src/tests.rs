//! Jobs against every check, and run on a claim of the played kernel, which does what
//! `mpp_iep2.c` does with the messages. No device node is opened.

use std::cell::RefCell;
use std::io;
use std::mem::offset_of;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::rc::Rc;

use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{
    Block, BufferId, Device, Message, OwnedClaim, Service, Session, SubmitError,
};
use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use crate::{
    At, Buffer, Chroma, DilMode, FieldOrder, Frame, Input, Job, JobError, MAX_STRIDE, Mode, Output,
    PARAMS_BYTES, Params, PdComp, RESULTS_BYTES, SLOTS, Sampling, Slot, Swap, TUNING_BYTES,
    TUNING_START, Tuning, scratch_len,
};

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

const W: u32 = 1920;
const H: u32 = 1088;
const LUMA: u64 = W as u64 * H as u64;
const CHROMA_420: u64 = LUMA / 2;

// ---------------------------------------------------------------------------------------------
// A played kernel

struct Played(Rc<RefCell<Kernel>>);

impl Session for Played {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.0.borrow_mut().call(messages)
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        let n = into.len();
        into.copy_from_slice(&self.0.borrow().areas[slot][..n]);
    }
}

struct PlayedDevice(Rc<RefCell<Kernel>>);

impl Device for PlayedDevice {
    type Session = Played;

    fn open(&mut self) -> io::Result<Played> {
        let mut kernel = self.0.borrow_mut();
        kernel.opens += 1;
        kernel.client = None;
        Ok(Played(Rc::clone(&self.0)))
    }

    fn confined(&mut self, _block: Block) -> io::Result<bool> {
        Ok(self.0.borrow().confined)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

fn kernel() -> Rc<RefCell<Kernel>> {
    let client = Block::Iep2.facts().client;
    Rc::new(RefCell::new(Kernel::new(1 << client)))
}

/// A claim on the IEP2 of the played kernel.
fn claim(kernel: &Rc<RefCell<Kernel>>) -> OwnedClaim<PlayedDevice> {
    Box::new(Service::new(PlayedDevice(Rc::clone(kernel))))
        .claim_owned(Block::Iep2, 0)
        .map_err(|(_, e)| e)
        .expect("claim")
}

fn memfd(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"rk-iep-test", len).expect("memfd")
}

/// A buffer the claim holds, and its descriptor as the kernel sees it.
fn held(claim: &mut OwnedClaim<PlayedDevice>, len: u64) -> (Buffer, u32) {
    let file = memfd(len);
    let fd = u32::try_from(file.as_raw_fd()).expect("descriptor");
    let id = claim.add_buffer(file).expect("buffer");
    assert_eq!(claim.buffer_size(id), Some(len));
    (Buffer { id, len }, fd)
}

/// Buffer ids for jobs that are only built: one claim's, of buffers of a page.
fn ids() -> [BufferId; 4] {
    let kernel = kernel();
    let mut claim = claim(&kernel);
    std::array::from_fn(|_| held(&mut claim, 4096).0.id)
}

fn buf(len: u64) -> Buffer {
    Buffer { id: ids()[0], len }
}

/// A semi-planar 4:2:0 frame with its planes in one buffer of exactly their size.
fn nv12_in(b: Buffer, stride: u32) -> Frame {
    let luma = u64::from(stride) * u64::from(H);
    Frame {
        luma: At::new(b, 0),
        chroma: Chroma::Semi(At::new(b, luma)),
    }
}

fn nv12(stride: u32) -> Frame {
    nv12_in(buf(u64::from(stride) * u64::from(H) * 3 / 2), stride)
}

fn input() -> Input {
    Input {
        sampling: Sampling::Yuv420,
        swap: Swap::SemiUv,
        luma_stride: W,
        chroma_stride: W,
        cur: nv12(W),
        next: nv12(W),
        prev: nv12(W),
    }
}

fn output() -> Output {
    Output {
        sampling: Sampling::Yuv420,
        swap: Swap::SemiVu,
        stride: W,
        top: nv12(W),
        bottom: nv12(W),
    }
}

fn scratch() -> At {
    At::new(buf(scratch_len(W, H)), 0)
}

const MODE: Mode = Mode {
    dil: DilMode::I5O2,
    field_order: FieldOrder::BottomFirst,
    pd: PdComp::CurNext,
};

const NO_TUNING: [u8; TUNING_BYTES] = [0; TUNING_BYTES];

fn build(input: &Input, output: &Output, md: At, mv: At) -> Result<Job, JobError> {
    Job::new(W, H, input, output, md, mv, MODE, &NO_TUNING)
}

/// The native-endian word at byte `at`.
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("word"))
}

// ---------------------------------------------------------------------------------------------
// Building a job

#[test]
fn a_job_fills_in_what_the_kernel_reads() {
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
        &input(),
        &output(),
        scratch(),
        scratch(),
        MODE,
        &tuning.encode(),
    )
    .expect("job");
    let p = job.params();
    let at = |at: usize| word(p, at);
    assert_eq!((at(16), at(20)), (120, 272));
    assert_eq!(
        (
            at(offset_of!(Params, src_fmt)),
            at(offset_of!(Params, src_yuv_swap)),
            at(offset_of!(Params, dst_fmt)),
            at(offset_of!(Params, dst_yuv_swap))
        ),
        (3, 0, 3, 1)
    );
    assert_eq!((at(24), at(28), at(32)), (480, 480, 480));
    assert_eq!(
        (
            at(offset_of!(Params, dil_mode)),
            at(offset_of!(Params, dil_out_mode)),
            at(offset_of!(Params, dil_field_order)),
            at(offset_of!(Params, pd_mode))
        ),
        (1, 0, 1, 1)
    );
    assert_eq!(at(offset_of!(Params, md_lambda)), 7);
    assert_eq!(
        at(offset_of!(Params, me_thr_offset)),
        (-3i32).cast_unsigned()
    );
    assert_eq!(at(offset_of!(Params, osd_area_num)), 8);
    let tru = offset_of!(Params, mv_tru_list);
    assert_eq!(
        p[tru..tru + 8],
        [-1i8, 2, -3, 4, 0, 0, 0, 9].map(i8::cast_unsigned)
    );
    assert_eq!(at(offset_of!(Params, mtn_en)), 1);
    assert_eq!(at(offset_of!(Params, mtn_tab) + 60), 5);
    assert_eq!(at(offset_of!(Params, roi_en)), 0);
    // The tuning item is the structure from md_theta to pd_mode, nothing more.
    assert_eq!(
        &p[TUNING_START..TUNING_START + TUNING_BYTES],
        &tuning.encode()
    );
    // No descriptor word holds anything: the claim writes them.
    for slot in Slot::ALL {
        assert_eq!(at(slot.word()), 0, "{slot:?}");
    }
    // Every slot has its plane, in slot order; a semi-planar frame's Cr word holds its chroma.
    let chroma = u32::try_from(LUMA).expect("offset");
    let offsets: Vec<_> = job.addresses().iter().map(|a| (a.word, a.offset)).collect();
    assert_eq!(
        offsets,
        [
            (36, 0),
            (40, chroma),
            (44, chroma),
            (48, 0),
            (52, chroma),
            (56, chroma),
            (60, 0),
            (64, chroma),
            (68, chroma),
            (72, 0),
            (76, chroma),
            (84, 0),
            (88, chroma),
            (96, 0),
            (100, 0),
        ]
    );
    assert_eq!(PARAMS_BYTES, p.len());
}

#[test]
fn a_frame_that_fills_its_buffer_exactly_is_taken() {
    let mut i = input();
    i.cur = nv12_in(buf(LUMA + CHROMA_420), W);
    build(&i, &output(), scratch(), scratch()).expect("exact fit");
}

/// Every plane, one byte short in turn, is refused by name.
#[test]
fn every_plane_one_byte_short_is_refused() {
    let luma_short = |frame: Frame| Frame {
        luma: At::new(buf(LUMA - 1), 0),
        chroma: frame.chroma,
    };
    let chroma_short = |frame: Frame| Frame {
        luma: frame.luma,
        chroma: Chroma::Semi(At::new(buf(CHROMA_420 - 1), 0)),
    };
    let mut cases: Vec<(Slot, Input, Output, At, At)> = Vec::new();
    for slot in [
        Slot::CurY,
        Slot::NextY,
        Slot::PrevY,
        Slot::CurC,
        Slot::NextC,
        Slot::PrevC,
    ] {
        let mut i = input();
        let target = match slot {
            Slot::CurY | Slot::CurC => &mut i.cur,
            Slot::NextY | Slot::NextC => &mut i.next,
            _ => &mut i.prev,
        };
        *target = match slot {
            Slot::CurY | Slot::NextY | Slot::PrevY => luma_short(*target),
            _ => chroma_short(*target),
        };
        cases.push((slot, i, output(), scratch(), scratch()));
    }
    for slot in [Slot::TopY, Slot::TopC, Slot::BottomY, Slot::BottomC] {
        let mut o = output();
        let target = if matches!(slot, Slot::TopY | Slot::TopC) {
            &mut o.top
        } else {
            &mut o.bottom
        };
        *target = if matches!(slot, Slot::TopY | Slot::BottomY) {
            luma_short(*target)
        } else {
            chroma_short(*target)
        };
        cases.push((slot, input(), o, scratch(), scratch()));
    }
    let small = At::new(buf(scratch_len(W, H) - 1), 0);
    cases.push((Slot::Md, input(), output(), small, scratch()));
    cases.push((Slot::Mv, input(), output(), scratch(), small));

    assert_eq!(cases.len(), 12);
    for (slot, i, o, md, mv) in cases {
        match build(&i, &o, md, mv) {
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
    let half = u64::from(W / 2) * u64::from(H / 2);
    let [y, cb, cr, _] = ids();
    let frame = |cr_len: u64| Frame {
        luma: At::new(Buffer { id: y, len: LUMA }, 0),
        chroma: Chroma::Planar {
            cb: At::new(Buffer { id: cb, len: half }, 0),
            cr: At::new(
                Buffer {
                    id: cr,
                    len: cr_len,
                },
                0,
            ),
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
    let job = build(&make(half, W / 2), &output(), scratch(), scratch()).expect("planar");
    assert_eq!(word(job.params(), offset_of!(Params, src_yuv_swap)), 3);
    let a = job.addresses();
    assert_eq!((a[0].buffer, a[1].buffer, a[2].buffer), (y, cb, cr));
    assert!(matches!(
        build(&make(half - 1, W / 2), &output(), scratch(), scratch()),
        Err(JobError::OutOfBuffer {
            slot: Slot::CurCr,
            ..
        })
    ));
    assert!(matches!(
        build(&make(half, W / 2 - 4), &output(), scratch(), scratch()),
        Err(JobError::Stride {
            slot: Slot::CurC,
            ..
        })
    ));
}

#[test]
fn offsets_past_the_buffer_or_overflowing_are_refused() {
    let mut i = input();
    let b = buf(u64::MAX);
    for (offset, want_end) in [(u64::MAX - 1, u64::MAX), (u64::from(u32::MAX) + 1, 0)] {
        i.cur.luma = At::new(b, offset);
        match build(&i, &output(), scratch(), scratch()) {
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
    i.cur.luma = At::new(buf(u64::from(u32::MAX)), at);
    let job = build(&i, &output(), scratch(), scratch()).expect("offset inside");
    assert_eq!(
        (job.addresses()[0].word, job.addresses()[0].offset),
        (36, u32::try_from(at).expect("offset"))
    );
}

#[test]
fn strides_are_checked() {
    for (luma, chroma, out, slot) in [
        (W - 4, W, W, Slot::CurY),
        (W + 2, W, W, Slot::CurY),
        (MAX_STRIDE + 4, W, W, Slot::CurY),
        (W, W - 4, W, Slot::CurC),
        (W, W, W - 16, Slot::TopY),
        (W, W, W + 1, Slot::TopY),
    ] {
        let mut i = input();
        i.luma_stride = luma;
        i.chroma_stride = chroma;
        let mut o = output();
        o.stride = out;
        assert!(
            matches!(
                build(&i, &o, scratch(), scratch()),
                Err(JobError::Stride { slot: s, .. }) if s == slot
            ),
            "{luma} {chroma} {out}"
        );
    }
    // A wider stride is checked against the frame it makes: the default buffers hold only W.
    let mut i = input();
    i.chroma_stride = W + 64;
    assert!(matches!(
        build(&i, &output(), scratch(), scratch()),
        Err(JobError::OutOfBuffer {
            slot: Slot::CurC,
            ..
        })
    ));
    let i = Input {
        luma_stride: W + 64,
        chroma_stride: W + 64,
        cur: nv12(W + 64),
        next: nv12(W + 64),
        prev: nv12(W + 64),
        ..input()
    };
    let job = build(&i, &output(), scratch(), scratch()).expect("wide stride");
    assert_eq!(word(job.params(), 24), (W + 64) / 4);
}

#[test]
fn sizes_are_whole_tiles_inside_the_block() {
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
                &input(),
                &output(),
                scratch(),
                scratch(),
                MODE,
                &NO_TUNING
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
        &input(),
        &output(),
        scratch(),
        scratch(),
        MODE,
        &NO_TUNING,
    )
    .expect("one tile");
    assert_eq!((word(small.params(), 16), word(small.params(), 20)), (1, 1));
}

#[test]
fn layouts_must_match_and_the_output_is_semi_planar() {
    let mut i = input();
    i.swap = Swap::Planar;
    i.chroma_stride = W / 2;
    assert_eq!(
        build(&i, &output(), scratch(), scratch()).map(|_| ()),
        Err(JobError::Layout { slot: Slot::CurY })
    );
    let mut o = output();
    o.swap = Swap::Planar;
    assert_eq!(
        build(&input(), &o, scratch(), scratch()).map(|_| ()),
        Err(JobError::Layout { slot: Slot::TopY })
    );
    let mut o = output();
    o.bottom.chroma = Chroma::Planar {
        cb: o.top.luma,
        cr: o.top.luma,
    };
    assert_eq!(
        build(&input(), &o, scratch(), scratch()).map(|_| ()),
        Err(JobError::Layout {
            slot: Slot::BottomY
        })
    );
}

#[test]
fn osd_areas_stop_at_eight() {
    let tuning = Tuning {
        osd_area_num: 9,
        ..Tuning::default()
    };
    assert_eq!(
        Job::new(
            W,
            H,
            &input(),
            &output(),
            scratch(),
            scratch(),
            MODE,
            &tuning.encode()
        )
        .map(|_| ()),
        Err(JobError::OsdAreas(9))
    );
}

// ---------------------------------------------------------------------------------------------
// On a claim

/// Buffers on `claim` for a whole job at the usual size: one for the frames, in and out, and
/// one for each scratch buffer. With the descriptors the kernel sees.
struct Held {
    frames: (Buffer, u32),
    md: (Buffer, u32),
    mv: (Buffer, u32),
}

const FRAME: u64 = LUMA + CHROMA_420;

fn hold(claim: &mut OwnedClaim<PlayedDevice>) -> Held {
    Held {
        frames: held(claim, 5 * FRAME),
        md: held(claim, scratch_len(W, H)),
        mv: held(claim, scratch_len(W, H)),
    }
}

/// The usual job on `held`: the five frames one after another in one buffer.
fn held_job(held: &Held) -> Job {
    let b = held.frames.0;
    let frame = |n: u64| Frame {
        luma: At::new(b, n * FRAME),
        chroma: Chroma::Semi(At::new(b, n * FRAME + LUMA)),
    };
    // Built field by field: the usual fixtures take buffer ids from a claim of their own.
    let i = Input {
        sampling: Sampling::Yuv420,
        swap: Swap::SemiUv,
        luma_stride: W,
        chroma_stride: W,
        cur: frame(0),
        next: frame(1),
        prev: frame(2),
    };
    let o = Output {
        sampling: Sampling::Yuv420,
        swap: Swap::SemiVu,
        stride: W,
        top: frame(3),
        bottom: frame(4),
    };
    build(&i, &o, At::new(held.md.0, 0), At::new(held.mv.0, 0)).expect("job")
}

#[test]
fn a_job_runs_on_the_claim_with_every_descriptor_word_a_held_plane() {
    let kernel = kernel();
    let mut claim = claim(&kernel);
    let held = hold(&mut claim);
    for (i, byte) in kernel.borrow_mut().output.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let job = held_job(&held);
    claim.submit_params(&job.param_job(5)).expect("submit");
    {
        let k = kernel.borrow();
        let task = &k.tasks[k.head];
        assert!(task.iep2);
        let mut bytes = [0u8; PARAMS_BYTES];
        for (b, r) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(&task.regs) {
            *b = r.to_ne_bytes();
        }
        // Each descriptor word is the plane's address: its buffer's, plus its offset.
        let frames = iova(held.frames.1);
        let chroma = |n: u64| frames + u32::try_from(n * FRAME + LUMA).expect("offset");
        let luma = |n: u64| frames + u32::try_from(n * FRAME).expect("offset");
        let expect = [
            (Slot::CurY, luma(0)),
            (Slot::CurC, chroma(0)),
            (Slot::CurCr, chroma(0)),
            (Slot::NextY, luma(1)),
            (Slot::NextC, chroma(1)),
            (Slot::NextCr, chroma(1)),
            (Slot::PrevY, luma(2)),
            (Slot::PrevC, chroma(2)),
            (Slot::PrevCr, chroma(2)),
            (Slot::TopY, luma(3)),
            (Slot::TopC, chroma(3)),
            (Slot::BottomY, luma(4)),
            (Slot::BottomC, chroma(4)),
            (Slot::Mv, iova(held.mv.1)),
            (Slot::Md, iova(held.md.1)),
        ];
        assert_eq!(expect.len(), SLOTS);
        for (slot, address) in expect {
            assert_eq!(word(&bytes, slot.word()), address, "{slot:?}");
        }
        // The outputs' third words stay 0, and the caller's settings are as they were.
        assert_eq!(word(&bytes, 80), 0);
        assert_eq!(word(&bytes, 92), 0);
        assert_eq!(word(&bytes, 16), 120);
        assert_eq!(word(&bytes, 104), MODE.dil as u32);
    }
    let mut results = [0u32; RESULTS_BYTES / 4];
    let done = claim.complete(&mut results).expect("complete");
    assert_eq!((done.identity, done.registers), (5, RESULTS_BYTES / 4));
    assert_eq!(results[0], u32::from_ne_bytes([0, 1, 2, 3]));
}

#[test]
fn a_plane_checked_here_is_one_the_claim_takes() {
    // A buffer one byte short of a frame is refused here, by name; the claim would take its
    // offset, which is inside, and the block would read past the end.
    let kernel = kernel();
    let mut claim = claim(&kernel);
    let mut short = hold(&mut claim);
    short.frames.0.len -= 1;
    let b = short.frames.0;
    let frame = |n: u64| Frame {
        luma: At::new(b, n * FRAME),
        chroma: Chroma::Semi(At::new(b, n * FRAME + LUMA)),
    };
    let o = Output {
        top: frame(3),
        bottom: frame(4),
        ..output()
    };
    let i = Input {
        cur: frame(0),
        next: frame(1),
        prev: frame(2),
        ..input()
    };
    assert!(matches!(
        build(&i, &o, At::new(short.md.0, 0), At::new(short.mv.0, 0)),
        Err(JobError::OutOfBuffer {
            slot: Slot::BottomC,
            ..
        })
    ));
    assert_eq!(kernel.borrow().queued, 0);

    // A job naming a buffer the claim doesn't hold is refused by the claim, and nothing is
    // queued.
    let job = held_job(&hold(&mut claim));
    // A buffer the claim held once and has given back.
    let (gone, _) = held(&mut claim, 4096);
    drop(claim.remove_buffer(gone.id).expect("remove"));
    let other = gone.id;
    let mut addresses = *job.addresses();
    addresses[0].buffer = other;
    let mut stale = job.param_job(1);
    stale.addresses = &addresses;
    let refused = claim.submit_params(&stale);
    assert!(matches!(refused, Err(SubmitError::Job(_))), "{refused:?}");
    assert_eq!(kernel.borrow().queued, 0);
}

#[test]
fn building_and_running_jobs_allocates_nothing() {
    let kernel = kernel();
    let mut claim = claim(&kernel);
    let held = hold(&mut claim);
    let mut results = [0u32; RESULTS_BYTES / 4];
    claim
        .submit_params(&held_job(&held).param_job(0))
        .expect("first job");
    claim.complete(&mut results).expect("first job");
    let ((), allocated) = allocations(|| {
        for identity in 1..=1_000 {
            let job = held_job(&held);
            claim
                .submit_params(&job.param_job(identity))
                .expect("submit");
            claim.complete(&mut results).expect("complete");
        }
    });
    assert_eq!(allocated, 0, "building and running a job");
    assert_eq!(kernel.borrow().polls, 1_001);
}
