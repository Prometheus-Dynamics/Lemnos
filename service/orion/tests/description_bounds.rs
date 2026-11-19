//! The description of every built-in driver, through the text form the bridge sends: the largest
//! one each driver gives must keep the bounds the reader takes (`description::MAX_*`). Simulated
//! drivers are described while running. A driver the simulator cannot host is added to the board as
//! a device that does not bind, so its description is the manifest's alone (`live=false`): that is
//! the same answer lemnosd gives any device that is not running, and its items are not counted.
//! Run with `--nocapture` to print the table.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_client::{Client, DeviceState};
use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_orion::{description, source};
use lemnos_sim_walk::{NOT_SIMULATED, NOT_WALKED, SIMULATED, walk};
use lemnosd::{Config, Service};

fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-bounds-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The driver each device of the walk's board runs, in board order.
fn walked_drivers(devices: &str) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    let mut drivers = Vec::new();
    for line in devices.lines() {
        if let Some(rest) = line.strip_prefix("id = \"") {
            ids.push(rest.trim_end_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("driver = \"") {
            drivers.push(rest.trim_end_matches('"').to_string());
        }
    }
    assert_eq!(ids.len(), drivers.len(), "every device names its driver");
    ids.into_iter().zip(drivers).collect()
}

#[test]
#[allow(clippy::print_stdout)]
fn every_built_in_drivers_description_keeps_the_bounds_through_the_text_form() {
    let dir = scratch("table");
    let walked = walk(&dir).unwrap();
    let simulated = walked_drivers(&walked.devices);
    let mut devices = walked.devices.clone();
    for (name, _) in NOT_SIMULATED.iter().chain(NOT_WALKED) {
        devices.push_str(&format!(
            "\n[[devices]]\nid = \"n-{name}\"\ndriver = \"{name}\"\n"
        ));
    }
    // Never started: the board file disables it, so no bind is tried.
    devices.push_str("\n[[devices]]\nid = \"never-started\"\ndriver = \"g2d\"\ndisabled = true\n");
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"walk\"\n{devices}");
    let socket = dir.join("sock");
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "bounds".to_string(),
        board: parse(&text).unwrap(),
        drivers: lemnosd::builtin::drivers(),
        backend: Box::new(walked.board.clone()),
        options: Options {
            call_deadline: Duration::from_millis(200),
            ..Options::default()
        },
    })
    .unwrap();
    let client = Client::connect(&socket, "bounds").unwrap();

    // The simulated devices bind with backoff: wait for them to run before describing them.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let list = client.list().unwrap();
        let pending = list.iter().any(|d| {
            simulated.iter().any(|(id, _)| *id == d.name) && d.state != DeviceState::Running
        });
        if !pending {
            break;
        }
        assert!(Instant::now() < deadline, "a simulated device did not bind");
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut examples: Vec<(&str, String)> = Vec::new();
    println!(
        "driver\tdevice\tlive\tbytes\tlines\tlongest_line\trecords\tmax_keys\tcontracts\tmethods\titems"
    );
    let mut worst: std::collections::BTreeMap<String, (usize, String)> = Default::default();
    for entry in client.list().unwrap() {
        let driver = entry.driver.clone();
        let description = source::describe(&client, entry.index).expect("describes");
        let live = description.live;
        let text = description::render(&description, None, None)
            .unwrap_or_else(|error| panic!("{} ({driver}): {error}", entry.name));
        let records = description::parse(&text)
            .unwrap_or_else(|error| panic!("{} ({driver}): {error}", entry.name));
        // A simulated device is running, so live. A driver the simulator cannot host is reported as
        // it came out: it is live only if its bind happened to succeed on the simulated board.
        if SIMULATED.contains(&driver.as_str()) {
            assert!(
                live,
                "{} ({driver}) is simulated and must be live",
                entry.name
            );
        }
        let longest = text.lines().skip(1).map(str::len).max().unwrap_or(0);
        let keys = records.iter().map(|r| r.fields.len()).max().unwrap_or(0);
        let count = |word: &str| records.iter().filter(|r| r.word == word).count();
        println!(
            "{driver}\t{}\t{live}\t{}\t{}\t{longest}\t{}\t{keys}\t{}\t{}\t{}",
            entry.name,
            text.len(),
            text.lines().count(),
            records.len(),
            count("contract"),
            count("method"),
            count("item"),
        );
        if driver == "thermal-zone" && examples.is_empty() {
            examples.push(("running thermal-zone", text.clone()));
        }
        if driver == "a733-di" {
            // A bind that keeps failing: retrying, with the reason the bind gave.
            assert_eq!(description.state, Some(1), "a733-di retries its bind");
            assert!(!description.reason.is_empty(), "a733-di says why");
            examples.push(("retrying a733-di (its bind keeps failing)", text.clone()));
        }
        if entry.name == "never-started" {
            assert_eq!(description.state, Some(2), "a disabled device is off");
            assert_eq!(description.reason, "disabled");
            assert!(!live);
            examples.push((
                "never started (disabled = true in the board file)",
                text.clone(),
            ));
        }
        assert!(
            text.len() <= description::MAX_TEXT,
            "{driver}: {} bytes",
            text.len()
        );
        assert!(
            longest <= description::MAX_LINE,
            "{driver}: a line of {longest}"
        );
        assert!(records.len() <= description::MAX_RECORDS, "{driver}");
        assert!(keys <= description::MAX_KEYS, "{driver}: {keys} keys");
        let slot = worst.entry(driver.clone()).or_insert((0, String::new()));
        if text.len() >= slot.0 {
            *slot = (text.len(), entry.name.clone());
        }
    }
    // A device switched off by its operator: no driver runs, and the reply still says so, with the
    // state and the reason, and the manifest's contracts.
    let zone = client
        .list()
        .unwrap()
        .into_iter()
        .find(|d| d.driver == "thermal-zone")
        .expect("a thermal zone");
    client.set_power(zone.index, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while client
        .list()
        .unwrap()
        .iter()
        .any(|d| d.index == zone.index && d.state != DeviceState::Off)
    {
        assert!(Instant::now() < deadline, "the device did not go off");
        std::thread::sleep(Duration::from_millis(20));
    }
    let off = source::describe(&client, zone.index).expect("an off device describes");
    assert!(!off.live, "an off device is not live");
    assert_eq!(off.state, Some(2), "off");
    let off_text = description::render(&off, None, None).unwrap();
    assert!(off_text.contains("state=off"), "{off_text}");
    assert!(off.items.is_empty());
    examples.push(("off thermal-zone (switched off by the operator)", off_text));
    for (title, text) in &examples {
        println!("=== example: {title} ===\n{text}=== end ===");
    }

    println!("-- largest description per driver --");
    for (driver, (bytes, device)) in &worst {
        println!("{driver}\t{device}\t{bytes}");
    }
    service.stop();
    let _ = std::fs::remove_dir_all(dir);
}
