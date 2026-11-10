//! The job streams through the host's own stream: a consumer attaches on the real ring, fills
//! buffers, posts jobs, and reads completions and pixels back, while the driver serves the
//! producer's end it is given. Sealed memfds stand in for dma-bufs on the streams that have
//! handles; the units are the mainline route's played-back node (`v4l2::tests::Mock`) and a
//! stand-in for `/dev/rga` that checks and records each request it is handed. No test opens a
//! device node.

use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use lemnos_contract_image_blend::{self as blend, BlendJob, BlendOp};
use lemnos_contract_image_convert::{self as convert, ConvertJob};
use lemnos_contract_image_scaler::{self as scaler, Filter, ScaleJob};
use lemnos_core::{CallerId, Encode, ErrorKind, Health, Lifecycle, Status, Writer};
use lemnos_driver_rga_sys::request::Job as Request;
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::StreamEndpoint;
use lemnos_host::board::parse;
use lemnos_host::stream::{Backing, BufferMemory, HeapAllocator, Lease, StreamAllocator};
use lemnos_host::{Host, Options, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::SimBoard;
use lemnos_wire::ring::{Consumer, FrameSlot, Layout as RingLayout};

use lemnos_contract_image_blend::ImageBlend;
use lemnos_contract_image_convert::ImageConvert;
use lemnos_contract_image_scaler::ImageScaler;

use super::hardware::Opener;
use super::*;
use crate::caps::Soc;
use crate::cpu::Matrix;
use crate::jobs::Settings;
use crate::multirga::Submit;
use crate::v4l2::M2mRoute;
use crate::v4l2::tests::Mock;
use crate::{BlendMode, Rga, RgaFormat, RgaNeeds};

/// Each stream's buffers: one 256 by 256 frame of four bytes a pixel.
const BYTES: u32 = 256 * 256 * 4;
const BUFFERS: u32 = 4;
const AR24: u32 = u32::from_le_bytes(*b"AR24");
const RGB3: u32 = u32::from_le_bytes(*b"RGB3");
const GREY: u32 = u32::from_le_bytes(*b"GREY");
const MS: u64 = 1_000_000;

/// What the streams' buffers are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Memory {
    /// Plain shared memory: no handles, `read` and `write` only.
    Plain,
    /// A sealed memfd per buffer, lent as its handle.
    Handles,
    /// A host that lends, for each buffer, a memfd shorter than the buffer: the driver must go
    /// by the handle's real size.
    ShortHandles,
}

/// The pool as the host addresses it, over one mapping per buffer.
struct Mapped {
    buffers: Vec<Mapping>,
    stride: usize,
    bytes: usize,
}

impl Mapped {
    fn at(&self, offset: usize, len: usize) -> Option<(&Mapping, usize)> {
        let inner = offset % self.stride;
        let fits = inner.checked_add(len).is_some_and(|end| end <= self.bytes);
        fits.then(|| self.buffers.get(offset / self.stride).map(|m| (m, inner)))?
    }
}

impl BufferMemory for Mapped {
    fn write(&self, offset: usize, data: &[u8]) -> bool {
        self.at(offset, data.len())
            .is_some_and(|(map, inner)| map.write_bytes(inner, data).is_some())
    }

    fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        let Some((map, inner)) = self.at(offset, out.len()) else {
            return false;
        };
        out.copy_from_slice(&map.as_slice()[inner..inner + out.len()]);
        true
    }
}

struct Allocator(Memory);

