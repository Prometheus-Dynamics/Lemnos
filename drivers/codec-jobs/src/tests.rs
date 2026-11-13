//! The server against the mock stream, with the test as the consumer and a block that records
//! what it was asked.

use lemnos_contract_codec_block::work::Builder;
use lemnos_contract_codec_block::{BufferRef, WORK_HEADER_BYTES};
use lemnos_hal::blocks::BufferKind;
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use super::*;

const BUFFERS: u32 = 8;
const BYTES: usize = 4096;

/// What the block was asked to start.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Asked {
    refs: Vec<BufferRef>,
    items: Vec<(u32, Vec<u8>)>,
    /// Which of buffers 0 to 7 it could get a handle of.
    handles: Vec<u32>,
}

#[derive(Default)]
struct Recorder {
    asked: Vec<Asked>,
    /// What `start` answers, and the read-back it leaves.
    answer: Option<Start>,
    read_back: Vec<u8>,
    finished: Vec<Finished>,
    released: u32,
    limits: Option<BlockInfo>,
}

impl Block for Recorder {
    fn name(&self) -> &str {
        "test:recorder"
    }

    fn limits(&self) -> BlockInfo {
        self.limits.unwrap_or(BlockInfo {
            max_work_bytes: 1024,
            max_refs: 4,
            max_items: 4,
            max_read_back: 64,
            max_buffers: BUFFERS,
        })
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        _now_ns: u64,
    ) -> Start {
        self.asked.push(Asked {
            refs: work.refs().collect(),
            items: work.items().map(|(tag, b)| (tag, b.to_vec())).collect(),
            handles: (0..BUFFERS)
                .filter(|&b| buffers.handle(b).is_some())
                .collect(),
        });
        read_back[..self.read_back.len()].copy_from_slice(&self.read_back);
        let answer = self.answer.unwrap_or(Start::Done(self.read_back.len()));
        // Each pending job gets the next token.
        if let Start::Pending(token) = answer {
            self.answer = Some(Start::Pending(token + 1));
        }
        answer
    }

    fn poll(&mut self, read_back: &mut [u8], _now_ns: u64) -> Option<Finished> {
        let finished = self.finished.pop()?;
        read_back[..finished.read_back].fill(0xee);
        Some(finished)
    }

    fn release(&mut self) {
        self.released += 1;
    }
}

struct Rig {
    server: Server,
    block: Recorder,
    stream: MockStream,
    consumer: MockStream,
    now: u64,
}

fn reference(slot: u32, buffer: u32) -> BufferRef {
    BufferRef {
        slot,
        buffer,
        offset: 0,
        length: 0,
    }
}

impl Rig {
    fn on(stream: MockStream) -> Self {
        Self {
            server: Server::new(),
            block: Recorder::default(),
            consumer: stream.clone(),
            stream,
            now: 0,
        }
    }

    fn new() -> Self {
        Self::on(MockStream::with_bytes(BUFFERS, BYTES))
    }

    fn tick(&mut self) -> bool {
        self.now += POLL_NS;
        self.server
            .serve(&mut self.stream, &mut self.block, self.now)
            .unwrap()
    }

    /// Puts work in `buffer` and returns its length.
    fn work(&self, buffer: u32, refs: &[BufferRef], items: &[(u32, &[u8])]) -> u32 {
        let mut bytes = vec![0u8; BYTES];
        let mut builder = Builder::new(&mut bytes).unwrap();
        for r in refs {
            builder.reference(*r).unwrap();
        }
        for (tag, data) in items {
            builder.item(*tag, data).unwrap();
        }
        let len = builder.finish();
        self.consumer.fill(buffer, &bytes[..len]);
        len as u32
    }

    fn post(&self, identity: u64, work: u32, work_bytes: u32, result: u32) {
        let job = Job {
            identity,
            work,
            work_bytes,
            result,
        };
        let mut bytes = [0u8; JOB_BYTES];
        let mut writer = Writer::new(&mut bytes);
        job.encode(&mut writer).unwrap();
        self.consumer.post_job(&bytes);
    }

