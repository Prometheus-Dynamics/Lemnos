//! The built-in `board-self-test` driver: serves `self-test/1` for a whole board by running the
//! self-tests of its member devices, one after another.
//!
//! It knows no kind of hardware. It asks each member for its `tests`, names them `member:test`,
//! starts a member's test as a long job and watches it, and reports each step's record and one
//! overall record. A member that is not running is `skipped` `ABSENT`; one that refuses because
//! another caller uses it is `skipped` `IN_USE`.
//!
//! Board-file settings (`config`): `members`, a list of device ids or the string `"all"` (every
//! device whose driver serves `self-test/1`), and `include_intrusive` (bool, default false), which
//! lets `all` run the members' intrusive tests too. At most 16 members.
//!
//! The driver is written by hand rather than with `#[driver]`, because its members are a list the
//! board names at bind, and `#[driver]` only knows fixed needs. Its contract dispatch is the one
//! `#[contract]` generates.

use std::string::String;
use std::vec::Vec;

use lemnos_contract_self_test::{
    Measure, Outcome, RECORD_BYTES, Record, SelfTest, read_last, read_tests, reason,
    write_last_entry, write_test,
};
use lemnos_core::{
    Config, Contract, Description, Dev, Driver, Encode, Item, JobId, JobState, Manifest, MethodId,
    Polled, Reader, Reply, Resources, Result, Status, Writer,
};

/// Most members a board's self-test serves.
pub const MAX_MEMBERS: usize = 16;
/// Most steps one run takes: every member's tests, at their most.
pub const MAX_STEPS: usize = MAX_MEMBERS * lemnos_contract_self_test::MAX_TESTS;
/// How often a running step is looked at, in nanoseconds.
const POLL_NS: u64 = 50_000_000;
/// The method `run` in this driver's `self-test/1`: the contract is its only one, so contract 0.
const RUN: MethodId = MethodId {
    contract: 0,
    method: 2,
};

/// One member device: its id on the board, and a route to its `self-test/1`.
struct Member {
    id: String,
    dev: Dev<dyn SelfTest>,
}

/// One step of a run: a member's test, or a member that is not running and has none to run.
struct Step {
    member: usize,
    /// The test's name on the member; empty for a member that is not running.
    test: String,
    absent: bool,
}

/// The tally a run keeps: outcome counts (pass, fail, skipped, not-supported), the reason of the
/// first step that did not pass, and the record of a run of one test.
#[derive(Default)]
struct Tally {
    counts: [i64; 4],
    first_reason: Option<u16>,
    single: Option<Record>,
}

// A record is fixed size and copied by value on purpose: boxing it would need an allocator.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy)]
enum Answer {
    Done(Record),
    Cancelled,
}

/// A run in progress.
struct Run {
    job: JobId,
    steps: Vec<Step>,
    /// `all`, which reports every step. Otherwise one test, whose record is the result.
    all: bool,
    confirmed: bool,
    next: usize,
    /// The member job of the step in progress, as the host numbers it.
    member_job: Option<JobId>,
    tally: Tally,
    answer: Option<Answer>,
}

#[derive(Default)]
pub struct BoardSelfTest {
    members: Vec<Member>,
    include_intrusive: bool,
    run: Option<Run>,
    /// The record of the last `all` run, which `last` reports under `all`.
    all_last: Option<Record>,
    next_job: u64,
}

impl BoardSelfTest {
    /// The member's tests as `(name, intrusive, expect_ms)`, or why they could not be read.
    fn member_tests(member: &mut Member) -> core::result::Result<Vec<(String, bool, u32)>, Status> {
        let mut buffer = [0u8; 2048];
        let used = member.dev.tests(&mut buffer)?;
        let tests = read_tests(&buffer[..used])?
            .map(|t| (String::from(t.name.as_str()), t.intrusive, t.expect_ms))
            .collect();
        Ok(tests)
    }

