//! Jobs checked against buffers of known sizes, one test per bound; the descriptors a plan makes;
//! and the claim against a played-back session.

use std::cell::RefCell;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::rc::Rc;
use std::time::Duration;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use crate::uapi::{DDR_END, drm, pixel, turn};
use crate::*;

fn id(slot: u8) -> BufferId {
    BufferId {
        slot,
        generation: 1,
    }
}

/// Buffers 0 to 3 of these sizes; anything else is not held.
fn sizes(id: BufferId) -> Option<u64> {
    const SIZES: [u64; 4] = [1 << 20, 1 << 20, 1 << 20, 64];
    (id.generation == 1)
        .then(|| SIZES.get(usize::from(id.slot)).copied())
        .flatten()
}

fn plane(slot: u8, offset: u64, pitch: u32) -> Plane {
    Plane {
        buffer: id(slot),
        offset,
        pitch,
    }
}

fn frame(format: Format, width: u32, height: u32, planes: &[Plane]) -> Frame<'_> {
    Frame {
        format,
        width,
        height,
        planes,
    }
}

fn job<'a>(source: Frame<'a>, destination: Frame<'a>, rotation: Rotation) -> Job<'a> {
    Job {
        identity: 42,
        source,
        destination,
        rotation,
    }
}

fn turn(turn: Turn) -> Rotation {
    Rotation {
        turn,
        ..Rotation::default()
    }
}

fn check(job: &Job<'_>) -> Result<Plan, JobError> {
    job::plan(job, sizes)
}

/// ARGB8888 64x32 from buffer 0 into buffer 1, turned a quarter: 32 wide, 64 tall.
fn argb_turned<'a>(src: &'a [Plane], dst: &'a [Plane]) -> Job<'a> {
    job(
        frame(Format::Argb8888, 64, 32, src),
        frame(Format::Argb8888, 32, 64, dst),
        turn(Turn::R90),
    )
}

#[test]
fn rotations_are_one_turn_and_any_mirroring() {
    for bits in [
        drm::ROTATE_0,
        drm::ROTATE_90 | drm::REFLECT_X,
        drm::ROTATE_180 | drm::REFLECT_Y,
        drm::ROTATE_270 | drm::REFLECT_X | drm::REFLECT_Y,
    ] {
        let r = Rotation::from_drm(bits).expect("valid");
        assert_eq!(r.drm(), bits);
    }
    for bits in [
        0,
        drm::REFLECT_X,
        drm::ROTATE_0 | drm::ROTATE_90,
        drm::ROTATE_90 | drm::ROTATE_270,
        drm::ROTATE_0 | 1 << 6,
        u32::MAX,
    ] {
        assert_eq!(Rotation::from_drm(bits), None, "{bits:#x}");
    }
    assert_eq!(turn(Turn::R270).code(), turn::R270);
    assert!(turn(Turn::R90).swaps() && !turn(Turn::R180).swaps());
}

#[test]
fn the_formats_are_the_drivers() {
    for f in Format::ALL {
        assert_eq!(Format::from_fourcc(f.fourcc()), Some(f));
    }
    assert_eq!(Format::from_fourcc(u32::from_le_bytes(*b"YUYV")), None);
    assert_eq!(Format::Rgb888.element(0), (3, pixel::BITS_24));
    assert_eq!(Format::Nv12.element(1), (2, pixel::BITS_16));
    assert_eq!(Format::Nv12.subsampling(1), (2, 2));
    assert_eq!(Format::Nv61.subsampling(1), (2, 1));
    assert_eq!(Format::Nv16.planes(), 2);
    assert_eq!(Format::Argb1555.planes(), 1);
}

#[test]
fn a_checked_job_spans_exactly_what_the_engine_touches() {
    let (s, d) = ([plane(0, 100, 256)], [plane(1, 0, 128)]);
    let plan = check(&argb_turned(&s, &d)).expect("valid");
    assert_eq!(plan.size(0), Some((64, 32)));
    // 32 rows of 256 bytes, the last 256 long: 100 + 31 * 256 + 256.
    assert_eq!(plan.source(0).unwrap().end, 100 + 32 * 256);
    // Turned: 64 rows of 32 * 4 = 128 bytes.
    assert_eq!(plan.destination(0).unwrap().end, 64 * 128);
    assert_eq!(plan.source(1), None);
}

