//! The claim and the driver against a played module that keeps a job's memory the way
//! `kernel/lemnos_vip.c` does: slot `n` at `(n + 1) << 28` for exactly the pages the job names,
//! nothing else, and a program that reaches past them faults (the frame comes back empty). The
//! played block runs one operation, the copy the QEMU harness's fake block runs (state 0x5000:
//! source, destination, bytes), so a program here is the same command words. No device node is
//! opened.

use std::io;
use std::sync::{Arc, Mutex};

use lemnos_contract_npu::{Npu, RESULT_BYTES, RunJob, RunResult, RunStatus};
use lemnos_core::{Decode, Encode, Reader, Status, Writer};
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use crate::claim::{
    Claim, ClaimError, Failure, MAX_PROGRAM_BYTES, MAX_PROGRAMS, MAX_SLOTS, Module, PAGE, Service,
    Session, backoff_ns, pages,
};
use crate::confine::{SLOT_SPAN, slot_address};

const BUFFERS: u32 = 10;
const BUFFER_BYTES: usize = 3 * PAGE as usize;

/// What the played module saw and holds.
#[derive(Default)]
struct Played {
    programs: Vec<Option<Vec<u32>>>,
    runs: u32,
    stages: u32,
    /// Every run's slot lengths, while recording.
    seen: Vec<Vec<u32>>,
    record: bool,
    open_fails: bool,
    opens: u32,
    /// The next run's answer, instead of running.
    answer: Option<io::Result<bool>>,
}

type Shared = Arc<Mutex<Played>>;

fn played() -> Shared {
    Arc::new(Mutex::new(Played {
        programs: vec![None; MAX_PROGRAMS],
        record: true,
        ..Played::default()
    }))
}

struct Node(Shared);

/// A session: the shared record, and its slots' memory, which the claim borrows.
struct Sess {
    shared: Shared,
    slots: Vec<Vec<u8>>,
}

impl Module for Node {
    type Session = Sess;

    fn open(&mut self) -> io::Result<Sess> {
        let mut p = self.0.lock().unwrap();
        p.opens += 1;
        if p.open_fails {
            return Err(io::Error::from_raw_os_error(16));
        }
        Ok(Sess {
            shared: Arc::clone(&self.0),
            slots: vec![Vec::new(); MAX_SLOTS as usize],
        })
    }
}

/// The module's check, enough of it: whole words, state loads above the control states, NOP.
fn scan(words: &[u32]) -> bool {
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let len = match w >> 27 {
            1 => {
                let (addr, count) = (w & 0xffff, (w >> 16) & 0x3ff);
                if addr < 0x400 {
                    return false;
                }
                (1 + count as usize).next_multiple_of(2)
            }
            3 | 9 => 2,
            _ => return false,
        };
        i += len;
    }
    i == words.len()
}

impl Session for Sess {
    fn load(&mut self, program: &[u8]) -> io::Result<u32> {
        let (chunks, _) = program.as_chunks::<4>();
        let words: Vec<u32> = chunks.iter().map(|c| u32::from_le_bytes(*c)).collect();
        if !scan(&words) {
            return Err(io::Error::from_raw_os_error(1));
        }
        let mut p = self.shared.lock().unwrap();
        let free = p.programs.iter().position(Option::is_none);
        let Some(free) = free else {
            return Err(io::Error::from_raw_os_error(16));
        };
        p.programs[free] = Some(words);
        Ok(free as u32 + 1)
    }

    fn unload(&mut self, program: u32) -> io::Result<()> {
        let mut p = self.shared.lock().unwrap();
        p.programs
            .get_mut(program as usize - 1)
            .and_then(Option::take)
            .map(|_| ())
            .ok_or_else(|| io::Error::from_raw_os_error(2))
    }

