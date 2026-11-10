//! The real service hears a backend's hints: a device that was absent binds when it arrives,
//! not when its retry comes round.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_core::{Config as DriverConfig, Lifecycle, Line, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

#[contract("plugged", version = 1)]
pub trait Plugged {
    #[method(1)]
    fn answer(&mut self) -> Result<u32>;
}

#[driver("plug", provides = [Plugged])]
pub struct Plug {
    #[needs]
    _wire: Line,
}

impl Lifecycle for Plug {
    type Needs = PlugNeeds;

    fn start(needs: PlugNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self { _wire: needs._wire })
    }
}

impl Plugged for Plug {
    fn answer(&mut self) -> Result<u32> {
        Ok(42)
    }
}

/// A service whose one device needs `port-a`, which isn't there, and whose retries are a minute
/// apart: only a hint can bind it within a test.
fn start() -> (Service, SimBoard) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnosd-hotplug-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"cam\"\ndriver = \"plug\"\nbus = \"port-a\"\n";
    let board = SimBoard::new();
    let service = Service::start(Config {
        socket: dir.join("sock"),
        name: "test-host".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<Plug>()],
        backend: Box::new(board.clone()),
        options: Options {
            retry_first: Duration::from_secs(60),
            retry_cap: Duration::from_secs(60),
            ..Options::default()
        },
    })
    .unwrap();
    (service, board)
}

fn running(service: &Service) -> bool {
    service.host().device("cam").unwrap().state == DeviceState::Running
}

#[test]
fn a_device_that_arrives_is_bound_at_the_hint() {
    let (mut service, board) = start();
    service.watch(Box::new(board.hints())).unwrap();
    assert!(!running(&service));
    // Long enough that the bind which failed at the start is further back than the gap.
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    board.add_line("port-a");
    while !running(&service) {
        assert!(start.elapsed() < Duration::from_secs(20), "never bound");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}, with retries a minute apart",
        start.elapsed()
    );
}

#[test]
fn without_the_hint_it_waits_for_its_retry() {
    let (service, board) = start();
    board.add_line("port-a");
    std::thread::sleep(Duration::from_millis(600));
    assert!(!running(&service), "nothing told the host");
}

#[test]
fn a_service_that_is_watching_stops_promptly() {
    let (mut service, board) = start();
    service.watch(Box::new(board.hints())).unwrap();
    let start = Instant::now();
    service.stop();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}
