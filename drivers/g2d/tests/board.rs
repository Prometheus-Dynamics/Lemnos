//! The board check: each kind of job through a real G2D engine, on dma-bufs from a DMA heap, read
//! back and compared with what it must produce. Ignored by default; run with `--ignored`.
//!
//! - `LEMNOS_G2D_NODE`: the engine's node, default `/dev/g2d`.
//! - `LEMNOS_G2D_HEAP`: the DMA heap the buffers come from, default `/dev/dma_heap/system`.
//!
//! What the sources leave open is printed rather than asserted: the fill colour's byte layout and
//! which way a quarter turn goes.

#![cfg(target_os = "linux")]
#![expect(
    clippy::print_stderr,
    reason = "the check reports what the sources leave open for the board run's record"
)]

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};

use lemnos_driver_g2d::{
    Blend, ColorRange, Device, G2d, Gamut, Job, PixelFormat, Rect, Surface, Turn, node,
};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE};
use lemnos_linux_sys::{dma_heap, mmap::Mapping};

const W: u32 = 64;
const H: u32 = 32;
const BYTES: usize = (W * H * 4) as usize;

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn allocate(heap: &File) -> OwnedFd {
    dma_heap::allocate(heap.as_fd(), BYTES as u64).expect("a dma-buf from the heap")
}

/// Writes `pixels` (RGBA, 4 bytes each) into `buffer`.
fn write(buffer: &OwnedFd, pixels: &[u8]) {
    let mut map = Mapping::new(buffer.as_fd(), pixels.len(), 0, true).expect("map");
    dma_buf::sync(buffer.as_fd(), SYNC_START | SYNC_WRITE).expect("sync");
    map.as_mut_slice().copy_from_slice(pixels);
    dma_buf::sync(buffer.as_fd(), SYNC_END | SYNC_WRITE).expect("sync");
}

fn read(buffer: &OwnedFd) -> Vec<u8> {
    let map = Mapping::new(buffer.as_fd(), BYTES, 0, false).expect("map");
    dma_buf::sync(buffer.as_fd(), SYNC_START | SYNC_READ).expect("sync");
    let out = map.as_slice().to_vec();
    dma_buf::sync(buffer.as_fd(), SYNC_END | SYNC_READ).expect("sync");
    out
}

/// A picture where every pixel is different: x in byte 0, y in byte 1, 0x5a, opaque.
fn gradient(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| (0..w).flat_map(move |x| [x as u8, y as u8, 0x5a, 0xff]))
        .collect()
}

fn pixel(frame: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * w + x) * 4) as usize;
    frame[at..at + 4].try_into().expect("a pixel")
}