    fn stage(&mut self, slot: u16, bytes: u32) -> io::Result<()> {
        assert!(bytes.is_multiple_of(PAGE));
        self.shared.lock().unwrap().stages += 1;
        let s = &mut self.slots[usize::from(slot)];
        if s.len() < bytes as usize {
            // Fresh memory holds whatever it held: the claim must clear what a run maps.
            *s = vec![0xee; bytes as usize];
        }
        Ok(())
    }

    fn memory(&mut self, slot: u16) -> Option<&mut [u8]> {
        self.slots
            .get_mut(usize::from(slot))
            .filter(|s| !s.is_empty())
            .map(Vec::as_mut_slice)
    }

    fn run(&mut self, program: u32, lens: &[u32]) -> io::Result<bool> {
        let mut p = self.shared.lock().unwrap();
        p.runs += 1;
        if p.record {
            p.seen.push(lens.to_vec());
        }
        if let Some(answer) = p.answer.take() {
            return answer;
        }
        for (i, &len) in lens.iter().enumerate() {
            assert!(len.is_multiple_of(PAGE));
            assert!(len as usize <= self.slots[i].len());
        }
        let Some(words) = p.programs[program as usize - 1].as_ref() else {
            return Err(io::Error::from_raw_os_error(22));
        };
        Ok(execute(&mut self.slots, lens, words))
    }
}

/// The played block: a copy through the job's slots, faulting outside them.
fn execute(slots: &mut [Vec<u8>], lens: &[u32], words: &[u32]) -> bool {
    let at = |address: u32| -> Option<(usize, usize)> {
        let slot = (address >> 28).checked_sub(1)? as usize;
        let offset = (address & 0x0fff_ffff) as usize;
        (slot < lens.len() && offset < lens[slot] as usize).then_some((slot, offset))
    };
    let mut i = 0;
    let (mut src, mut dst) = (0, 0);
    while i < words.len() {
        let w = words[i];
        if w >> 27 == 1 {
            let (addr, count) = (w & 0xffff, (w >> 16) & 0x3ff);
            for k in 0..count {
                let v = words[i + 1 + k as usize];
                match addr + k {
                    0x5000 => src = v,
                    0x5001 => dst = v,
                    0x5002 => {
                        for b in 0..v {
                            let (Some((ss, so)), Some((ds, d0))) = (at(src + b), at(dst + b))
                            else {
                                return false;
                            };
                            slots[ds][d0] = slots[ss][so];
                        }
                    }
                    _ => {}
                }
            }
            i += (1 + count as usize).next_multiple_of(2);
        } else {
            i += 2;
        }
    }
    true
}

fn copy(src: u32, dst: u32, len: u32) -> Vec<u8> {
    [0x0803_5000u32, src, dst, len, 3 << 27, 0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect()
}

fn slot(n: u16) -> u32 {
    slot_address(n, 1).unwrap().start as u32
}

fn stream() -> MockStream {
    MockStream::with_bytes(BUFFERS, BUFFER_BYTES)
}

fn post(stream: &MockStream, job: RunJob) {
    let mut bytes = [0u8; 16];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).unwrap();
    let used = writer.len();
    stream.post_job(&bytes[..used]);
}

fn completion(stream: &MockStream) -> (u64, RunStatus) {
    let frame = stream.take_frame().expect("a completion");
    assert_eq!(frame.bytes as usize, RESULT_BYTES);
    let status = RunResult::decode(&mut Reader::new(&stream.contents(frame.buffer)[..1]))
        .unwrap()
        .status;
    stream.return_buffer(frame.buffer);
    (frame.identity, status)
}

fn job(identity: u64, program: u32, result: u32) -> RunJob {
    RunJob {
        identity,
        program,
        result,
    }
}

fn claimed(shared: &Shared) -> Claim<Node> {
    Service::new(Node(Arc::clone(shared))).claim(0).unwrap()
}