    /// The next completion: identity, record, and read-back.
    fn completion(&self) -> Option<(u64, Completion, Vec<u8>)> {
        let frame = self.consumer.take_frame()?;
        let contents = self.consumer.contents(frame.buffer);
        let record = Completion::decode(&mut Reader::new(&contents[..COMPLETION_BYTES])).unwrap();
        assert_eq!(frame.bytes as usize, DATA_OFFSET + record.count as usize);
        let data = contents[DATA_OFFSET..DATA_OFFSET + record.count as usize].to_vec();
        Some((frame.identity, record, data))
    }

    fn status(&self, identity: u64) -> JobStatus {
        let (got, record, _) = self.completion().expect("a completion");
        assert_eq!(got, identity);
        record.status
    }
}

#[test]
fn a_job_reaches_the_block_as_it_was_written_and_completes_with_its_read_back() {
    let mut rig = Rig::new();
    rig.block.read_back = vec![1, 2, 3, 4, 5];
    let refs = [
        BufferRef {
            slot: 64,
            buffer: 2,
            offset: 16,
            length: 100,
        },
        reference(65, 3),
    ];
    let len = rig.work(0, &refs, &[(7, &[9, 9]), (8, &[])]);
    rig.post(0xabc, 0, len, 1);
    assert!(!rig.tick());
    assert_eq!(
        rig.block.asked,
        [Asked {
            refs: refs.to_vec(),
            items: vec![(7, vec![9, 9]), (8, vec![])],
            // A stream of plain memory has no handles to lend.
            handles: vec![],
        }]
    );
    let (identity, record, data) = rig.completion().unwrap();
    assert_eq!(
        (identity, record.status, record.count),
        (0xabc, JobStatus::Done, 5)
    );
    assert_eq!(data, [1, 2, 3, 4, 5]);
    assert_eq!(rig.server.counters().jobs, 1);

    // The result buffer may be the work's: the work was read before the record was written.
    rig.consumer.return_buffer(1);
    let len = rig.work(0, &[], &[(1, &[1])]);
    rig.post(2, 0, len, 0);
    rig.tick();
    assert_eq!(rig.status(2), JobStatus::Done);
    assert_eq!(rig.block.asked[1].items, [(1, vec![1])]);
    let info = Server::info(&rig.block);
    assert_eq!((info.max_work_bytes, info.max_read_back), (1024, 64));
}

#[test]
fn work_that_is_malformed_or_names_what_it_may_not_is_refused_before_the_block() {
    let mut rig = Rig::new();
    let good = rig.work(0, &[reference(1, 2)], &[(1, &[0; 8])]);
    let mut identity = 0;
    let mut refused = |rig: &mut Rig, what: &str, work: u32, bytes: u32, result: u32| {
        identity += 1;
        rig.post(identity, work, bytes, result);
        rig.tick();
        assert_eq!(rig.status(identity), JobStatus::Refused, "{what}");
        assert!(rig.block.asked.is_empty(), "{what}");
        rig.consumer.return_buffer(result);
    };
    refused(&mut rig, "cut short", 0, good - 1, 1);
    refused(&mut rig, "a byte over", 0, good + 1, 1);
    refused(&mut rig, "no header", 0, WORK_HEADER_BYTES as u32 - 1, 1);
    refused(&mut rig, "over the block's limit", 0, 1025, 1);
    refused(&mut rig, "past the buffer", 0, BYTES as u32 + 1, 1);
    refused(&mut rig, "the largest length", 0, u32::MAX, 1);

    let len = rig.work(0, &[reference(1, 1)], &[]);
    refused(&mut rig, "a reference to the result buffer", 0, len, 1);
    let len = rig.work(0, &[reference(1, BUFFERS)], &[]);
    refused(&mut rig, "a buffer the stream doesn't have", 0, len, 1);
    let len = rig.work(0, &[reference(1, 63)], &[]);
    refused(&mut rig, "a buffer past the block's", 0, len, 1);
    let len = rig.work(0, &[reference(1, 64)], &[]);
    refused(&mut rig, "a buffer past the server's", 0, len, 1);
    let len = rig.work(0, &[reference(1, u32::MAX)], &[]);
    refused(&mut rig, "the largest buffer number", 0, len, 1);
    let five = [reference(0, 2); 5];
    let len = rig.work(0, &five, &[]);
    refused(&mut rig, "more references than the block takes", 0, len, 1);
    let items: [(u32, &[u8]); 5] = [(0, &[]); 5];
    let len = rig.work(0, &[], &items);
    refused(&mut rig, "more items than the block takes", 0, len, 1);

    // A buffer the consumer has (a completion it has not returned) can't be referenced.
    let len = rig.work(0, &[], &[]);
    rig.post(100, 0, len, 5);
    rig.tick();
    assert_eq!(rig.status(100), JobStatus::Done);
    rig.block.asked.clear();
    let len = rig.work(0, &[reference(1, 5)], &[]);
    refused(&mut rig, "a buffer the consumer has", 0, len, 1);
    assert_eq!(
        rig.consumer.faults(),
        0,
        "a refused job is no broken consumer"
    );
    assert_eq!(rig.server.counters().refused, 14);
}

