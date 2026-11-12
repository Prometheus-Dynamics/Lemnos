//! `lemnos-module-check gsdma <device> <heap>`: the graphics DMA driver's Linux device
//! (`lemnos-driver-k230-gsdma`) end to end against the module's node and the harness's fake
//! engine. A claim registers its buffers on a stream, a turned copy lands pixel for pixel, a job
//! past a buffer is refused before the module sees it, a job the fake engine hangs on comes back
//! stopped and the claim goes on, steady-state jobs allocate nothing, and a second claim after
//! the first works.

use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use lemnos_driver_k230_gsdma::{
    BufferId, Claim, Completion, Format, Frame, Gsdma, Job, JobError, LinuxDevice, Plane, Rotation,
    Settings, SubmitError, Turn,
};
use lemnos_linux_sys::{dma_buf, dma_heap, mmap::Mapping};
use lemnos_measure::test_alloc::allocations;

use crate::Report;

const BYTES: u64 = 64 * 1024;
/// The source: ARGB8888, 16 by 8, rows 64 bytes apart.
const W: u32 = 16;
const H: u32 = 8;

fn px(x: u32, y: u32) -> u32 {
    0xa500_0000 | y << 8 | x
}

fn job<'a>(identity: u64, src: &'a [Plane; 1], dst: &'a [Plane; 1]) -> Job<'a> {
    Job {
        identity,
        source: Frame {
            format: Format::Argb8888,
            width: W,
            height: H,
            planes: src,
        },
        destination: Frame {
            format: Format::Argb8888,
            width: H,
            height: W,
            planes: dst,
        },
        rotation: Rotation {
            turn: Turn::R90,
            ..Rotation::default()
        },
    }
}

/// Completes the job in flight, waiting up to 2 s in all.
fn complete(claim: &mut Claim<'_, LinuxDevice>) -> Option<Completion> {
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        if let Ok(done) = claim.complete(Duration::from_millis(100)) {
            return Some(done);
        }
    }
    None
}

fn word(map: &Mapping, i: usize) -> u32 {
    map.read_volatile::<4>(i * 4).map_or(0, u32::from_le_bytes)
}

fn put_sys(device: &str, name: &str, value: &str) -> bool {
    std::fs::write(format!("/sys/bus/platform/devices/{device}/{name}"), value).is_ok()
}

struct Images {
    src: Mapping,
    dst: Mapping,
    src_fd: OwnedFd,
    dst_fd: OwnedFd,
}

fn images(heap: &str) -> Option<(Images, OwnedFd, OwnedFd)> {
    let heap = std::fs::File::open(format!("/dev/dma_heap/{heap}")).ok()?;
    let src_fd = dma_heap::allocate(heap.as_fd(), BYTES).ok()?;
    let dst_fd = dma_heap::allocate(heap.as_fd(), BYTES).ok()?;
    let src = Mapping::new(src_fd.as_fd(), BYTES as usize, 0, true).ok()?;
    let dst = Mapping::new(dst_fd.as_fd(), BYTES as usize, 0, true).ok()?;
    let (a, b) = (src_fd.try_clone().ok()?, dst_fd.try_clone().ok()?);
    Some((
        Images {
            src,
            dst,
            src_fd,
            dst_fd,
        },
        a,
        b,
    ))
}

/// Fills the source, clears the destination, and brackets the CPU's writes.
fn prepare(im: &mut Images) {
    let _ = dma_buf::sync(im.src_fd.as_fd(), dma_buf::SYNC_START | dma_buf::SYNC_WRITE);
    for y in 0..H {
        for x in 0..W {
            let _ = im
                .src
                .write_volatile((y * W + x) as usize * 4, px(x, y).to_le_bytes());
        }
    }
    let _ = dma_buf::sync(im.src_fd.as_fd(), dma_buf::SYNC_END | dma_buf::SYNC_WRITE);
    let _ = dma_buf::sync(im.dst_fd.as_fd(), dma_buf::SYNC_START | dma_buf::SYNC_WRITE);
    for i in 0..(W * H) as usize {
        let _ = im.dst.write_volatile(i * 4, [0; 4]);
    }
    let _ = dma_buf::sync(im.dst_fd.as_fd(), dma_buf::SYNC_END | dma_buf::SYNC_WRITE);
}