/// Loads `program` from buffer 0 and attaches `slots` (buffer, bytes) in order.
fn load(
    claim: &mut Claim<Node>,
    stream: &mut MockStream,
    program: &[u8],
    slots: &[(u32, u32)],
) -> Result<u32, Status> {
    stream.fill(0, program);
    let n = claim.load(stream, 0, program.len() as u32)?;
    for (s, &(buffer, bytes)) in slots.iter().enumerate() {
        claim.attach(stream, n, s as u16, buffer, bytes)?;
    }
    Ok(n)
}

fn run_one(claim: &mut Claim<Node>, stream: &mut MockStream, job: RunJob) -> (u64, RunStatus) {
    post(stream, job);
    assert_eq!(claim.serve(stream, 0), Ok(1));
    completion(stream)
}

#[test]
fn a_loaded_program_runs_on_copies_of_its_attached_buffers() {
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    let program = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(1), 8),
        &[(1, 8), (2, 8)],
    )
    .unwrap();
    stream.fill(1, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(7, program, 3)),
        (7, RunStatus::Done)
    );
    assert_eq!(&stream.contents(2)[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
    // Each slot is mapped as whole pages, slots 0 and 1 only.
    assert_eq!(shared.lock().unwrap().seen, vec![vec![PAGE, PAGE]]);
    assert_eq!(claim.counters().jobs, 1);
    assert_eq!(claim.counters().errors, 0);
}

#[test]
fn a_program_reaching_past_its_slots_fails_and_the_next_run_goes_on() {
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    // One byte past slot 1's page: the played MMU faults.
    let past = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(1) + PAGE - 4, 8),
        &[(1, 8), (2, 8)],
    )
    .unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, past, 3)),
        (1, RunStatus::Failed)
    );
    assert_eq!(claim.last_failure(), Some(Failure::Device));
    // Into a slot it didn't name.
    let other = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(2), 8),
        &[(1, 8), (2, 8)],
    )
    .unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, other, 3)),
        (2, RunStatus::Failed)
    );
    // The claim goes on.
    let good = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(1), 8),
        &[(1, 8), (2, 8)],
    )
    .unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(3, good, 3)),
        (3, RunStatus::Done)
    );
    assert!(!claim.lost());
    assert_eq!(claim.counters().jobs, 1);
    assert_eq!(claim.counters().errors, 2);
}

#[test]
fn a_run_that_cannot_start_is_refused_and_nothing_reaches_the_block() {
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    let program = load(&mut claim, &mut stream, &copy(slot(0), slot(1), 8), &[]).unwrap();
    // Slot 1 attached, slot 0 not: a gap.
    claim.attach(&mut stream, program, 1, 2, 8).unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, program, 3)),
        (1, RunStatus::Refused)
    );
    // The result buffer attached.
    claim.attach(&mut stream, program, 0, 3, 8).unwrap();
    assert_eq!(
        run_one(&mut claim, &mut stream, job(2, program, 3)),
        (2, RunStatus::Refused)
    );
    // An unknown program, and program 0.
    assert_eq!(
        run_one(&mut claim, &mut stream, job(3, 5, 4)),
        (3, RunStatus::Refused)
    );
    assert_eq!(
        run_one(&mut claim, &mut stream, job(4, 0, 4)),
        (4, RunStatus::Refused)
    );
    assert_eq!(shared.lock().unwrap().runs, 0);
    assert_eq!(claim.last_failure(), Some(Failure::Refused));
    // A detached slot is a gap again.
    claim.detach(program, 0).unwrap();
    assert_eq!(claim.detach(program, 0), Err(Status::NOT_FOUND));
    assert_eq!(
        run_one(&mut claim, &mut stream, job(5, program, 4)),
        (5, RunStatus::Refused)
    );
    assert_eq!(shared.lock().unwrap().runs, 0);
}