#[test]
fn a_size_must_fit_the_descriptor() {
    for (w, h) in [(0, 32), (64, 0), (MAX_FIELD + 1, 32), (64, MAX_FIELD + 1)] {
        let (s, d) = ([plane(0, 0, 65535)], [plane(1, 0, 65535)]);
        let j = job(
            frame(Format::Rgb565, w, h, &s),
            frame(Format::Rgb565, w, h, &d),
            turn(Turn::R0),
        );
        assert_eq!(check(&j), Err(JobError::Size { source: true }), "{w}x{h}");
    }
    let (s, d) = ([plane(0, 0, 128)], [plane(1, 0, 128)]);
    let j = job(
        frame(Format::Rgb565, 64, 32, &s),
        frame(Format::Rgb565, 64, 0, &d),
        turn(Turn::R0),
    );
    assert_eq!(check(&j), Err(JobError::Size { source: false }));
}

#[test]
fn the_destination_is_the_source_turned_in_the_same_format() {
    let (s, d) = ([plane(0, 0, 256)], [plane(1, 0, 256)]);
    // Not swapped for a quarter turn.
    let j = job(
        frame(Format::Argb8888, 64, 32, &s),
        frame(Format::Argb8888, 64, 32, &d),
        turn(Turn::R90),
    );
    assert_eq!(check(&j), Err(JobError::Turned));
    // Swapped for a half turn.
    let j = job(
        frame(Format::Argb8888, 64, 32, &s),
        frame(Format::Argb8888, 32, 64, &d),
        turn(Turn::R180),
    );
    assert_eq!(check(&j), Err(JobError::Turned));
    let j = job(
        frame(Format::Argb8888, 64, 32, &s),
        frame(Format::Rgb565, 32, 64, &d),
        turn(Turn::R90),
    );
    assert_eq!(check(&j), Err(JobError::Format));
}

#[test]
fn every_plane_of_the_format_and_no_more() {
    let one = [plane(0, 0, 64)];
    let two = [plane(1, 0, 64), plane(1, 4096, 64)];
    let j = job(
        frame(Format::Nv12, 64, 32, &one),
        frame(Format::Nv12, 64, 32, &two),
        turn(Turn::R0),
    );
    assert_eq!(check(&j), Err(JobError::Planes { source: true }));
    let three = [plane(1, 0, 64), plane(1, 4096, 64), plane(1, 8192, 64)];
    let src = [plane(0, 0, 64), plane(0, 4096, 64)];
    let j = job(
        frame(Format::Nv12, 64, 32, &src),
        frame(Format::Nv12, 64, 32, &three),
        turn(Turn::R0),
    );
    assert_eq!(check(&j), Err(JobError::Planes { source: false }));
}

#[test]
fn every_plane_names_a_held_buffer() {
    let (s, d) = ([plane(0, 0, 256)], [plane(1, 0, 128)]);
    let mut stale = argb_turned(&s, &d);
    let gone = [Plane {
        buffer: BufferId {
            slot: 1,
            generation: 2,
        },
        ..d[0]
    }];
    stale.destination.planes = &gone;
    assert_eq!(
        check(&stale),
        Err(JobError::UnknownBuffer {
            source: false,
            plane: 0
        })
    );
    let forged = [plane(200, 0, 256)];
    let j = argb_turned(&forged, &d);
    assert_eq!(
        check(&j),
        Err(JobError::UnknownBuffer {
            source: true,
            plane: 0
        })
    );
}

#[test]
fn a_stride_must_fit_the_descriptor_and_hold_a_row() {
    // Source rows are 64 * 4 = 256 bytes; turned destination rows are 32 * 4 = 128.
    for (src, dst, want) in [
        (0, 128, (true, 0, 256)),
        (255, 128, (true, 255, 256)),
        (MAX_FIELD + 1, 128, (true, MAX_FIELD + 1, 256)),
        (u32::MAX, 128, (true, u32::MAX, 256)),
        (256, 0, (false, 0, 128)),
        (256, 127, (false, 127, 128)),
        (256, MAX_FIELD + 1, (false, MAX_FIELD + 1, 128)),
    ] {
        let (s, d) = ([plane(0, 0, src)], [plane(1, 0, dst)]);
        let (source, pitch, row) = want;
        assert_eq!(
            check(&argb_turned(&s, &d)),
            Err(JobError::Pitch {
                source,
                plane: 0,
                pitch,
                row
            }),
            "{src} {dst}"
        );
    }
    // A turned destination's row is the source's height: a stride of 128 that would be short for
    // an unturned row of 256 is enough.
    let (s, d) = ([plane(0, 0, 256)], [plane(1, 0, 128)]);
    assert!(check(&argb_turned(&s, &d)).is_ok());
}