    /// Starts a run of `name`: `all`, or `member:test`.
    fn start(&mut self, name: &str, confirmed: bool) -> Result<JobId> {
        if self.run.is_some() {
            return Err(Status::BUSY);
        }
        let mut steps = Vec::new();
        let all = name == "all";
        if all {
            for (index, member) in self.members.iter_mut().enumerate() {
                match Self::member_tests(member) {
                    Ok(tests) => {
                        for (test, intrusive, _) in tests {
                            if intrusive && !self.include_intrusive {
                                continue;
                            }
                            steps.push(Step {
                                member: index,
                                test,
                                absent: false,
                            });
                        }
                    }
                    Err(Status::NO_DEVICE) => steps.push(Step {
                        member: index,
                        test: String::new(),
                        absent: true,
                    }),
                    Err(status) => return Err(status),
                }
            }
        } else {
            let (id, test) = name.split_once(':').ok_or(Status::INVALID_ARGUMENT)?;
            let index = self
                .members
                .iter()
                .position(|member| member.id == id)
                .ok_or(Status::INVALID_ARGUMENT)?;
            let tests = Self::member_tests(&mut self.members[index])?;
            if !tests.iter().any(|(listed, _, _)| listed == test) {
                return Err(Status::INVALID_ARGUMENT);
            }
            steps.push(Step {
                member: index,
                test: String::from(test),
                absent: false,
            });
        }
        if steps.len() > MAX_STEPS {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.next_job = self.next_job.wrapping_add(1).max(1);
        let job = JobId(self.next_job);
        self.run = Some(Run {
            job,
            steps,
            all,
            confirmed,
            next: 0,
            member_job: None,
            tally: Tally::default(),
            answer: None,
        });
        Ok(job)
    }

    /// Moves the run on as far as the members let it: looks at the step in progress, and starts
    /// the next when there is none. Sets the answer when the last step is done.
    fn advance(&mut self) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        if run.answer.is_some() {
            return;
        }
        if let Some(job) = run.member_job {
            let member = &self.members[run.steps[run.next].member];
            let mut buffer = [0u8; RECORD_BYTES + 8];
            match member.dev.caller.poll(job, &mut buffer) {
                Ok(Polled::Running { .. }) => return,
                Ok(Polled::Finished { status, len }) => {
                    let record = status
                        .is_ok()
                        .then(|| Record::read(&mut Reader::new(&buffer[..len])).ok())
                        .flatten();
                    match record {
                        Some(record) => {
                            tally(&mut run.tally, record, run.steps.len() == 1 && !run.all)
                        }
                        None => tally_skip_or_fail(&mut run.tally, status),
                    }
                }
                // The member forgot the job: it was lost, which is a skip, not a failure.
                Err(_) => tally_skip(&mut run.tally, reason::ABSENT),
            }
            run.member_job = None;
            run.next += 1;
        }
        while run.member_job.is_none() && run.next < run.steps.len() {
            let step = &run.steps[run.next];
            if step.absent {
                tally_skip(&mut run.tally, reason::ABSENT);
                run.next += 1;
                continue;
            }
            let member = &mut self.members[step.member];
            match member.dev.run(&step.test, run.confirmed) {
                Ok(job) => run.member_job = Some(job),
                Err(status) => {
                    match status {
                        Status::BUSY => tally_skip(&mut run.tally, reason::IN_USE),
                        Status::NO_DEVICE | Status::NOT_FOUND => {
                            tally_skip(&mut run.tally, reason::ABSENT)
                        }
                        Status::INVALID_ARGUMENT => tally_outcome(
                            &mut run.tally,
                            Outcome::NotSupported,
                            reason::UNKNOWN_TEST,
                        ),
                        _ => tally_outcome(&mut run.tally, Outcome::Fail, reason::NO_RESPONSE),
                    }
                    run.next += 1;
                }
            }
        }
        if run.member_job.is_none() && run.next >= run.steps.len() {
            let record = if run.all {
                aggregate(&run.tally, run.steps.len())
            } else {
                match run.tally.single {
                    Some(record) => record,
                    None => aggregate(&run.tally, run.steps.len()),
                }
            };
            if run.all {
                self.all_last = Some(record);
            }
            run.answer = Some(Answer::Done(record));
        }
    }