#[test]
fn bad_loads_and_attachments_are_refused() {
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    stream.fill(0, &copy(slot(0), slot(1), 8));
    assert_eq!(claim.load(&mut stream, 0, 0), Err(Status::INVALID_ARGUMENT));
    assert_eq!(
        claim.load(&mut stream, 0, 12),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.load(&mut stream, 0, MAX_PROGRAM_BYTES + 8),
        Err(Status::INVALID_ARGUMENT)
    );
    // Past the buffer.
    assert_eq!(
        claim.load(&mut stream, 0, BUFFER_BYTES as u32 + 8),
        Err(Status::INVALID_ARGUMENT)
    );
    // A program the module refuses: an END, which only the module's own tail may hold.
    let end: Vec<u8> = [2u32 << 27, 0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    assert_eq!(
        load(&mut claim, &mut stream, &end, &[]),
        Err(Status::INVALID_ARGUMENT)
    );
    // As many programs as the module holds, then BUSY.
    let nop: Vec<u8> = [3u32 << 27, 0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    let mut loaded = Vec::new();
    for _ in 0..MAX_PROGRAMS {
        loaded.push(load(&mut claim, &mut stream, &nop, &[]).unwrap());
    }
    assert_eq!(load(&mut claim, &mut stream, &nop, &[]), Err(Status::BUSY));
    let p = loaded[0];
    // Slots: past the last, empty, past the buffer, longer than a slot's span.
    assert_eq!(
        claim.attach(&mut stream, p, MAX_SLOTS, 1, 8),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, p, 0, 1, 0),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, p, 0, 1, BUFFER_BYTES as u32 + 1),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, p, 0, 1, SLOT_SPAN as u32 + 1),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        claim.attach(&mut stream, 99, 0, 1, 8),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(claim.unload(p), Ok(()));
    assert_eq!(claim.unload(p), Err(Status::NOT_FOUND));
    assert_eq!(
        claim.attach(&mut stream, p, 0, 1, 8),
        Err(Status::NOT_FOUND)
    );
    // Unloaded in the module too, so one more fits.
    assert!(load(&mut claim, &mut stream, &nop, &[]).is_ok());
}

#[test]
fn what_a_run_maps_past_its_buffers_bytes_is_cleared_first() {
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    // Slot 0 gets two pages from one program; another maps one page of it with 8 bytes.
    let wide = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(1), 8),
        &[(1, 2 * PAGE)],
    )
    .unwrap();
    let _ = wide;
    let reads_past = load(
        &mut claim,
        &mut stream,
        &copy(slot(0) + 100, slot(1), 8),
        &[(1, 8), (2, 8)],
    )
    .unwrap();
    stream.fill(1, &[9; 8]);
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, reads_past, 3)),
        (1, RunStatus::Done)
    );
    // Bytes 100..108 of slot 0 were past buffer 1's 8 bytes: zero, not what the memory held.
    assert_eq!(&stream.contents(2)[..8], &[0; 8]);
    assert_eq!(shared.lock().unwrap().seen, vec![vec![PAGE, PAGE]]);
}

#[test]
fn slot_memory_is_made_once_and_grown_only_when_needed() {
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    let a = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(1), 8),
        &[(1, 8), (2, 8)],
    )
    .unwrap();
    assert_eq!(shared.lock().unwrap().stages, 2);
    claim.attach(&mut stream, a, 0, 1, PAGE).unwrap();
    assert_eq!(shared.lock().unwrap().stages, 2);
    claim.attach(&mut stream, a, 0, 1, PAGE + 1).unwrap();
    assert_eq!(shared.lock().unwrap().stages, 3);
    assert_eq!(pages(PAGE + 1), Some(2 * PAGE));
    assert_eq!(pages(u32::MAX), None);
}

