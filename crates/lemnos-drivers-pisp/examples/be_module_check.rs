//! On a Pi 5: the back end through Lemnos's module (`/dev/lemnos-pisp-be`, feature
//! `be-module`) against the stock `pispbe` nodes, on one op: an NV12 640x480 image downscaled
//! by 2 to GREY on output 1, from and to dma-heap buffers.
//!
//! ```text
//! be_module_check latency [JOBS]   the module: submit to wake-up, interrupt to wake-up, output check
//! be_module_check v4l2 [JOBS] [GROUP]  the pispbe nodes (group 0 or 1): queue to dequeue, output check
//! be_module_check one              one job through the module and its done record
//! be_module_check lockup [ENABLES] a job that stops the block, then the two soft recoveries
//! ```
//!
//! `latency` and `v4l2` print p50, p99 and max in microseconds; run them under `chrt -f 50` for
//! the real-time figures. `latency` needs the module bound (`pispbe` unbound), `v4l2` the stock
//! driver. `one` with the module's `test_unmap_plane` set is the IOMMU test. `lockup` needs the
//! module's `test_raw_enables` set: it queues a Bayer pipe on an RGB input, which stops the block
//! until a reboot unless a recovery revives it.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::os::fd::AsFd;
use std::time::Duration;

use lemnos_drivers_pisp::be::ops::{BeOp, Pixels, downscale};
use lemnos_drivers_pisp::device::BeImageDevice;
use lemnos_drivers_pisp::device::be_module::{BeModule, BeModuleJob, Done, RecoverMode};
use lemnos_drivers_pisp::uapi::bayer_enable;
use lemnos_linux::dma::{Access, DmaBuf, DmaHeap, Phase, sync};
use lemnos_linux_sys::time::monotonic_now;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const SIZE: (u16, u16) = (640, 480);
const TIMEOUT: Duration = Duration::from_secs(2);

fn op() -> Result<BeOp> {
    Ok(downscale(Pixels::Grey8, SIZE, 2)?)
}

/// The test luma: a ramp both ways plus a fixed pseudo-random texture.
fn sample(x: usize, y: usize) -> u8 {
    let n = (x as u32).wrapping_mul(2_654_435_761) ^ (y as u32).wrapping_mul(40_503);
    ((x + 2 * y) as u32 + (n >> 27)) as u8
}

fn heap() -> Result<DmaHeap> {
    Ok(DmaHeap::open("linux,cma").or_else(|_| DmaHeap::open("system"))?)
}

/// An input buffer of `len` bytes, the luma at `stride` filled and the chroma at 128.
fn input(heap: &DmaHeap, len: usize, stride: usize) -> Result<DmaBuf> {
    let buf = heap.allocate(len)?;
    let mut map = buf.map()?;
    sync(buf.as_fd(), Phase::Begin, Access::Write)?;
    let m = map.as_mut_slice();
    let (w, h) = (usize::from(SIZE.0), usize::from(SIZE.1));
    m.fill(128);
    for y in 0..h {
        for x in 0..w {
            m[y * stride + x] = sample(x, y);
        }
    }
    sync(buf.as_fd(), Phase::End, Access::Write)?;
    Ok(buf)
}

/// Output samples off the truncated 2x2 mean by more than the block's documented one.
fn check(out: &DmaBuf, stride: usize) -> Result<usize> {
    let map = out.map()?;
    sync(out.as_fd(), Phase::Begin, Access::Read)?;
    let m = map.as_slice();
    let (w, h) = (usize::from(SIZE.0) / 2, usize::from(SIZE.1) / 2);
    let mut bad = 0;
    for y in 0..h {
        for x in 0..w {
            let s: u32 = (0..4)
                .map(|k| u32::from(sample(2 * x + k % 2, 2 * y + k / 2)))
                .sum();
            let want = (s + 2) / 4;
            let got = u32::from(m[y * stride + x]);
            bad += usize::from(got > want || want - got > 1);
        }
    }
    sync(out.as_fd(), Phase::End, Access::Read)?;
    Ok(bad)
}