#[test]
fn a_job_with_nothing_to_complete_on_faults_its_consumer() {
    type Post<'a> = &'a dyn Fn(&Rig, u32);
    let broken: [(&str, Post<'_>); 7] = [
        ("too few bytes", &|rig, _| rig.consumer.post_job(&[1, 2, 3])),
        ("too many bytes", &|rig, _| {
            rig.consumer.post_job(&[0u8; JOB_BYTES + 1]);
        }),
        ("a work buffer the stream doesn't have", &|rig, len| {
            rig.post(9, BUFFERS, len, 1);
        }),
        ("a result buffer the stream doesn't have", &|rig, len| {
            rig.post(9, 0, len, u32::MAX);
        }),
        ("a result buffer the consumer has", &|rig, len| {
            rig.post(9, 0, len, 5);
        }),
        ("a work buffer the consumer has", &|rig, len| {
            rig.post(9, 5, len, 1);
        }),
        ("a result buffer a job in flight uses", &|rig, len| {
            rig.post(9, 0, len, 6);
        }),
    ];
    for (what, post) in broken {
        let mut rig = Rig::new();
        let len = rig.work(0, &[], &[]);
        // Buffer 5 is completed on and stays with the consumer; buffer 6 is referenced by a
        // job the block keeps.
        rig.post(1, 0, len, 5);
        rig.tick();
        assert_eq!(rig.status(1), JobStatus::Done, "{what}");
        rig.block.answer = Some(Start::Pending(77));
        let pending = rig.work(2, &[reference(1, 6)], &[]);
        rig.post(2, 2, pending, 7);
        rig.tick();
        let before = (rig.block.asked.len(), rig.server.counters().refused);
        post(&rig, len);
        // A job behind the broken one is not run.
        rig.post(10, 0, len, 3);
        rig.tick();
        assert_eq!(rig.consumer.faults(), 1, "{what}");
        assert_eq!(rig.block.asked.len(), before.0, "{what}");
        assert_eq!(rig.server.counters().refused, before.1 + 1, "{what}");
        assert_eq!(rig.block.released, 1, "{what}");
        assert!(rig.completion().is_none(), "{what}");
    }
}

#[test]
fn pending_jobs_complete_when_the_block_says_so_in_any_order() {
    let mut rig = Rig::new();
    let len = rig.work(0, &[reference(1, 4)], &[]);
    for (identity, token, result) in [(1, 10, 1), (2, 20, 2), (3, 30, 3)] {
        rig.block.answer = Some(Start::Pending(token));
        rig.post(identity, 0, len, result);
        rig.tick();
    }
    assert!(rig.completion().is_none());
    // The last and the first are over; a token nobody was given completes nothing.
    rig.block.finished = vec![
        Finished {
            token: 10,
            status: JobStatus::Failed,
            read_back: 4,
        },
        Finished {
            token: 99,
            status: JobStatus::Done,
            read_back: 0,
        },
        Finished {
            token: 30,
            status: JobStatus::Done,
            read_back: 2,
        },
    ];
    rig.tick();
    let (identity, record, data) = rig.completion().unwrap();
    assert_eq!(
        (identity, record.status, data),
        (3, JobStatus::Done, vec![0xee; 2])
    );
    let (identity, record, data) = rig.completion().unwrap();
    assert_eq!(
        (identity, record.status, record.count),
        (1, JobStatus::Failed, 0)
    );
    assert!(data.is_empty(), "a job that failed reads nothing back");
    assert!(rig.completion().is_none());
    let counters = rig.server.counters();
    assert_eq!((counters.jobs, counters.failed), (1, 1));
}

#[test]
fn the_block_has_room_for_so_many_and_the_rest_wait_on_the_queue() {
    let mut rig = Rig::on(MockStream::with_bytes(MAX_IN_FLIGHT as u32 + 4, 256));
    rig.block.limits = Some(BlockInfo {
        max_buffers: 64,
        ..rig.block.limits()
    });
    let len = rig.work(0, &[], &[]);
    rig.block.answer = Some(Start::Pending(0));
    for job in 0..MAX_IN_FLIGHT as u32 + 2 {
        rig.post(u64::from(job), 0, len, job + 1);
    }
    // A round's worth at a time, and the server asks to be ticked again at once.
    assert!(rig.tick());
    assert!(rig.tick());
    assert!(!rig.tick());
    assert_eq!(rig.block.asked.len(), MAX_IN_FLIGHT);
    rig.block.finished = vec![Finished {
        token: 0,
        status: JobStatus::Done,
        read_back: 0,
    }];
    rig.tick();
    assert_eq!(rig.status(0), JobStatus::Done);
    assert_eq!(rig.block.asked.len(), MAX_IN_FLIGHT + 1);
}

#[test]
fn read_back_the_result_buffer_cannot_hold_fails_the_job() {
    let mut rig = Rig::on(MockStream::with_bytes(BUFFERS, DATA_OFFSET + 4));
    rig.block.read_back = vec![7; 5];
    let len = rig.work(0, &[], &[]);
    rig.post(1, 0, len, 1);
    rig.tick();
    let (_, record, _) = rig.completion().unwrap();
    assert_eq!((record.status, record.count), (JobStatus::Failed, 0));
    rig.block.read_back = vec![7; 4];
    rig.post(2, 0, len, 2);
    rig.tick();
    let (_, record, data) = rig.completion().unwrap();
    assert_eq!((record.status, data), (JobStatus::Done, vec![7; 4]));
}

#[test]
fn a_consumer_that_goes_takes_its_jobs_with_it_and_the_block_lets_go() {
    let mut rig = Rig::new();
    let len = rig.work(0, &[reference(1, 4)], &[]);
    rig.block.answer = Some(Start::Pending(1));
    rig.post(1, 0, len, 1);
    rig.tick();
    // Another consumer takes the stream between two ticks, on memory of its own.
    rig.consumer.change_consumer();
    rig.block.answer = None;
    let len = rig.work(0, &[], &[(5, &[5])]);
    rig.post(2, 0, len, 1);
    rig.block.finished = vec![Finished {
        token: 1,
        status: JobStatus::Done,
        read_back: 0,
    }];
    rig.tick();
    assert_eq!(rig.block.released, 1);
    // The old consumer's job completes to nobody; the new one's, naming the same buffers,
    // is served.
    assert_eq!(rig.status(2), JobStatus::Done);
    assert!(rig.completion().is_none());
    assert_eq!(rig.block.asked[1].items, [(5, vec![5])]);
}

#[cfg(target_os = "linux")]
#[test]
fn a_block_gets_the_handles_of_the_buffers_its_job_references_and_no_others() {
    use lemnos_linux_sys::dma_buf::sealed_memfd;
    let fds = (0..BUFFERS)
        .map(|_| sealed_memfd(c"codec-jobs-test", BYTES as u64).unwrap())
        .collect();
    let mut rig = Rig::on(MockStream::with_handles(fds));
    assert_eq!(rig.stream.buffer_kind(), BufferKind::Handles);
    let len = rig.work(0, &[reference(1, 2), reference(2, 4)], &[]);
    rig.post(1, 0, len, 1);
    rig.tick();
    assert_eq!(rig.status(1), JobStatus::Done);
    // Not the work's buffer, not the result's, not one nobody named.
    assert_eq!(rig.block.asked[0].handles, [2, 4]);
}

/// A block that answers at once and keeps nothing.
struct Quick;

impl Block for Quick {
    fn name(&self) -> &str {
        "test:quick"
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: 1024,
            max_refs: 4,
            max_items: 4,
            max_read_back: 64,
            max_buffers: 4,
        }
    }

    fn start(&mut self, work: &Work<'_>, _: &Buffers<'_>, read_back: &mut [u8], _: u64) -> Start {
        let items = work.items().count();
        read_back[..8].fill(items as u8);
        Start::Done(8)
    }

    fn release(&mut self) {}
}

