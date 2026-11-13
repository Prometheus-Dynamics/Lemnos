use lemnos_contract_image_scaler::{
    Counters, Filter, ImageRef, ImageScaler, JOB_BYTES, MAX_STREAM_NAME, ScaleJob, ScalerInfo,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_wire::ring::{Consumer, FrameSlot, Layout, Producer, heap_words};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const INFO: lemnos_core::ContractInfo = <dyn ImageScaler as Contract>::INFO;
const RGBA: u32 = u32::from_le_bytes(*b"AR24");
const PIXEL: usize = 4;

/// A caller that reaches a mock block the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: ImageScaler> Caller for Direct<T> {
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
    assert_eq!(INFO.tag.as_str(), "image.scaler/1");
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

fn job() -> ScaleJob {
    ScaleJob {
        identity: u64::MAX,
        input: image(0, 1920, 1080),
        output: image(3, 640, 360),
        filter: Filter::Bilinear,
    }
}

#[test]
fn every_type_round_trips() {
    round_trip(job());
    round_trip(ScalerInfo {
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
    assert_eq!(max_len(&ScaleJob::SHAPE), Some(JOB_BYTES));
    assert_eq!(JOB_BYTES, 61);
}

#[test]
fn an_unknown_filter_is_refused() {
    // A default job, with its filter, the last byte, set to an undefined code.
    let mut bytes = [0u8; JOB_BYTES];
    ScaleJob::default()
        .encode(&mut Writer::new(&mut bytes))
        .unwrap();
    bytes[JOB_BYTES - 1] = 200;
    assert_eq!(
        ScaleJob::decode(&mut Reader::new(&bytes)).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[derive(Default)]
struct Mock;

impl ImageScaler for Mock {
    fn info(&mut self) -> Result<ScalerInfo> {
        Ok(ScalerInfo {
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
            .copy_from_slice(b"scale");
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
    assert_eq!(&name[..len], b"scale");
    assert_eq!(block.counters(), Ok(Counters::default()));
}

// Simulated jobs. The block's end does the work on the buffers both sides can see, and the
// consumer's end posts jobs and reads completions over the same ring the frame stream uses. The
// block's arithmetic is nearest sampling, enough to check bytes move; it is not a filter.

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
        let job = match ScaleJob::decode(&mut Reader::new(&bytes[..len])) {
            Ok(job) => job,
            Err(_) => {
                self.ring.close(lemnos_wire::ring::STATE_FAULT);
                return false;
            }
        };
        let written = scale(pool, &job).unwrap_or_else(|| {
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

/// Nearest sampling from the input to the output. `None` when the job is refused.
fn scale(pool: &mut [Vec<u8>], job: &ScaleJob) -> Option<u32> {
    let (i, o) = (job.input, job.output);
    let sized = |img: ImageRef| {
        img.width > 0
            && img.height > 0
            && img.width <= 1920
            && img.height <= 1080
            && img.stride as usize >= img.width as usize * PIXEL
            && img.stride as usize * img.height as usize <= BUFFER_BYTES
    };
    if job.filter != Filter::Area
        || i.fourcc != RGBA
        || o.fourcc != RGBA
        || i.buffer == o.buffer
        || !sized(i)
        || !sized(o)
        || i.buffer >= BUFFERS
        || o.buffer >= BUFFERS
    {
        return None;
    }
    let [src, dst] = pool
        .get_disjoint_mut([i.buffer as usize, o.buffer as usize])
        .ok()?;
    for y in 0..o.height {
        let sy = y * i.height / o.height;
        for x in 0..o.width {
            let sx = x * i.width / o.width;
            let from = sy as usize * i.stride as usize + sx as usize * PIXEL;
            let to = y as usize * o.stride as usize + x as usize * PIXEL;
            dst[to..to + PIXEL].copy_from_slice(&src[from..from + PIXEL]);
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
    fn scale(&mut self, job: &ScaleJob) -> FrameSlot {
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
}

#[test]
fn a_simulated_job_completes_with_its_identity_and_output() {
    let mut rig = Rig::new();
    // A 4x2 input, one row of pixels 0..4 then 4..8, so each output pixel is recognisable.
    for n in 0..8 {
        rig.pool[0][n * PIXEL..n * PIXEL + PIXEL].copy_from_slice(&[n as u8, 0, 0, 255]);
    }
    let job = ScaleJob {
        identity: 0xfeed,
        input: image(0, 4, 2),
        output: image(1, 2, 1),
        filter: Filter::Area,
    };
    let done = rig.scale(&job);
    assert_eq!(done.identity, 0xfeed);
    assert_eq!(done.buffer, 1);
    assert_eq!(done.bytes, 8);
    // Output pixel 1 samples input column 2 of row 0: pixel 2.
    assert_eq!(&rig.pool[1][..8], &[0, 0, 0, 255, 2, 0, 0, 255]);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    assert_eq!(rig.block.counters, Counters { jobs: 1, errors: 0 });
}

#[test]
fn a_refused_job_still_completes_as_failed() {
    let mut rig = Rig::new();
    let job = ScaleJob {
        identity: 5,
        input: image(0, 4, 2),
        output: image(1, 0, 1),
        filter: Filter::Area,
    };
    let done = rig.scale(&job);
    assert_eq!(done.identity, 5);
    assert_eq!(done.bytes, 0);
    assert_eq!(rig.block.counters, Counters { jobs: 0, errors: 1 });
}

#[test]
fn a_filter_the_block_lacks_completes_as_failed() {
    let mut rig = Rig::new();
    let job = ScaleJob {
        identity: 6,
        input: image(0, 4, 2),
        output: image(1, 2, 1),
        filter: Filter::Bilinear,
    };
    assert_eq!(rig.scale(&job).bytes, 0);
}

#[test]
fn jobs_in_a_row_reuse_the_buffers_without_allocating() {
    let mut rig = Rig::new();
    let job = ScaleJob {
        identity: 1,
        input: image(0, 8, 4),
        output: image(1, 4, 2),
        filter: Filter::Area,
    };
    // Warm up: the first job sets up whatever the thread needs, and its output is released.
    let done = rig.scale(&job);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    let ((), count) = allocations(|| {
        for identity in 2..1002 {
            let done = rig.scale(&ScaleJob { identity, ..job });
            assert_eq!(done.identity, identity);
            assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
        }
    });
    assert_eq!(count, 0, "allocations in 1000 jobs");
    assert_eq!(rig.block.counters.jobs, 1001);
}