fn report(name: &str, mut us: Vec<u64>) {
    if us.is_empty() {
        return;
    }
    us.sort_unstable();
    let at = |q: usize| us[(us.len() - 1) * q / 100];
    println!(
        "  {name}: p50 {} us, p99 {} us, max {} us ({} jobs)",
        at(50),
        at(99),
        us[us.len() - 1],
        us.len()
    );
}

fn print_done(d: &Done) {
    println!(
        "  done: tag {} status {} batch 0x{:08x} int 0x{:08x} block 0x{:08x}",
        d.tag, d.status, d.batch_status, d.int_status, d.block_status
    );
}

fn module_latency(jobs: usize) -> Result<()> {
    let be = BeModule::open()?;
    let info = be.info()?;
    println!(
        "lemnos-pisp-be: hw 0x{:08x}, depth {}",
        info.hw_version, info.queue_depth
    );
    let mut op = op()?;
    let job = BeModuleJob::new(&mut op)?;
    let heap = heap()?;
    let stride = job.config().config.input_format.stride as usize;
    let inp = input(&heap, job.input_len(), stride)?;
    let out = heap.allocate(job.output_len(1).ok_or("no output 1")?)?;
    let out_stride = job.config().config.output_format[1].image.stride as usize;
    let (mut round, mut wake) = (Vec::new(), Vec::new());
    for tag in 0..jobs as u64 {
        let start = monotonic_now();
        be.submit(&job, tag, inp.as_fd(), [None, Some(out.as_fd())])?;
        let done = be.wait(TIMEOUT)?;
        let now = monotonic_now();
        let d = done.first().ok_or("no job finished in 2 s")?;
        if d.status != 0 || d.tag != tag {
            print_done(d);
            return Err("job failed".into());
        }
        round.push((now - start).as_micros() as u64);
        wake.push((now.as_nanos() as u64).saturating_sub(d.irq_ns) / 1000);
    }
    println!(
        "  output: {} samples off the 2x2 mean",
        check(&out, out_stride)?
    );
    report("submit to wake-up", round);
    report("interrupt to wake-up", wake);
    Ok(())
}

fn v4l2(jobs: usize, group: usize) -> Result<()> {
    let mut op = op()?;
    let mut be = BeImageDevice::open(group, &mut op)?;
    let heap = heap()?;
    let stride = op.input.stride as usize;
    let inp = input(&heap, be.input_len() as usize, stride)?;
    let out = heap.allocate(be.output_len() as usize)?;
    let mut round = Vec::new();
    for _ in 0..jobs {
        round.push(be.run(inp.as_fd(), out.as_fd(), TIMEOUT)?.as_micros() as u64);
    }
    println!("pispbe nodes, group {group}");
    println!(
        "  output: {} samples off the 2x2 mean",
        check(&out, op.out.stride as usize)?
    );
    report("queue to dequeue", round);
    be.stop()?;
    Ok(())
}

fn one() -> Result<()> {
    let be = BeModule::open()?;
    let mut op = op()?;
    let job = BeModuleJob::new(&mut op)?;
    let heap = heap()?;
    let inp = input(
        &heap,
        job.input_len(),
        job.config().config.input_format.stride as usize,
    )?;
    let out = heap.allocate(job.output_len(1).ok_or("no output 1")?)?;
    let out_stride = job.config().config.output_format[1].image.stride as usize;
    {
        let mut map = out.map()?;
        sync(out.as_fd(), Phase::Begin, Access::Write)?;
        map.as_mut_slice().fill(0xaa);
        sync(out.as_fd(), Phase::End, Access::Write)?;
    }
    be.submit(&job, 1, inp.as_fd(), [None, Some(out.as_fd())])?;
    match be.wait(TIMEOUT)?.first() {
        Some(d) => print_done(d),
        None => println!("  no job finished in 2 s"),
    }
    println!("  dead: {}", be.info()?.dead);
    let map = out.map()?;
    sync(out.as_fd(), Phase::Begin, Access::Read)?;
    let (w, h) = (usize::from(SIZE.0) / 2, usize::from(SIZE.1) / 2);
    let left = (0..h)
        .flat_map(|y| map.as_slice()[y * out_stride..][..w].iter())
        .filter(|&&v| v == 0xaa)
        .count();
    sync(out.as_fd(), Phase::End, Access::Read)?;
    println!(
        "  output: {left} of {} samples still 0xaa (not written), {} off the 2x2 mean",
        w * h,
        check(&out, out_stride)?
    );
    Ok(())
}

