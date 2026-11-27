//! The deadline of `Options::for_tests` is the one a test's calls run under. A call that takes 2.5 s
//! of wall time asleep is judged hung (TIMED_OUT) under the default 2 s deadline and answers under
//! the tests' deadline. A test that is about a hang keeps its own short deadline, and a hung call is
//! still cut there, long before it would have returned.

mod test_options;

use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_core::{Lifecycle, MethodId, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;

/// How long the stub's call sleeps: past the default 2 s deadline, far inside the tests' deadline.
const SLEEP: Duration = Duration::from_millis(2500);

#[contract("sleep-call", version = 1)]
pub trait SleepCall {
    #[method(1)]
    fn sleep(&mut self) -> Result<u32>;
}

/// Answers 7 after sleeping [`SLEEP`]: asleep, not running, as a driver waiting on a device is.
#[driver("sleeper", provides = [SleepCall])]
pub struct Sleeper {
    answer: u32,
}

impl Lifecycle for Sleeper {
    type Needs = SleeperNeeds;

    fn start(_needs: SleeperNeeds, _cfg: &lemnos_core::Config<'_>) -> Result<Self> {
        Ok(Self { answer: 7 })
    }
}

impl SleepCall for Sleeper {
    fn sleep(&mut self) -> Result<u32> {
        std::thread::sleep(SLEEP);
        Ok(self.answer)
    }
}

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"sleep\"\n\
    [[devices]]\nid = \"sleeper\"\ndriver = \"sleeper\"\n";

/// The call's answer and how long it took.
fn call(options: Options) -> (core::result::Result<u32, Status>, Duration) {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(BOARD).unwrap(),
        &[entry::<Sleeper>()],
        Box::new(sim),
        Arc::new(clock),
        options,
    )
    .unwrap();
    let index = host.device("sleeper").unwrap().index;
    let mut reply = [0; 4];
    let start = Instant::now();
    let answer = host
        .call(
            index,
            MethodId {
                contract: 0,
                method: 1,
            },
            &[],
            &mut reply,
        )
        .map(|_| u32::from_le_bytes(reply));
    (answer, start.elapsed())
}

#[test]
fn a_call_past_the_default_deadline_is_judged_hung() {
    assert!(SLEEP > Options::default().call_deadline);
    let (answer, _) = call(Options::default());
    assert_eq!(answer, Err(Status::TIMED_OUT));
}

#[test]
fn the_same_call_answers_under_the_tests_deadline() {
    assert_eq!(
        test_options::options().call_deadline,
        test_options::CALL_DEADLINE
    );
    assert!(SLEEP < test_options::CALL_DEADLINE);
    let (answer, took) = call(test_options::options());
    assert_eq!(answer, Ok(7));
    assert!(took >= SLEEP, "answered after its sleep: {took:?}");
}

#[test]
fn a_hang_test_keeps_its_own_deadline_and_is_cut_there() {
    let options = Options {
        call_deadline: Duration::from_millis(100),
        ..test_options::options()
    };
    let (answer, took) = call(options);
    assert_eq!(answer, Err(Status::TIMED_OUT));
    // Cut by its own deadline, long before the call would have returned on its own.
    assert!(
        took < SLEEP / 2,
        "cut at its deadline, not at the end of the call: {took:?}"
    );
}
