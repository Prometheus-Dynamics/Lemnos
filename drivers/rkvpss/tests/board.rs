//! The board check: real jobs through the offline scaler, from dma-heap buffers. Ignored by
//! default; run with `--ignored` on a board whose kernel has the `rkvpss-offline` node.
//!
//! - `LEMNOS_RKVPSS_NODE`: the node; without it, the one [`find`] gives.
//! - `LEMNOS_RKVPSS_HEAP`: the dma-heap the buffers come from; `/dev/dma_heap/system` without it.
//! - `LEMNOS_RKVPSS_CHANNEL`: the channel to write, 0 to 5; 0 without it. Selecting a channel
//!   takes it from the block's camera path, so pick one that path does not use.
//!
//! The input is a 1920x1080 NV12 frame whose luma rises from 0 at the top to 255 at the bottom,
//! with grey chroma. It is scaled to 640x360 NV12, once as it is and once flipped, and the output's
//! rows are compared with the ramp. Then 100 more jobs are timed and counted for allocations.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::time::Instant;

use lemnos_driver_rkvpss::{Channel, Crop, Job, Output, PixelFormat, Surface, find, open_at};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE};
use lemnos_linux_sys::{dma_heap, mmap::Mapping};
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const IN: (u32, u32) = (1920, 1080);
const OUT: (u32, u32) = (640, 360);
/// How far a scaled row may be from the ramp.
const TOLERANCE: i32 = 6;

fn buffer(heap: &File, len: u64) -> OwnedFd {
    dma_heap::allocate(heap.as_fd(), len).expect("dma-heap allocation")
}

fn with_mapping<T>(fd: &OwnedFd, len: u64, write: bool, f: impl FnOnce(&mut Mapping) -> T) -> T {
    let access = if write { SYNC_WRITE } else { SYNC_READ };
    let len = usize::try_from(len).expect("length");
    let mut map = Mapping::new(fd.as_fd(), len, 0, write).expect("mmap");
    dma_buf::sync(fd.as_fd(), SYNC_START | access).expect("sync start");
    let value = f(&mut map);
    dma_buf::sync(fd.as_fd(), SYNC_END | access).expect("sync end");
    value
}

/// The luma of `row` of an `height`-row ramp from 0 to 255.
fn ramp(row: u32, height: u32) -> i32 {
    i32::try_from(row * 255 / (height - 1)).expect("small")
}

#[test]
#[ignore = "needs the rkvpss-offline node and a dma-heap"]
fn scaled_and_flipped_frames_follow_the_ramp() {
    let node = std::env::var_os("LEMNOS_RKVPSS_NODE")
        .map(PathBuf::from)
        .or_else(find)
        .expect("no rkvpss-offline node");
    let heap_path =
        std::env::var("LEMNOS_RKVPSS_HEAP").unwrap_or_else(|_| "/dev/dma_heap/system".into());
    let index: usize = std::env::var("LEMNOS_RKVPSS_CHANNEL")
        .ok()
        .map_or(0, |c| c.parse().expect("a channel number"));
    let channel = Channel::ALL[index];
    let heap = File::open(&heap_path).expect("dma-heap");

    let in_len = PixelFormat::Nv12.frame_len(IN.0, IN.1);
    let out_len = PixelFormat::Nv12.frame_len(OUT.0, OUT.1);
    let input = buffer(&heap, in_len);
    let output = buffer(&heap, out_len);
    with_mapping(&input, in_len, true, |map| {
        let bytes = map.as_mut_slice();
        let (luma, chroma) = bytes.split_at_mut((IN.0 * IN.1) as usize);
        for (row, line) in luma.chunks_mut(IN.0 as usize).enumerate() {
            line.fill(u8::try_from(ramp(row as u32, IN.1)).expect("a byte"));
        }
        chroma.fill(128);
    });

    let mut scaler = open_at(node.clone(), channel.bit());
    let start = Instant::now();
    let now = || u64::try_from(start.elapsed().as_nanos()).expect("ns");
    let source = Surface::new(input.as_fd(), PixelFormat::Nv12, IN.0, IN.1, IN.0).expect("input");
    let target =
        Surface::new(output.as_fd(), PixelFormat::Nv12, OUT.0, OUT.1, OUT.0).expect("output");

    for flip in [false, true] {
        let out = Output {
            channel,
            crop: Crop::full(IN.0, IN.1),
            target,
            flip,
        };
        let mut job = Job::new(source, &[out]).expect("job");
        scaler.run(now(), &mut job).expect("run");
        with_mapping(&output, out_len, false, |map| {
            let bytes = map.as_slice();
            for row in [0, OUT.1 / 4, OUT.1 / 2, OUT.1 * 3 / 4, OUT.1 - 1] {
                let line = &bytes[(row * OUT.0) as usize..][..OUT.0 as usize];
                let shown = if flip { OUT.1 - 1 - row } else { row };
                let want = ramp(shown, OUT.1);
                for (x, &y) in line.iter().enumerate().step_by(97) {
                    let got = i32::from(y);
                    assert!(
                        (got - want).abs() <= TOLERANCE,
                        "flip {flip}, row {row}, x {x}: luma {got}, want {want}"
                    );
                }
            }
            let chroma = &bytes[(OUT.0 * OUT.1) as usize..out_len as usize];
            assert!(
                chroma.iter().all(|&c| (i32::from(c) - 128).abs() <= 2),
                "flip {flip}: chroma is not grey"
            );
        });
    }

    let out = Output {
        channel,
        crop: Crop::full(IN.0, IN.1),
        target,
        flip: false,
    };
    let mut times = Vec::with_capacity(100);
    let ((), allocated) = allocations(|| {
        for _ in 0..100 {
            let mut job = Job::new(source, &[out]).expect("job");
            let before = Instant::now();
            scaler.run(now(), &mut job).expect("run");
            times.push(before.elapsed());
        }
    });
    times.sort();
    assert_eq!(allocated, 0, "100 jobs on an open node");
    let report = format!(
        "{}: 1920x1080 to 640x360 NV12 on {channel:?}: median {:?}, slowest {:?}",
        node.display(),
        times[50],
        times[99]
    );
    // The board run reads this line.
    std::io::Write::write_all(&mut std::io::stderr(), format!("{report}\n").as_bytes())
        .expect("stderr");
}
