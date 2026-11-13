use lemnos_contract_image_blend::{
    BlendInfo, BlendJob, BlendOp, Counters, ImageBlend, ImageRef, JOB_BYTES, MAX_STREAM_NAME,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_wire::ring::{Consumer, FrameSlot, Layout, Producer, STATE_FAULT, heap_words};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const INFO: lemnos_core::ContractInfo = <dyn ImageBlend as Contract>::INFO;
const RGBA: u32 = u32::from_le_bytes(*b"AR24");
const PIXEL: usize = 4;

/// A caller that reaches a mock block the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: ImageBlend> Caller for Direct<T> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let mut writer = Reply::new(reply);
        let status = self
            .driver
            .__serve(method, &self.args[..args_len], &mut writer);
        if status.is_ok() {
            Ok(writer.len())
        } else {
            Err(status)
        }
    }
}

/// The most bytes a shape can take on the wire, or `None` for a variable-length one.
fn max_len(shape: &Shape) -> Option<usize> {
    Some(match shape {
        Shape::Unit => 0,
        Shape::Bool | Shape::U8 | Shape::I8 => 1,
        Shape::U16 | Shape::I16 => 2,
        Shape::U32 | Shape::I32 | Shape::F32 => 4,
        Shape::U64 | Shape::I64 | Shape::F64 => 8,
        Shape::Str | Shape::Bytes => return None,
        Shape::Option(inner) => 1 + max_len(inner)?,
        Shape::Array(inner, count) => max_len(inner)? * *count as usize,
        Shape::Record(fields) => {
            let mut total = 4;
            for field in *fields {
                total += max_len(&field.shape)?;
            }
            total
        }
    })
}

#[test]
fn every_call_fits_a_local_caller_and_an_inline_reply() {
    for method in INFO.methods {
        let mut args = if method.has_out_buffer { 4 } else { 0 };
        for param in method.params {
            args += max_len(&param.shape).expect("a fixed-size parameter");
        }
        assert!(args <= LOCAL_ARGS, "{} takes {args} bytes", method.name);
        if !method.has_out_buffer {
            let reply = max_len(&method.returns).expect("a fixed-size reply");
            assert!(
                reply <= REPLY_INLINE,
                "{} returns {reply} bytes",
                method.name
            );
        }
    }
}

#[test]
fn the_tag_and_numbers_are_stable() {
    assert_eq!(INFO.tag.as_str(), "image.blend/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("info", 1),
            ("format_count", 2),
            ("format", 3),
            ("job_stream", 4),
            ("counters", 5),
        ]
    );
    assert!(INFO.methods[3].has_out_buffer);
}

fn round_trip<T: Encode + for<'a> Decode<'a> + PartialEq + core::fmt::Debug>(value: T) {
    let mut buffer = [0; REPLY_INLINE];
    let mut writer = Writer::new(&mut buffer);
    value.encode(&mut writer).unwrap();
    let used = writer.len();
    assert_eq!(used, max_len(&T::SHAPE).unwrap());
    let mut reader = Reader::new(&buffer[..used]);
    assert_eq!(T::decode(&mut reader), Ok(value));
    assert_eq!(reader.remaining(), 0);
    // Every shorter prefix is refused.
    for short in 0..used {
        assert_eq!(
            T::decode(&mut Reader::new(&buffer[..short])).err(),
            Some(Status::INVALID_ARGUMENT)
        );
    }
}

fn image(buffer: u32, width: u32, height: u32) -> ImageRef {
    ImageRef {
        buffer,
        width,
        height,
        fourcc: RGBA,
        stride: width * PIXEL as u32,
    }
}

fn job() -> BlendJob {
    BlendJob {
        identity: u64::MAX,
        op: BlendOp::SrcOver,
        foreground: image(0, 640, 360),
        background: image(1, 640, 360),
        output: image(2, 640, 360),
    }
}

#[test]
fn every_type_round_trips() {
    round_trip(job());
    round_trip(BlendInfo {
        max_width: 8192,
        max_height: 8192,
    });
    round_trip(Counters {
        jobs: 1,
        errors: u64::MAX,
    });
    round_trip(image(9, 4, 2));
}

#[test]
fn a_job_is_a_fixed_small_record() {
    assert_eq!(max_len(&BlendJob::SHAPE), Some(JOB_BYTES));
    assert_eq!(JOB_BYTES, 85);
}

