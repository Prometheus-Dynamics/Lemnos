use lemnos_contract_npu::{
    Counters, JOB_BYTES, MAX_STREAM_NAME, Npu, NpuInfo, RESULT_BYTES, RunJob, RunResult, RunStatus,
};
use lemnos_core::{Contract, Decode, Encode, REPLY_INLINE, Reader, Result, Shape, Status, Writer};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_wire::ring::{Consumer, FrameSlot, Layout, Producer, STATE_FAULT, heap_words};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const INFO: lemnos_core::ContractInfo = <dyn Npu as Contract>::INFO;

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
        assert!(
            args <= lemnos_core::LOCAL_ARGS,
            "{} takes {args} bytes",
            method.name
        );
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
    assert_eq!(INFO.tag.as_str(), "npu/1");
    let ids: Vec<_> = INFO.methods.iter().map(|m| (m.name, m.id)).collect();
    assert_eq!(
        ids,
        [
            ("info", 1),
            ("job_stream", 2),
            ("load", 3),
            ("unload", 4),
            ("attach", 5),
            ("detach", 6),
            ("counters", 7),
        ]
    );
    assert!(INFO.methods[1].has_out_buffer);
    assert_eq!(MAX_STREAM_NAME, 32);
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

#[test]
fn every_type_round_trips() {
    round_trip(NpuInfo {
        max_program_bytes: u32::MAX,
        max_programs: 8,
        max_attached: 16,
    });
    round_trip(RunJob {
        identity: u64::MAX,
        program: 3,
        result: 9,
    });
    round_trip(RunResult {
        status: RunStatus::Failed,
    });
    round_trip(Counters {
        jobs: 1,
        errors: u64::MAX,
    });
}

#[test]
fn a_run_is_a_fixed_small_record_and_a_result_is_a_block_of_one_byte() {
    assert_eq!(max_len(&RunJob::SHAPE), Some(JOB_BYTES));
    assert_eq!(JOB_BYTES, 4 + 8 + 4 + 4);
    assert_eq!(max_len(&RunResult::SHAPE), Some(RESULT_BYTES));
    assert_eq!(RESULT_BYTES, 4 + 1);
}

