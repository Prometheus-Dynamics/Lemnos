//! The tool against a real service. It holds no code for these devices.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

struct Running {
    _service: Service,
    socket: PathBuf,
}

fn start() -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
        [[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n[devices.config]\nstart = 41\n\
        [[devices]]\nid = \"broken\"\ndriver = \"nothing-by-this-name\"\n\
        [[devices]]\nid = \"sensor\"\ndriver = \"fake-counter\"\nlink = \"receiver\"\n\
        [[devices]]\nid = \"receiver\"\ndriver = \"fake-counter\"\nlink = \"isp\"\n\
        [[devices]]\nid = \"isp\"\ndriver = \"fake-counter\"\n\
        [[devices]]\nid = \"stray\"\ndriver = \"fake-counter\"\nlink = \"nowhere\"\n\
        [[devices]]\nid = \"round\"\ndriver = \"fake-counter\"\nlink = \"about\"\n\
        [[devices]]\nid = \"about\"\ndriver = \"fake-counter\"\nlink = \"round\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    Running {
        _service: service,
        socket,
    }
}

/// Runs the tool. A device answers `BUSY` to a describe while its driver holds it (a tick, a
/// call): the host never waits on a driver, which may be stuck. So a busy answer is not the
/// answer; ask again until the device is free, or fail after a deadline.
fn ctl(running: &Running, words: &[&str]) -> Result<String, String> {
    let mut args = vec!["--socket".to_string(), running.socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut out = Vec::new();
        match lemnos_ctl::run(&args, &mut out) {
            Err(error) if error.starts_with("refused (-16)") && Instant::now() < deadline => {
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
            Ok(()) => return Ok(String::from_utf8(out).unwrap()),
        }
    }
}

#[test]
fn list_shows_every_device_and_why_one_is_down() {
    let running = start();
    let text = ctl(&running, &["list"]).unwrap();
    let rows: Vec<&str> = text.lines().collect();
    assert_eq!(rows.len(), 8);
    assert!(rows[0].starts_with("thing "));
    assert!(rows[0].contains("running"));
    assert!(rows[0].contains("example.counter/1"));
    assert!(!rows[0].contains("->"), "wired to nothing");
    assert!(rows[1].contains("retrying"));
    assert!(rows[1].contains("no driver named"));
    // What a device is wired to, as the board file's `link` has it.
    assert!(rows[2].starts_with("sensor "));
    assert!(rows[2].ends_with("  -> receiver"), "{}", rows[2]);
    assert!(rows[3].ends_with("  -> isp"), "{}", rows[3]);
    assert!(rows[4].starts_with("isp ") && !rows[4].contains("->"));
}

#[test]
fn chain_follows_a_device_to_what_it_is_wired_to() {
    let running = start();
    let names = |from: &str| -> Vec<String> {
        ctl(&running, &["chain", from])
            .unwrap()
            .lines()
            .map(|row| row.split_whitespace().next().unwrap().to_string())
            .collect()
    };
    // The tool has no code for a sensor, a receiver or an ISP: the board's links are all it has.
    assert_eq!(names("sensor"), ["sensor", "receiver", "isp"]);
    assert_eq!(names("receiver"), ["receiver", "isp"]);
    assert_eq!(names("thing"), ["thing"]);
    let text = ctl(&running, &["chain", "sensor"]).unwrap();
    assert!(text.lines().all(|row| row.contains("running")), "{text}");
    // Where the board's links lead nowhere, or round, it says so and ends.
    let stray = ctl(&running, &["chain", "stray"]).unwrap();
    assert_eq!(stray.lines().count(), 2);
    assert!(stray.ends_with("-> nowhere: no such device\n"), "{stray}");
    let round = ctl(&running, &["chain", "round"]).unwrap();
    assert_eq!(names("round")[..2], ["round", "about"]);
    assert!(round.ends_with("-> round: already above\n"), "{round}");
    assert!(
        ctl(&running, &["chain", "nothing"])
            .unwrap_err()
            .contains("no device")
    );
}

#[test]
fn describe_comes_from_the_device() {
    let running = start();
    let text = ctl(&running, &["describe", "thing"]).unwrap();
    assert!(text.contains("example.counter/1"));
    assert!(text.contains("set(value: u32) -> ()"));
    assert!(text.contains("control target"));
}

#[test]
fn read_gets_one_value_or_all() {
    let running = start();
    assert_eq!(
        ctl(&running, &["read", "thing", "count"]).unwrap(),
        "count = 42\n"
    );
    assert_eq!(ctl(&running, &["read", "thing"]).unwrap(), "count = 43\n");
}

#[test]
fn set_writes_a_control_and_refuses_a_value() {
    let running = start();
    ctl(&running, &["set", "thing", "target", "9"]).unwrap();
    assert_eq!(ctl(&running, &["call", "thing", "peek"]).unwrap(), "9\n");
    assert!(ctl(&running, &["set", "thing", "count", "1"]).is_err());
}

#[test]
fn call_reaches_any_method_by_name_or_with_its_tag() {
    let running = start();
    assert_eq!(ctl(&running, &["call", "thing", "get"]).unwrap(), "42\n");
    assert_eq!(
        ctl(&running, &["call", "thing", "example.counter/1:get"]).unwrap(),
        "43\n"
    );
    assert_eq!(ctl(&running, &["call", "thing", "set", "5"]).unwrap(), "");
    assert!(ctl(&running, &["call", "thing", "set", "lots"]).is_err());
    assert!(ctl(&running, &["call", "thing", "nope"]).is_err());
}

#[test]
fn watch_prints_as_many_as_asked_for() {
    let running = start();
    let text = ctl(
        &running,
        &[
            "watch",
            "thing",
            "count",
            "--period-ms",
            "1",
            "--count",
            "3",
        ],
    )
    .unwrap();
    assert_eq!(text.lines().count(), 3);
    assert!(text.lines().all(|l| l.starts_with("count = ")));
}

#[test]
fn off_and_on_take_effect() {
    let running = start();
    ctl(&running, &["off", "thing"]).unwrap();
    assert!(
        ctl(&running, &["list"])
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .contains("off")
    );
    assert!(ctl(&running, &["call", "thing", "get"]).is_err());
    ctl(&running, &["on", "thing"]).unwrap();
    assert_eq!(ctl(&running, &["call", "thing", "peek"]).unwrap(), "41\n");
}

#[test]
fn unknown_things_are_explained() {
    let running = start();
    assert!(
        ctl(&running, &["read", "ghost"])
            .unwrap_err()
            .contains("ghost")
    );
    assert!(
        ctl(&running, &["read", "thing", "ghost"])
            .unwrap_err()
            .contains("ghost")
    );
    assert!(
        ctl(&running, &["nonsense"])
            .unwrap_err()
            .contains("lemnos-ctl")
    );
}