impl StreamAllocator for Allocator {
    fn allocate(&self, name: &str, layout: &RingLayout) -> Result<Backing, Status> {
        let plain = HeapAllocator.allocate(name, layout)?;
        if self.0 == Memory::Plain {
            return Ok(plain);
        }
        let (mut handles, mut buffers) = (Vec::new(), Vec::new());
        for _ in 0..layout.buffers {
            let fd: OwnedFd =
                sealed_memfd(c"rga-stream-test", layout.stride()).map_err(|_| Status::IO)?;
            let bytes = layout.buffer_bytes as usize;
            buffers.push(Mapping::new(fd.as_fd(), bytes, 0, true).map_err(|_| Status::IO)?);
            handles.push(match self.0 {
                Memory::ShortHandles => sealed_memfd(c"rga-short", 4096).map_err(|_| Status::IO)?,
                _ => fd,
            });
        }
        Ok(Backing {
            buffers: Arc::new(Mapped {
                buffers,
                stride: layout.stride() as usize,
                bytes: layout.buffer_bytes as usize,
            }),
            handles: Some(handles.into()),
            ..plain
        })
    }
}

fn host(memory: Memory) -> Host {
    let stream = |name| {
        format!(
            "[devices.streams.{name}]\nbuffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n\
             job_slots = 16\njob_bytes = 128\n"
        )
    };
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n{}{}{}",
        stream("scale"),
        stream("convert"),
        stream("blend")
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
        Arc::new(Allocator(memory)),
    )
    .unwrap()
}

/// One request the `/dev/rga` stand-in was handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    render_mode: u8,
    alpha_rop_flag: u16,
    pd_mode: u8,
    yuv2rgb_mode: u8,
    /// The source and destination: descriptor, format, virtual width and height.
    src: (u64, u32, u16, u16),
    dst: (u64, u32, u16, u16),
}

#[derive(Default)]
struct Requests {
    seen: [Option<Seen>; 2],
    count: usize,
    fail: bool,
}

/// `/dev/rga` as the driver reaches it: each request built by the crate's own builders, checked
/// as the node's call checks it, and recorded instead of sent. Recording allocates nothing.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Requests>>);

impl Recorder {
    fn with<T>(&self, f: impl FnOnce(&mut Requests) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }

    fn take(&self) -> Vec<Seen> {
        self.with(|r| {
            let n = r.count.min(2);
            let seen = r.seen[..n].iter().flatten().copied().collect();
            *r = Requests {
                fail: r.fail,
                ..Requests::default()
            };
            seen
        })
    }
}

impl Submit for Recorder {
    fn submit(&self, job: &mut Request<'_>) -> io::Result<()> {
        job.check()?;
        let r = job.request();
        let image =
            |i: &lemnos_driver_rga_sys::request::ImgInfo| (i.yrgb_addr, i.format, i.vir_w, i.vir_h);
        let seen = Seen {
            render_mode: r.render_mode,
            alpha_rop_flag: r.alpha_rop_flag,
            pd_mode: r.pd_mode,
            yuv2rgb_mode: r.yuv2rgb_mode,
            src: image(r.src()),
            dst: image(r.dst()),
        };
        self.with(|r| {
            if r.fail {
                return Err(io::Error::from_raw_os_error(5));
            }
            if let Some(slot) = r.seen.get_mut(r.count) {
                *slot = Some(seen);
            }
            r.count += 1;
            Ok(())
        })
    }
}

type Producer = Box<dyn StreamEndpoint<Error = ErrorKind> + Send>;

fn producer(host: &Host, name: &str) -> Producer {
    Box::new(host.stream_producer(0, name).unwrap())
}

/// The driver on the host's three streams, with the units given.
fn driver(
    host: &Host,
    settings: Settings,
    soc: Option<Soc>,
    m2m: Option<&Mock>,
    multi: Option<&Recorder>,
) -> Rga {
    let m2m = m2m
        .cloned()
        .map(|mock| -> Opener<Box<dyn M2mRoute>> { Box::new(move || Ok(Box::new(mock.rga()))) });
    let multi = multi
        .cloned()
        .map(|rec| -> Opener<Box<dyn Submit>> { Box::new(move || Ok(Box::new(rec.clone()))) });
    let needs = RgaNeeds {
        scale: Some(producer(host, "scale")),
        convert: Some(producer(host, "convert")),
        blend: Some(producer(host, "blend")),
    };
    Rga::serving(needs, settings, Routes::new(soc, m2m, multi))
}

