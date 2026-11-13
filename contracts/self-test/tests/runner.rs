//! The runner's rules, against a scripted bench: a run ends with its restore done, a cancel
//! puts back at once, an intrusive run is refused while another caller uses the device, and an
//! operator's fixture that is not confirmed skips without touching the device.

use std::collections::VecDeque;

use lemnos_contract_self_test::{
    Bench, Flow, MAX_TESTS, Measure, Outcome, RECORD_BYTES, RESTORE_CAP_NS, RESTORE_FIRST_NS,
    Record, Runner, Scratch, Spec, reason,
};
use lemnos_core::{CallerId, JobId, JobState, Reader, Status, Writer};

const SECOND: u64 = 1_000_000_000;

const SPECS: [Spec; 4] = [
    Spec::read("identity", "the chip ID", 20),
    Spec::intrusive("spin", "the fan spins", "the fan free to spin", 3000),
    Spec {
        name: "lamp",
        description: "the lamp shows",
        intrusive: true,
        operator: true,
        fixture: "an operator watching",
        expect_ms: 2000,
    },
    Spec::read("bus", "the bus voltage", 20),
];

/// A bench whose steps come from a script and whose restore can be made to fail.
struct Scripted {
    steps: VecDeque<Flow>,
    restore_failures: u32,
    steps_taken: u32,
    restores: u32,
    /// What the restore saw in scratch, for checking it restores what was saved.
    saw: Vec<Scratch>,
}

impl Scripted {
    fn new(steps: Vec<Flow>) -> Self {
        Self {
            steps: steps.into(),
            restore_failures: 0,
            steps_taken: 0,
            restores: 0,
            saw: Vec::new(),
        }
    }
}

impl Bench for Scripted {
    fn step(&mut self, _test: usize, _now_ns: u64, scratch: &mut Scratch) -> Flow {
        self.steps_taken += 1;
        scratch.step += 1;
        scratch.saved[0] = 42;
        self.steps
            .pop_front()
            .unwrap_or(Flow::Done(Record::pass(reason::OK)))
    }

    fn restore(&mut self, scratch: &mut Scratch) -> Result<(), Status> {
        self.restores += 1;
        self.saw.push(*scratch);
        if self.restore_failures > 0 {
            self.restore_failures -= 1;
            return Err(Status::IO);
        }
        Ok(())
    }
}

const ASKER: CallerId = CallerId(7);
const OTHER: CallerId = CallerId(9);

/// The answer of a job, as the driver's `job` gives it: the status, and the record it carries.
fn answer(runner: &mut Runner, job: JobId) -> (JobState, Option<Record>) {
    let mut buffer = [0u8; RECORD_BYTES + 8];
    let mut writer = Writer::new(&mut buffer);
    let state = runner.job(job, &mut writer);
    let used = writer.len();
    let record = match state {
        JobState::Finished(Status::OK) => {
            Some(Record::read(&mut Reader::new(&buffer[..used])).expect("a record"))
        }
        _ => None,
    };
    (state, record)
}

#[test]
fn a_passing_test_answers_with_its_record_and_is_kept_as_last() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Done(
        Record::pass(reason::OK).measure(Measure::new("id", 0x32, 0, "")),
    )]);
    let job = runner.start("identity", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    assert_eq!(bench.restores, 1, "the test put back what it changed");
    let (state, record) = answer(&mut runner, job);
    assert_eq!(state, JobState::Finished(Status::OK));
    let record = record.expect("the record is the result");
    assert_eq!(record.outcome, Outcome::Pass);
    assert_eq!(runner.last("identity"), Some(record));
    assert!(!runner.busy());
    let (again, _) = answer(&mut runner, job);
    assert_eq!(
        again,
        JobState::Finished(Status::NOT_FOUND),
        "answered once"
    );
}

