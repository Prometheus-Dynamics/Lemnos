//! Random interleavings of one publisher and one reader that holds frames while the publisher
//! keeps going. Every frame the reader still holds must carry, in its bytes, the sequence its
//! metadata names, and must never be torn. The grant is two frames: a third is never handed out.

use shm_ring::{FieldId, Ring, SampleTime, Spec};
use shm_ring_buffers::{BufferReader, BufferWriter, Held, NextBatch, Pool};
use shm_ring_holds::{HoldTable, Limits, Marks};

const FRAME: usize = 4096;
const MS: u64 = 1_000_000;
const GRANT: usize = 2;

type Reader = BufferReader<shm_ring::ReadMap, Marks>;

/// A small xorshift, so every run is the same run for its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

struct Topic {
    writer: BufferWriter,
    table: HoldTable,
    ring: Ring,
    buffers: u32,
}

impl Topic {
    fn new(buffers: u32) -> Self {
        let ring = Ring::create(&Spec {
            fields: 1,
            history: 16,
            max_payload: 64,
            expiry_ms: 0,
        })
        .expect("ring");
        let pool = Pool::in_memory(1, buffers, FRAME).expect("pool");
        let writer = BufferWriter::new(ring.writer().expect("writer"), FieldId(0), pool);
        Self {
            writer,
            table: HoldTable::new(buffers),
            ring,
            buffers,
        }
    }

    fn reader(&mut self, limits: Limits) -> Reader {
        let slot = self.table.free_slot().expect("a free slot");
        let marks = Marks::create(self.buffers, slot, limits.max_held).expect("marks");
        self.table
            .add(slot, marks.page_fd().expect("page"), limits)
            .expect("add reader");
        let view = self.writer.pool().view().expect("view");
        BufferReader::new(self.ring.reader().expect("reader"), FieldId(0), view, marks)
    }

    /// Publishes frame `seq`: every byte is `seq`, its metadata is `seq`. False if it was dropped.
    fn send(&mut self, seq: u64) -> bool {
        self.table.enforce(seq * MS, self.writer.pool().page());
        let Ok(buffer) = self.writer.take(&self.table) else {
            return false;
        };
        self.writer
            .buffer_mut(buffer)
            .expect("a free buffer")
            .fill(seq as u8);
        self.writer
            .publish(
                &[buffer],
                SampleTime::from_nanos(seq * MS),
                &seq.to_le_bytes(),
            )
            .expect("publish");
        true
    }
}

/// Whether the held frame's bytes are all `seq`, read in place and not torn.
fn intact(held: &Held<Marks>, seq: u64) -> bool {
    matches!(
        held.read(0, |bytes| {
            bytes.len() == FRAME && bytes.iter().all(|byte| *byte == seq as u8)
        }),
        Some(Ok(true))
    )
}

fn run(seed: u64, buffers: u32) {
    let mut topic = Topic::new(buffers);
    let mut reader = topic.reader(Limits {
        max_held: GRANT as u32,
        max_hold: None,
    });
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut held: Vec<(u64, Held<Marks>)> = Vec::new();
    let mut attempts = 0_u64;
    for step in 0..600 {
        match rng.next() % 4 {
            0 | 1 => {
                for _ in 0..=(rng.next() % 3) {
                    attempts += 1;
                    topic.send(attempts);
                }
            }
            2 => match reader.next() {
                NextBatch::Frame(batch) => {
                    let seq = u64::from_le_bytes(batch.meta[..8].try_into().expect("8 bytes"));
                    assert!(
                        held.len() < GRANT,
                        "seed {seed} step {step}: a frame was granted past the grant of {GRANT}"
                    );
                    held.push((seq, batch.held));
                }
                NextBatch::Limited => assert!(
                    held.len() >= GRANT,
                    "seed {seed} step {step}: refused below the grant"
                ),
                _ => {}
            },
            _ => {
                if !held.is_empty() {
                    let at = (rng.next() % held.len() as u64) as usize;
                    held.swap_remove(at);
                }
            }
        }
        for (seq, frame) in &held {
            assert!(
                intact(frame, *seq),
                "seed {seed} step {step}: held frame with sequence {seq} is not its own bytes"
            );
        }
    }
}

#[test]
fn held_frames_keep_their_bytes_and_their_sequence_under_random_interleavings() {
    for seed in 1..=300 {
        run(seed, 4);
    }
}

/// The same check with the reader on its own thread, reading while the publisher keeps going.
fn run_threaded(seed: u64, frames: u64) {
    let mut topic = Topic::new(4);
    let mut reader = topic.reader(Limits {
        max_held: GRANT as u32,
        max_hold: None,
    });
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let consumer = std::thread::spawn(move || {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut held: Vec<(u64, Held<Marks>)> = Vec::new();
        let mut frames_held = 0_u64;
        while !flag.load(std::sync::atomic::Ordering::Acquire) || reader.pending() > 0 {
            match reader.next() {
                NextBatch::Frame(batch) => {
                    let seq = u64::from_le_bytes(batch.meta[..8].try_into().expect("8 bytes"));
                    assert!(held.len() < GRANT, "seed {seed}: granted past {GRANT}");
                    held.push((seq, batch.held));
                    frames_held += 1;
                }
                NextBatch::Limited => {
                    assert!(held.len() >= GRANT, "seed {seed}: refused below the grant");
                }
                _ => {}
            }
            if !held.is_empty() && rng.next().is_multiple_of(3) {
                let at = (rng.next() % held.len() as u64) as usize;
                held.swap_remove(at);
            }
            for _ in 0..(rng.next() % 40) {
                std::hint::spin_loop();
            }
            for (seq, frame) in &held {
                assert!(
                    intact(frame, *seq),
                    "seed {seed}: a held frame with sequence {seq} is not its own bytes"
                );
            }
        }
        frames_held
    });
    let mut published = 0_u64;
    for attempts in 1..=frames {
        published += u64::from(topic.send(attempts));
        std::thread::yield_now();
    }
    done.store(true, std::sync::atomic::Ordering::Release);
    let checked = consumer
        .join()
        .expect("the reader checked every held frame");
    assert!(
        checked > 0,
        "seed {seed}: published {published} of {frames}, and the reader checked none"
    );
}

#[test]
fn held_frames_keep_their_bytes_and_sequence_with_the_reader_on_its_own_thread() {
    for seed in 1..=200 {
        run_threaded(seed, 20_000);
    }
}
