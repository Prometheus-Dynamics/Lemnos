//! `lemnos-ctl show` over every built-in driver the simulator can host, on the shared walk board
//! (`testing/crates/sim-walk`). Every built-in driver is either walked or named, with the reason,
//! in `NOT_SIMULATED`; a walked device must bind and print its panel.

#[path = "support/scratch.rs"]
mod scratch;
use scratch::Scratch;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_sim::SimBoard;
use lemnos_sim_walk::{NOT_SIMULATED, NOT_WALKED, SIMULATED, walk};
use lemnosd::{Config, Service};

/// How long a test waits for a walked device to bind. A device whose bind fails retries after the
/// host's first retry (`retry_first`, 1 s by default), so two of those cover one retry.
const BIND_WAIT: Duration = Duration::from_secs(2);

struct Running {
    _service: Service,
    socket: PathBuf,
    _dir: Scratch,
}

fn scratch(_tag: &str) -> Scratch {
    let dir = Scratch(lemnos_socket_dir::scratch_dir().unwrap());
    std::fs::create_dir_all(&*dir).unwrap();
    dir
}

fn board_text(devices: &str) -> String {
    format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"walk\"\n{devices}")
}

fn start(devices: &str, board: SimBoard) -> Running {
    let dir = scratch("show");
    let socket = lemnos_socket_dir::socket(&dir, "sock").unwrap();
    let config = Config {
        socket: socket.clone(),
        name: "walk".to_string(),
        board: parse(&board_text(devices)).unwrap(),
        drivers: lemnosd::builtin::drivers(),
        backend: Box::new(board),
        options: Options {
            call_deadline: Duration::from_millis(500),
            ..Options::default()
        },
    };
    let service = Service::start(config).unwrap();
    Running {
        _dir: dir,
        _service: service,
        socket,
    }
}

fn show(running: &Running, device: &str) -> Result<String, String> {
    let args = vec![
        "--socket".to_string(),
        running.socket.display().to_string(),
        "show".to_string(),
        device.to_string(),
    ];
    let deadline = Instant::now() + BIND_WAIT;
    loop {
        let mut out = Vec::new();
        match lemnos_ctl::run(&args, &mut out) {
            // Busy, not bound yet, or no answer in time under load: a device retries its bind with
            // backoff, and a slow daemon answers the next ask.
            Err(error)
                if (error.starts_with("refused (-16)")
                    || error.starts_with("refused (-19)")
                    || error == "no answer in time"
                    || error.starts_with("refused (-110)"))
                    && Instant::now() < deadline =>
            {
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
            Ok(()) => return Ok(String::from_utf8(out).unwrap()),
        }
    }
}

/// Every built-in driver is walked or named, and none is named twice.
#[test]
fn every_built_in_driver_is_walked_or_named_with_its_reason() {
    let dir = scratch("names");
    let walked = walk(&dir).unwrap();
    let board = parse(&board_text(&walked.devices)).unwrap();
    let host = lemnos_host::Host::start(
        &board,
        &lemnosd::builtin::drivers(),
        Box::new(walked.board.clone()),
        std::sync::Arc::new(walked.board.clock()),
        lemnos_test_options::options(),
    )
    .unwrap();
    let mut built_in = host.driver_names();
    built_in.sort_unstable();
    let mut listed: Vec<&str> = SIMULATED
        .iter()
        .copied()
        .chain(NOT_SIMULATED.iter().map(|(name, _)| *name))
        .chain(NOT_WALKED.iter().map(|(name, _)| *name))
        .collect();
    listed.sort_unstable();
    let before = listed.len();
    listed.dedup();
    assert_eq!(before, listed.len(), "a driver is named twice");
    assert_eq!(
        built_in, listed,
        "walked or named: every built-in driver, once"
    );
    // Every walked device binds. A device retries its bind with backoff, so wait for it up to a
    // deadline; a device that never binds says why, from the host.
    let deadline = Instant::now() + BIND_WAIT;
    loop {
        let pending: Vec<_> = host
            .devices()
            .into_iter()
            .filter(|d| SIMULATED.contains(&d.driver.as_str()))
            .filter(|d| d.state != lemnos_host::DeviceState::Running)
            .collect();
        if pending.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            let device = &pending[0];
            panic!("{} ({}): {:?}", device.id, device.driver, device.health);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for (name, reason) in NOT_SIMULATED.iter().chain(NOT_WALKED) {
        assert!(!reason.is_empty(), "{name} has no reason");
    }
    host.shutdown();
    let _ = std::fs::remove_dir_all(&*dir);
}

/// Every walked device binds, and its panel says so.
#[test]
fn every_walked_device_prints_its_panel_running() {
    let dir = scratch("panels");
    let walked = walk(&dir).unwrap();
    let running = start(&walked.devices, walked.board.clone());
    let mut ids = Vec::new();
    for line in walked.devices.lines() {
        if let Some(rest) = line.strip_prefix("id = \"") {
            ids.push(rest.trim_end_matches('"').to_string());
        }
    }
    assert!(!ids.is_empty());
    for id in &ids {
        let panel = show(&running, id).unwrap_or_else(|error| panic!("{id}: {error}"));
        assert!(panel.contains("\n  kind     "), "{id}: {panel}");
        assert!(panel.contains("\n  state    running"), "{id}: {panel}");
    }
    let _ = std::fs::remove_dir_all(&*dir);
}