/// The Bayer stages `lockup` turns on by default: every one that reads or writes no image of
/// its own (an unset address is below the IOMMU's aperture, so it reaches memory untranslated),
/// demosaic among them.
const LOCKUP_ENABLES: u32 = bayer_enable::DPC
    | bayer_enable::GEQ
    | bayer_enable::SDN
    | bayer_enable::BLC
    | bayer_enable::WBG
    | bayer_enable::CDN
    | bayer_enable::TONEMAP
    | bayer_enable::CAC
    | bayer_enable::DEMOSAIC;

fn lockup(enables: u32) -> Result<()> {
    let be = BeModule::open()?;
    let mut op = op()?;
    let mut job = BeModuleJob::new(&mut op)?;
    // The Bayer pipe's stages on, with the RGB input: what pispbe's address translation clears.
    job.config_mut().config.global.bayer_enables = enables;
    println!("  Bayer enables 0x{enables:06x} on the RGB input");
    let heap = heap()?;
    let inp = input(
        &heap,
        job.input_len(),
        job.config().config.input_format.stride as usize,
    )?;
    let out = heap.allocate(job.output_len(1).ok_or("no output 1")?)?;
    be.submit(&job, 1, inp.as_fd(), [None, Some(out.as_fd())])?;
    let done = be.wait(TIMEOUT)?;
    match done.first() {
        Some(d) => print_done(d),
        None => {
            // The watchdog ends it after a second; wait for that record.
            match be.wait(TIMEOUT)?.first() {
                Some(d) => print_done(d),
                None => println!("  no record in 4 s"),
            }
        }
    }
    println!("  dead: {}", be.info()?.dead);
    for mode in [RecoverMode::Axi, RecoverMode::Clock] {
        match be.recover(mode) {
            Ok(r) => println!(
                "  recover {mode:?}: status 0x{:08x} batch 0x{:08x} revived {}",
                r.status, r.batch_status, r.revived
            ),
            Err(e) => println!("  recover {mode:?}: {e}"),
        }
    }
    let mut op = self::op()?;
    let job = BeModuleJob::new(&mut op)?;
    match be.submit(&job, 2, inp.as_fd(), [None, Some(out.as_fd())]) {
        Ok(()) => match be.wait(TIMEOUT)?.first() {
            Some(d) => print_done(d),
            None => println!("  a good job after recovery: no record in 2 s"),
        },
        Err(e) => println!("  a good job after recovery: {e}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let jobs = || args.get(1).map_or(Ok(2000), |s| s.parse());
    match args.first().map(String::as_str) {
        Some("latency") => module_latency(jobs()?),
        Some("v4l2") => v4l2(jobs()?, args.get(2).map_or(Ok(0), |s| s.parse())?),
        Some("one") => one(),
        Some("lockup") => lockup(match args.get(1) {
            Some(s) => u32::from_str_radix(s.trim_start_matches("0x"), 16)?,
            None => LOCKUP_ENABLES,
        }),
        _ => Err(
            "usage: be_module_check latency [JOBS] | v4l2 [JOBS] [GROUP] | one | lockup [ENABLES]"
                .into(),
        ),
    }
}