fn cpu_driver(host: &Host) -> Rga {
    driver(host, Settings::default(), None, None, None)
}

/// A consumer attached to stream `name`.
struct Client {
    lease: Lease,
    ring: Consumer,
}

impl Client {
    fn attach(host: &Host, name: &str, caller: u64) -> Self {
        let lease = host.stream_attach(0, name, CallerId(caller)).unwrap();
        let ring = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
        Self { lease, ring }
    }

    fn fill(&self, buffer: u32, bytes: &[u8]) {
        let offset = self.lease.layout.buffer_offset(buffer).unwrap();
        assert!(self.lease.buffers.write(offset, bytes));
    }

    fn bytes(&self, buffer: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let offset = self.lease.layout.buffer_offset(buffer).unwrap();
        assert!(self.lease.buffers.read(offset, &mut out));
        out
    }

    fn post(&mut self, job: &impl Encode) {
        let mut bytes = [0u8; 128];
        let mut writer = Writer::new(&mut bytes);
        job.encode(&mut writer).unwrap();
        let used = writer.len();
        assert!(self.ring.push_job(&bytes[..used]).unwrap());
    }

    fn done(&mut self) -> FrameSlot {
        self.ring.pop_frame().unwrap().expect("a completion")
    }

    fn release(&mut self, frame: FrameSlot) {
        assert!(self.ring.release(frame.buffer, frame.fence).unwrap());
    }

    /// The raw descriptor of `buffer`'s handle, as the driver is lent it.
    fn fd(&self, buffer: u32) -> u64 {
        self.lease.handles.as_ref().unwrap()[buffer as usize].as_raw_fd() as u64
    }
}

fn scale_ref(buffer: u32, width: u32, height: u32) -> scaler::ImageRef {
    scaler::ImageRef {
        buffer,
        width,
        height,
        fourcc: AR24,
        stride: width * 4,
    }
}

fn scale_job(identity: u64, input: u32, output: u32, from: u32, to: u32) -> ScaleJob {
    ScaleJob {
        identity,
        input: scale_ref(input, from, from),
        output: scale_ref(output, to, to),
        filter: Filter::Area,
    }
}

fn convert_ref(buffer: u32, width: u32, height: u32, fourcc: u32, bpp: u32) -> convert::ImageRef {
    convert::ImageRef {
        buffer,
        width,
        height,
        fourcc,
        stride: width * bpp,
    }
}

fn blend_ref(buffer: u32, width: u32, height: u32) -> blend::ImageRef {
    blend::ImageRef {
        buffer,
        width,
        height,
        fourcc: AR24,
        stride: width * 4,
    }
}

fn blend_job(identity: u64, fg: u32, bg: u32, out: u32) -> BlendJob {
    BlendJob {
        identity,
        op: BlendOp::SrcOver,
        foreground: blend_ref(fg, 2, 2),
        background: blend_ref(bg, 2, 2),
        output: blend_ref(out, 2, 2),
    }
}

/// A frame of `len` bytes that is not all one value.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|n| (n % 251) as u8 ^ seed.wrapping_mul(31))
        .collect()
}

/// What the CPU route makes of a resize, from the crate's own checked `Jobs` on plain slices.
fn reference_scale(job: &ScaleJob, input: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; (job.output.stride * job.output.height) as usize];
    let mut jobs = Jobs::new(Settings::default());
    assert_ne!(jobs.scale(job, input, &mut out), 0);
    out
}