/// Pixels of the destination that are not the source turned a quarter clockwise.
fn wrong(im: &Images) -> usize {
    let _ = dma_buf::sync(im.dst_fd.as_fd(), dma_buf::SYNC_START | dma_buf::SYNC_READ);
    let mut bad = 0;
    for y in 0..H {
        for x in 0..W {
            bad += usize::from(word(&im.dst, (x * H + (H - 1 - y)) as usize) != px(x, y));
        }
    }
    let _ = dma_buf::sync(im.dst_fd.as_fd(), dma_buf::SYNC_END | dma_buf::SYNC_READ);
    bad
}

fn planes(a: BufferId, b: BufferId) -> ([Plane; 1], [Plane; 1]) {
    (
        [Plane {
            buffer: a,
            offset: 0,
            pitch: W * 4,
        }],
        [Plane {
            buffer: b,
            offset: 0,
            pitch: H * 4,
        }],
    )
}

/// Runs the checks; the number that failed.
pub fn run(device: &str, heap: &str) -> u32 {
    let mut report = Report { failures: 0 };
    let Ok(settings) = Settings::new(device, heap) else {
        report.check(false, "gsdma: settings");
        return report.failures;
    };
    let Some((mut im, src_fd, dst_fd)) = images(heap) else {
        report.check(false, "gsdma: images allocated and mapped");
        return report.failures;
    };
    let mut gsdma = Gsdma::new(LinuxDevice::new(settings));
    {
        let mut claim = match gsdma.claim(0) {
            Ok(claim) => claim,
            Err(e) => {
                report.check(false, &format!("gsdma: claim ({e:?})"));
                return report.failures;
            }
        };
        report.check(true, "gsdma: claim (a stream on the engine)");
        let (Ok(a), Ok(b)) = (claim.add_buffer(src_fd), claim.add_buffer(dst_fd)) else {
            report.check(false, "gsdma: buffers registered");
            return report.failures;
        };
        report.check(true, "gsdma: buffers registered");
        let (s, d) = planes(a, b);

        prepare(&mut im);
        let submitted = claim.submit(&job(7, &s, &d));
        report.check(submitted.is_ok(), "gsdma: a quarter turn submitted");
        let done = complete(&mut claim);
        report.check(
            done == Some(Completion {
                identity: 7,
                stopped: false,
            }),
            &format!("gsdma: and completed ({done:?})"),
        );
        let bad = wrong(&im);
        report.check(
            bad == 0,
            &format!("gsdma: turned pixel for pixel ({bad} wrong)"),
        );

        // Past the destination's end by one row: refused here, before the module.
        let d_past = [Plane {
            offset: BYTES - u64::from(H * 4) * u64::from(W) + u64::from(H * 4),
            ..d[0]
        }];
        report.check(
            matches!(
                claim.submit(&job(8, &s, &d_past)),
                Err(SubmitError::Job(JobError::Outside { source: false, .. }))
            ),
            "gsdma: a destination past its buffer refused before the module",
        );
        report.check(!claim.busy(), "gsdma: and nothing in flight");

        // The fake engine hangs for 20 ms after it is turned off: the module stops the job.
        report.check(
            put_sys(device, "hang", "20"),
            "gsdma: fake engine set to hang",
        );
        let submitted = claim.submit(&job(9, &s, &d));
        let done = complete(&mut claim);
        report.check(
            submitted.is_ok()
                && done
                    == Some(Completion {
                        identity: 9,
                        stopped: true,
                    }),
            &format!("gsdma: a job the engine hangs on completes stopped ({done:?})"),
        );
        report.check(
            put_sys(device, "hang", "0"),
            "gsdma: fake engine set to run",
        );

        prepare(&mut im);
        let submitted = claim.submit(&job(10, &s, &d));
        let done = complete(&mut claim);
        report.check(
            submitted.is_ok() && done.is_some_and(|c| !c.stopped) && wrong(&im) == 0,
            "gsdma: the claim goes on after a stopped job",
        );

        let ((), n) = allocations(|| {
            for i in 0..20 {
                if claim.submit(&job(100 + i, &s, &d)).is_err() || complete(&mut claim).is_none() {
                    report.failures += 1;
                }
            }
        });
        report.check(n == 0, &format!("gsdma: 20 jobs allocated nothing ({n})"));

        report.check(
            claim.remove_buffer(a).is_ok() && claim.remove_buffer(b).is_ok(),
            "gsdma: buffers given back",
        );
    }
    let again = gsdma.claim(0);
    report.check(again.is_ok(), "gsdma: a second claim after the first");
    drop(again);
    let _ = writeln!(
        std::io::stdout(),
        "lemnos-module-check gsdma: {} failed",
        report.failures
    );
    report.failures
}
