//! The driver's jobs over a bare job ring, the way the image contracts carry them: the consumer
//! posts a job, the block's end decodes it, runs it on the buffers both sides can see, and
//! publishes a completion with the job's identity and the bytes written (0 when refused).
//!
//! This exercises [`Jobs`] alone, on buffers the test hands it. The driver serving the real
//! host stream, on every route, is tested in `src/serve/tests.rs` and `tests/stream.rs`.

use lemnos_contract_image_scaler::{Filter, ImageRef, JOB_BYTES, ScaleJob};
use lemnos_core::{Decode, Encode, Reader, Writer};
use lemnos_driver_rga::jobs::{Jobs, Settings};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_wire::ring::{Consumer, FrameSlot, Layout, Producer, heap_words};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const BUFFERS: u32 = 4;
const BUFFER_BYTES: usize = 4096;
const RGBA: u32 = u32::from_le_bytes(*b"AR24");

fn image(buffer: u32, width: u32, height: u32) -> ImageRef {
    ImageRef {
        buffer,
        width,
        height,
        fourcc: RGBA,
        stride: width * 4,
    }
}

/// The block's end: the driver's jobs and the ring's producer.
struct Block {
    ring: Producer,
    jobs: Jobs,
    sequence: u64,
}

impl Block {
    fn run_one(&mut self, pool: &mut [Vec<u8>]) -> bool {
        while self.ring.pop_return().unwrap().is_some() {}
        let mut bytes = [0u8; JOB_BYTES];
        let Some(len) = self.ring.pop_job(&mut bytes).unwrap() else {
            return false;
        };
        let job = ScaleJob::decode(&mut Reader::new(&bytes[..len])).unwrap();
        let (i, o) = (job.input.buffer as usize, job.output.buffer as usize);
        // The same buffer twice never reaches the driver: it sees only the slices it is handed,
        // and two can't alias. The job still completes, as failed.
        let written = match pool.get_disjoint_mut([i, o]) {
            Ok([input, output]) => self.jobs.scale(&job, input, output),
            Err(_) => 0,
        };
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
                jobs: Jobs::new(Settings {
                    max_width: 64,
                    max_height: 64,
                    ..Settings::default()
                }),
                sequence: 0,
            },
            consumer: Consumer::attach(memory, layout).unwrap(),
            pool: vec![vec![0; BUFFER_BYTES]; BUFFERS as usize],
        }
    }

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
fn a_job_completes_with_its_identity_and_the_scaled_image() {
    let mut rig = Rig::new();
    // 4x2 of solid blocks: the left half 10s, the right half 30s.
    for y in 0..2 {
        for x in 0..4 {
            let v = if x < 2 { 10 } else { 30 };
            rig.pool[0][(y * 4 + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
        }
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
    assert_eq!(&rig.pool[1][..8], &[10, 10, 10, 255, 30, 30, 30, 255]);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    let c = rig.block.jobs.scale_counts();
    assert_eq!((c.jobs, c.errors), (1, 0));
}

#[test]
fn refused_jobs_complete_as_failed() {
    let mut rig = Rig::new();
    let good = ScaleJob {
        identity: 1,
        input: image(0, 4, 2),
        output: image(1, 2, 1),
        filter: Filter::Bilinear,
    };
    let bad = [
        // More rows than the buffer holds.
        ScaleJob {
            input: image(0, 32, 33),
            ..good
        },
        // Wider than the block's limit.
        ScaleJob {
            input: image(0, 65, 1),
            ..good
        },
        // The same buffer on both sides.
        ScaleJob {
            output: image(0, 2, 1),
            ..good
        },
    ];
    for (n, job) in bad.iter().enumerate() {
        let done = rig.scale(&ScaleJob {
            identity: 100 + n as u64,
            ..*job
        });
        assert_eq!(done.identity, 100 + n as u64);
        assert_eq!(done.bytes, 0, "{job:?}");
        assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    }
    assert_eq!(rig.scale(&good).bytes, 8);
    let c = rig.block.jobs.scale_counts();
    // The one the pool turned away never reached the driver's counters.
    assert_eq!((c.jobs, c.errors), (1, 2));
}

#[test]
fn jobs_in_a_row_allocate_nothing() {
    let mut rig = Rig::new();
    let job = ScaleJob {
        identity: 1,
        input: image(0, 32, 32),
        output: image(1, 20, 12),
        filter: Filter::Bilinear,
    };
    let done = rig.scale(&job);
    assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
    let ((), count) = allocations(|| {
        for identity in 2..1002 {
            let done = rig.scale(&ScaleJob { identity, ..job });
            assert_eq!(done.identity, identity);
            assert_eq!(done.bytes, 20 * 12 * 4);
            assert!(rig.consumer.release(done.buffer, done.fence).unwrap());
        }
    });
    assert_eq!(count, 0, "allocations in 1000 jobs");
    assert_eq!(rig.block.jobs.scale_counts().jobs, 1001);
}
