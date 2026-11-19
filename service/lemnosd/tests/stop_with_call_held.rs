//! A stop with a driver call in flight. The call is held by a gate the test controls: the driver
//! reports it has entered, then waits until the test releases it. The stop must finish while the
//! call is still held, with the socket gone, and the held call must end once the gate opens.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_core::{Config as DriverConfig, Lifecycle, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

/// The longest the test waits for any one thing. A failure is reported, never hung on.
const PATIENCE: Duration = Duration::from_secs(20);
/// How long a stop may take with the call still held. A stop that waits for the call waits for the
/// test, so this is what separates a bounded stop from a stuck one.
const STOP_BOUND: Duration = Duration::from_secs(10);

static ENTERED: AtomicBool = AtomicBool::new(false);
static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

fn release() {
    *GATE.0.lock().unwrap() = true;
    GATE.1.notify_all();
}

#[contract("gate", version = 1)]
pub trait Gating {
    #[method(1)]
    fn hold(&mut self) -> Result<()>;
}

#[driver("gate-holder", provides = [Gating])]
pub struct GateHolder {
    _held: (),
}

impl Lifecycle for GateHolder {
    type Needs = GateHolderNeeds;

    fn start(_: GateHolderNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self { _held: () })
    }
}

impl Gating for GateHolder {
    fn hold(&mut self) -> Result<()> {
        ENTERED.store(true, Ordering::Release);
        let mut open = GATE.0.lock().unwrap();
        while !*open {
            open = GATE.1.wait(open).unwrap();
        }
        Ok(())
    }
}

#[test]
fn a_stop_finishes_with_a_call_held_and_removes_the_socket() {
    let dir = std::env::temp_dir().join(format!("lemnosd-held-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"g\"\ndriver = \"gate-holder\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<GateHolder>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let caller_socket = socket.clone();
    let caller = std::thread::spawn(move || {
        let client = Client::connect(&caller_socket, "caller").unwrap();
        let mut gate = client.open::<dyn Gating>("g").unwrap();
        let _ = gate.hold();
        let _ = done_tx.send(());
    });

    // The explicit sync point: the driver has entered the call and is waiting on the gate.
    let start = Instant::now();
    while !ENTERED.load(Ordering::Acquire) {
        assert!(
            start.elapsed() < PATIENCE,
            "the call never reached the driver"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    let (stopped_tx, stopped_rx) = mpsc::channel();
    let stopper = std::thread::spawn(move || {
        service.stop();
        let _ = stopped_tx.send(());
    });
    let finished = stopped_rx.recv_timeout(STOP_BOUND).is_ok();
    // Open the gate in every case, so a failing run does not leave a thread blocked.
    release();
    let _ = caller.join();
    let _ = stopper.join();
    assert!(
        finished,
        "the stop did not finish within {STOP_BOUND:?} while a call was held"
    );
    assert!(!socket.exists(), "the socket was left behind");
    let _ = done_rx.recv_timeout(PATIENCE);
    let _ = std::fs::remove_dir_all(&dir);
}
