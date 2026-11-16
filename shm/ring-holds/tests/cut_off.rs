//! Cutting a reader off, with real pages. Its buffers come back, it is told on its next call,
//! nobody else is touched, and its slot stays its own until its owner removes it. A value that a
//! cut-off reader was still using when its buffer was reused comes back as `Torn`, never as good.

use shm_ring::{FieldId, Ring, SampleTime, Spec};
use shm_ring_buffers::{
    BufferIndex, BufferReader, BufferWriter, Dropped, Held, Holders, NextBatch, Pool, Torn,
};
use shm_ring_holds::{HoldTable, Limits, Marks};

const FRAME: usize = 4096;
const MS: u64 = 1_000_000;

type Reader = BufferReader<shm_ring::ReadMap, Marks>;

struct Topic {
    ring: Ring,
    writer: BufferWriter,
    table: HoldTable,
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
            ring,
            writer,
            table: HoldTable::new(buffers),
            buffers,
        }
    }

    fn reader(&mut self, limits: Limits) -> (u32, Reader) {
        let slot = self.table.free_slot().expect("a free slot");
        let marks = Marks::create(self.buffers, slot, limits.max_held).expect("marks");
        self.table
            .add(slot, marks.page_fd().expect("page"), limits)
            .expect("add reader");
        let view = self.writer.pool().view().expect("view");
        let reader =
            BufferReader::new(self.ring.reader().expect("reader"), FieldId(0), view, marks);
        (slot, reader)
    }

    /// One frame at `now_ms`, every byte `fill`. `Err` if it had to be dropped.
    fn send(&mut self, fill: u8, now_ms: u64) -> Result<BufferIndex, Dropped> {
        self.table.enforce(now_ms * MS, self.writer.pool().page());
        let buffer = self.writer.take(&self.table)?;
        self.writer.buffer_mut(buffer).expect("free").fill(fill);
        self.writer
            .publish(&[buffer], SampleTime::from_nanos(now_ms * MS), &[fill])
            .expect("publish");
        Ok(buffer)
    }
}

/// Reads until a frame comes, and holds it.
fn hold(reader: &mut Reader) -> Held<Marks> {
    for _ in 0..100 {
        if let NextBatch::Frame(frame) = reader.next() {
            return frame.held;
        }
    }
    panic!("no frame");
}

#[test]
fn holders_of_lists_the_readers_that_mark_a_buffer_and_not_the_cut_off_ones() {
    let mut topic = Topic::new(3);
    let (a, mut reader_a) = topic.reader(Limits::default());
    let (b, mut reader_b) = topic.reader(Limits::default());
    // Both read the first frame, so both hold its buffer.
    let first = topic.send(1, 0).expect("frame");
    let _held_a = hold(&mut reader_a);
    let _held_b = hold(&mut reader_b);
    let second = topic.send(2, 1).expect("frame");

    assert_eq!(topic.table.holders_of(first), vec![a, b]);
    assert!(topic.table.holders_of(second).is_empty());
    assert!(topic.table.holders_of(BufferIndex(2)).is_empty());
    assert!(topic.table.holders_of(BufferIndex(99)).is_empty());

    assert!(topic.table.cut_off(a, topic.writer.pool().page()));
    assert_eq!(
        topic.table.holders_of(first),
        vec![b],
        "a cut-off reader holds nothing"
    );
}

#[test]
fn a_cut_off_holders_buffers_become_reusable_and_it_is_told_on_its_next_call() {
    let mut topic = Topic::new(3);
    let (slot, mut reader) = topic.reader(Limits::default());
    let first = topic.send(1, 0).expect("frame");
    let held = hold(&mut reader);
    assert_eq!(held.buffer(0), Some(first));
    // The buffer is held: it is not reused however often the publisher asks.
    for round in 2..6_u8 {
        assert_ne!(topic.send(round, u64::from(round)).expect("frame"), first);
    }

    assert!(topic.table.cut_off(slot, topic.writer.pool().page()));
    assert!(!topic.table.held(first));
    let reused = (6..9_u8).any(|round| topic.send(round, 10).expect("frame") == first);
    assert!(reused, "a cut-off reader's buffer is reused");

    // Its next operation says so; it does not read on as though nothing happened.
    assert!(matches!(reader.next(), NextBatch::CutOff));
    assert!(matches!(reader.next(), NextBatch::CutOff));
}

#[test]
fn cutting_one_reader_off_leaves_the_others_holding() {
    let mut topic = Topic::new(3);
    let (a, mut reader_a) = topic.reader(Limits::default());
    let (b, mut reader_b) = topic.reader(Limits::default());
    let first = topic.send(1, 0).expect("frame");
    let held_a = hold(&mut reader_a);
    let held_b = hold(&mut reader_b);

    assert!(topic.table.cut_off(a, topic.writer.pool().page()));
    assert!(topic.table.is_cut_off(a) && !topic.table.is_cut_off(b));
    // Reader b still holds the first frame's buffer, so it is not reused.
    assert!(topic.table.held(first));
    for round in 2..8_u8 {
        assert_ne!(topic.send(round, u64::from(round)).expect("frame"), first);
    }
    assert_eq!(held_b.read(0, |bytes| bytes[0]), Some(Ok(1)));
    // Only the cut-off reader is told.
    assert!(matches!(reader_a.next(), NextBatch::CutOff));
    assert!(!matches!(reader_b.next(), NextBatch::CutOff));
    drop(held_a);
}

