//! The board check: conversions through a real engine, on dma-bufs from a DMA heap, read back
//! and compared with what they must produce. Ignored by default; run with `--ignored`.
//!
//! - `LEMNOS_NONAI2D_NODE`: the engine's node, default the one [`find`] answers.
//! - `LEMNOS_NONAI2D_HEAP`: the DMA heap the buffers come from (the queues are
//!   `vb2_dma_contig`, so a contiguous heap), default `/dev/dma_heap/linux,cma`.
//!
//! What the sources leave open is printed rather than asserted: what an NV12 frame of one colour
//! becomes in RGB.

#![cfg(target_os = "linux")]
#![expect(
    clippy::print_stderr,
    reason = "the check reports what the sources leave open for the board run's record"
)]

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;

use lemnos_driver_nonai2d::{Frame, Layout, Nonai2d, PixelFormat, find, node};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE};
use lemnos_linux_sys::{dma_heap, mmap::Mapping};

const W: u32 = 64;
const H: u32 = 32;

fn allocate(heap: &File, len: u32) -> OwnedFd {
    dma_heap::allocate(heap.as_fd(), u64::from(len)).expect("a dma-buf from the heap")
}

fn write(buffer: &OwnedFd, bytes: &[u8]) {
    let mut map = Mapping::new(buffer.as_fd(), bytes.len(), 0, true).expect("map");
    dma_buf::sync(buffer.as_fd(), SYNC_START | SYNC_WRITE).expect("sync");
    map.as_mut_slice().copy_from_slice(bytes);
    dma_buf::sync(buffer.as_fd(), SYNC_END | SYNC_WRITE).expect("sync");
}

fn read(buffer: &OwnedFd, len: usize) -> Vec<u8> {
    let map = Mapping::new(buffer.as_fd(), len, 0, false).expect("map");
    dma_buf::sync(buffer.as_fd(), SYNC_START | SYNC_READ).expect("sync");
    let bytes = map.as_slice().to_vec();
    dma_buf::sync(buffer.as_fd(), SYNC_END | SYNC_READ).expect("sync");
    bytes
}

#[test]
#[ignore = "needs the engine and a DMA heap"]
fn conversions_on_the_engine() {
    let path = std::env::var_os("LEMNOS_NONAI2D_NODE")
        .map(PathBuf::from)
        .or_else(find)
        .expect("the nonai-2d node");
    let heap_path = std::env::var("LEMNOS_NONAI2D_HEAP")
        .unwrap_or_else(|_| "/dev/dma_heap/linux,cma".to_owned());
    let heap = File::open(&heap_path).expect("the heap");
    eprintln!("node {}, heap {heap_path}", path.display());
    let mut engine = Nonai2d::new(node(&path));
    let layout = |format| Layout::new(format, W, H).expect("layout");

    // A, R, G, B to B, G, R, A: every pixel's bytes reversed.
    let (argb, abgr) = (layout(PixelFormat::Argb32), layout(PixelFormat::Abgr32));
    let len = argb.bytes();
    let (src, dst) = (allocate(&heap, len), allocate(&heap, len));
    write(&src, &[0xff, 0x10, 0x20, 0x30].repeat((W * H) as usize));
    write(&dst, &vec![0; len as usize]);
    let from = Frame::new(argb, &[src.as_fd()]).expect("source");
    let to = Frame::new(abgr, &[dst.as_fd()]).expect("destination");
    engine.run(0, &from, &to).expect("converted");
    let out = read(&dst, len as usize);
    assert!(
        out.chunks(4).all(|p| p == [0x30, 0x20, 0x10, 0xff]),
        "first pixels {:02x?}",
        &out[..16]
    );

    // NV12 of one colour to A, R, G, B: every pixel the same, printed for the record.
    let nv12 = layout(PixelFormat::Nv12);
    let planes = nv12.planes();
    let (y, uv) = (
        allocate(&heap, planes[0].size),
        allocate(&heap, planes[1].size),
    );
    write(&y, &vec![0x80; planes[0].size as usize]);
    write(&uv, &[0x40, 0xc0].repeat(planes[1].size as usize / 2));
    let from = Frame::new(nv12, &[y.as_fd(), uv.as_fd()]).expect("source");
    let to = Frame::new(argb, &[dst.as_fd()]).expect("destination");
    engine.run(1, &from, &to).expect("converted");
    let out = read(&dst, len as usize);
    assert!(out.chunks(4).all(|p| p == &out[..4]), "one colour");
    eprintln!(
        "NV12 Y=0x80 U=0x40 V=0xc0 became A, R, G, B = {:02x?}",
        &out[..4]
    );
}