#[test]
fn an_unknown_run_status_is_refused() {
    assert_eq!(
        RunResult::decode(&mut Reader::new(&[7])).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

// Simulated runs. The NPU's end holds the programs and their attachments, runs on the buffers both
// sides can see, and publishes each run's completion over the ring the frame stream uses. Its
// arithmetic is a stand-in for a program: a run copies slot 0 to slot 1, adding one to each byte.
// A program whose first byte is 0xff is one the chip refuses, and one whose first byte is 0xee
// stops mid-run.

const BUFFERS: u32 = 6;
const BUFFER_BYTES: usize = 128;
const PROGRAMS: usize = 2;
const SLOTS: usize = 2;
const MAX_PROGRAM: u32 = 64;
const REFUSED_BY_CHIP: u8 = 0xff;
const STOPS_MID_RUN: u8 = 0xee;

#[derive(Clone, Copy)]
struct Attachment {
    buffer: u32,
    bytes: u32,
}

#[derive(Clone)]
struct Program {
    bytes: Vec<u8>,
    slots: [Option<Attachment>; SLOTS],
}

/// The NPU's end: loads and attaches, and runs jobs on the ring.
struct SimNpu {
    pool: Vec<Vec<u8>>,
    programs: Vec<Option<Program>>,
    ring: Producer,
    sequence: u64,
    counters: Counters,
}

impl SimNpu {
    /// Takes at most one job. Returns `false` when none is waiting.
    fn run_one(&mut self) -> bool {
        // Collect the buffers the consumer has released, so its return queue never fills.
        while self.ring.pop_return().unwrap().is_some() {}
        let mut bytes = [0u8; JOB_BYTES];
        let Some(len) = self.ring.pop_job(&mut bytes).unwrap() else {
            return false;
        };
        let job = match RunJob::decode(&mut Reader::new(&bytes[..len])) {
            Ok(job) if (job.result as usize) < self.pool.len() => job,
            _ => {
                self.ring.close(STATE_FAULT);
                return false;
            }
        };
        let status = self.run(&job);
        match status {
            RunStatus::Done => self.counters.jobs += 1,
            RunStatus::Refused | RunStatus::Failed => self.counters.errors += 1,
        }
        let mut record = [0u8; RESULT_BYTES];
        let used = {
            let mut writer = Writer::new(&mut record);
            RunResult { status }.encode(&mut writer).unwrap();
            writer.len()
        };
        self.pool[job.result as usize][..used].copy_from_slice(&record[..used]);
        let sent = self
            .ring
            .push_frame(FrameSlot {
                buffer: job.result,
                bytes: used as u32,
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

    fn run(&mut self, job: &RunJob) -> RunStatus {
        let Some(program) = (job.program as usize)
            .checked_sub(1)
            .and_then(|at| self.programs.get(at))
            .and_then(Option::as_ref)
        else {
            return RunStatus::Refused;
        };
        let [Some(input), Some(output)] = program.slots[..2] else {
            return RunStatus::Refused;
        };
        let attached = program
            .slots
            .iter()
            .flatten()
            .any(|a| a.buffer == job.result);
        if attached || input.buffer == output.buffer {
            return RunStatus::Refused;
        }
        if program.bytes.first() == Some(&STOPS_MID_RUN) {
            return RunStatus::Failed;
        }
        let n = input.bytes.min(output.bytes) as usize;
        let [src, dst] = self
            .pool
            .get_disjoint_mut([input.buffer as usize, output.buffer as usize])
            .unwrap();
        for (to, from) in dst[..n].iter_mut().zip(&src[..n]) {
            *to = from.wrapping_add(1);
        }
        RunStatus::Done
    }
}

impl Npu for SimNpu {
    fn info(&mut self) -> Result<NpuInfo> {
        Ok(NpuInfo {
            max_program_bytes: MAX_PROGRAM,
            max_programs: PROGRAMS as u16,
            max_attached: SLOTS as u16,
        })
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        into.get_mut(..3)
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(b"npu");
        Ok(3)
    }

    fn load(&mut self, buffer: u32, bytes: u32) -> Result<u32> {
        if buffer >= BUFFERS || bytes == 0 || bytes > MAX_PROGRAM || bytes as usize > BUFFER_BYTES {
            return Err(Status::INVALID_ARGUMENT);
        }
        let program = self.pool[buffer as usize][..bytes as usize].to_vec();
        if program.first() == Some(&REFUSED_BY_CHIP) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let at = self
            .programs
            .iter()
            .position(Option::is_none)
            .ok_or(Status::BUSY)?;
        self.programs[at] = Some(Program {
            bytes: program,
            slots: [None; SLOTS],
        });
        Ok(at as u32 + 1)
    }

    fn unload(&mut self, program: u32) -> Result<()> {
        let slot = (program as usize)
            .checked_sub(1)
            .and_then(|at| self.programs.get_mut(at))
            .ok_or(Status::NOT_FOUND)?;
        if slot.take().is_none() {
            return Err(Status::NOT_FOUND);
        }
        Ok(())
    }

    fn attach(&mut self, program: u32, slot: u16, buffer: u32, bytes: u32) -> Result<()> {
        if usize::from(slot) >= SLOTS || buffer >= BUFFERS || bytes as usize > BUFFER_BYTES {
            return Err(Status::INVALID_ARGUMENT);
        }
        let program = (program as usize)
            .checked_sub(1)
            .and_then(|at| self.programs.get_mut(at))
            .and_then(Option::as_mut)
            .ok_or(Status::NOT_FOUND)?;
        program.slots[usize::from(slot)] = Some(Attachment { buffer, bytes });
        Ok(())
    }

    fn detach(&mut self, program: u32, slot: u16) -> Result<()> {
        let program = (program as usize)
            .checked_sub(1)
            .and_then(|at| self.programs.get_mut(at))
            .and_then(Option::as_mut)
            .ok_or(Status::NOT_FOUND)?;
        let slot = program
            .slots
            .get_mut(usize::from(slot))
            .ok_or(Status::NOT_FOUND)?;
        if slot.take().is_none() {
            return Err(Status::NOT_FOUND);
        }
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }
}

struct Rig {
    npu: SimNpu,
    consumer: Consumer,
}

impl Rig {
    fn new() -> Self {
        let layout = Layout::new(BUFFERS, BUFFER_BYTES as u32, 4, JOB_BYTES as u32).unwrap();
        let memory = heap_words(layout.ring_words());
        Self {
            npu: SimNpu {
                pool: vec![vec![0; BUFFER_BYTES]; BUFFERS as usize],
                programs: vec![None; PROGRAMS],
                ring: Producer::new(memory.clone(), layout).unwrap(),
                sequence: 0,
                counters: Counters::default(),
            },
            consumer: Consumer::attach(memory, layout).unwrap(),
        }
    }

    /// Writes program bytes into buffer 0 and loads them, the way a caller does.
    fn load(&mut self, bytes: &[u8]) -> Result<u32> {
        self.npu.pool[0][..bytes.len()].copy_from_slice(bytes);
        self.npu.load(0, bytes.len() as u32)
    }

    /// Posts one run, lets the NPU run it, releases its result, and says what it completed with.
    fn run(&mut self, job: &RunJob) -> (u64, RunStatus) {
        let mut bytes = [0u8; JOB_BYTES];
        let used = {
            let mut writer = Writer::new(&mut bytes);
            job.encode(&mut writer).unwrap();
            writer.len()
        };
        assert!(self.consumer.push_job(&bytes[..used]).unwrap());
        assert!(self.npu.run_one());
        let done = self.consumer.pop_frame().unwrap().expect("a completion");
        assert_eq!(done.buffer, job.result);
        assert_eq!(done.bytes as usize, RESULT_BYTES);
        let status = RunResult::decode(&mut Reader::new(
            &self.npu.pool[done.buffer as usize][..RESULT_BYTES],
        ))
        .unwrap()
        .status;
        assert!(self.consumer.release(done.buffer, done.fence).unwrap());
        (done.identity, status)
    }
}

fn attach_pair(rig: &mut Rig, program: u32) {
    rig.npu.attach(program, 0, 1, 4).unwrap();
    rig.npu.attach(program, 1, 2, 4).unwrap();
}

#[test]
fn a_loaded_program_runs_on_its_attached_buffers() {
    let mut rig = Rig::new();
    let program = rig.load(&[1, 2, 3]).unwrap();
    assert_eq!(program, 1);
    attach_pair(&mut rig, program);
    rig.npu.pool[1][..4].copy_from_slice(&[10, 20, 30, 40]);
    let (identity, status) = rig.run(&RunJob {
        identity: 0xabc,
        program,
        result: 3,
    });
    assert_eq!((identity, status), (0xabc, RunStatus::Done));
    assert_eq!(&rig.npu.pool[2][..4], &[11, 21, 31, 41]);
    assert_eq!(rig.npu.counters, Counters { jobs: 1, errors: 0 });
}

#[test]
fn a_run_on_a_slot_that_is_not_attached_is_refused() {
    let mut rig = Rig::new();
    let program = rig.load(&[1]).unwrap();
    rig.npu.attach(program, 0, 1, 4).unwrap();
    let (_, status) = rig.run(&RunJob {
        identity: 1,
        program,
        result: 3,
    });
    assert_eq!(status, RunStatus::Refused);
    assert_eq!(rig.npu.counters, Counters { jobs: 0, errors: 1 });
}

#[test]
fn a_run_of_an_unloaded_program_is_refused() {
    let mut rig = Rig::new();
    let program = rig.load(&[1]).unwrap();
    attach_pair(&mut rig, program);
    rig.npu.unload(program).unwrap();
    let (_, status) = rig.run(&RunJob {
        identity: 2,
        program,
        result: 3,
    });
    assert_eq!(status, RunStatus::Refused);
    assert_eq!(rig.npu.unload(program), Err(Status::NOT_FOUND));
}

#[test]
fn a_result_buffer_the_program_is_attached_to_is_refused() {
    let mut rig = Rig::new();
    let program = rig.load(&[1]).unwrap();
    attach_pair(&mut rig, program);
    let (_, status) = rig.run(&RunJob {
        identity: 3,
        program,
        result: 2,
    });
    assert_eq!(status, RunStatus::Refused);
}

#[test]
fn a_program_the_chip_refuses_does_not_load() {
    let mut rig = Rig::new();
    assert_eq!(
        rig.load(&[REFUSED_BY_CHIP, 1]),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(rig.load(&[1; 65]), Err(Status::INVALID_ARGUMENT));
    assert_eq!(rig.load(&[]), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn a_chip_that_stops_mid_run_completes_as_failed() {
    let mut rig = Rig::new();
    let program = rig.load(&[STOPS_MID_RUN]).unwrap();
    attach_pair(&mut rig, program);
    let (identity, status) = rig.run(&RunJob {
        identity: 4,
        program,
        result: 3,
    });
    assert_eq!((identity, status), (4, RunStatus::Failed));
    assert_eq!(rig.npu.counters, Counters { jobs: 0, errors: 1 });
}

#[test]
fn a_result_buffer_off_the_stream_faults_the_stream() {
    let mut rig = Rig::new();
    let program = rig.load(&[1]).unwrap();
    attach_pair(&mut rig, program);
    let mut bytes = [0u8; JOB_BYTES];
    let used = {
        let mut writer = Writer::new(&mut bytes);
        RunJob {
            identity: 5,
            program,
            result: BUFFERS,
        }
        .encode(&mut writer)
        .unwrap();
        writer.len()
    };
    assert!(rig.consumer.push_job(&bytes[..used]).unwrap());
    assert!(!rig.npu.run_one());
    assert_eq!(rig.consumer.state(), STATE_FAULT);
}

#[test]
fn a_detached_slot_cannot_be_detached_twice() {
    let mut rig = Rig::new();
    let program = rig.load(&[1]).unwrap();
    rig.npu.attach(program, 0, 1, 4).unwrap();
    assert_eq!(rig.npu.detach(program, 0), Ok(()));
    assert_eq!(rig.npu.detach(program, 0), Err(Status::NOT_FOUND));
    assert_eq!(rig.npu.attach(9, 0, 1, 4), Err(Status::NOT_FOUND));
    assert_eq!(
        rig.npu.attach(program, 2, 1, 4),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn runs_in_a_row_allocate_nothing() {
    let mut rig = Rig::new();
    let program = rig.load(&[1]).unwrap();
    attach_pair(&mut rig, program);
    let job = RunJob {
        identity: 0,
        program,
        result: 3,
    };
    // Warm up: the first run sets up whatever the thread needs.
    rig.run(&job);
    let ((), count) = allocations(|| {
        for identity in 1..1001 {
            let (got, status) = rig.run(&RunJob { identity, ..job });
            assert_eq!((got, status), (identity, RunStatus::Done));
        }
    });
    assert_eq!(count, 0, "allocations in 1000 runs");
    assert_eq!(rig.npu.counters.jobs, 1001);
}