#[test]
fn a_source_plane_ends_inside_its_buffer() {
    let size = 1 << 20;
    // 32 rows of 256 bytes, 256 apart: 8192 bytes. The last offset that fits, then one more.
    let last = size - 32 * 256;
    let (s, d) = ([plane(0, last, 256)], [plane(1, 0, 128)]);
    assert!(check(&argb_turned(&s, &d)).is_ok());
    for offset in [last + 1, size, u64::MAX - 100, u64::MAX] {
        let s = [plane(0, offset, 256)];
        assert_eq!(
            check(&argb_turned(&s, &d)),
            Err(JobError::Outside {
                source: true,
                plane: 0,
                size
            }),
            "{offset}"
        );
    }
    // A wide stride: the last row starts 31 strides in, past the end even at offset 0.
    let s = [plane(0, 0, 65535)];
    assert!(matches!(
        check(&argb_turned(&s, &d)),
        Err(JobError::Outside { source: true, .. })
    ));
}

#[test]
fn a_turned_destination_ends_inside_its_buffer() {
    // Buffer 3 holds 64 bytes. RGB565 4x2 turned: 4 rows of 2 * 2 = 4 bytes, 16 apart:
    // 3 * 16 + 4 = 52.
    let s = [plane(0, 0, 8)];
    let d = [plane(3, 12, 16)];
    let j = job(
        frame(Format::Rgb565, 4, 2, &s),
        frame(Format::Rgb565, 2, 4, &d),
        turn(Turn::R270),
    );
    assert_eq!(check(&j).unwrap().destination(0).unwrap().end, 64);
    let d = [plane(3, 13, 16)];
    let j = job(
        frame(Format::Rgb565, 4, 2, &s),
        frame(Format::Rgb565, 2, 4, &d),
        turn(Turn::R270),
    );
    assert_eq!(
        check(&j),
        Err(JobError::Outside {
            source: false,
            plane: 0,
            size: 64
        })
    );
    // Unturned, the same frame is 2 rows of 8 bytes: it fits at 13 (16 + 8 = 24 more).
    let j = job(
        frame(Format::Rgb565, 4, 2, &s),
        frame(Format::Rgb565, 4, 2, &d),
        turn(Turn::R180),
    );
    assert_eq!(check(&j).unwrap().destination(0).unwrap().end, 13 + 16 + 8);
}

#[test]
fn chroma_planes_are_checked_at_their_rounded_up_size() {
    // NV12 5x3: luma 5x3 bytes; chroma 3x2 elements of 2 bytes (6-byte rows), rounded up.
    let s = [plane(0, 0, 5), plane(0, 64, 6)];
    let d = [plane(3, 0, 3), plane(3, 16, 4)];
    let j = job(
        frame(Format::Nv12, 5, 3, &s),
        frame(Format::Nv12, 3, 5, &d),
        turn(Turn::R90),
    );
    let plan = check(&j).expect("valid");
    assert_eq!(plan.size(1), Some((3, 2)));
    assert_eq!(plan.source(1).unwrap().end, 64 + 6 + 6);
    // Turned chroma: 3 rows of 2 * 2 = 4 bytes, 4 apart.
    assert_eq!(plan.destination(1).unwrap().end, 16 + 2 * 4 + 4);
    // A chroma stride that holds a row only if the width were rounded down is refused.
    let s = [plane(0, 0, 5), plane(0, 64, 5)];
    let j = job(
        frame(Format::Nv12, 5, 3, &s),
        frame(Format::Nv12, 3, 5, &d),
        turn(Turn::R90),
    );
    assert_eq!(
        check(&j),
        Err(JobError::Pitch {
            source: true,
            plane: 1,
            pitch: 5,
            row: 6
        })
    );
    // And a turned chroma row of 4 bytes needs a stride of 4.
    let s = [plane(0, 0, 5), plane(0, 64, 6)];
    let d = [plane(3, 0, 3), plane(3, 16, 3)];
    let j = job(
        frame(Format::Nv12, 5, 3, &s),
        frame(Format::Nv12, 3, 5, &d),
        turn(Turn::R90),
    );
    assert_eq!(
        check(&j),
        Err(JobError::Pitch {
            source: false,
            plane: 1,
            pitch: 3,
            row: 4
        })
    );
}

