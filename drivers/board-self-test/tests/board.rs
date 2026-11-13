//! The board's self-test over a real host on the simulated board: members that serve
//! `self-test/1` are run in board order, their steps reported, and the board's own answer.

use std::sync::Arc;
use std::time::Duration;

use lemnos_contract_self_test::{Outcome, RECORD_BYTES, Record, read_last, read_tests, reason};
use lemnos_core::{CallerId, MethodId, Status};
use lemnos_driver_board_self_test::BoardSelfTest;
use lemnos_fake_devices::FakeProbe;
use lemnos_host::board::parse;
use lemnos_host::{DriverEntry, Host, JobEvent, JobUpdate, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

const CALLER: CallerId = CallerId(1);
const OTHER: CallerId = CallerId(5);

fn drivers() -> Vec<DriverEntry> {
    vec![entry::<FakeProbe>(), entry::<BoardSelfTest>()]
}

/// A board of members `a` and `b`, and the board's own device (`board`), with the board's config.
fn host(config: &str) -> (Host, SimClock) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"a\"\ndriver = \"fake-probe\"\n\
         [[devices]]\nid = \"b\"\ndriver = \"fake-probe\"\n\
         [[devices]]\nid = \"board\"\ndriver = \"board-self-test\"\nconfig = {{ {config} }}\n"
    );
    let board = parse(&text).unwrap();
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &board,
        &drivers(),
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

/// The capacity an out-buffer method takes first: the buffer's length, as a u32.
fn capacity(len: usize) -> Vec<u8> {
    (len as u32).to_le_bytes().to_vec()
}

fn index(host: &Host, id: &str) -> u32 {
    host.device(id).unwrap().index
}

const MEMBERS: &str = "members = [\"a\", \"b\"]";

/// The board's test names, in the order its `tests` lists them.
fn listed(host: &Host) -> Vec<(String, bool)> {
    let mut buffer = [0u8; 4096];
    let used = host
        .call(
            index(host, "board"),
            MethodId {
                contract: 0,
                method: 1,
            },
            &capacity(4096),
            &mut buffer,
        )
        .unwrap();
    read_tests(&buffer[..used])
        .unwrap()
        .map(|t| (t.name.as_str().to_string(), t.intrusive))
        .collect()
}

/// Starts a board run of `name` and returns its job number.
fn start(host: &Host, name: &str, confirmed: bool) -> Result<u64, Status> {
    let mut args = Vec::new();
    args.extend_from_slice(&(name.len() as u32).to_le_bytes());
    args.extend_from_slice(name.as_bytes());
    args.push(u8::from(confirmed));
    host.start_job(
        CALLER,
        index(host, "board"),
        MethodId {
            contract: 0,
            method: 2,
        },
        &args,
    )
}

/// Ticks the host until `job` ends, and returns its status and result.
fn finish(host: &Host, clock: &SimClock, job: u64) -> (Status, Vec<u8>) {
    for _ in 0..4_000 {
        host.tick();
        let mut updates: Vec<JobUpdate> = Vec::new();
        host.take_job_updates(&mut updates);
        for update in updates {
            if let JobEvent::Finished { status, result } = update.event
                && update.job == job
            {
                return (status, result);
            }
        }
        clock.advance(Duration::from_millis(10));
    }
    panic!("the board's run did not end");
}

/// The record a finished run carries.
fn record(result: &[u8]) -> Record {
    assert_eq!(result.len(), RECORD_BYTES, "the result is one record");
    Record::read(&mut lemnos_core::Reader::new(result)).unwrap()
}

/// The board's `last` entries, by name.
fn last(host: &Host) -> Vec<(String, Record)> {
    let mut buffer = [0u8; 61440];
    let used = host
        .call(
            index(host, "board"),
            MethodId {
                contract: 0,
                method: 3,
            },
            &capacity(61440),
            &mut buffer,
        )
        .unwrap();
    read_last(&buffer[..used])
        .unwrap()
        .map(|entry| (entry.name.as_str().to_string(), entry.record))
        .collect()
}