/// A stream that posts the same job over and over and takes its completions, without
/// allocating: buffer 0 holds the work, buffer 1 is the result and comes straight back.
struct Loop {
    memory: [[u8; 256]; 2],
    free: [bool; 2],
    posted: u64,
    published: u64,
}

impl StreamEndpoint for Loop {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        2
    }

    fn publish(&mut self, frame: StreamFrame) -> Result<(), ErrorKind> {
        self.published += 1;
        self.free[frame.buffer as usize] = true;
        Ok(())
    }

    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        let at = self.free.iter().position(|free| *free);
        if let Some(at) = at {
            self.free[at] = false;
        }
        Ok(at.map(|at| at as u32))
    }

    fn next_job(&mut self, out: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        if self.free[1] || self.posted > self.published {
            return Ok(None);
        }
        self.posted += 1;
        let job = Job {
            identity: self.posted,
            work: 0,
            work_bytes: (WORK_HEADER_BYTES + (4 + 4 * 4) + (4 + 4 + 4) + 4) as u32,
            result: 1,
        };
        let mut writer = Writer::new(out);
        job.encode(&mut writer).map_err(|_| ErrorKind::Failed)?;
        Ok(Some(writer.len()))
    }

    fn write(&mut self, buffer: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        let to = self.memory[buffer as usize]
            .get_mut(offset..offset + data.len())
            .ok_or(ErrorKind::InvalidInput)?;
        to.copy_from_slice(data);
        Ok(true)
    }

    fn read(&mut self, buffer: u32, offset: usize, out: &mut [u8]) -> Result<bool, ErrorKind> {
        let from = self.memory[buffer as usize]
            .get(offset..offset + out.len())
            .ok_or(ErrorKind::InvalidInput)?;
        out.copy_from_slice(from);
        Ok(true)
    }

    fn buffer_handle(&self, _: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Plain
    }

    fn buffer_current(&self, _: u32) -> Result<bool, ErrorKind> {
        Ok(true)
    }

    fn fault_consumer(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}