#[test]
fn a_failed_open_backs_off_doubling_to_eight_seconds_and_never_gives_up() {
    let shared = played();
    shared.lock().unwrap().open_fails = true;
    let mut service = Service::new(Node(Arc::clone(&shared)));
    let mut now = 0;
    for failures in 1..=8u32 {
        let retry = now + backoff_ns(failures);
        let Err(ClaimError::Device { retry_at_ns, .. }) = service.claim(now) else {
            panic!();
        };
        assert_eq!(retry_at_ns, retry);
        let opens = shared.lock().unwrap().opens;
        assert!(matches!(
            service.claim(retry - 1),
            Err(ClaimError::BackingOff { retry_at_ns }) if retry_at_ns == retry
        ));
        assert_eq!(shared.lock().unwrap().opens, opens);
        now = retry;
    }
    assert_eq!(backoff_ns(6), 8_000_000_000);
    assert_eq!(backoff_ns(100), 8_000_000_000);
    shared.lock().unwrap().open_fails = false;
    let claim = service.claim(now).unwrap();
    assert!(matches!(service.claim(now), Err(ClaimError::Claimed)));
    drop(claim);
    assert!(service.claim(now).is_ok());
}

#[test]
fn a_wedged_block_or_ended_stream_loses_the_claim() {
    for error in [
        io::Error::from_raw_os_error(5),
        io::Error::from_raw_os_error(19),
        io::Error::from(io::ErrorKind::BrokenPipe),
    ] {
        let shared = played();
        let mut claim = claimed(&shared);
        let mut stream = stream();
        let p = load(
            &mut claim,
            &mut stream,
            &copy(slot(0), slot(1), 8),
            &[(1, 8), (2, 8)],
        )
        .unwrap();
        shared.lock().unwrap().answer = Some(Err(error));
        assert_eq!(
            run_one(&mut claim, &mut stream, job(1, p, 3)),
            (1, RunStatus::Failed)
        );
        assert!(claim.lost());
    }
    // A refusal that leaves the stream open (a timeout of the wait, say) does not.
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    let p = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(1), 8),
        &[(1, 8), (2, 8)],
    )
    .unwrap();
    shared.lock().unwrap().answer = Some(Err(io::ErrorKind::TimedOut.into()));
    assert_eq!(
        run_one(&mut claim, &mut stream, job(1, p, 3)),
        (1, RunStatus::Failed)
    );
    assert!(!claim.lost());
}

#[test]
fn runs_in_a_row_allocate_nothing() {
    let shared = played();
    let mut claim = claimed(&shared);
    let mut stream = stream();
    let p = load(
        &mut claim,
        &mut stream,
        &copy(slot(0), slot(1), 64),
        &[(1, 64), (2, 64)],
    )
    .unwrap();
    shared.lock().unwrap().record = false;
    let mut count = 0;
    for n in 0..260u64 {
        post(&stream, job(n, p, 3));
        let (served, allocated) = allocations(|| claim.serve(&mut stream, n));
        assert_eq!(served, Ok(1));
        let frame = stream.take_frame().unwrap();
        stream.return_buffer(frame.buffer);
        // The first rounds warm the played stream's queues up.
        if n >= 10 {
            count += allocated;
        }
    }
    assert_eq!(count, 0, "allocations in 250 runs");
    assert_eq!(claim.counters().jobs, 260);
}

#[cfg(target_os = "linux")]
mod driver {
    use super::*;
    use crate::npu::{BoxedSession, Closed, Opener, POLL_NS, Route};
    use crate::{Vip9000Driver, Vip9000DriverNeeds};
    use lemnos_core::{CallerId, Driver, Health};

    fn opener(shared: &Shared) -> Opener {
        let mut node = Node(Arc::clone(shared));
        Box::new(move || node.open().map(|s| Box::new(s) as BoxedSession))
    }

    fn needs(stream: &MockStream) -> Vip9000DriverNeeds {
        Vip9000DriverNeeds {
            jobs: lemnos_core::Held::boxed(Box::new(stream.clone())),
        }
    }

