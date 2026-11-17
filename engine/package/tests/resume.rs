//! A package's driver keeps what its resume record says across a host rebind. What is observed is
//! the driver's own counter: a count that carries on from before the rebind means the new instance
//! took the record up (Kept); a count that starts again means it did not (Lost). The host's
//! stream word for Kept and Lost is tested against a stand-in in `engine/host/tests/resume.rs`; a
//! packaged driver cannot yet declare a stream's kind, so it has no stream to read here.
//! - the Rust fixture with `keep_state`: its count is kept;
//! - the same fixture without it: nothing is kept;
//! - the raw package built against the boundary before the resume entries (the extras layout, an
//!   old-shape package): nothing is kept, so its counter starts again;
//! - the raw package built with the resume entries: its counter is kept.

mod common;

use std::time::Duration;

use common::{count, fixtures, host, host_on, index, method, raw_package, scratch};
use lemnos_host::{DeviceState, Host, Options};
use lemnos_package::{Loader, Mode};
use lemnos_sim::{SimBoard, SimClock};

/// The fixture, loaded on its own.
fn loaded() -> Loader {
    let mut loader = Loader::new(Mode::Open);
    loader.load(&fixtures().good).unwrap();
    loader
}

/// One fixture device, with the settings `config` gives it.
fn keeper(config: &str) -> String {
    format!("\n[[devices]]\nid = \"keeper\"\ndriver = \"package-fixture\"\n{config}\n")
}

/// The fixture's `leave`: the driver says what it was bound with is gone.
fn leave(host: &Host, device: u32) {
    host.call(device, method(8), &[], &mut [0; 8]).unwrap();
}

/// Lets the clock run until the device is bound again.
fn bring_back(host: &Host, clock: &SimClock, id: &str) {
    for _ in 0..20 {
        clock.advance(Duration::from_secs(10));
        host.tick();
        if host.device(id).unwrap().state == DeviceState::Running {
            return;
        }
    }
    panic!("{id} did not come back");
}

#[test]
fn a_package_that_keeps_its_state_resumes_kept_across_a_rebind() {
    let loader = loaded();
    let (host, clock) = host(&keeper("config = { keep_state = 1 }"), &loader.entries());
    let device = index(&host, "keeper");
    assert_eq!(count(&host, device), Ok(1));
    assert_eq!(count(&host, device), Ok(2));

    leave(&host, device);
    assert!(matches!(
        host.device("keeper").unwrap().state,
        DeviceState::Backoff { .. }
    ));
    bring_back(&host, &clock, "keeper");
    assert_eq!(
        count(&host, device),
        Ok(3),
        "the new instance took the record up: its count carries on from the old one's"
    );
}

#[test]
fn a_package_that_keeps_nothing_resumes_lost_and_starts_again() {
    let loader = loaded();
    let (host, clock) = host(&keeper(""), &loader.entries());
    let device = index(&host, "keeper");
    assert_eq!(count(&host, device), Ok(1));

    leave(&host, device);
    bring_back(&host, &clock, "keeper");
    assert_eq!(
        count(&host, device),
        Ok(1),
        "nothing was kept: the new instance starts its count again"
    );
}

/// The raw package's `bump`: counts up and answers the count.
fn bump(host: &Host, device: u32) -> u32 {
    let mut reply = [0; 4];
    host.call(device, method(4), &[], &mut reply).unwrap();
    u32::from_le_bytes(reply)
}

/// Loads the raw package built with `cfgs` and binds it, with the line it uses.
fn raw_host(name: &str, cfgs: &[&str]) -> (Host, SimClock) {
    let dir = scratch(name);
    let file = raw_package(&dir, &format!("{name}.so"), cfgs);
    let mut loader = Loader::new(Mode::Open);
    loader.load(&file).unwrap();
    let sim = SimBoard::new();
    let _line = sim.add_line("gpio7");
    let devices = r#"
[[devices]]
id = "old"
driver = "raw-package"
uses = { line = "gpio7" }
config = { base = 40 }
"#;
    let clock = sim.clock();
    let (host, _) = host_on(sim, devices, &loader.entries(), Options::default());
    let _ = std::fs::remove_dir_all(dir);
    (host, clock)
}

#[test]
fn an_old_package_built_without_the_resume_entries_resumes_lost() {
    // The default build of the raw fixture: the extras layout, with no resume entries.
    let (host, clock) = raw_host("raw-old-shape", &[]);
    let device = index(&host, "old");
    assert_eq!(bump(&host, device), 1);
    assert_eq!(bump(&host, device), 2);

    // The raw fixture's `leave` is method 3 (the Rust fixture's is 8).
    host.call(device, method(3), &[], &mut [0; 8]).unwrap();
    bring_back(&host, &clock, "old");
    assert_eq!(
        bump(&host, device),
        1,
        "an old-shape package keeps nothing: its new instance starts from zero"
    );
}

#[test]
fn a_package_built_with_the_resume_entries_keeps_its_counter_across_a_rebind() {
    let (host, clock) = raw_host("raw-resume", &["with_resume"]);
    let device = index(&host, "old");
    assert_eq!(bump(&host, device), 1);
    assert_eq!(bump(&host, device), 2);

    // The raw fixture's `leave` is method 3 (the Rust fixture's is 8).
    host.call(device, method(3), &[], &mut [0; 8]).unwrap();
    bring_back(&host, &clock, "old");
    assert_eq!(
        bump(&host, device),
        3,
        "the new instance took the record up and carries on from it"
    );
}