#[test]
fn a_destination_never_overlaps_what_the_job_reads_or_writes() {
    // Source in bytes 0..8192 of buffer 0; a destination there too.
    let s = [plane(0, 0, 256)];
    let d = [plane(0, 8191, 128)];
    assert_eq!(
        check(&argb_turned(&s, &d)),
        Err(JobError::Overlap { plane: 0 })
    );
    // Right after it is fine.
    let d = [plane(0, 8192, 128)];
    assert!(check(&argb_turned(&s, &d)).is_ok());
    // NV12 64x32: luma 2048 bytes, chroma 1024. Destination chroma on top of its luma.
    let s = [plane(0, 0, 64), plane(0, 2048, 64)];
    let d = [plane(1, 0, 64), plane(1, 2047, 64)];
    let j = job(
        frame(Format::Nv12, 64, 32, &s),
        frame(Format::Nv12, 64, 32, &d),
        turn(Turn::R0),
    );
    assert_eq!(check(&j), Err(JobError::Overlap { plane: 1 }));
    // Two source planes may share bytes: the engine only reads them.
    let s = [plane(0, 0, 64), plane(0, 0, 64)];
    let d = [plane(1, 0, 64), plane(1, 2048, 64)];
    let j = job(
        frame(Format::Nv12, 64, 32, &s),
        frame(Format::Nv12, 64, 32, &d),
        turn(Turn::R0),
    );
    assert!(check(&j).is_ok());
}

#[test]
fn descriptors_carry_the_plan_at_the_buffers_addresses() {
    let s = [plane(0, 0, 64), plane(0, 4096, 64)];
    let d = [plane(1, 0, 32), plane(1, 4096, 32)];
    let j = job(
        frame(Format::Nv12, 64, 32, &s),
        frame(Format::Nv12, 32, 64, &d),
        Rotation {
            turn: Turn::R90,
            reflect_x: true,
            reflect_y: false,
        },
    );
    let plan = check(&j).expect("valid");
    let base = |b: BufferId| Some([0x1000_0000, 0x2000_0000][usize::from(b.slot)]);
    let (chain, n) = plan.descriptors(base, 0x3000_0000).expect("addresses");
    assert_eq!(n, 2);
    let (y, c) = (chain[0], chain[1]);
    assert_eq!(
        (y.rotation_mode, y.x_mirror, y.y_mirror),
        (turn::R90, true, false)
    );
    assert_eq!(
        (y.pixel_width, c.pixel_width),
        (pixel::BITS_8, pixel::BITS_16)
    );
    assert_eq!((y.width, y.height, c.width, c.height), (64, 32, 32, 16));
    assert_eq!((y.src_addr, y.dst_addr), (0x1000_0000, 0x2000_0000));
    assert_eq!((c.src_addr, c.dst_addr), (0x1000_1000, 0x2000_1000));
    assert_eq!((y.src_stride, y.dst_stride), (64, 32));
    assert_eq!((y.next, c.next), (0x3000_0018, 0));
    assert_eq!((y.node_intr, c.node_intr), (false, true));
    assert_eq!(chain[2], crate::uapi::Descriptor::default());
}

