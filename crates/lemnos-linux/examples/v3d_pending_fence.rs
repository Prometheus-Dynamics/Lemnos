#![allow(clippy::print_stdout, clippy::print_stderr)]

//! A pending sync-file fence from a real v3d job.
//!
//! A 4 MiB byte copy runs on the v3d TFU (texture formatting unit), which needs
//! no shader: 1024 TFU jobs, one row of 4096 bytes each, submitted in order on
//! one render node. The last job carries an out-syncobj. The example exports
//! that syncobj's fence as a sync file straight after the submit, while the
//! copy is still running (pending), polls the sync file until it signals, times
//! the signal, and checks every byte of the destination. Five runs, each with
//! its own syncobj.
//!
//! Hardware only: needs the v3d render node (`/dev/dri/renderD128` on the
//! BCM2712 boards) and a kernel with v3d and sync-file export. Run it on a
//! board, as root or with access to the node:
//!
//! ```text
//! v3d_pending_fence [/dev/dri/renderD128]
//! ```
//!
//! The TFU's `icfg` value and one-row-per-job shape were found on board #2 by
//! a sweep, not from a register reference (see `docs/dma.md`); the byte-for-byte
//! check is what makes them count here.

use std::io;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use lemnos_linux::dma::SyncFence;
use lemnos_linux_sys::{drm, mmap::Mapping, v3d};

const WIDTH: u32 = 4096;
const HEIGHT: u32 = 1024;
const STRIDE: u32 = WIDTH;
const LEN: u32 = STRIDE * HEIGHT;
const RUNS: usize = 5;
/// Longest wait for the fence before the run is reported as stuck.
const SIGNAL_LIMIT: Duration = Duration::from_secs(5);
/// TFU `icfg` that copies bytes unchanged, one row per job (see `docs/dma.md`).
const ICFG_BYTE_COPY: u32 = 32;

/// The pattern the source holds: a different word per position.
fn pattern(i: usize) -> [u8; 4] {
    (i as u32)
        .wrapping_mul(0x9e37_79b9)
        .rotate_left(7)
        .to_le_bytes()
}

/// One row per job: the copy's unit on this board (see `docs/dma.md`).
fn row_job(src: &v3d::CreatedBo, dst: &v3d::CreatedBo, row: u32, out_sync: u32) -> v3d::SubmitTfu {
    v3d::SubmitTfu {
        icfg: ICFG_BYTE_COPY,
        iia: src.offset + row * STRIDE,
        iis: STRIDE,
        ioa: dst.offset + row * STRIDE,
        ios: (1 << 16) | WIDTH,
        bo_handles: [dst.handle, src.handle, 0, 0],
        out_sync,
        ..v3d::SubmitTfu::default()
    }
}

fn run(fd: &std::os::fd::OwnedFd) -> io::Result<()> {
    let dev = fd.as_fd();
    let src = v3d::create_bo(dev, LEN)?;
    let dst = v3d::create_bo(dev, LEN)?;
    let mut src_map = Mapping::new(dev, LEN as usize, v3d::mmap_offset(dev, src.handle)?, true)?;
    let mut dst_map = Mapping::new(dev, LEN as usize, v3d::mmap_offset(dev, dst.handle)?, true)?;
    let (words, _) = src_map.as_mut_slice().as_chunks_mut::<4>();
    for (i, word) in words.iter_mut().enumerate() {
        *word = pattern(i);
    }
    dst_map.as_mut_slice().fill(0xEE);

    // The jobs run in submission order on this descriptor, so the last job's
    // out-syncobj signals after every row has been copied.
    let sync = drm::syncobj_create(dev)?;
    let start = Instant::now();
    for row in 0..HEIGHT - 1 {
        v3d::submit_tfu(dev, &row_job(&src, &dst, row, 0))?;
    }
    v3d::submit_tfu(dev, &row_job(&src, &dst, HEIGHT - 1, sync))?;
    let submitted = start.elapsed();
    let fence = SyncFence::from_fd(drm::syncobj_export_sync_file(dev, sync)?);
    let pending_at_export = !fence.is_signaled()?;

    let mut polls = 0u64;
    while !fence.is_signaled()? {
        polls += 1;
        if start.elapsed() > SIGNAL_LIMIT {
            drm::syncobj_destroy(dev, sync)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "fence did not signal",
            ));
        }
    }
    let signalled = start.elapsed();
    drm::syncobj_destroy(dev, sync)?;

    let mismatches = src_map
        .as_slice()
        .iter()
        .zip(dst_map.as_slice())
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "pending at export: {pending_at_export}, submit {:.1} us, unsignalled polls: {polls}, \
         signalled after {:.1} us, bytes differing: {mismatches} of {LEN}",
        submitted.as_secs_f64() * 1e6,
        signalled.as_secs_f64() * 1e6,
    );
    if mismatches != 0 || !pending_at_export {
        return Err(io::Error::other(
            "the run did not show a pending fence and a correct copy",
        ));
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/dri/renderD128".to_owned());
    let fd = drm::open_render_node(std::path::Path::new(&path))?;
    let mut failed = 0;
    for n in 1..=RUNS {
        print!("run {n}: ");
        if let Err(e) = run(&fd) {
            println!("failed: {e}");
            failed += 1;
        }
    }
    if failed != 0 {
        return Err(io::Error::other(format!("{failed} of {RUNS} runs failed")));
    }
    Ok(())
}