#[test]
fn an_unknown_operation_is_refused() {
    // A block of the job's body (its length after the 4-byte prefix), then the identity (8)
    // and the operation.
    let mut bytes = [0u8; JOB_BYTES];
    bytes[..4].copy_from_slice(&((JOB_BYTES - 4) as u32).to_le_bytes());
    bytes[12] = 200;
    assert_eq!(
        BlendJob::decode(&mut Reader::new(&bytes)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[derive(Default)]
struct Mock;

impl ImageBlend for Mock {
    fn info(&mut self) -> Result<BlendInfo> {
        Ok(BlendInfo {
            max_width: 1920,
            max_height: 1080,
        })
    }

    fn format_count(&mut self) -> Result<u16> {
        Ok(1)
    }

    fn format(&mut self, index: u16) -> Result<u32> {
        if index == 0 {
            Ok(RGBA)
        } else {
            Err(Status::NOT_FOUND)
        }
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        into.get_mut(..5)
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(b"blend");
        Ok(5)
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters::default())
    }
}

#[test]
fn a_caller_finds_the_formats_and_the_stream() {
    let mut block = Bound::new(Direct {
        driver: Mock,
        args: [0; LOCAL_ARGS],
    });
    assert_eq!(block.info().unwrap().max_width, 1920);
    assert_eq!(block.format_count(), Ok(1));
    assert_eq!(block.format(0), Ok(RGBA));
    assert_eq!(block.format(1), Err(Status::NOT_FOUND));
    let mut name = [0; MAX_STREAM_NAME];
    let len = block.job_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"blend");
    assert_eq!(block.counters(), Ok(Counters::default()));
}

// Simulated jobs. The block's end composites on the buffers both sides can see, and the consumer's
// end posts jobs and reads completions over the ring the frame stream uses. The block's arithmetic
// is the premultiplied Porter-Duff formula on RGBA8, written out for the test.

const BUFFERS: u32 = 4;
const BUFFER_BYTES: usize = 256;

/// The block's end: reads a job, writes its output, publishes the completion.
struct Block {
    ring: Producer,
    sequence: u64,
    counters: Counters,
}

impl Block {
    /// Takes at most one job. Returns `false` when none is waiting.
    fn run_one(&mut self, pool: &mut [Vec<u8>]) -> bool {
        // Collect the buffers the consumer has released, so its return queue never fills.
        while self.ring.pop_return().unwrap().is_some() {}
        let mut bytes = [0u8; JOB_BYTES];
        let Some(len) = self.ring.pop_job(&mut bytes).unwrap() else {
            return false;
        };
        let job = match BlendJob::decode(&mut Reader::new(&bytes[..len])) {
            Ok(job) => job,
            Err(_) => {
                self.ring.close(STATE_FAULT);
                return false;
            }
        };
        let written = blend(pool, &job).unwrap_or_else(|| {
            self.counters.errors += 1;
            0
        });
        if written != 0 {
            self.counters.jobs += 1;
        }
        let sent = self
            .ring
            .push_frame(FrameSlot {
                buffer: job.output.buffer,
                bytes: written,
                sequence: self.sequence,
                identity: job.identity,
                timestamp_ns: 0,
                fence: 0,
            })
            .unwrap();
        assert!(sent, "the completion ring is full");
        self.sequence += 1;
        true
    }
}

/// One channel of `s` over `d`, where `a` is the alpha that covers `d`.
fn over(s: u8, d: u8, a: u8) -> u8 {
    (u32::from(s) + u32::from(d) * (255 - u32::from(a)) / 255).min(255) as u8
}

/// Composites the job. `None` when the job is refused.
fn blend(pool: &mut [Vec<u8>], job: &BlendJob) -> Option<u32> {
    let (f, b, o) = (job.foreground, job.background, job.output);
    let same = [f, b, o]
        .iter()
        .all(|img| img.width == o.width && img.height == o.height && img.fourcc == RGBA);
    let sized = o.width > 0
        && o.height > 0
        && o.width <= 1920
        && o.height <= 1080
        && o.stride as usize >= o.width as usize * PIXEL
        && o.stride as usize * o.height as usize <= BUFFER_BYTES;
    let [fb, bb, ob] = [f.buffer, b.buffer, o.buffer];
    if !same
        || !sized
        || fb == bb
        || fb == ob
        || bb == ob
        || [fb, bb, ob].iter().any(|&buffer| buffer >= BUFFERS)
        || [f, b].iter().any(|img| img.stride != o.stride)
    {
        return None;
    }
    let [fg, bg, out] = pool
        .get_disjoint_mut([fb as usize, bb as usize, ob as usize])
        .ok()?;
    for y in 0..o.height as usize {
        for x in 0..o.width as usize {
            let at = y * o.stride as usize + x * PIXEL;
            let s: [u8; 4] = fg[at..at + 4].try_into().unwrap();
            let d: [u8; 4] = bg[at..at + 4].try_into().unwrap();
            let px: [u8; 4] = match job.op {
                BlendOp::Src => s,
                BlendOp::SrcOver => std::array::from_fn(|c| over(s[c], d[c], s[3])),
                BlendOp::DstOver => std::array::from_fn(|c| over(d[c], s[c], d[3])),
            };
            out[at..at + 4].copy_from_slice(&px);
        }
    }
    Some(o.stride * o.height)
}

struct Rig {
    block: Block,
    consumer: Consumer,
    pool: Vec<Vec<u8>>,
}

impl Rig {
    fn new() -> Self {
        let layout = Layout::new(BUFFERS, BUFFER_BYTES as u32, 4, JOB_BYTES as u32).unwrap();
        let memory = heap_words(layout.ring_words());
        Self {
            block: Block {
                ring: Producer::new(memory.clone(), layout).unwrap(),
                sequence: 0,
                counters: Counters::default(),
            },
            consumer: Consumer::attach(memory, layout).unwrap(),
            pool: vec![vec![0; BUFFER_BYTES]; BUFFERS as usize],
        }
    }

    /// Posts one job, lets the block run it, and takes its completion.
    fn blend(&mut self, job: &BlendJob) -> FrameSlot {
        let mut bytes = [0u8; JOB_BYTES];
        let used = {
            let mut writer = Writer::new(&mut bytes);
            job.encode(&mut writer).unwrap();
            writer.len()
        };
        assert!(self.consumer.push_job(&bytes[..used]).unwrap());
        assert!(self.block.run_one(&mut self.pool));
        self.consumer.pop_frame().unwrap().expect("a completion")
    }

    /// Writes pixels into the start of one buffer, the consumer's side of an image.
    fn put(&mut self, buffer: usize, pixels: &[[u8; 4]]) {
        for (at, px) in pixels.iter().enumerate() {
            self.pool[buffer][at * PIXEL..at * PIXEL + PIXEL].copy_from_slice(px);
        }
    }

    fn pixels(&self, buffer: usize, count: usize) -> Vec<[u8; 4]> {
        (0..count)
            .map(|at| {
                self.pool[buffer][at * PIXEL..at * PIXEL + PIXEL]
                    .try_into()
                    .unwrap()
            })
            .collect()
    }
}

fn two_by_one(op: BlendOp) -> BlendJob {
    BlendJob {
        identity: 0xbeef,
        op,
        foreground: image(0, 2, 1),
        background: image(1, 2, 1),
        output: image(2, 2, 1),
    }
}

#[test]
fn a_simulated_source_over_weighs_the_foreground_by_its_alpha() {
    let mut rig = Rig::new();
    // Half-transparent red over opaque grey, and a fully transparent pixel over the same grey.
    rig.put(0, &[[100, 0, 0, 128], [0, 0, 0, 0]]);
    rig.put(1, &[[200, 200, 200, 255], [200, 200, 200, 255]]);
    let done = rig.blend(&two_by_one(BlendOp::SrcOver));
    assert_eq!(done.identity, 0xbeef);
    assert_eq!(done.bytes, 8);
    assert_eq!(rig.pixels(2, 2), [[199, 99, 99, 255], [200, 200, 200, 255]]);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    assert_eq!(rig.block.counters, Counters { jobs: 1, errors: 0 });
}

#[test]
fn a_simulated_source_replaces_the_background() {
    let mut rig = Rig::new();
    rig.put(0, &[[1, 2, 3, 4], [5, 6, 7, 8]]);
    rig.put(1, &[[200, 200, 200, 255], [200, 200, 200, 255]]);
    let done = rig.blend(&two_by_one(BlendOp::Src));
    assert_eq!(done.bytes, 8);
    assert_eq!(rig.pixels(2, 2), [[1, 2, 3, 4], [5, 6, 7, 8]]);
}

#[test]
fn a_simulated_dst_over_keeps_an_opaque_background() {
    let mut rig = Rig::new();
    rig.put(0, &[[100, 0, 0, 128], [9, 9, 9, 255]]);
    rig.put(1, &[[200, 200, 200, 255], [30, 40, 50, 255]]);
    let done = rig.blend(&two_by_one(BlendOp::DstOver));
    assert_eq!(done.bytes, 8);
    assert_eq!(rig.pixels(2, 2), [[200, 200, 200, 255], [30, 40, 50, 255]]);
}

#[test]
fn a_refused_job_still_completes_as_failed() {
    let mut rig = Rig::new();
    // The background is one pixel wider, so the three images are not the same size.
    let job = BlendJob {
        background: image(1, 3, 1),
        ..two_by_one(BlendOp::SrcOver)
    };
    let done = rig.blend(&job);
    assert_eq!(done.identity, 0xbeef);
    assert_eq!(done.bytes, 0);
    assert_eq!(rig.block.counters, Counters { jobs: 0, errors: 1 });
}

#[test]
fn a_job_reusing_a_buffer_completes_as_failed() {
    let mut rig = Rig::new();
    let job = BlendJob {
        output: image(0, 2, 1),
        ..two_by_one(BlendOp::SrcOver)
    };
    assert_eq!(rig.blend(&job).bytes, 0);
}

#[test]
fn jobs_in_a_row_reuse_the_buffers_without_allocating() {
    let mut rig = Rig::new();
    let job = two_by_one(BlendOp::SrcOver);
    // Warm up: the first job sets up whatever the thread needs, and its output is released.
    let done = rig.blend(&job);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    let ((), count) = allocations(|| {
        for identity in 1..1001 {
            let done = rig.blend(&BlendJob { identity, ..job });
            assert_eq!(done.identity, identity);
            assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
        }
    });
    assert_eq!(count, 0, "allocations in 1000 jobs");
    assert_eq!(rig.block.counters.jobs, 1001);
}