#[test]
fn descriptors_end_inside_the_memory_the_engine_may_reach() {
    let (s, d) = ([plane(0, 100, 256)], [plane(1, 0, 128)]);
    let plan = check(&argb_turned(&s, &d)).expect("valid");
    let src_end = 100 + 32 * 256;
    let at = |src: u64| move |b: BufferId| Some(if b.slot == 0 { src } else { 0x100_0000 });
    // The source's last byte exactly at the end of DDR, then one past.
    assert!(plan.descriptors(at(DDR_END - src_end), 0).is_ok());
    assert_eq!(
        plan.descriptors(at(DDR_END - src_end + 1), 0),
        Err(AddressError::Outside {
            source: true,
            plane: 0
        })
    );
    assert_eq!(
        plan.descriptors(at(u64::MAX), 0),
        Err(AddressError::Outside {
            source: true,
            plane: 0
        })
    );
    let dst = |b: BufferId| {
        Some(if b.slot == 0 {
            0
        } else {
            DDR_END - 64 * 128 + 1
        })
    };
    assert_eq!(
        plan.descriptors(dst, 0),
        Err(AddressError::Outside {
            source: false,
            plane: 0
        })
    );
    assert_eq!(plan.descriptors(|_| None, 0), Err(AddressError::Unknown));
    assert!(plan.descriptors(at(0), DDR_END - 24).is_ok());
    for chain in [DDR_END - 23, u64::MAX] {
        assert_eq!(plan.descriptors(at(0), chain), Err(AddressError::Chain));
    }
}

// The claim.

#[derive(Default)]
struct Played {
    opens: u32,
    fail_opens: u32,
    /// Per submit: the identity, and the raw descriptors of the buffers handed over.
    submits: Vec<(u64, [i32; 4])>,
    refuse: bool,
    /// What the next waits answer.
    finished: bool,
    dropped: bool,
}

#[derive(Clone, Default)]
struct Mock(Rc<RefCell<Played>>);

impl Mock {
    fn with<T>(&self, f: impl FnOnce(&mut Played) -> T) -> T {
        f(&mut self.0.borrow_mut())
    }
}

struct MockSession(Mock);

impl Session for MockSession {
    fn submit(&mut self, request: &Request<'_>) -> io::Result<()> {
        let plan = request.plan();
        let mut fds = [-1; 4];
        let spans = (0..plan.planes())
            .flat_map(|i| [plan.source(i).unwrap(), plan.destination(i).unwrap()]);
        for (to, span) in fds.iter_mut().zip(spans) {
            *to = request.buffer(span.buffer).expect("held").as_raw_fd();
        }
        self.0.with(|p| {
            if p.refuse {
                return Err(io::Error::from_raw_os_error(22));
            }
            p.submits.push((plan.identity(), fds));
            Ok(())
        })
    }

    fn wait(&mut self, _: Duration) -> io::Result<bool> {
        Ok(self.0.with(|p| p.finished))
    }
}

impl Drop for MockSession {
    fn drop(&mut self) {
        self.0.with(|p| p.dropped = true);
    }
}

struct MockDevice(Mock);

impl Device for MockDevice {
    type Session = MockSession;

