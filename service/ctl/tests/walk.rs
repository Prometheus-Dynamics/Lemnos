//! `lemnos-ctl show` over every built-in driver the simulator can host, on the shared walk board
//! (`testing/crates/sim-walk`). Every built-in driver is either walked or named, with the reason,
//! in `NOT_SIMULATED`; a walked device must bind and print its panel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_sim::SimBoard;
use lemnos_sim_walk::{NOT_SIMULATED, NOT_WALKED, SIMULATED, walk};
use lemnosd::{Config, Service};

struct Running {
    _service: Service,
    socket: PathBuf,
}

fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-walk-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn board_text(devices: &str) -> String {
    format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"walk\"\n{devices}")
}

fn start(devices: &str, board: SimBoard) -> Running {
    let dir = scratch("show");
    let socket = dir.join("sock");
    let config = Config {
        socket: socket.clone(),
        name: "walk".to_string(),
        board: parse(&board_text(devices)).unwrap(),
        drivers: lemnosd::builtin::drivers(),
        backend: Box::new(board),
        options: Options {
            call_deadline: Duration::from_millis(200),
            ..Options::default()
        },
    };
    let service = Service::start(config).unwrap();
    Running {
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
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut out = Vec::new();
        match lemnos_ctl::run(&args, &mut out) {
            // Busy, or not bound yet: a device retries its bind with backoff, so wait for it.
            Err(error)
                if (error.starts_with("refused (-16)") || error.starts_with("refused (-19)"))
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
        Options::default(),
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
    let deadline = Instant::now() + Duration::from_secs(15);
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
    let _ = std::fs::remove_dir_all(dir);
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
    let _ = std::fs::remove_dir_all(dir);
}
