//! The `lamp` self-test on a fake `pixels/1` device: an operator confirms, the state is shown and
//! put back, and nothing else may change the LEDs while it runs.

mod support;

use lemnos_contract_indicator::{Indicator as _, NO_PROGRESS};
use lemnos_contract_self_test::{Outcome, Record, SelfTest, reason};
use lemnos_core::{
    CallerId, ConfigValue, Driver, JobId, JobState, Lifecycle, Reader, Status, Writer,
};
use lemnos_driver_indicator::Indicator;
use support::{FakePixels, driver, settle, tick};

const GREEN: [u8; 4] = [0, 255, 0, 0];

/// A strip of two LEDs whose `done` state is solid green, so the state it puts back is visible.
fn green_done(fake: &'static FakePixels) -> Indicator {
    driver(fake, &[("state.done", ConfigValue::Str("solid #00ff00"))])
}

/// Ticks `indicator` at `now_ms`, which is when the test's steps run.
fn at(indicator: &mut Indicator, now_ms: u64) -> Option<u64> {
    tick(indicator, now_ms)
}

/// The answer of `job` as the driver gives it, and the record it carries.
fn answer(indicator: &mut Indicator, job: JobId) -> (JobState, Option<Record>) {
    let mut buffer = [0u8; lemnos_contract_self_test::RECORD_BYTES + 8];
    let mut writer = Writer::new(&mut buffer);
    let state = Lifecycle::job(indicator, job, &mut writer);
    let used = writer.len();
    let record = match state {
        JobState::Finished(Status::OK) => {
            Some(Record::read(&mut Reader::new(&buffer[..used])).unwrap())
        }
        _ => None,
    };
    (state, record)
}

#[test]
fn the_lamp_test_is_listed_and_needs_an_operator() {
    let fake = FakePixels::leak(2);
    let mut indicator = green_done(fake);
    let mut listed = [0u8; 512];
    let used = SelfTest::tests(&mut indicator, &mut listed).unwrap();
    assert!(used > 1, "one test is listed");
    let mut indicator = green_done(fake);
    let job = SelfTest::run(&mut indicator, "lamp", false).unwrap();
    at(&mut indicator, 0);
    let (state, record) = answer(&mut indicator, job);
    assert_eq!(state, JobState::Finished(Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Skipped);
    assert_eq!(record.reason, reason::NEEDS_OPERATOR);
}

#[test]
fn an_unconfirmed_lamp_test_touches_no_led() {
    let fake = FakePixels::leak(2);
    let mut indicator = green_done(fake);
    indicator.set("done", NO_PROGRESS).unwrap();
    settle(&mut indicator, 0, 1_000);
    let shows = fake.shows();
    let job = SelfTest::run(&mut indicator, "lamp", false).unwrap();
    at(&mut indicator, 0);
    answer(&mut indicator, job);
    assert_eq!(fake.shows(), shows, "nothing was drawn");
    assert_eq!(fake.last(), vec![GREEN; 2]);
}

#[test]
fn a_confirmed_lamp_shows_the_state_for_two_seconds_then_puts_back_the_one_it_found() {
    let fake = FakePixels::leak(2);
    let mut indicator = green_done(fake);
    indicator.set("done", NO_PROGRESS).unwrap();
    settle(&mut indicator, 0, 1_000);
    let job = SelfTest::run(&mut indicator, "lamp", true).unwrap();
    let next = at(&mut indicator, 0).expect("the test asks to be ticked again");
    assert!(
        next <= 2_000,
        "the hold ends within two seconds, got {next} ms"
    );
    // The hold lasts two seconds: at one second it is still showing the receiving state.
    at(&mut indicator, 1_000);
    assert_ne!(
        fake.last(),
        vec![GREEN; 2],
        "the receiving state is on the LEDs"
    );
    at(&mut indicator, 2_000);
    let (state, record) = answer(&mut indicator, job);
    assert_eq!(state, JobState::Finished(Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Pass);
    assert_eq!(record.reason, reason::OPERATOR_CONFIRMED);
    settle(&mut indicator, 2_000, 3_000);
    assert_eq!(fake.last(), vec![GREEN; 2], "the done state is back");
}

#[test]
fn a_state_set_while_the_lamp_test_runs_is_refused() {
    let fake = FakePixels::leak(2);
    let mut indicator = green_done(fake);
    let job = SelfTest::run(&mut indicator, "lamp", true).unwrap();
    at(&mut indicator, 0);
    assert_eq!(indicator.set("failed", NO_PROGRESS), Err(Status::BUSY));
    at(&mut indicator, 2_000);
    answer(&mut indicator, job);
    assert!(indicator.set("failed", NO_PROGRESS).is_ok());
}

#[test]
fn cancelling_the_lamp_test_puts_the_state_back_at_once() {
    let fake = FakePixels::leak(2);
    let mut indicator = green_done(fake);
    indicator.set("done", NO_PROGRESS).unwrap();
    settle(&mut indicator, 0, 1_000);
    let job = SelfTest::run(&mut indicator, "lamp", true).unwrap();
    at(&mut indicator, 0);
    Lifecycle::job_cancel(&mut indicator, job);
    settle(&mut indicator, 0, 1_000);
    assert_eq!(fake.last(), vec![GREEN; 2], "back on the done state");
    assert_eq!(
        answer(&mut indicator, job).0,
        JobState::Finished(Status::CANCELLED)
    );
}

#[test]
fn a_lamp_test_is_refused_while_another_caller_uses_the_indicator() {
    let fake = FakePixels::leak(2);
    let mut indicator = green_done(fake);
    Driver::caller(&mut indicator, CallerId(5));
    Driver::caller(&mut indicator, CallerId(7));
    assert_eq!(
        SelfTest::run(&mut indicator, "lamp", true),
        Err(Status::BUSY)
    );
}