#[test]
fn the_cpu_route_serves_all_three_contracts_with_real_pixels_on_a_plain_stream() {
    let host = host(Memory::Plain);
    let mut rga = cpu_driver(&host);
    let mut scale = Client::attach(&host, "scale", 1);
    let mut conv = Client::attach(&host, "convert", 2);
    let mut blend = Client::attach(&host, "blend", 3);
    assert_eq!(
        rga.tick(0),
        Some(MS),
        "holds the free buffers, and looks again in 1 ms"
    );

    // A 2x area downscale of 8 by 8: each output pixel is its 2 by 2 block's mean.
    let input: Vec<u8> = (0..=255).collect();
    scale.fill(0, &input);
    let job = scale_job(0xA, 0, 1, 8, 4);
    scale.post(&job);
    // RGB to B, G, R, A.
    conv.fill(0, &[10, 20, 30, 40, 50, 60]);
    conv.post(&ConvertJob {
        identity: 0xB,
        input: convert_ref(0, 2, 1, RGB3, 3),
        output: convert_ref(1, 2, 1, AR24, 4),
    });
    // Half-transparent red over opaque blue, premultiplied.
    blend.fill(0, &[0, 0, 128, 128].repeat(4));
    blend.fill(1, &[200, 0, 0, 255].repeat(4));
    blend.post(&blend_job(0xC, 0, 1, 2));
    rga.tick(MS);

    let done = scale.done();
    assert_eq!(
        (done.buffer, done.identity, done.bytes),
        (1, 0xA, 4 * 4 * 4)
    );
    let got = scale.bytes(1, 64);
    // The first byte of the first block: bytes 0, 4, 32 and 36, whose mean is 18.
    assert_eq!(got[0], 18);
    assert_eq!(got, reference_scale(&job, &input));

    let done = conv.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 0xB, 8));
    assert_eq!(conv.bytes(1, 8), [30, 20, 10, 255, 60, 50, 40, 255]);

    let done = blend.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (2, 0xC, 16));
    // Blue: 200 * (255 - 128) / 255 rounds to 100; alpha: 128 + 255 * 127 / 255.
    assert_eq!(blend.bytes(2, 16), [100, 0, 128, 255].repeat(4));

    assert_eq!(ImageScaler::counters(&mut rga).unwrap().jobs, 1);
    assert_eq!(ImageConvert::counters(&mut rga).unwrap().jobs, 1);
    assert_eq!(ImageBlend::counters(&mut rga).unwrap().jobs, 1);
    let mut name = [0u8; 32];
    assert_eq!(ImageBlend::job_stream(&mut rga, &mut name), Ok(5));
    assert_eq!(&name[..5], b"blend");
    for stream in ["scale", "convert", "blend"] {
        let stats = host.stream_stats(0, stream).unwrap();
        assert_eq!((stats.faults, stats.dropped), (0, 0), "{stream}");
    }
}

#[test]
fn the_cpu_route_works_the_same_on_a_stream_with_handles() {
    let host = host(Memory::Handles);
    let mut rga = cpu_driver(&host);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    let input = pattern(16 * 16 * 4, 2);
    scale.fill(2, &input);
    let job = scale_job(5, 2, 3, 16, 8);
    scale.post(&job);
    rga.tick(1);
    let done = scale.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (3, 5, 256));
    assert_eq!(scale.bytes(3, 256), reference_scale(&job, &input));
}

#[test]
fn jobs_the_driver_cannot_run_complete_as_failed_and_are_counted() {
    let host = host(Memory::Plain);
    let mut rga = cpu_driver(&host);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    scale.post(&scale_job(1, 0, 1, 8, 4));
    rga.tick(1);
    let first = scale.done();
    assert_eq!(first.bytes, 64);

    // Buffer 1 is the consumer's until it is returned: not an input.
    scale.post(&scale_job(2, 1, 2, 4, 2));
    rga.tick(2);
    assert_eq!(rga.last_failure(), Some(Failure::Buffers));
    let failed = scale.done();
    assert_eq!((failed.buffer, failed.identity, failed.bytes), (2, 2, 0));
    // The same buffer twice.
    scale.post(&scale_job(3, 3, 3, 8, 4));
    rga.tick(3);
    assert_eq!(rga.last_failure(), Some(Failure::Refused));
    let failed = scale.done();
    assert_eq!((failed.buffer, failed.identity, failed.bytes), (3, 3, 0));
    scale.release(failed);
    // An empty image.
    scale.release(first);
    scale.post(&ScaleJob {
        input: ScaleJob::default().input,
        ..scale_job(4, 0, 1, 8, 4)
    });
    rga.tick(4);
    assert_eq!(rga.last_failure(), Some(Failure::Refused));
    assert_eq!(scale.done().bytes, 0);
    scale.post(&ScaleJob {
        input: scaler::ImageRef {
            height: 257,
            ..scale_ref(0, 256, 256)
        },
        ..scale_job(5, 0, 3, 8, 4)
    });
    // One row taller than the stream's buffer.
    rga.tick(5);
    assert_eq!(rga.last_failure(), Some(Failure::Size));
    let done = scale.done();
    assert_eq!((done.identity, done.bytes), (5, 0));
    // Bytes that are no job: counted, and nothing to complete.
    assert!(scale.ring.push_job(&[1, 2, 3]).unwrap());
    rga.tick(6);
    assert_eq!(rga.last_failure(), Some(Failure::Undecodable));
    assert_eq!(scale.ring.pop_frame().unwrap(), None);
    let c = ImageScaler::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (1, 5));
}

