//! A driver is told the host's call deadline when it starts, so a driver whose work comes in steps
//! can bound each step by a share of it: the deadline the host's options give, and the default.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{Config, Lifecycle, MethodId, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;

#[contract("deadline-read", version = 1)]
pub trait DeadlineRead {
    #[method(1)]
    fn deadline_ms(&mut self) -> Result<u64>;
}

/// Keeps the deadline its config gave and answers it.
#[driver("deadline-probe", provides = [DeadlineRead])]
pub struct DeadlineProbe {
    deadline_ms: u64,
}

impl Lifecycle for DeadlineProbe {
    type Needs = DeadlineProbeNeeds;

    fn start(_needs: DeadlineProbeNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            deadline_ms: cfg.call_deadline_ms(),
        })
    }
}

impl DeadlineRead for DeadlineProbe {
    fn deadline_ms(&mut self) -> Result<u64> {
        Ok(self.deadline_ms)
    }
}

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"deadline\"\n\
    [[devices]]\nid = \"probe\"\ndriver = \"deadline-probe\"\n";

fn told(options: Options) -> u64 {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(BOARD).unwrap(),
        &[entry::<DeadlineProbe>()],
        Box::new(sim),
        Arc::new(clock),
        options,
    )
    .unwrap();
    let index = host.device("probe").unwrap().index;
    let mut reply = [0; 8];
    host.call(
        index,
        MethodId {
            contract: 0,
            method: 1,
        },
        &[],
        &mut reply,
    )
    .unwrap();
    u64::from_le_bytes(reply)
}

#[test]
fn a_driver_is_told_the_call_deadline_the_host_runs_it_under() {
    assert_eq!(
        told(Options::default()),
        u64::try_from(Options::default().call_deadline.as_millis()).unwrap()
    );
    assert_eq!(told(Options::default()), 2000, "the default deadline");
    let options = Options {
        call_deadline: Duration::from_millis(750),
        ..Options::default()
    };
    assert_eq!(told(options), 750);
}
