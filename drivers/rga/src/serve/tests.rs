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
use lemnos_wire::ring::{Consumer, FrameSlot, Layout as RingLayout, STATE_FAULT};

use lemnos_contract_image_blend::ImageBlend;
use lemnos_contract_image_convert::ImageConvert;
use lemnos_contract_image_scaler::ImageScaler;

use super::hardware::Opener;
use super::*;
use crate::caps::Soc;
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
        HeapAllocator.allocate(name, layout)
    }

    fn allocate_handles(
        &self,
        name: &str,
        layout: &RingLayout,
        _: &str,
    ) -> Result<Backing, Status> {
        let plain = HeapAllocator.allocate(name, layout)?;
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
    // The board file says what the buffers are, and the allocator is asked for that.
    let declared = if memory == Memory::Plain {
        ""
    } else {
        "memory = \"dma-buf\"\n"
    };
    let stream = |name| {
        format!(
            "[devices.streams.{name}]\n{declared}buffers = {BUFFERS}\nbuffer_bytes = {BYTES}\n\
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
    /// Called as each request starts, with the requests before it, so a test can act on the
    /// consumer while the unit runs.
    between: Option<Box<dyn FnMut(usize) + Send>>,
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
            if let Some(hook) = r.between.as_mut() {
                hook(r.count);
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
    soc: Soc,
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
        scale: Some(lemnos_core::Held::boxed(producer(host, "scale"))),
        convert: Some(lemnos_core::Held::boxed(producer(host, "convert"))),
        blend: Some(lemnos_core::Held::boxed(producer(host, "blend"))),
    };
    Rga::serving(needs, settings, soc, Routes::new(soc, m2m, multi))
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

/// What an output buffer holds before a job: a job that fails must leave it so.
const UNTOUCHED: u8 = 0x5a;

/// Fills `buffer` of `client` with [`UNTOUCHED`].
fn mark(client: &Client, buffer: u32, len: usize) {
    client.fill(buffer, &vec![UNTOUCHED; len]);
}

/// Whether `buffer`'s first `len` bytes are still [`UNTOUCHED`].
fn untouched(client: &Client, buffer: u32, len: usize) -> bool {
    client.bytes(buffer, len).iter().all(|&b| b == UNTOUCHED)
}

/// RK3588 with both units there, on `memory`: the driver, the mainline node, the `/dev/rga`
/// stand-in.
fn on_rk3588(memory: Memory) -> (Host, Rga, Mock, Recorder) {
    let host = host(memory);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let rga = rk3588(&host, &mock, &rec);
    (host, rga, mock, rec)
}

#[test]
fn a_job_no_hardware_route_does_fails_and_nothing_is_written() {
    let (host, mut rga, mock, rec) = on_rk3588(Memory::Handles);
    let mut scale = Client::attach(&host, "scale", 1);
    let mut conv = Client::attach(&host, "convert", 2);
    rga.tick(0);
    scale.fill(0, &pattern(16 * 16 * 4, 1));
    mark(&scale, 1, 8 * 8 * 4);
    // A bilinear downscale: neither unit's filter does it.
    scale.post(&ScaleJob {
        filter: Filter::Bilinear,
        ..scale_job(0xA, 0, 1, 16, 8)
    });
    rga.tick(MS);
    let done = scale.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 0xA, 0));
    assert_eq!(rga.last_failure(), Some(Failure::NoRoute));
    assert!(
        untouched(&scale, 1, 8 * 8 * 4),
        "nothing computed in software"
    );
    // RGB to AR24 at 2 by 1: below both units' smallest frame.
    conv.fill(0, &[10, 20, 30, 40, 50, 60]);
    mark(&conv, 1, 8);
    conv.post(&ConvertJob {
        identity: 0xB,
        input: convert_ref(0, 2, 1, RGB3, 3),
        output: convert_ref(1, 2, 1, AR24, 4),
    });
    rga.tick(2 * MS);
    let done = conv.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (1, 0xB, 0));
    assert_eq!(rga.last_failure(), Some(Failure::NoRoute));
    assert!(untouched(&conv, 1, 8));

    // No unit was asked, and none is at fault.
    assert_eq!(mock.with(|d| d.opens), 0);
    assert!(rec.take().is_empty());
    assert_eq!(rga.health(), Health::Ok);
    let c = ImageScaler::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (0, 1));
    let c = ImageConvert::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (0, 1));
    for stream in ["scale", "convert"] {
        let stats = host.stream_stats(0, stream).unwrap();
        assert_eq!((stats.faults, stats.dropped), (0, 0), "{stream}");
    }
}