/// The measure of a run's record named `name`.
fn count(record: &Record, name: &str) -> i64 {
    record
        .measures()
        .find(|m| m.name.as_str() == name)
        .map(|m| m.value)
        .unwrap_or(-1)
}

#[test]
fn the_board_lists_every_members_tests_in_order_and_then_all() {
    let (host, _clock) = host(MEMBERS);
    let names: Vec<String> = listed(&host).into_iter().map(|(name, _)| name).collect();
    let mut expect = Vec::new();
    for member in ["a", "b"] {
        for test in ["ok", "bad", "slow", "spin", "lamp"] {
            expect.push(format!("{member}:{test}"));
        }
    }
    expect.push("all".to_string());
    assert_eq!(names, expect);
    let intrusive: Vec<bool> = listed(&host)
        .into_iter()
        .map(|(_, intrusive)| intrusive)
        .collect();
    assert!(intrusive[3], "a:spin is intrusive");
    assert!(
        !*intrusive.last().unwrap(),
        "all is not, unless include_intrusive"
    );
}

#[test]
fn all_runs_each_members_non_intrusive_tests_in_order_and_reports_the_counts() {
    let (host, clock) = host(MEMBERS);
    let job = start(&host, "all", false).unwrap();
    let (status, result) = finish(&host, &clock, job);
    assert_eq!(status, Status::OK);
    let overall = record(&result);
    assert_eq!(overall.outcome, Outcome::Fail, "one member test failed");
    assert_eq!(
        overall.reason,
        reason::OUT_OF_RANGE,
        "the first step that did not pass"
    );
    assert_eq!(count(&overall, "pass"), 4);
    assert_eq!(count(&overall, "fail"), 2);
    assert_eq!(count(&overall, "skipped"), 0, "intrusive tests are not run");
    let names: Vec<String> = last(&host).into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        ["a:ok", "a:bad", "a:slow", "b:ok", "b:bad", "b:slow", "all"]
    );
}

#[test]
fn include_intrusive_runs_them_and_an_operator_test_skips_until_confirmed() {
    let (host, clock) = host("members = [\"a\", \"b\"], include_intrusive = true");
    let job = start(&host, "all", false).unwrap();
    let (_, result) = finish(&host, &clock, job);
    let overall = record(&result);
    assert_eq!(
        count(&overall, "pass"),
        6,
        "ok, slow and spin of each member"
    );
    assert_eq!(
        count(&overall, "skipped"),
        2,
        "each member's lamp needs its operator"
    );
    let lamp = last(&host)
        .into_iter()
        .find(|(name, _)| name == "a:lamp")
        .unwrap()
        .1;
    assert_eq!(lamp.outcome, Outcome::Skipped);
    assert_eq!(
        lamp.reason,
        reason::NEEDS_OPERATOR,
        "the member says why it skipped"
    );
}

#[test]
fn a_single_test_answers_with_the_members_own_record() {
    let (host, clock) = host(MEMBERS);
    let job = start(&host, "a:bad", false).unwrap();
    let (status, result) = finish(&host, &clock, job);
    assert_eq!(status, Status::OK);
    let answer = record(&result);
    assert_eq!(answer.outcome, Outcome::Fail);
    assert_eq!(answer.reason, reason::OUT_OF_RANGE);
    assert_eq!(
        answer.measures().next().unwrap().value,
        9,
        "the member's measure, not counts"
    );
}

#[test]
fn a_member_that_is_not_running_is_skipped_as_absent() {
    let (host, clock) = host(MEMBERS);
    host.off("b").unwrap();
    let job = start(&host, "all", false).unwrap();
    let (_, result) = finish(&host, &clock, job);
    let overall = record(&result);
    assert_eq!(
        count(&overall, "skipped"),
        1,
        "b has no tests to name while it is off: one absent step"
    );
    assert_eq!(count(&overall, "pass"), 2, "a's ok and slow");
    assert_eq!(
        overall.reason,
        reason::OUT_OF_RANGE,
        "the first step that did not pass: a's bad"
    );
}

