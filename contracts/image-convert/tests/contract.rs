use lemnos_contract_image_convert::{
    ConversionPair, ConvertInfo, ConvertJob, Counters, ImageConvert, ImageRef, JOB_BYTES,
    MAX_STREAM_NAME,
};
use lemnos_core::{
    Bound, Caller, Contract, Decode, Encode, LOCAL_ARGS, REPLY_INLINE, Reader, Reply, Result,
    Shape, Status, Writer,
};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_wire::ring::{Consumer, FrameSlot, Layout, Producer, STATE_FAULT, heap_words};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const INFO: lemnos_core::ContractInfo = <dyn ImageConvert as Contract>::INFO;
const ARGB: u32 = u32::from_le_bytes(*b"AR24");
const XRGB: u32 = u32::from_le_bytes(*b"XR24");
const PIXEL: usize = 4;

/// The conversions the mock and the simulated block both make.
const CONVERSIONS: [ConversionPair; 1] = [ConversionPair {
    from: ARGB,
    to: XRGB,
}];

/// A caller that reaches a mock block the way a host does: bytes in, bytes out.
struct Direct<T> {
    driver: T,
    args: [u8; LOCAL_ARGS],
}

impl<T: ImageConvert> Caller for Direct<T> {
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
    assert_eq!(INFO.tag.as_str(), "image.convert/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("info", 1),
            ("conversion_count", 2),
            ("conversion", 3),
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

fn image(buffer: u32, width: u32, height: u32, fourcc: u32) -> ImageRef {
    ImageRef {
        buffer,
        width,
        height,
        fourcc,
        stride: width * PIXEL as u32,
    }
}

#[test]
fn every_type_round_trips() {
    round_trip(ConvertJob {
        identity: u64::MAX,
        input: image(0, 1920, 1080, ARGB),
        output: image(1, 1920, 1080, XRGB),
    });
    round_trip(ConvertInfo {
        max_width: 8192,
        max_height: 8192,
    });
    round_trip(ConversionPair {
        from: ARGB,
        to: XRGB,
    });
    round_trip(Counters {
        jobs: 1,
        errors: u64::MAX,
    });
}

#[test]
fn a_job_is_a_fixed_small_record() {
    assert_eq!(max_len(&ConvertJob::SHAPE), Some(JOB_BYTES));
    assert_eq!(JOB_BYTES, 60);
}

#[derive(Default)]
struct Mock;

impl ImageConvert for Mock {
    fn info(&mut self) -> Result<ConvertInfo> {
        Ok(ConvertInfo {
            max_width: 1920,
            max_height: 1080,
        })
    }

    fn conversion_count(&mut self) -> Result<u16> {
        Ok(CONVERSIONS.len() as u16)
    }

    fn conversion(&mut self, index: u16) -> Result<ConversionPair> {
        CONVERSIONS
            .get(usize::from(index))
            .copied()
            .ok_or(Status::NOT_FOUND)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        into.get_mut(..7)
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(b"convert");
        Ok(7)
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters::default())
    }
}

#[test]
fn a_caller_finds_the_conversions_and_the_stream() {
    let mut block = Bound::new(Direct {
        driver: Mock,
        args: [0; LOCAL_ARGS],
    });
    assert_eq!(block.info().unwrap().max_height, 1080);
    assert_eq!(block.conversion_count(), Ok(1));
    assert_eq!(
        block.conversion(0),
        Ok(ConversionPair {
            from: ARGB,
            to: XRGB
        })
    );
    assert_eq!(block.conversion(1), Err(Status::NOT_FOUND));
    let mut name = [0; MAX_STREAM_NAME];
    let len = block.job_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"convert");
    assert_eq!(block.counters(), Ok(Counters::default()));
}

// Simulated jobs. The block's end converts on the buffers both sides can see, and the consumer's
// end posts jobs and reads completions over the ring the frame stream uses. The block makes the one
// conversion in CONVERSIONS: it drops the alpha byte of each pixel and sets the unused byte.

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
        let job = match ConvertJob::decode(&mut Reader::new(&bytes[..len])) {
            Ok(job) => job,
            Err(_) => {
                self.ring.close(STATE_FAULT);
                return false;
            }
        };
        let written = convert(pool, &job).unwrap_or_else(|| {
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

/// Converts the job. `None` when the job is refused.
fn convert(pool: &mut [Vec<u8>], job: &ConvertJob) -> Option<u32> {
    let (i, o) = (job.input, job.output);
    let pair = ConversionPair {
        from: i.fourcc,
        to: o.fourcc,
    };
    let sized = o.width > 0
        && o.height > 0
        && o.width <= 1920
        && o.height <= 1080
        && o.stride as usize >= o.width as usize * PIXEL
        && o.stride as usize * o.height as usize <= BUFFER_BYTES;
    if !CONVERSIONS.contains(&pair)
        || i.width != o.width
        || i.height != o.height
        || i.stride != o.stride
        || !sized
        || i.buffer == o.buffer
        || i.buffer >= BUFFERS
        || o.buffer >= BUFFERS
    {
        return None;
    }
    let [src, dst] = pool
        .get_disjoint_mut([i.buffer as usize, o.buffer as usize])
        .ok()?;
    for at in (0..o.stride as usize * o.height as usize).step_by(PIXEL) {
        dst[at..at + 3].copy_from_slice(&src[at..at + 3]);
        dst[at + 3] = 255;
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
    fn convert(&mut self, job: &ConvertJob) -> FrameSlot {
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

fn two_by_one(output_fourcc: u32) -> ConvertJob {
    ConvertJob {
        identity: 0xcafe,
        input: image(0, 2, 1, ARGB),
        output: image(1, 2, 1, output_fourcc),
    }
}

#[test]
fn a_simulated_conversion_keeps_colour_and_drops_alpha() {
    let mut rig = Rig::new();
    rig.pool[0][..8].copy_from_slice(&[1, 2, 3, 9, 4, 5, 6, 0]);
    let done = rig.convert(&two_by_one(XRGB));
    assert_eq!(done.identity, 0xcafe);
    assert_eq!(done.bytes, 8);
    assert_eq!(&rig.pool[1][..8], &[1, 2, 3, 255, 4, 5, 6, 255]);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
}

#[test]
fn a_conversion_the_block_lacks_completes_as_failed() {
    let mut rig = Rig::new();
    let done = rig.convert(&two_by_one(ARGB));
    assert_eq!(done.identity, 0xcafe);
    assert_eq!(done.bytes, 0);
    assert_eq!(rig.block.counters, Counters { jobs: 0, errors: 1 });
}

#[test]
fn a_size_change_completes_as_failed() {
    let mut rig = Rig::new();
    let job = ConvertJob {
        output: image(1, 1, 1, XRGB),
        ..two_by_one(XRGB)
    };
    assert_eq!(rig.convert(&job).bytes, 0);
}

#[test]
fn jobs_in_a_row_reuse_the_buffers_without_allocating() {
    let mut rig = Rig::new();
    let job = two_by_one(XRGB);
    // Warm up: the first job sets up whatever the thread needs, and its output is released.
    let done = rig.convert(&job);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    let ((), count) = allocations(|| {
        for identity in 1..1001 {
            let done = rig.convert(&ConvertJob { identity, ..job });
            assert_eq!(done.identity, identity);
            assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
        }
    });
    assert_eq!(count, 0, "allocations in 1000 jobs");
    assert_eq!(rig.block.counters.jobs, 1001);
}