#[test]
fn an_image_past_its_buffer_is_refused_before_any_memory_is_taken_for_it() {
    let host = host(Memory::Plain);
    let mut rga = cpu_driver(&host);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    // Inside the configured limits, 64 MiB, far past the stream's 256 KiB buffer.
    scale.post(&scale_job(1, 0, 1, 4096, 8));
    let ((), count) = allocations(|| {
        rga.tick(1);
    });
    assert_eq!(rga.last_failure(), Some(Failure::Size));
    assert_eq!(scale.done().bytes, 0);
    assert_eq!(count, 0, "allocations for a job its buffer can't hold");
}

#[test]
fn a_thousand_cpu_jobs_through_the_stream_allocate_nothing() {
    let host = host(Memory::Plain);
    let mut rga = cpu_driver(&host);
    let mut scale = Client::attach(&host, "scale", 1);
    let mut conv = Client::attach(&host, "convert", 2);
    let mut blend = Client::attach(&host, "blend", 3);
    rga.tick(0);
    let mut cycle = |n: u64| {
        let out = 1 + (n % 3) as u32;
        scale.post(&scale_job(n, 0, out, 32, 16));
        conv.post(&ConvertJob {
            identity: n,
            input: convert_ref(0, 32, 32, AR24, 4),
            output: convert_ref(out, 32, 32, u32::from_le_bytes(*b"NV12"), 1),
        });
        blend.post(&blend_job(n, 0, if out == 1 { 2 } else { 1 }, out));
        rga.tick(n * MS);
        for (client, bytes) in [(&mut scale, 1024), (&mut conv, 1536), (&mut blend, 16)] {
            let done = client.done();
            assert_eq!((done.identity, done.bytes), (n, bytes));
            client.release(done);
        }
    };
    for n in 0..10 {
        cycle(n);
    }
    let ((), count) = allocations(|| {
        for n in 10..340 {
            cycle(n);
        }
    });
    assert_eq!(count, 0, "allocations in 990 jobs");
}

#[test]
fn a_new_consumer_on_a_plain_stream_gets_its_completions() {
    let host = host(Memory::Plain);
    let mut rga = cpu_driver(&host);
    let first = Client::attach(&host, "scale", 1);
    rga.tick(0);
    drop(first);
    host.stream_detach(0, "scale", CallerId(1)).unwrap();
    // No consumer: everything held is given up, and nothing is held until one comes.
    rga.tick(1);
    rga.tick(2);
    let mut second = Client::attach(&host, "scale", 2);
    second.post(&scale_job(9, 0, 1, 8, 4));
    rga.tick(3);
    let done = second.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 9, 64));
}

#[test]
fn a_new_consumer_on_a_stream_with_handles_gets_its_completions_at_once() {
    let host = host(Memory::Handles);
    let mut rga = cpu_driver(&host);
    let _first = Client::attach(&host, "scale", 1);
    rga.tick(0);
    host.stream_detach(0, "scale", CallerId(1)).unwrap();
    // A new consumer before the driver looks again: the job's buffers are renewed by handle.
    let mut second = Client::attach(&host, "scale", 2);
    second.post(&scale_job(9, 2, 3, 8, 4));
    rga.tick(1);
    let done = second.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (3, 9, 64));
}