    /// The `tests` list: every member's tests under `member:test`, then `all`.
    fn write_tests(&mut self, into: &mut [u8]) -> Result<usize> {
        let mut writer = Writer::new(into);
        let mut scratch = [0u8; 2048];
        let mut count = 1usize;
        for member in &mut self.members {
            if let Ok(used) = member.dev.tests(&mut scratch) {
                count += read_tests(&scratch[..used]).map_or(0, Iterator::count);
            }
        }
        (count as u8).encode(&mut writer)?;
        let mut total_expect = 0u32;
        for member in &mut self.members {
            let Ok(used) = member.dev.tests(&mut scratch) else {
                continue;
            };
            for test in read_tests(&scratch[..used])? {
                let name = format!("{}:{}", member.id, test.name.as_str());
                write_test(
                    &mut writer,
                    &name,
                    test.description.as_str(),
                    test.intrusive,
                    test.operator,
                    test.fixture.as_str(),
                    test.expect_ms,
                )?;
                if !test.intrusive || self.include_intrusive {
                    total_expect = total_expect.saturating_add(test.expect_ms);
                }
            }
        }
        write_test(
            &mut writer,
            "all",
            "every member's test that is not intrusive; intrusive ones too with include_intrusive",
            self.include_intrusive,
            false,
            "",
            total_expect,
        )?;
        Ok(writer.len())
    }

    /// The `last` list: every member's last results under `member:test`, then the board's `all`.
    fn write_last(&mut self, into: &mut [u8]) -> Result<usize> {
        let mut scratch = [0u8; 4096];
        let mut count = usize::from(self.all_last.is_some());
        for member in &mut self.members {
            if let Ok(used) = member.dev.last(&mut scratch) {
                count += read_last(&scratch[..used]).map_or(0, Iterator::count);
            }
        }
        let mut writer = Writer::new(into);
        (count as u8).encode(&mut writer)?;
        for member in &mut self.members {
            let Ok(used) = member.dev.last(&mut scratch) else {
                continue;
            };
            for entry in read_last(&scratch[..used])? {
                let name = format!("{}:{}", member.id, entry.name.as_str());
                write_last_entry(&mut writer, &name, &entry.record)?;
            }
        }
        if let Some(record) = self.all_last {
            write_last_entry(&mut writer, "all", &record)?;
        }
        Ok(writer.len())
    }
}

/// Counts a step's record under its outcome, and keeps it when the run is of one test.
fn tally(tally: &mut Tally, record: Record, single: bool) {
    if single {
        tally.single = Some(record);
    }
    tally_outcome(tally, record.outcome, record.reason);
}

fn tally_skip(tally: &mut Tally, why: u16) {
    tally_outcome(tally, Outcome::Skipped, why);
}

/// A step that ended without a record: a skip when the member lost it, a failure otherwise.
fn tally_skip_or_fail(tally: &mut Tally, status: Status) {
    match status {
        Status::INTERRUPTED | Status::NO_DEVICE | Status::NOT_FOUND => {
            tally_skip(tally, reason::ABSENT)
        }
        _ => tally_outcome(tally, Outcome::Fail, reason::NO_RESPONSE),
    }
}

fn tally_outcome(tally: &mut Tally, outcome: Outcome, why: u16) {
    let slot = match outcome {
        Outcome::Pass => 0,
        Outcome::Fail => 1,
        Outcome::Skipped => 2,
        Outcome::NotSupported => 3,
    };
    tally.counts[slot] += 1;
    if outcome != Outcome::Pass && tally.first_reason.is_none() {
        tally.first_reason = Some(why);
    }
}

