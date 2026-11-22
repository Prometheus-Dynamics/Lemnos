//! `lemnos-ctl call` whose answer never comes back (the client's deadline passes while the driver
//! still runs) reads the call's outcome from the service, instead of printing a bare timeout.

#[path = "support/scratch.rs"]
mod scratch;
use scratch::Scratch;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{Config as DriverConfig, Lifecycle, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

/// How long `mark` runs: longer than the tool's 5 s client deadline, shorter than the host's.
const MARK_SECS: u64 = 7;

#[contract("slow", version = 1)]
pub trait Slow {
    #[method(1)]
    fn mark(&mut self) -> Result<()>;
}

#[driver("slow", provides = [Slow])]
pub struct SlowDriver {
    marks: u32,
}

impl Lifecycle for SlowDriver {
    type Needs = SlowDriverNeeds;

    fn start(_: SlowDriverNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self { marks: 0 })
    }
}

impl Slow for SlowDriver {
    fn mark(&mut self) -> Result<()> {
        self.marks += 1;
        std::thread::sleep(Duration::from_secs(MARK_SECS));
        Ok(())
    }
}

fn start(call_deadline: Duration) -> (Service, Scratch, PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = Scratch(std::env::temp_dir().join(format!(
        "lemnos-ctl-outcome-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
        [[devices]]\nid = \"s\"\ndriver = \"slow\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<SlowDriver>()],
        backend: Box::new(SimBoard::new()),
        options: Options {
            call_deadline,
            ..Options::default()
        },
    })
    .unwrap();
    (service, dir, socket)
}

fn mark(socket: &std::path::Path) -> std::result::Result<Vec<u8>, String> {
    let args: Vec<String> = [
        "--socket",
        &socket.display().to_string(),
        "call",
        "s",
        "mark",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out).map(|()| out)
}

/// The driver's call applied and its answer was lost: the tool reports the outcome and exits 0.
#[test]
fn a_lost_reply_whose_call_applied_prints_the_outcome_and_exits_zero() {
    let (_service, _dir, socket) = start(Duration::from_secs(20));
    let out = mark(&socket).expect("an applied call exits 0");
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("s: no answer in time; outcome: applied (status 0)"),
        "the tool said: {text}"
    );
}

/// The call is still running when the tool asks: the outcome is pending, and the tool exits nonzero.
#[test]
fn a_lost_reply_whose_call_is_still_running_exits_nonzero() {
    let (_service, _dir, socket) = start(Duration::from_secs(1));
    let error = mark(&socket).expect_err("a pending call exits nonzero");
    assert!(
        error.contains("outcome: still running"),
        "the tool said: {error}"
    );
}