/// The mainline route on the played-back node, set to report a whole 128 by 128 frame written.
fn m2m_mock() -> (Mock, Vec<u8>) {
    let mock = Mock::default();
    let frame = pattern(128 * 128 * 4, 7);
    mock.with(|d| d.capture_fill = frame.clone());
    (mock, frame)
}

fn rk3588(host: &Host, mock: &Mock, rec: &Recorder) -> Rga {
    driver(
        host,
        Settings::default(),
        Some(Soc::Rk3588),
        Some(mock),
        Some(rec),
    )
}

#[test]
fn a_resize_goes_to_the_mainline_route_with_the_streams_dma_bufs() {
    let host = host(Memory::Handles);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    scale.post(&scale_job(1, 0, 1, 256, 128));
    rga.tick(1);
    let done = scale.done();
    assert_eq!(
        (done.buffer, done.identity, done.bytes),
        (1, 1, 128 * 128 * 4)
    );
    mock.with(|d| {
        assert_eq!((d.queued_dmabuf, d.queued_mmap), (2, 0));
        // The source and the result are the stream's own buffers, by descriptor.
        assert_eq!(d.planes[0].2.map(|fd| fd as u64), Some(scale.fd(0)));
        assert_eq!(d.planes[1].2.map(|fd| fd as u64), Some(scale.fd(1)));
        assert_eq!(d.planes[0].0, BYTES);
    });
    assert!(rec.take().is_empty());
    scale.release(done);

    let ((), count) = allocations(|| {
        for n in 0..100 {
            scale.post(&scale_job(n, 0, 1, 256, 128));
            rga.tick(2 + n);
            let done = scale.done();
            assert_eq!(done.bytes, 128 * 128 * 4);
            scale.release(done);
        }
    });
    assert_eq!(count, 0, "allocations in 100 jobs on dma-bufs");
    assert_eq!(mock.with(|d| (d.opens, d.queued_mmap)), (1, 0));
}

#[test]
fn a_resize_on_a_plain_stream_goes_to_the_mainline_route_as_bytes() {
    let host = host(Memory::Plain);
    let (mock, frame) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    scale.fill(0, &pattern(BYTES as usize, 3));
    scale.post(&scale_job(1, 0, 1, 256, 128));
    rga.tick(1);
    let done = scale.done();
    assert_eq!((done.identity, done.bytes), (1, 128 * 128 * 4));
    // What the device wrote is what the consumer reads.
    assert_eq!(scale.bytes(1, frame.len()), frame);
    assert_eq!(mock.with(|d| (d.queued_dmabuf, d.queued_mmap)), (0, 2));
    scale.release(done);
    let ((), count) = allocations(|| {
        for n in 0..100 {
            scale.post(&scale_job(n, 0, 1, 256, 128));
            rga.tick(2 + n);
            let done = scale.done();
            assert_eq!(done.bytes, 128 * 128 * 4);
            scale.release(done);
        }
    });
    assert_eq!(count, 0, "allocations in 100 jobs copied through");
}