/// The overall record of a run: fail if any step failed, else pass if any passed, else skipped
/// if any was skipped, else not-supported. The reason is the first step that did not pass.
fn aggregate(tally: &Tally, steps: usize) -> Record {
    if steps == 0 {
        return Record::not_supported(reason::UNKNOWN_TEST).said("no tests to run");
    }
    let [pass, fail, skipped, unsupported] = tally.counts;
    let outcome = if fail > 0 {
        Outcome::Fail
    } else if pass > 0 {
        Outcome::Pass
    } else if skipped > 0 {
        Outcome::Skipped
    } else {
        Outcome::NotSupported
    };
    let mut record = Record::new(outcome, tally.first_reason.unwrap_or(reason::OK))
        .said("the steps ran; the measures count each outcome");
    for (name, value) in [
        ("pass", pass),
        ("fail", fail),
        ("skipped", skipped),
        ("not_supported", unsupported),
    ] {
        record = record.measure(Measure::new(name, value, 0, "steps"));
    }
    record
}

impl Driver for BoardSelfTest {
    const MANIFEST: Manifest = Manifest {
        name: "board-self-test",
        needs: &[],
        provides: &[<dyn SelfTest as Contract>::INFO],
    };

    fn bind(res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Self> {
        let include_intrusive = cfg.bool("include_intrusive").unwrap_or(false);
        let mut ids: Vec<String> = Vec::new();
        if let Some(listed) = cfg.list("members") {
            ids.extend(listed.iter().map(|id| String::from(*id)));
        } else if cfg.str("members") == Some("all") {
            res.members::<dyn SelfTest>(&mut |id| ids.push(String::from(id)))?;
        } else {
            return Err(Status::INVALID_ARGUMENT);
        }
        if ids.len() > MAX_MEMBERS {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut members = Vec::with_capacity(ids.len());
        for id in ids {
            let dev = res.member::<dyn SelfTest>(&id)?;
            members.push(Member { id, dev });
        }
        Ok(Self {
            members,
            include_intrusive,
            run: None,
            all_last: None,
            next_job: 0,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("self_test", RUN));
    }

    const TICKS: bool = true;

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.advance();
        match &self.run {
            Some(run) if run.answer.is_none() => {
                let _ = now_ns;
                Some(now_ns + POLL_NS)
            }
            _ => None,
        }
    }

    const JOBS: bool = true;

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        let Some(run) = self.run.as_ref().filter(|run| run.job == job) else {
            return JobState::Finished(Status::NOT_FOUND);
        };
        let (done, total, answer) = (run.next as u64, run.steps.len() as u64, run.answer);
        match answer {
            None => JobState::Running { done, total },
            Some(Answer::Done(record)) => {
                let state = match record.write(result) {
                    Ok(()) => JobState::Finished(Status::OK),
                    Err(status) => JobState::Finished(status),
                };
                self.run = None;
                state
            }
            Some(Answer::Cancelled) => {
                self.run = None;
                JobState::Finished(Status::CANCELLED)
            }
        }
    }

    fn job_cancel(&mut self, job: JobId) {
        let Some(run) = self.run.as_mut().filter(|run| run.job == job) else {
            return;
        };
        if run.answer.is_some() {
            return;
        }
        if let Some(member_job) = run.member_job.take() {
            let member = &self.members[run.steps[run.next].member];
            let _ = member.dev.caller.cancel(member_job);
        }
        run.answer = Some(Answer::Cancelled);
    }

    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status {
        match method.contract {
            0 => <Self as SelfTest>::__serve(self, method.method, args, reply),
            _ => Status::NOT_SUPPORTED,
        }
    }
}

impl SelfTest for BoardSelfTest {
    fn tests(&mut self, into: &mut [u8]) -> Result<usize> {
        self.write_tests(into)
    }

    fn run(&mut self, name: &str, confirmed: bool) -> Result<JobId> {
        self.start(name, confirmed)
    }

    fn last(&mut self, into: &mut [u8]) -> Result<usize> {
        self.write_last(into)
    }
}