    #[test]
    fn the_vendor_route_binds_to_refuse() {
        let shared = played();
        let stream = stream();
        let mut d =
            Vip9000Driver::with_opener(needs(&stream), Route::Vipcore, opener(&shared)).unwrap();
        assert_eq!(Driver::tick(&mut d, 0), None);
        assert!(matches!(Driver::health(&d), Health::Failed(_)));
        assert_eq!(d.closed(), Some(Closed::Vipcore));
        assert_eq!(d.load(0, 8), Err(Status::NOT_SUPPORTED));
        assert_eq!(d.job_stream(&mut [0; 32]), Err(Status::NOT_SUPPORTED));
        assert_eq!(d.info().unwrap().max_programs, 0);
        assert_eq!(shared.lock().unwrap().opens, 0);
    }

    #[test]
    fn the_module_route_runs_jobs_for_the_caller_that_loaded_them() {
        let shared = played();
        let stream = stream();
        let mut d =
            Vip9000Driver::with_opener(needs(&stream), Route::Lemnos, opener(&shared)).unwrap();
        assert_eq!(Driver::tick(&mut d, 0), Some(POLL_NS));
        assert!(matches!(Driver::health(&d), Health::Ok));
        let me = CallerId(1);
        Driver::caller(&mut d, me);
        stream.fill(0, &copy(slot(0), slot(1), 4));
        // The driver holds the buffers after its tick took them.
        let p = d.load(0, 24).unwrap();
        d.attach(p, 0, 1, 4).unwrap();
        d.attach(p, 1, 2, 4).unwrap();
        stream.fill(1, &[4, 3, 2, 1]);
        post(&stream, job(9, p, 3));
        Driver::tick(&mut d, POLL_NS);
        assert_eq!(completion(&stream), (9, RunStatus::Done));
        assert_eq!(&stream.contents(2)[..4], &[4, 3, 2, 1]);
        // Another caller can't touch it; when its caller goes, it is unloaded.
        Driver::caller(&mut d, CallerId(2));
        assert_eq!(d.unload(p), Err(Status::NOT_FOUND));
        assert_eq!(d.detach(p, 0), Err(Status::NOT_FOUND));
        Driver::caller_gone(&mut d, me);
        assert!(shared.lock().unwrap().programs.iter().all(Option::is_none));
        let mut name = [0u8; 32];
        let n = d.job_stream(&mut name).unwrap();
        assert_eq!(&name[..n], b"jobs");
    }

    #[test]
    fn a_node_that_fails_to_open_is_tried_again_on_the_backoff() {
        let shared = played();
        shared.lock().unwrap().open_fails = true;
        let stream = stream();
        let mut d =
            Vip9000Driver::with_opener(needs(&stream), Route::Lemnos, opener(&shared)).unwrap();
        assert_eq!(Driver::tick(&mut d, 0), Some(backoff_ns(1)));
        assert!(matches!(Driver::health(&d), Health::Degraded(_)));
        assert_eq!(Driver::tick(&mut d, 1), Some(backoff_ns(1)));
        assert_eq!(shared.lock().unwrap().opens, 1);
        shared.lock().unwrap().open_fails = false;
        assert_eq!(
            Driver::tick(&mut d, backoff_ns(1)),
            Some(backoff_ns(1) + POLL_NS)
        );
        assert!(d.claim().is_some());
        // A lost claim is dropped and taken again at the next tick.
        let p = {
            Driver::caller(&mut d, CallerId(1));
            stream.fill(0, &copy(slot(0), slot(1), 4));
            d.load(0, 24).unwrap()
        };
        d.attach(p, 0, 1, 4).unwrap();
        d.attach(p, 1, 2, 4).unwrap();
        shared.lock().unwrap().answer = Some(Err(io::Error::from_raw_os_error(5)));
        post(&stream, job(1, p, 3));
        Driver::tick(&mut d, 2 * backoff_ns(1));
        assert_eq!(completion(&stream), (1, RunStatus::Failed));
        assert_eq!(d.closed(), Some(Closed::Lost));
        Driver::tick(&mut d, 3 * backoff_ns(1));
        assert!(d.claim().is_some());
        assert_eq!(shared.lock().unwrap().opens, 3);
    }
}
