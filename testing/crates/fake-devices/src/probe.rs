//! A fake device that serves `self-test/1` with a few tests of known outcome, for tests of the
//! self-test path through a host, the board's self-test and the bridge.
//!
//! Tests: `ok` and `slow` pass (`slow` after a second), `bad` fails `OUT_OF_RANGE`, `spin` is
//! intrusive, and `lamp` is intrusive and needs an operator.

use lemnos_contract_self_test::{
    Bench, Flow, Measure, Record, Runner, Scratch, SelfTest, Spec, reason,
};
use lemnos_core::{CallerId, Config, JobId, JobState, Lifecycle, Reply, Result, Status, driver};

/// The tests the probe offers. Their names are stable.
pub const SPECS: &[Spec] = &[
    Spec::read("ok", "always passes", 20),
    Spec::read("bad", "always fails", 20),
    Spec::read("slow", "passes after a second", 1000),
    Spec::intrusive(
        "spin",
        "changes the device for a second",
        "a free device",
        1000,
    ),
    Spec {
        name: "lamp",
        description: "shows a state for an operator",
        intrusive: true,
        operator: true,
        fixture: "an operator",
        expect_ms: 1000,
    },
];

/// A device that serves `self-test/1` with [`SPECS`], and nothing else.
#[driver("fake-probe", provides = [SelfTest])]
pub struct FakeProbe {
    runner: Runner,
    calling: CallerId,
}

impl Lifecycle for FakeProbe {
    type Needs = FakeProbeNeeds;
    const TICKS: bool = true;
    const JOBS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn start(_needs: FakeProbeNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            runner: Runner::new(SPECS),
            calling: CallerId::LOCAL,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.runner.tick(now_ns, &mut Steps)
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
        self.runner.caller(caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        self.runner.caller_gone(caller);
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        self.runner.job(job, result)
    }

    fn job_cancel(&mut self, job: JobId) {
        self.runner.cancel(job, &mut Steps);
    }
}

impl SelfTest for FakeProbe {
    fn tests(&mut self, into: &mut [u8]) -> Result<usize> {
        self.runner.write_tests(into)
    }

    fn run(&mut self, name: &str, confirmed: bool) -> Result<JobId> {
        self.runner.start(name, confirmed, self.calling)
    }

    fn last(&mut self, into: &mut [u8]) -> Result<usize> {
        self.runner.write_last(into)
    }
}

/// The probe's steps: the slow test and the intrusive ones wait a second, the others answer now.
struct Steps;

impl Bench for Steps {
    fn step(&mut self, test: usize, now_ns: u64, scratch: &mut Scratch) -> Flow {
        let waits = matches!(SPECS[test].name, "slow" | "spin" | "lamp");
        if waits && scratch.step == 0 {
            scratch.step = 1;
            return Flow::Wait(now_ns + 1_000_000_000);
        }
        Flow::Done(match SPECS[test].name {
            "bad" => {
                Record::fail(reason::OUT_OF_RANGE).measure(Measure::new("v", 9, 0, "").within(0, 1))
            }
            _ => Record::pass(reason::OK),
        })
    }

    fn restore(&mut self, _scratch: &mut Scratch) -> core::result::Result<(), Status> {
        Ok(())
    }
}