#[test]
fn a_failed_unit_hands_its_jobs_to_the_cpu_and_is_retried_capped_at_eight_seconds() {
    let host = host(Memory::Handles);
    let (mock, frame) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    let input = pattern(BYTES as usize, 4);
    scale.fill(0, &input);
    let job = scale_job(1, 0, 1, 256, 128);
    let reference = reference_scale(&job, &input);
    assert_ne!(reference, frame);
    mock.with(|d| d.fail = true);

    let mut now = 1_000 * MS;
    let run = |scale: &mut Client, rga: &mut Rga, now: u64| {
        scale.post(&job);
        rga.tick(now);
        let done = scale.done();
        assert_eq!(done.bytes, 128 * 128 * 4, "every job completes");
        let out = scale.bytes(1, done.bytes as usize);
        scale.release(done);
        out
    };
    // It fails: the CPU's result, and a wait.
    assert_eq!(run(&mut scale, &mut rga, now), reference);
    assert!(matches!(rga.health(), Health::Degraded(_)));
    let mut waits = Vec::new();
    for _ in 0..8 {
        let due = rga_due(&rga);
        waits.push((due - now) / MS);
        // Before the wait is over the node isn't opened again; the CPU serves.
        let opens = mock.with(|d| d.opens);
        assert_eq!(run(&mut scale, &mut rga, due - 1), reference);
        assert_eq!(mock.with(|d| d.opens), opens);
        now = due;
        assert_eq!(run(&mut scale, &mut rga, now), reference);
        assert_eq!(mock.with(|d| d.opens), opens + 1, "opened again once");
    }
    assert_eq!(waits, [250, 500, 1000, 2000, 4000, 8000, 8000, 8000]);
    // It answers again: the job is the unit's, and the driver is healthy.
    mock.with(|d| d.fail = false);
    let due = rga_due(&rga);
    run(&mut scale, &mut rga, due);
    assert_eq!(mock.with(|d| d.queued_dmabuf), 2);
    assert_eq!(rga.health(), Health::Ok);
    assert!(rga.server.routes().m2m().unwrap().is_open());
}

fn rga_due(rga: &Rga) -> u64 {
    rga.server.routes().m2m().unwrap().due_ns()
}

#[test]
fn a_blend_goes_to_dev_rga_as_a_copy_and_a_blend_on_the_streams_dma_bufs() {
    let host = host(Memory::Handles);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let mut blend = Client::attach(&host, "blend", 1);
    rga.tick(0);
    blend.post(&blend_job(7, 0, 1, 2));
    rga.tick(1);
    let done = blend.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (2, 7, 16));
    let bgra = RgaFormat::Bgra8888.code() as u32;
    let image = |buffer| (blend.fd(buffer), bgra, 2, 2);
    let seen = rec.take();
    assert_eq!(seen.len(), 2);
    // The background into the output, as a plain blit.
    assert_eq!(
        (
            seen[0].render_mode,
            seen[0].alpha_rop_flag,
            seen[0].src,
            seen[0].dst
        ),
        (0, 0, image(1), image(2))
    );
    // The foreground onto it: Porter-Duff, source over.
    assert_eq!(
        (
            seen[1].alpha_rop_flag,
            seen[1].pd_mode,
            seen[1].src,
            seen[1].dst
        ),
        (0x9, BlendMode::SrcOver.pd_code(), image(0), image(2))
    );
    blend.release(done);
    let ((), count) = allocations(|| {
        for n in 0..100 {
            blend.post(&blend_job(n, 0, 1, 2));
            rga.tick(2 + n);
            let done = blend.done();
            assert_eq!(done.bytes, 16);
            blend.release(done);
        }
    });
    assert_eq!(count, 0, "allocations in 100 blends on /dev/rga");
    assert_eq!(rec.with(|r| r.count), 200);
}

#[test]
fn dev_rga_takes_dma_bufs_only_so_a_plain_stream_blends_on_the_cpu() {
    let host = host(Memory::Plain);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let mut blend = Client::attach(&host, "blend", 1);
    rga.tick(0);
    blend.fill(0, &[0, 0, 128, 128].repeat(4));
    blend.fill(1, &[200, 0, 0, 255].repeat(4));
    blend.post(&blend_job(7, 0, 1, 2));
    rga.tick(1);
    assert_eq!(blend.done().bytes, 16);
    assert_eq!(blend.bytes(2, 16), [100, 0, 128, 255].repeat(4));
    assert!(rec.take().is_empty());
}