#[test]
fn a_cut_off_slot_is_kept_until_its_owner_removes_it() {
    let mut topic = Topic::new(2);
    let (slot, _reader) = topic.reader(Limits::default());
    assert_eq!(topic.table.readers(), 1);

    assert!(topic.table.cut_off(slot, topic.writer.pool().page()));
    assert_eq!(topic.table.readers(), 1, "cut off is not removed");
    assert!(
        !topic.table.cut_off(slot, topic.writer.pool().page()),
        "cutting off twice does nothing the second time"
    );
    assert!(
        !topic.table.cut_off(7, topic.writer.pool().page()),
        "no reader in slot 7"
    );
    assert_ne!(topic.table.free_slot(), Some(slot), "the slot is not free");

    let marks = Marks::create(topic.buffers, slot, 0).expect("marks");
    assert!(
        topic
            .table
            .add(slot, marks.page_fd().expect("page"), Limits::default())
            .is_err(),
        "a cut-off slot is not handed to a new holder"
    );

    // The owner's normal release: the slot is clean and can be given again.
    topic.table.remove(slot, topic.writer.pool().page());
    assert!(!topic.table.is_cut_off(slot));
    assert_eq!(topic.table.free_slot(), Some(slot));
    topic
        .table
        .add(slot, marks.page_fd().expect("page"), Limits::default())
        .expect("the released slot is given again");
    assert!(!topic.table.is_cut_off(slot));
}

#[test]
fn a_value_is_good_when_the_buffer_was_not_reused_during_the_use() {
    let mut topic = Topic::new(2);
    let (_, mut reader) = topic.reader(Limits::default());
    let first = topic.send(1, 0).expect("frame");
    topic.send(2, 1).expect("frame");
    let held = hold(&mut reader);
    assert_eq!(held.buffer(0), Some(first));

    assert_eq!(
        held.read(0, |bytes| bytes.iter().all(|byte| *byte == 1)),
        Some(Ok(true))
    );
    assert_eq!(reader.torn(), 0);
    assert_eq!(held.read(5, |_| ()), None, "no such buffer in the frame");
}

/// The reuse under the reader: it is cut off, the publisher takes its buffer back and writes
/// over it while the reader is in the middle of using the bytes. The value is dropped as torn,
/// the count goes up, and the bytes the reader saw are not returned as good.
#[test]
fn a_buffer_reused_during_the_use_gives_torn_and_counts_it() {
    let mut topic = Topic::new(2);
    let (slot, mut reader) = topic.reader(Limits::default());
    let first = topic.send(1, 0).expect("frame");
    topic.send(2, 1).expect("frame");
    let held = hold(&mut reader);

    let verdict = held.read(0, |bytes| {
        let before = bytes[0];
        // The reader is cut off, which leaves its marks ignored; then the publisher reuses the
        // buffer it held, with a new frame, all while the reader is in the closure.
        assert!(topic.table.cut_off(slot, topic.writer.pool().page()));
        let reused = topic.send(9, 2).expect("frame");
        assert_eq!(
            reused, first,
            "the cut-off reader's buffer is the one reused"
        );
        before
    });
    assert_eq!(verdict, Some(Err(Torn { buffer: first })));
    assert_eq!(reader.torn(), 1, "the torn value is counted");
    assert!(
        Torn { buffer: first }.to_string().contains("dropped"),
        "the torn value says it was dropped"
    );

    // Checked after the fact, the same buffer is torn again, and each dropped value is counted.
    assert!(matches!(held.read(0, |bytes| bytes[0]), Some(Err(_))));
    assert_eq!(reader.torn(), 2);
    // The unchecked accessor still shows what the buffer holds now: that is what it is for.
    assert_eq!(held.bytes(0).expect("bytes")[0], 9);
}

/// A pool view read without a hold: the value is checked against the use it began in, so a reuse
/// during the closure is torn, and an undisturbed read is good.
#[test]
fn a_pool_view_read_is_torn_when_the_buffer_is_reused_during_it() {
    let mut topic = Topic::new(2);
    let (slot, _reader) = topic.reader(Limits::default());
    let first = topic.send(1, 0).expect("frame");
    topic.send(2, 1).expect("frame");
    let view = topic.writer.pool().view().expect("view");

    assert_eq!(view.read(first, |b| b[0]), Some(Ok(1)));
    let verdict = view.read(first, |bytes| {
        let before = bytes[0];
        assert!(topic.table.cut_off(slot, topic.writer.pool().page()));
        assert_eq!(topic.send(9, 2).expect("frame"), first);
        before
    });
    assert_eq!(verdict, Some(Err(Torn { buffer: first })));
    assert!(
        view.read(BufferIndex(7), |_| ()).is_none(),
        "not in the pool"
    );
}