    fn open(&mut self) -> io::Result<MockSession> {
        self.0.with(|p| {
            p.opens += 1;
            if p.fail_opens > 0 {
                p.fail_opens -= 1;
                return Err(io::Error::from_raw_os_error(19));
            }
            Ok(())
        })?;
        Ok(MockSession(self.0.clone()))
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

fn buffer(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"k230-gsdma-test", len).expect("memfd")
}

#[test]
fn the_backoff_doubles_from_250_ms_to_8_s_and_never_gives_up() {
    let ms: Vec<u64> = (0..9).map(|n| backoff_ns(n) / 1_000_000).collect();
    assert_eq!(ms, [250, 250, 500, 1000, 2000, 4000, 8000, 8000, 8000]);
    let mock = Mock::default();
    mock.with(|p| p.fail_opens = 10);
    let mut gsdma = Gsdma::new(MockDevice(mock.clone()));
    let mut now = 0;
    for failures in 1..=10 {
        let wait = backoff_ns(failures);
        match gsdma.claim(now) {
            Err(ClaimError::Device { retry_at_ns, .. }) => assert_eq!(retry_at_ns, now + wait),
            other => panic!("expected a device error, got {:?}", other.err()),
        }
        let opens = mock.with(|p| p.opens);
        assert!(matches!(
            gsdma.claim(now + wait - 1),
            Err(ClaimError::BackingOff { .. })
        ));
        assert_eq!(mock.with(|p| p.opens), opens);
        now += wait;
    }
    assert!(gsdma.claim(now).is_ok());
}

#[test]
fn a_claim_measures_its_buffers_and_hands_the_session_only_those() {
    let mock = Mock::default();
    let mut gsdma = Gsdma::new(MockDevice(mock.clone()));
    let mut claim = gsdma.claim(0).expect("claim");
    assert!(matches!(
        claim.add_buffer(buffer(0)),
        Err(BufferError::Size(None))
    ));
    let (a, b) = (buffer(8192), buffer(8192));
    let (a_raw, b_raw) = (a.as_raw_fd(), b.as_raw_fd());
    let (a, b) = (claim.add_buffer(a).unwrap(), claim.add_buffer(b).unwrap());
    assert_eq!(claim.buffer_size(a), Some(8192));
    let (s, d) = (
        [Plane {
            buffer: a,
            offset: 0,
            pitch: 256,
        }],
        [Plane {
            buffer: b,
            offset: 0,
            pitch: 128,
        }],
    );
    claim.submit(&argb_turned(&s, &d)).expect("submitted");
    assert!(claim.busy());
    assert!(matches!(
        claim.submit(&argb_turned(&s, &d)),
        Err(SubmitError::Busy)
    ));
    assert!(matches!(claim.remove_buffer(a), Err(BufferError::Busy)));
    assert!(matches!(claim.remove_buffer(b), Err(BufferError::Busy)));
    assert!(matches!(
        claim.complete(TIMEOUT),
        Err(CompleteError::Timeout)
    ));
    assert!(claim.busy(), "a job that times out stays in flight");
    mock.with(|p| p.finished = true);
    assert_eq!(
        claim.complete(TIMEOUT).unwrap(),
        Completion { identity: 42 }
    );
    assert!(matches!(
        claim.complete(TIMEOUT),
        Err(CompleteError::Nothing)
    ));
    claim.remove_buffer(a).expect("given back");
    // A removed buffer is no longer a job's to name.
    assert!(matches!(
        claim.submit(&argb_turned(&s, &d)),
        Err(SubmitError::Job(JobError::UnknownBuffer {
            source: true,
            ..
        }))
    ));
    drop(claim);
    mock.with(|p| {
        assert_eq!(p.submits, [(42, [a_raw, b_raw, -1, -1])]);
        assert!(p.dropped);
    });
}

#[test]
fn a_refused_job_leaves_nothing_in_flight() {
    let mock = Mock::default();
    let mut gsdma = Gsdma::new(MockDevice(mock.clone()));
    let mut claim = gsdma.claim(0).expect("claim");
    let a = claim.add_buffer(buffer(8192)).unwrap();
    let b = claim.add_buffer(buffer(8192)).unwrap();
    let (s, d) = (
        [Plane {
            buffer: a,
            offset: 0,
            pitch: 256,
        }],
        [Plane {
            buffer: b,
            offset: 1,
            pitch: 128,
        }],
    );
    // Past the end by one byte: refused before the session sees it.
    assert!(matches!(
        claim.submit(&argb_turned(&s, &d)),
        Err(SubmitError::Job(JobError::Outside { source: false, .. }))
    ));
    let d = [Plane { offset: 0, ..d[0] }];
    mock.with(|p| p.refuse = true);
    assert!(matches!(
        claim.submit(&argb_turned(&s, &d)),
        Err(SubmitError::Device(_))
    ));
    assert!(!claim.busy());
    claim.remove_buffer(a).expect("not held");
    drop(claim);
    assert!(mock.with(|p| p.submits.is_empty()));
}

#[test]
fn submitting_and_completing_allocate_nothing() {
    let mock = Mock::default();
    mock.with(|p| {
        p.finished = true;
        p.submits.reserve(64);
    });
    let mut gsdma = Gsdma::new(MockDevice(mock.clone()));
    let mut claim = gsdma.claim(0).expect("claim");
    let a = claim.add_buffer(buffer(8192)).unwrap();
    let b = claim.add_buffer(buffer(8192)).unwrap();
    let s = [Plane {
        buffer: a,
        offset: 0,
        pitch: 256,
    }];
    let d = [Plane {
        buffer: b,
        offset: 0,
        pitch: 128,
    }];
    let ((), n) = allocations(|| {
        for _ in 0..50 {
            claim.submit(&argb_turned(&s, &d)).unwrap();
            claim.complete(TIMEOUT).unwrap();
        }
    });
    assert_eq!(n, 0);
}