#[test]
fn jobs_the_driver_cannot_run_complete_as_failed_and_are_counted() {
    let (host, mut rga, _mock, _rec) = on_rk3588(Memory::Plain);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    scale.post(&scale_job(1, 0, 1, 256, 128));
    rga.tick(1);
    let first = scale.done();
    assert_eq!(first.bytes, 128 * 128 * 4);

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
    let c = ImageScaler::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (1, 4));
}

#[test]
fn bytes_that_are_no_job_fault_that_consumers_stream_and_no_other() {
    let (host, mut rga, _mock, _rec) = on_rk3588(Memory::Plain);
    let mut scale = Client::attach(&host, "scale", 1);
    let mut conv = Client::attach(&host, "convert", 2);
    rga.tick(0);
    assert!(scale.ring.push_job(&[1, 2, 3]).unwrap());
    scale.post(&scale_job(1, 0, 1, 256, 128));
    // A job the converter completes, as failed: no route does it.
    conv.post(&ConvertJob {
        identity: 0xB,
        input: convert_ref(0, 2, 1, RGB3, 3),
        output: convert_ref(1, 2, 1, AR24, 4),
    });
    rga.tick(1);
    // The scaler's consumer is cut off, and the job it posted behind the bad bytes ran nowhere.
    assert_eq!(scale.ring.state(), STATE_FAULT);
    assert_eq!(scale.ring.pop_frame().unwrap(), None);
    assert_eq!(
        rga.health(),
        Health::Ok,
        "the consumer's mistake, not the driver's"
    );
    let c = ImageScaler::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (0, 1));
    let stats = host.stream_stats(0, "scale").unwrap();
    assert_eq!((stats.faults, stats.attached), (1, false));
    assert_eq!((stats.producer_held, stats.consumer_held), (0, 0));
    // The converter's consumer is served as before.
    assert_eq!(conv.done().identity, 0xB);
    let stats = host.stream_stats(0, "convert").unwrap();
    assert_eq!((stats.faults, stats.attached), (0, true));
    // The next consumer of the scaler's stream is served.
    let mut next = Client::attach(&host, "scale", 3);
    next.post(&scale_job(4, 0, 1, 256, 128));
    rga.tick(2);
    rga.tick(3);
    let done = next.done();
    assert_eq!((done.identity, done.bytes), (4, 128 * 128 * 4));
    assert_eq!(host.stream_stats(0, "scale").unwrap().faults, 1);
}

#[test]
fn an_image_past_its_buffer_is_refused_before_any_memory_is_taken_for_it() {
    let (host, mut rga, mock, _rec) = on_rk3588(Memory::Plain);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    // Inside the configured limits, 64 MiB, far past the stream's 256 KiB buffer.
    scale.post(&scale_job(1, 0, 1, 4096, 2048));
    let ((), count) = allocations(|| {
        rga.tick(1);
    });
    assert_eq!(rga.last_failure(), Some(Failure::Size));
    assert_eq!(scale.done().bytes, 0);
    assert_eq!(count, 0, "allocations for a job its buffer can't hold");
    assert_eq!(mock.with(|d| d.opens), 0);
}