#[test]
fn a_step_that_waits_is_called_again_when_due() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Wait(3 * SECOND), Flow::Done(Record::pass(0))]);
    let job = runner.start("spin", false, ASKER).unwrap();
    assert_eq!(runner.tick(SECOND, &mut bench), Some(3 * SECOND));
    assert_eq!(
        bench.restores, 0,
        "nothing is put back while the test is still running"
    );
    let (state, _) = answer(&mut runner, job);
    assert_eq!(
        state,
        JobState::Running {
            done: 0,
            total: 3000
        },
        "progress is milliseconds of the bound"
    );
    assert_eq!(runner.tick(4 * SECOND, &mut bench), None);
    assert_eq!(bench.restores, 1);
}

#[test]
fn progress_reports_elapsed_milliseconds_of_the_bound() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Wait(10 * SECOND), Flow::Wait(10 * SECOND)]);
    let job = runner.start("spin", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    runner.tick(SECOND, &mut bench);
    let (state, _) = answer(&mut runner, job);
    assert_eq!(
        state,
        JobState::Running {
            done: 1000,
            total: 3000
        }
    );
}

#[test]
fn a_tick_before_a_step_is_due_takes_no_step() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Wait(3 * SECOND), Flow::Done(Record::pass(0))]);
    let job = runner.start("spin", false, ASKER).unwrap();
    assert_eq!(runner.tick(0, &mut bench), Some(3 * SECOND));
    assert_eq!(
        runner.tick(SECOND, &mut bench),
        Some(3 * SECOND),
        "asked again at the due time"
    );
    assert_eq!(bench.steps_taken, 1, "the early tick took no step");
    assert_eq!(bench.restores, 0);
    assert_eq!(runner.tick(3 * SECOND, &mut bench), None);
    assert_eq!(bench.steps_taken, 2);
    assert_eq!(answer(&mut runner, job).0, JobState::Finished(Status::OK));
}

#[test]
fn a_cancel_puts_the_device_back_at_once_and_leaves_last_as_it_was() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Wait(10 * SECOND)]);
    let job = runner.start("spin", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    runner.cancel(job, &mut bench);
    assert_eq!(
        bench.restores, 1,
        "restored by the cancel, not on a later tick"
    );
    assert_eq!(bench.saw[0].step, 1, "it restores with what the test saved");
    let (state, record) = answer(&mut runner, job);
    assert_eq!(state, JobState::Finished(Status::CANCELLED));
    assert!(record.is_none());
    assert_eq!(
        runner.last("spin"),
        None,
        "a cancelled test leaves last as it was"
    );
    assert!(!runner.busy());
}

#[test]
fn a_cancel_after_the_answer_keeps_the_answer() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Done(Record::pass(0))]);
    let job = runner.start("identity", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    runner.cancel(job, &mut bench);
    let (state, record) = answer(&mut runner, job);
    assert_eq!(
        state,
        JobState::Finished(Status::OK),
        "it really ended first"
    );
    assert!(record.is_some());
}

#[test]
fn a_failed_put_back_is_retried_on_the_ticks_and_turns_a_pass_into_a_fail() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Done(Record::pass(reason::OK))]);
    bench.restore_failures = 1;
    let job = runner.start("identity", false, ASKER).unwrap();
    assert_eq!(runner.tick(0, &mut bench), Some(RESTORE_FIRST_NS));
    let (state, _) = answer(&mut runner, job);
    assert_eq!(
        state,
        JobState::Running { done: 0, total: 20 },
        "the answer waits for the put-back"
    );
    assert!(
        runner.health().is_none(),
        "a test-owed put-back is the job's, not the device's"
    );
    assert_eq!(runner.tick(RESTORE_FIRST_NS, &mut bench), None);
    assert_eq!(bench.restores, 2);
    let (state, record) = answer(&mut runner, job);
    assert_eq!(state, JobState::Finished(Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Fail);
    assert_eq!(record.reason, reason::RESTORE_FAILED);
}