#[test]
fn a_member_in_use_by_another_caller_refuses_its_intrusive_test_as_in_use() {
    let (host, clock) = host("members = [\"a\", \"b\"], include_intrusive = true");
    let mut reply = [0u8; 2048];
    host.call_as(
        OTHER,
        index(&host, "a"),
        MethodId {
            contract: 0,
            method: 1,
        },
        &capacity(2048),
        &mut reply,
    )
    .unwrap();
    let job = start(&host, "all", false).unwrap();
    let (_, result) = finish(&host, &clock, job);
    let overall = record(&result);
    let steps = last(&host);
    assert!(
        steps.iter().all(|(name, _)| !name.starts_with("a:spin")),
        "a's spin was refused before it ran"
    );
    assert!(
        count(&overall, "skipped") >= 2,
        "a's spin and lamp were refused"
    );
}

#[test]
fn an_unknown_name_is_refused_and_one_run_at_a_time() {
    let (host, clock) = host(MEMBERS);
    assert_eq!(start(&host, "a:nope", false), Err(Status::INVALID_ARGUMENT));
    assert_eq!(
        start(&host, "nope:ok", false),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(start(&host, "ok", false), Err(Status::INVALID_ARGUMENT));
    let job = start(&host, "a:slow", false).unwrap();
    assert_eq!(start(&host, "b:ok", false), Err(Status::BUSY));
    finish(&host, &clock, job);
}

#[test]
fn cancelling_the_board_run_cancels_the_member_test_it_waits_on() {
    let (host, clock) = host(MEMBERS);
    let job = start(&host, "a:slow", false).unwrap();
    host.tick();
    clock.advance(Duration::from_millis(100));
    host.tick();
    host.cancel_job(CALLER, job).unwrap();
    let (status, _) = finish(&host, &clock, job);
    assert_eq!(status, Status::CANCELLED);
    assert!(
        last(&host).iter().all(|(name, _)| name != "a:slow"),
        "the cancelled member test leaves its last result as it was"
    );
}

#[test]
fn a_board_switched_off_mid_run_cancels_the_member_test_it_started() {
    let (host, clock) = host(MEMBERS);
    start(&host, "a:slow", false).unwrap();
    host.tick();
    clock.advance(Duration::from_millis(100));
    host.tick();
    host.off("board").unwrap();
    host.on("board").unwrap();
    // The member's test was cancelled with its starter: a new run of it is not refused as busy.
    let job = start(&host, "a:slow", false).expect("the member is free again");
    let (status, _) = finish(&host, &clock, job);
    assert_eq!(status, Status::OK);
}

#[test]
fn a_members_job_is_never_reported_to_a_client() {
    let (host, clock) = host(MEMBERS);
    let job = start(&host, "all", false).unwrap();
    let mut seen = Vec::new();
    let mut ended = false;
    for _ in 0..4_000 {
        host.tick();
        let mut updates: Vec<JobUpdate> = Vec::new();
        host.take_job_updates(&mut updates);
        ended |= updates
            .iter()
            .any(|u| u.job == job && matches!(u.event, JobEvent::Finished { .. }));
        seen.extend(updates);
        if ended {
            break;
        }
        clock.advance(Duration::from_millis(10));
    }
    assert!(ended, "the board's run ended");
    assert!(
        seen.iter().all(|update| update.job == job),
        "only the board's own job is reported"
    );
    assert!(
        seen.iter()
            .any(|u| matches!(u.event, JobEvent::Progress { .. })),
        "its progress is"
    );
}

#[test]
fn another_caller_cannot_cancel_the_board_run() {
    let (host, clock) = host(MEMBERS);
    let job = start(&host, "a:slow", false).unwrap();
    assert_eq!(host.cancel_job(OTHER, job), Err(Status::NOT_PERMITTED));
    let (status, _) = finish(&host, &clock, job);
    assert_eq!(status, Status::OK, "it ran on");
}