#[test]
fn a_handle_shorter_than_its_image_is_caught_by_its_real_size() {
    let host = host(Memory::ShortHandles);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let mut blend = Client::attach(&host, "blend", 1);
    rga.tick(0);
    // 64 by 32 at four bytes is 8 KiB: inside the stream's buffer, past the 4 KiB handle.
    blend.post(&BlendJob {
        foreground: blend_ref(0, 64, 32),
        background: blend_ref(1, 64, 32),
        output: blend_ref(2, 64, 32),
        ..blend_job(3, 0, 1, 2)
    });
    rga.tick(1);
    assert_eq!(rga.last_failure(), Some(Failure::Size));
    assert_eq!(blend.done().bytes, 0);
    assert!(rec.take().is_empty(), "no request left");
}

#[test]
fn packed_gray_input_goes_to_dev_rga_and_padded_gray_to_the_cpu() {
    // The node's full-range gray input repeats the byte, as the CPU does under every matrix.
    for (matrix, stride, to_unit) in [
        (Matrix::Bt601Limited, 4, true),
        (Matrix::Bt709Limited, 4, true),
        (Matrix::Bt601Limited, 8, false),
    ] {
        let host = host(Memory::Handles);
        let (mock, _) = m2m_mock();
        let rec = Recorder::default();
        let settings = Settings {
            matrix,
            ..Settings::default()
        };
        let mut rga = driver(&host, settings, Some(Soc::Rk3588), Some(&mock), Some(&rec));
        let mut conv = Client::attach(&host, "convert", 1);
        rga.tick(0);
        conv.fill(0, &[16, 235, 128, 0, 9, 9, 9, 9]);
        conv.post(&ConvertJob {
            identity: 4,
            input: convert::ImageRef {
                stride,
                ..convert_ref(0, 4, 1, GREY, 1)
            },
            output: convert_ref(1, 4, 1, AR24, 4),
        });
        rga.tick(1);
        assert_eq!(conv.done().bytes, 16);
        let seen = rec.take();
        if to_unit {
            assert_eq!(seen.len(), 1, "{matrix:?}");
            let gray = RgaFormat::Gray8.code() as u32;
            let rgba = RgaFormat::Rgba8888.code() as u32;
            assert_eq!(seen[0].src, (conv.fd(0), gray, 4, 1));
            assert_eq!(seen[0].dst, (conv.fd(1), rgba, 4, 1));
            assert_eq!(seen[0].yuv2rgb_mode, 1);
        } else {
            assert!(seen.is_empty());
            let want: Vec<u8> = [16, 235, 128, 0]
                .iter()
                .flat_map(|&g| [g, g, g, 255])
                .collect();
            assert_eq!(conv.bytes(1, 16), want);
        }
    }
}

#[test]
fn without_a_soc_no_unit_is_ever_opened() {
    let host = host(Memory::Handles);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = driver(&host, Settings::default(), None, Some(&mock), Some(&rec));
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    scale.post(&scale_job(1, 0, 1, 256, 128));
    rga.tick(1);
    assert_eq!(scale.done().bytes, 128 * 128 * 4);
    assert_eq!(mock.with(|d| d.opens), 0);
    assert!(rec.take().is_empty());
}

#[test]
fn a_failing_dev_rga_leaves_the_blend_to_the_cpu_with_the_same_result() {
    let host = host(Memory::Handles);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    rec.with(|r| r.fail = true);
    let mut rga = rk3588(&host, &mock, &rec);
    let mut blend = Client::attach(&host, "blend", 1);
    rga.tick(0);
    blend.fill(0, &[0, 0, 128, 128].repeat(4));
    blend.fill(1, &[200, 0, 0, 255].repeat(4));
    blend.post(&blend_job(7, 0, 1, 2));
    rga.tick(MS);
    assert_eq!(blend.done().bytes, 16);
    assert_eq!(blend.bytes(2, 16), [100, 0, 128, 255].repeat(4));
    assert!(matches!(rga.health(), Health::Degraded(_)));
    let slot = rga.server.routes().multi().unwrap();
    assert!(!slot.is_open());
    assert_eq!(slot.due_ns(), MS + 250 * MS);
}