#[test]
fn a_failed_put_back_keeps_a_test_failure_and_its_reason() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Done(Record::fail(reason::IDENTITY))]);
    bench.restore_failures = 1;
    let job = runner.start("identity", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    runner.tick(RESTORE_FIRST_NS, &mut bench);
    let (_, record) = answer(&mut runner, job);
    let record = record.unwrap();
    assert_eq!(
        record.reason,
        reason::IDENTITY,
        "the test's own reason stands"
    );
}

#[test]
fn a_put_back_that_keeps_failing_is_retried_with_backoff_capped_at_eight_seconds() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Wait(SECOND), Flow::Done(Record::pass(0))]);
    let job = runner.start("spin", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    bench.restore_failures = u32::MAX;
    runner.cancel(job, &mut bench);
    assert_eq!(
        runner.health(),
        Some("a self-test could not put the device back: retried")
    );
    let mut now = 2 * SECOND;
    let mut gaps = Vec::new();
    for _ in 0..8 {
        let due = runner.tick(now, &mut bench).expect("retry is scheduled");
        gaps.push(due - now);
        now = due;
    }
    assert_eq!(
        gaps[0], 200_000_000,
        "the first retry doubles the first wait"
    );
    assert!(
        gaps.iter().all(|gap| *gap <= RESTORE_CAP_NS),
        "never beyond the cap"
    );
    assert_eq!(*gaps.last().unwrap(), RESTORE_CAP_NS, "and it reaches it");
    bench.restore_failures = 0;
    assert_eq!(runner.tick(now + RESTORE_CAP_NS, &mut bench), None);
    assert!(
        runner.health().is_none(),
        "the device is back, so the health clears"
    );
    let (state, _) = answer(&mut runner, job);
    assert_eq!(state, JobState::Finished(Status::CANCELLED));
    assert!(!runner.busy());
}

#[test]
fn an_operator_fixture_not_confirmed_is_skipped_without_touching_the_device() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![]);
    let job = runner.start("lamp", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    assert_eq!(bench.steps_taken, 0, "no step ran");
    assert_eq!(bench.restores, 0);
    let (state, record) = answer(&mut runner, job);
    assert_eq!(state, JobState::Finished(Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Skipped);
    assert_eq!(record.reason, reason::NEEDS_OPERATOR);
    assert_eq!(runner.last("lamp"), Some(record));
}

#[test]
fn an_unconfirmed_operator_test_is_skipped_even_while_another_caller_uses_the_device() {
    let mut runner = Runner::new(&SPECS);
    runner.caller(OTHER);
    let job = runner
        .start("lamp", false, ASKER)
        .expect("skipped, not refused");
    let (_, record) = answer(&mut runner, job);
    assert_eq!(record.unwrap().reason, reason::NEEDS_OPERATOR);
    assert_eq!(runner.start("lamp", true, ASKER), Err(Status::BUSY));
}

#[test]
fn a_confirmed_operator_fixture_runs() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Done(Record::pass(reason::OPERATOR_CONFIRMED))]);
    let job = runner.start("lamp", true, ASKER).unwrap();
    runner.tick(0, &mut bench);
    let (_, record) = answer(&mut runner, job);
    assert_eq!(record.unwrap().reason, reason::OPERATOR_CONFIRMED);
    assert_eq!(bench.steps_taken, 1);
}

#[test]
fn a_test_is_refused_while_another_caller_uses_an_intrusive_test_but_not_a_read() {
    let mut runner = Runner::new(&SPECS);
    runner.caller(OTHER);
    assert_eq!(runner.start("spin", false, ASKER), Err(Status::BUSY));
    assert!(
        runner.start("identity", false, ASKER).is_ok(),
        "a read is never refused for use"
    );
}

#[test]
fn the_caller_asking_is_not_another_user() {
    let mut runner = Runner::new(&SPECS);
    runner.caller(ASKER);
    assert!(runner.start("spin", false, ASKER).is_ok());
}