#[test]
#[ignore = "needs a G2D engine and a DMA heap"]
fn every_job_kind_on_the_engine() {
    let heap = File::open(env("LEMNOS_G2D_HEAP", "/dev/dma_heap/system")).expect("the heap");
    let mut g2d: G2d<Device, _> = G2d::new(node(env("LEMNOS_G2D_NODE", "/dev/g2d")));
    let (a, b, d) = (allocate(&heap), allocate(&heap), allocate(&heap));
    let rgba = PixelFormat::Rgba8888;
    let now = || 0;

    // Fill: every pixel the same, and not left zero.
    write(&d, &[0; BYTES]);
    let out = Surface::new(d.as_fd(), rgba, W, H).expect("surface");
    let job = Job::fill(
        &out,
        out.full(),
        0x8040_2010,
        Gamut::Bt601,
        ColorRange::Full,
    )
    .expect("job");
    g2d.run(now(), &job).expect("fill");
    eprintln!("engine versions: {:?}", g2d.version());
    let filled = read(&d);
    let first = pixel(&filled, W, 0, 0);
    eprintln!("fill 0x80402010 as bytes: {first:02x?}");
    assert_ne!(first, [0; 4], "the fill wrote nothing");
    assert!(
        filled.chunks(4).all(|p| p == first),
        "the fill is not uniform"
    );

    // A part-rectangle fill leaves the rest alone.
    write(&d, &[0; BYTES]);
    let part = Rect {
        x: 8,
        y: 4,
        w: 16,
        h: 8,
    };
    let job = Job::fill(&out, part, 0x8040_2010, Gamut::Bt601, ColorRange::Full).expect("job");
    g2d.run(now(), &job).expect("fill part");
    let filled = read(&d);
    for y in 0..H {
        for x in 0..W {
            let inside = (8..24).contains(&x) && (4..12).contains(&y);
            assert_eq!(pixel(&filled, W, x, y) == first, inside, "pixel {x},{y}");
        }
    }

    // Copy at the same size and format: byte for byte.
    write(&a, &gradient(W, H));
    let src = Surface::new(a.as_fd(), rgba, W, H).expect("surface");
    let job = Job::blit(
        &src,
        src.full(),
        &out,
        out.full(),
        Gamut::Bt601,
        ColorRange::Full,
    )
    .expect("job");
    g2d.run(now(), &job).expect("copy");
    assert_eq!(read(&d), gradient(W, H), "a plain copy changed the picture");

    // Half size: corners land near where they should.
    write(&d, &[0; BYTES]);
    let half = Rect {
        x: 0,
        y: 0,
        w: W / 2,
        h: H / 2,
    };
    let job = Job::blit(&src, src.full(), &out, half, Gamut::Bt601, ColorRange::Full).expect("job");
    g2d.run(now(), &job).expect("scale");
    let scaled = read(&d);
    let corner = pixel(&scaled, W, W / 2 - 1, H / 2 - 1);
    eprintln!("half-size corner: {corner:02x?}");
    assert!(
        corner[0] >= 56 && corner[1] >= 24,
        "the scaled corner is {corner:?}"
    );
    assert_eq!(
        pixel(&scaled, W, W / 2, H / 2),
        [0; 4],
        "wrote outside its rectangle"
    );

    // Quarter turn: a 64x32 picture into 32x64; every pixel lands somewhere, record where.
    write(&d, &[0; BYTES]);
    let tall = Surface::new(d.as_fd(), rgba, H, W).expect("surface");
    let job = Job::rotate(
        &src,
        src.full(),
        &tall,
        tall.full(),
        Turn::Cw90,
        false,
        false,
    )
    .expect("job");
    g2d.run(now(), &job).expect("turn");
    let turned = read(&d);
    let top_left = pixel(&turned, H, 0, 0);
    eprintln!(
        "after Cw90, top-left came from x={}, y={}",
        top_left[0], top_left[1]
    );
    let clockwise = top_left[..2] == [0, (H - 1) as u8];
    for y in 0..W {
        for x in 0..H {
            let (sx, sy) = if clockwise {
                (y, H - 1 - x)
            } else {
                (W - 1 - y, x)
            };
            assert_eq!(
                pixel(&turned, H, x, y),
                pixel(&gradient(W, H), W, sx, sy),
                "{x},{y}"
            );
        }
    }

    // Blend: an opaque foreground over anything is the foreground.
    write(&b, &[0x33; BYTES]);
    let argb = PixelFormat::Argb8888;
    let fg = Surface::new(a.as_fd(), argb, W, H).expect("surface");
    let bg = Surface::new(b.as_fd(), argb, W, H).expect("surface");
    let out = Surface::new(d.as_fd(), argb, W, H).expect("surface");
    let opaque: Vec<u8> = gradient(W, H)
        .chunks(4)
        .flat_map(|p| [p[0], p[1], p[2], 0xff])
        .collect();
    write(&a, &opaque);
    let job = Job::blend(
        &fg,
        fg.full(),
        &bg,
        bg.full(),
        &out,
        out.full(),
        Blend::SrcOver,
    )
    .expect("job");
    g2d.run(now(), &job).expect("blend");
    let blended = read(&d);
    eprintln!("blend, first pixel: {:02x?}", pixel(&blended, W, 0, 0));
    assert_eq!(blended, opaque, "an opaque foreground over the background");
}
