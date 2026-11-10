//! The board check: a frame encoded to JPEG and the JPEG decoded back, through the real block, on
//! dma-bufs from a DMA heap. Ignored by default; run with `--ignored`.
//!
//! - `LEMNOS_K230_VPU_NODE`: the block's node, default the one [`LinuxDevice::find`] answers.
//! - `LEMNOS_K230_VPU_HEAP`: the DMA heap the buffers come from, default
//!   `/dev/dma_heap/system` (the queues are `vb2_dma_sg`, and the block has its own MMU).
//!
//! What the sources leave open is printed rather than asserted: the formats and plane sizes the
//! kernel answers, the encoded size, and the decoded frame's first bytes.

#![cfg(target_os = "linux")]
#![expect(
    clippy::print_stderr,
    reason = "the check reports what the sources leave open for the board run's record"
)]

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use lemnos_driver_k230_vpu::{
    BufferId, Claim, Destination, Device, Direction, LinuxDevice, Payload, PixFormatMplane,
    PlaneFormat, Ready, Setup, Source, Vpu,
};
use lemnos_linux_sys::dma_buf::{self, SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE};
use lemnos_linux_sys::v4l2::BUF_FLAG_ERROR;
use lemnos_linux_sys::{dma_heap, mmap::Mapping};

const W: u32 = 64;
const H: u32 = 64;
const JPEG: u32 = u32::from_le_bytes(*b"JPEG");
const NV12: u32 = u32::from_le_bytes(*b"NV12");

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

fn format(pixelformat: u32, sizes: &[u32]) -> PixFormatMplane {
    let mut f = PixFormatMplane {
        width: W,
        height: H,
        pixelformat,
        num_planes: sizes.len() as u32,
        ..PixFormatMplane::default()
    };
    for (to, &size) in f.planes.iter_mut().zip(sizes) {
        *to = PlaneFormat {
            size_image: size,
            bytes_per_line: 0,
        };
    }
    f
}

/// Buffers for every plane of `format`, at the kernel's plane sizes.
fn planes<D: Device>(
    claim: &mut Claim<'_, D>,
    heap: &File,
    format: &PixFormatMplane,
    keep: &mut Vec<OwnedFd>,
) -> Vec<BufferId> {
    (0..format.num_planes as usize)
        .map(|i| {
            let fd = allocate(heap, format.planes[i].size_image.max(4096));
            keep.push(fd.try_clone().expect("dup"));
            claim.add_buffer(fd).expect("added")
        })
        .collect()
}

fn wait_for<D: Device>(claim: &Claim<'_, D>, want: Ready) {
    let ready = claim
        .wait(want, Some(Duration::from_secs(5)))
        .expect("wait");
    assert!(ready != Ready::default(), "the block answered within 5 s");
}

#[test]
#[ignore = "needs the codec block and a DMA heap"]
fn a_frame_round_trips_through_jpeg() {
    let device = match std::env::var("LEMNOS_K230_VPU_NODE") {
        Ok(path) => LinuxDevice::new(path),
        Err(_) => LinuxDevice::find().expect("listing").expect("the mvx node"),
    };
    let heap_path =
        std::env::var("LEMNOS_K230_VPU_HEAP").unwrap_or_else(|_| "/dev/dma_heap/system".into());
    let heap = File::open(&heap_path).expect("the heap");
    eprintln!("node {}, heap {heap_path}", device.path().display());
    let mut vpu = Vpu::new(device);
    let mut keep = Vec::new();
    let both = Ready {
        output: true,
        capture: true,
        event: false,
    };

    // Encode: a grey ramp in NV12 to JPEG.
    let encode = Setup {
        direction: Direction::Encode,
        source: format(NV12, &[W * H, W * H / 2]),
        destination: format(JPEG, &[1 << 16]),
        sources: 1,
        destinations: 1,
        controls: &[],
    };
    let mut claim = vpu.claim(&encode, 0).expect("an encoder claim");
    let (src, dst) = (claim.source_format(), claim.destination_format());
    eprintln!("encode: source {src:?}\nencode: destination {dst:?}");
    let src_ids = planes(&mut claim, &heap, &src, &mut keep);
    let dst_ids = planes(&mut claim, &heap, &dst, &mut keep);
    let luma: Vec<u8> = (0..W * H).map(|i| (i % W * 4) as u8).collect();
    write(&keep[0], &luma);
    write(&keep[1], &vec![0x80; (W * H / 2) as usize]);
    claim
        .give(&Destination {
            identity: 1,
            planes: &dst_ids,
        })
        .expect("given");
    let payload: Vec<Payload> = src_ids
        .iter()
        .zip([W * H, W * H / 2])
        .map(|(&buffer, bytes)| Payload { buffer, bytes })
        .collect();
    claim
        .submit(&Source {
            identity: 2,
            planes: &payload,
            timestamp: Duration::from_millis(1),
        })
        .expect("submitted");
    let started = Instant::now();
    let done = loop {
        wait_for(&claim, both);
        let _ = claim.complete_source().expect("source");
        if let Some(done) = claim.complete_destination().expect("destination") {
            break done;
        }
    };
    eprintln!("encode took {:?}: {done:?}", started.elapsed());
    assert_eq!(done.flags & BUF_FLAG_ERROR, 0);
    let jpeg = read(&keep[2], done.bytes[0] as usize);
    assert_eq!(&jpeg[..2], [0xff, 0xd8], "a JPEG starts with SOI");
    assert_eq!(&jpeg[jpeg.len() - 2..], [0xff, 0xd9], "and ends with EOI");
    drop(claim);

    // Decode it back: the destinations wait for the stream's format.
    let decode = Setup {
        direction: Direction::Decode,
        source: format(JPEG, &[1 << 16]),
        destination: format(NV12, &[W * H, W * H / 2]),
        sources: 1,
        destinations: 0,
        controls: &[],
    };
    let mut claim = vpu.claim(&decode, 0).expect("a decoder claim");
    let input = allocate(&heap, 1 << 16);
    write(&input, &jpeg);
    let src_id = claim.add_buffer(input).expect("added");
    claim
        .submit(&Source {
            identity: 3,
            planes: &[Payload {
                buffer: src_id,
                bytes: jpeg.len() as u32,
            }],
            timestamp: Duration::from_millis(2),
        })
        .expect("submitted");
    claim.drain().expect("drain");
    wait_for(
        &claim,
        Ready {
            event: true,
            ..Ready::default()
        },
    );
    let change = claim
        .source_change()
        .expect("events")
        .expect("a source change");
    eprintln!("decode: source change {change:?}");
    claim.adopt_destinations(1).expect("adopted");
    let out = claim.destination_format();
    eprintln!("decode: destination {out:?}");
    let mut out_fds = Vec::new();
    let out_ids = planes(&mut claim, &heap, &out, &mut out_fds);
    claim
        .give(&Destination {
            identity: 4,
            planes: &out_ids,
        })
        .expect("given");
    let done = loop {
        wait_for(&claim, both);
        let _ = claim.complete_source().expect("source");
        if let Some(done) = claim.complete_destination().expect("destination") {
            break done;
        }
    };
    eprintln!("decode: {done:?}");
    assert_eq!(done.flags & BUF_FLAG_ERROR, 0);
    let y = read(&out_fds[0], done.bytes[0] as usize);
    eprintln!("decoded luma, first row: {:02x?}", &y[..W as usize]);
}