#[test]
fn serving_jobs_allocates_nothing() {
    let mut stream = Loop {
        memory: [[0; 256]; 2],
        free: [true; 2],
        posted: 0,
        published: 0,
    };
    let len = {
        let mut work = Builder::new(&mut stream.memory[0]).unwrap();
        work.reference(reference(1, 0)).unwrap();
        work.item(3, &[1, 2, 3, 4]).unwrap();
        work.finish()
    };
    assert_eq!(len, WORK_HEADER_BYTES + (4 + 4 * 4) + (4 + 4 + 4) + 4);
    let mut server = Server::new();
    let mut block = Quick;
    for now in 0..4 {
        server.serve(&mut stream, &mut block, now).unwrap();
    }
    assert!(server.counters().jobs > 0, "{:?}", server.counters());
    let before = server.counters().jobs;
    let ((), allocated) = allocations(|| {
        for now in 0..1000 {
            server.serve(&mut stream, &mut block, now).unwrap();
        }
    });
    assert_eq!(allocated, 0);
    let counters = server.counters();
    assert!(counters.jobs >= before + 400, "{counters:?}");
    assert_eq!((counters.refused, counters.failed), (0, 0));
    assert_eq!(
        stream.memory[1][DATA_OFFSET], 1,
        "the block's read-back landed"
    );
}