#[test]
fn a_caller_that_goes_no_longer_counts() {
    let mut runner = Runner::new(&SPECS);
    runner.caller(OTHER);
    runner.caller_gone(OTHER);
    assert!(runner.start("spin", false, ASKER).is_ok());
}

#[test]
fn more_callers_than_the_set_holds_count_as_using_the_device() {
    let mut runner = Runner::new(&SPECS);
    for id in 100..(100 + lemnos_contract_self_test::MAX_USERS as u64) {
        runner.caller(CallerId(id));
    }
    runner.caller(CallerId(999));
    assert_eq!(runner.start("spin", false, ASKER), Err(Status::BUSY));
}

#[test]
fn one_test_runs_at_a_time_and_an_unknown_name_is_refused() {
    let mut runner = Runner::new(&SPECS);
    runner.start("identity", false, ASKER).unwrap();
    assert_eq!(runner.start("bus", false, ASKER), Err(Status::BUSY));
    let mut other = Runner::new(&SPECS);
    assert_eq!(
        other.start("nope", false, ASKER),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        other.start("all", false, ASKER),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_busy_put_back_refuses_a_new_test() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Wait(SECOND)]);
    let job = runner.start("spin", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    bench.restore_failures = 1;
    runner.cancel(job, &mut bench);
    assert_eq!(runner.start("identity", false, ASKER), Err(Status::BUSY));
}

#[test]
fn a_pending_put_back_refuses_a_new_test_after_its_answer_is_taken() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Wait(SECOND)]);
    let job = runner.start("spin", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    bench.restore_failures = 1;
    runner.cancel(job, &mut bench);
    let (state, _) = answer(&mut runner, job);
    assert_eq!(state, JobState::Finished(Status::CANCELLED));
    assert_eq!(
        runner.start("identity", false, ASKER),
        Err(Status::BUSY),
        "the put-back is still owed"
    );
    runner.tick(RESTORE_FIRST_NS, &mut bench);
    assert!(runner.start("identity", false, ASKER).is_ok());
}

#[test]
fn a_put_back_that_fails_after_a_finished_test_backs_off_capped_at_eight_seconds() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![Flow::Done(Record::pass(reason::OK))]);
    bench.restore_failures = 64;
    runner.start("identity", false, ASKER).unwrap();
    let mut now = 0;
    let mut next = runner.tick(now, &mut bench).expect("a retry is due");
    let mut longest = 0;
    for _ in 0..40 {
        let gap = next - now;
        assert!(gap <= RESTORE_CAP_NS, "a retry {gap} ns after the last");
        longest = longest.max(gap);
        now = next;
        next = runner.tick(now, &mut bench).expect("a retry is due");
    }
    assert_eq!(longest, RESTORE_CAP_NS, "the backoff reaches its cap");
}

#[test]
fn last_lists_each_test_that_has_a_result_in_declaration_order() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![
        Flow::Done(Record::pass(0)),
        Flow::Done(Record::fail(8)),
    ]);
    let job = runner.start("bus", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    answer(&mut runner, job);
    let job = runner.start("identity", false, ASKER).unwrap();
    runner.tick(0, &mut bench);
    answer(&mut runner, job);
    let mut buffer = [0u8; 4096];
    let used = runner.write_last(&mut buffer).unwrap();
    let names: Vec<_> = lemnos_contract_self_test::read_last(&buffer[..used])
        .unwrap()
        .map(|entry| entry.name.as_str().to_string())
        .collect();
    assert_eq!(names, ["identity", "bus"]);
    assert!(MAX_TESTS >= SPECS.len());
}

#[test]
fn nothing_running_costs_nothing() {
    let mut runner = Runner::new(&SPECS);
    let mut bench = Scripted::new(vec![]);
    assert_eq!(runner.next(), None);
    assert_eq!(runner.tick(SECOND, &mut bench), None);
    assert_eq!(bench.steps_taken + bench.restores, 0);
    assert!(!runner.busy());
}