#[test]
fn a_thousand_failed_jobs_through_the_stream_allocate_nothing() {
    let (host, mut rga, _mock, _rec) = on_rk3588(Memory::Plain);
    let mut scale = Client::attach(&host, "scale", 1);
    let mut conv = Client::attach(&host, "convert", 2);
    let mut blend = Client::attach(&host, "blend", 3);
    rga.tick(0);
    let mut cycle = |n: u64| {
        let out = 1 + (n % 3) as u32;
        // No route: a bilinear downscale.
        scale.post(&ScaleJob {
            filter: Filter::Bilinear,
            ..scale_job(n, 0, out, 32, 16)
        });
        // No route: below the units' smallest frame.
        conv.post(&ConvertJob {
            identity: n,
            input: convert_ref(0, 32, 32, AR24, 4),
            output: convert_ref(out, 32, 32, u32::from_le_bytes(*b"NV12"), 1),
        });
        // `/dev/rga` takes dma-bufs only, and this stream has none.
        blend.post(&blend_job(n, 0, if out == 1 { 2 } else { 1 }, out));
        rga.tick(n * MS);
        for client in [&mut scale, &mut conv, &mut blend] {
            let done = client.done();
            assert_eq!((done.identity, done.bytes), (n, 0));
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
    assert_eq!(count, 0, "allocations in 990 failed jobs");
    assert_eq!(ImageBlend::counters(&mut rga).unwrap().errors, 340);
}

#[test]
fn a_buffer_given_back_and_named_again_while_a_round_runs_completes_its_job() {
    let host = host(Memory::Handles);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let shared = Arc::new(Mutex::new(Client::attach(&host, "blend", 1)));
    rga.tick(0);
    // While the unit runs the second blend's copy, the consumer takes the first one's
    // completion, gives its buffer back and posts a third blend writing into that buffer.
    let hook = {
        let shared = Arc::clone(&shared);
        move |request: usize| {
            if request != 2 {
                return;
            }
            let mut blend = shared.lock().unwrap();
            let done = blend.done();
            assert_eq!((done.buffer, done.identity, done.bytes), (2, 7, 16));
            blend.release(done);
            blend.post(&blend_job(9, 0, 1, 2));
        }
    };
    rec.with(|r| r.between = Some(Box::new(hook)));
    {
        let mut blend = shared.lock().unwrap();
        blend.post(&blend_job(7, 0, 1, 2));
        blend.post(&blend_job(8, 0, 1, 3));
    }
    // One round reads all three: each blend is two requests on the unit.
    rga.tick(MS);
    assert_eq!(rec.with(|r| r.count), 6);
    assert_eq!(rga.last_failure(), None);
    let c = ImageBlend::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (3, 0));
    let mut blend = shared.lock().unwrap();
    let second = blend.done();
    assert_eq!((second.buffer, second.identity, second.bytes), (3, 8, 16));
    let third = blend.done();
    assert_eq!((third.buffer, third.identity, third.bytes), (2, 9, 16));
    assert_eq!(blend.ring.pop_frame().unwrap(), None);
}

#[test]
fn a_new_consumer_on_a_plain_stream_gets_its_completions() {
    let (host, mut rga, _mock, _rec) = on_rk3588(Memory::Plain);
    let first = Client::attach(&host, "scale", 1);
    rga.tick(0);
    drop(first);
    host.stream_detach(0, "scale", CallerId(1)).unwrap();
    // No consumer: everything held is given up, and nothing is held until one comes.
    rga.tick(1);
    rga.tick(2);
    let mut second = Client::attach(&host, "scale", 2);
    second.post(&scale_job(9, 0, 1, 256, 128));
    rga.tick(3);
    let done = second.done();
    assert_eq!(
        (done.buffer, done.identity, done.bytes),
        (1, 9, 128 * 128 * 4)
    );
}

#[test]
fn a_new_consumer_on_a_stream_with_handles_gets_its_completions_at_once() {
    let (host, mut rga, _mock, _rec) = on_rk3588(Memory::Handles);
    let _first = Client::attach(&host, "scale", 1);
    rga.tick(0);
    host.stream_detach(0, "scale", CallerId(1)).unwrap();
    // A new consumer before the driver looks again: the job's buffers are renewed by handle.
    let mut second = Client::attach(&host, "scale", 2);
    second.post(&scale_job(9, 2, 3, 256, 128));
    rga.tick(1);
    let done = second.done();
    assert_eq!(
        (done.buffer, done.identity, done.bytes),
        (3, 9, 128 * 128 * 4)
    );
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
        Soc::Rk3588,
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
fn a_failed_unit_fails_its_jobs_and_is_retried_capped_at_eight_seconds() {
    let host = host(Memory::Plain);
    let (mock, _) = m2m_mock();
    let rec = Recorder::default();
    let mut rga = rk3588(&host, &mock, &rec);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    scale.fill(0, &pattern(BYTES as usize, 4));
    let job = scale_job(1, 0, 1, 256, 128);
    mock.with(|d| d.fail = true);

    let mut now = 1_000 * MS;
    // Each job completes, as failed and with nothing written, or with the unit's result.
    let run = |scale: &mut Client, rga: &mut Rga, now: u64| {
        mark(scale, 1, 128 * 128 * 4);
        scale.post(&job);
        rga.tick(now);
        let done = scale.done();
        let written = done.bytes;
        let left = untouched(scale, 1, 128 * 128 * 4);
        scale.release(done);
        (written, left, rga.last_failure())
    };
    // It fails: the job fails with it, and the unit waits.
    assert_eq!(
        run(&mut scale, &mut rga, now),
        (0, true, Some(Failure::UnitFailed))
    );
    assert_eq!(rga.health(), Health::Degraded(crate::UNIT_FAILED));
    assert!(!rga.server.routes().m2m().unwrap().is_open());
    let mut waits = Vec::new();
    for _ in 0..8 {
        let due = rga_due(&rga);
        waits.push((due - now) / MS);
        // Before the wait is over the node isn't opened again, and the job runs nowhere else.
        let opens = mock.with(|d| d.opens);
        assert_eq!(
            run(&mut scale, &mut rga, due - 1),
            (0, true, Some(Failure::UnitDown))
        );
        assert_eq!(mock.with(|d| d.opens), opens);
        now = due;
        assert_eq!(
            run(&mut scale, &mut rga, now),
            (0, true, Some(Failure::UnitFailed))
        );
        assert_eq!(mock.with(|d| d.opens), opens + 1, "opened again once");
        assert!(matches!(rga.health(), Health::Degraded(_)));
    }
    assert_eq!(waits, [250, 500, 1000, 2000, 4000, 8000, 8000, 8000]);
    // It answers again: the job is the unit's, and the driver is healthy.
    mock.with(|d| d.fail = false);
    let due = rga_due(&rga);
    let (written, left, _) = run(&mut scale, &mut rga, due);
    assert_eq!((written, left), (128 * 128 * 4, false));
    assert_eq!(rga.health(), Health::Ok);
    assert!(rga.server.routes().m2m().unwrap().is_open());
    let c = ImageScaler::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (1, 17));
}

#[test]
fn a_unit_whose_node_is_not_there_fails_its_jobs_and_is_looked_for_again() {
    let host = host(Memory::Plain);
    let rec = Recorder::default();
    let missing: Opener<Box<dyn M2mRoute>> =
        Box::new(|| Err(io::Error::from(io::ErrorKind::NotFound)));
    let multi: Opener<Box<dyn Submit>> = {
        let rec = rec.clone();
        Box::new(move || Ok(Box::new(rec.clone())))
    };
    let needs = RgaNeeds {
        scale: Some(lemnos_core::Held::boxed(producer(&host, "scale"))),
        convert: None,
        blend: None,
    };
    let soc = Soc::Rk3588;
    let routes = Routes::new(soc, Some(missing), Some(multi));
    let mut rga = Rga::serving(needs, Settings::default(), soc, routes);
    let mut scale = Client::attach(&host, "scale", 1);
    rga.tick(0);
    mark(&scale, 1, 128 * 128 * 4);
    scale.post(&scale_job(1, 0, 1, 256, 128));
    rga.tick(MS);
    assert_eq!(scale.done().bytes, 0);
    assert_eq!(rga.last_failure(), Some(Failure::UnitDown));
    assert!(untouched(&scale, 1, 128 * 128 * 4));
    assert_eq!(rga.health(), Health::Degraded(crate::UNIT_MISSING));
    assert_eq!(rga_due(&rga), MS + 250 * MS);
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
fn dev_rga_takes_dma_bufs_only_so_a_blend_on_a_plain_stream_fails() {
    let (host, mut rga, mock, rec) = on_rk3588(Memory::Plain);
    let mut blend = Client::attach(&host, "blend", 1);
    rga.tick(0);
    blend.fill(0, &[0, 0, 128, 128].repeat(4));
    blend.fill(1, &[200, 0, 0, 255].repeat(4));
    mark(&blend, 2, 16);
    blend.post(&blend_job(7, 0, 1, 2));
    rga.tick(1);
    let done = blend.done();
    assert_eq!((done.buffer, done.identity, done.bytes), (2, 7, 0));
    assert_eq!(rga.last_failure(), Some(Failure::NoHandles));
    assert!(untouched(&blend, 2, 16), "nothing blended in software");
    assert!(rec.take().is_empty());
    assert_eq!(mock.with(|d| d.opens), 0);
    // The board file has to change, and health says so.
    assert_eq!(rga.health(), Health::Degraded(crate::NO_HANDLES));
    let c = ImageBlend::counters(&mut rga).unwrap();
    assert_eq!((c.jobs, c.errors), (0, 1));
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
fn packed_gray_input_goes_to_dev_rga_and_padded_gray_fails() {
    for (stride, to_unit) in [(4, true), (8, false)] {
        let (host, mut rga, _mock, rec) = on_rk3588(Memory::Handles);
        let mut conv = Client::attach(&host, "convert", 1);
        rga.tick(0);
        conv.fill(0, &[16, 235, 128, 0, 9, 9, 9, 9]);
        mark(&conv, 1, 16);
        conv.post(&ConvertJob {
            identity: 4,
            input: convert::ImageRef {
                stride,
                ..convert_ref(0, 4, 1, GREY, 1)
            },
            output: convert_ref(1, 4, 1, AR24, 4),
        });
        rga.tick(1);
        let seen = rec.take();
        if to_unit {
            assert_eq!(conv.done().bytes, 16);
            assert_eq!(seen.len(), 1);
            let gray = RgaFormat::Gray8.code() as u32;
            let rgba = RgaFormat::Rgba8888.code() as u32;
            assert_eq!(seen[0].src, (conv.fd(0), gray, 4, 1));
            assert_eq!(seen[0].dst, (conv.fd(1), rgba, 4, 1));
            assert_eq!(seen[0].yuv2rgb_mode, 1);
        } else {
            // The request can't carry the padded row: the route refuses it, and the unit is
            // not at fault.
            assert_eq!(conv.done().bytes, 0);
            assert_eq!(rga.last_failure(), Some(Failure::RouteRefused));
            assert!(seen.is_empty());
            assert!(untouched(&conv, 1, 16));
            assert_eq!(rga.health(), Health::Ok);
        }
    }
}

#[test]
fn a_failing_dev_rga_fails_the_blend_and_runs_it_nowhere_else() {
    let (host, mut rga, mock, rec) = on_rk3588(Memory::Handles);
    rec.with(|r| r.fail = true);
    let mut blend = Client::attach(&host, "blend", 1);
    rga.tick(0);
    blend.fill(0, &[0, 0, 128, 128].repeat(4));
    blend.fill(1, &[200, 0, 0, 255].repeat(4));
    mark(&blend, 2, 16);
    blend.post(&blend_job(7, 0, 1, 2));
    rga.tick(MS);
    let done = blend.done();
    assert_eq!(done.bytes, 0);
    assert_eq!(rga.last_failure(), Some(Failure::UnitFailed));
    assert!(untouched(&blend, 2, 16));
    assert_eq!(rga.health(), Health::Degraded(crate::UNIT_FAILED));
    let slot = rga.server.routes().multi().unwrap();
    assert!(!slot.is_open());
    assert_eq!(slot.due_ns(), MS + 250 * MS);
    // The mainline unit is not asked in its place.
    assert_eq!(mock.with(|d| d.opens), 0);
    blend.release(done);

    // While it waits, a blend fails at once without a request.
    rec.with(|r| r.fail = false);
    blend.post(&blend_job(8, 0, 1, 2));
    rga.tick(2 * MS);
    let done = blend.done();
    assert_eq!(done.bytes, 0);
    assert_eq!(rga.last_failure(), Some(Failure::UnitDown));
    assert_eq!(rec.with(|r| r.count), 0);
    blend.release(done);

    // Opened again after the wait, it serves, and health clears.
    blend.post(&blend_job(9, 0, 1, 2));
    rga.tick(MS + 250 * MS);
    assert_eq!(blend.done().bytes, 16);
    assert_eq!(rec.take().len(), 2);
    assert_eq!(rga.health(), Health::Ok);
}
