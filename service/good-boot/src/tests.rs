use super::*;
use std::collections::VecDeque;

/// A probe that answers from a script, one pair per look, and records what it was asked.
struct Script {
    looks: VecDeque<(bool, String)>,
    last: (bool, String),
    asked: Vec<&'static str>,
}

impl Script {
    fn new(looks: &[(bool, &str)]) -> Self {
        Self {
            looks: looks.iter().map(|(r, m)| (*r, m.to_string())).collect(),
            last: (false, String::new()),
            asked: Vec::new(),
        }
    }

    /// Advances to the next scripted look (the readiness is asked first, then the marker).
    fn next_look(&mut self) {
        if let Some(look) = self.looks.pop_front() {
            self.last = look;
        }
    }
}

impl Probe for Script {
    fn lemnosd_ready(&mut self) -> bool {
        self.asked.push("ready");
        self.next_look();
        self.last.0
    }

    fn booted_marker(&mut self) -> String {
        self.asked.push("marker");
        self.last.1.clone()
    }
}

/// A clock that moves only when the waiter sleeps.
#[derive(Default)]
struct Fake {
    now: Duration,
    sleeps: Vec<Duration>,
}

impl Clock for Fake {
    fn elapsed(&self) -> Duration {
        self.now
    }

    fn sleep(&mut self, for_: Duration) {
        self.sleeps.push(for_);
        self.now += for_;
    }
}

#[test]
fn a_marker_that_appears_with_lemnosd_ready_is_good_when_it_appears() {
    // Not yet at the first look, set and ready from the third.
    let mut probe = Script::new(&[(false, ""), (false, ""), (true, "os-2\n")]);
    let mut clock = Fake::default();
    let outcome = wait(&mut probe, &mut clock, Duration::from_secs(300));
    assert_eq!(
        outcome,
        Outcome::Good {
            slot: "os-2".into(),
            after: Duration::from_secs(2),
        }
    );
    assert_eq!(clock.sleeps, vec![POLL, POLL]);
}

#[test]
fn a_boot_that_is_never_good_fails_at_the_deadline_and_not_before() {
    let mut probe = Script::new(&[]);
    let mut clock = Fake::default();
    let outcome = wait(&mut probe, &mut clock, Duration::from_secs(30));
    assert_eq!(
        outcome,
        Outcome::Late {
            ready: false,
            marker: String::new(),
        }
    );
    assert_eq!(clock.now, Duration::from_secs(30), "the deadline is exact");
}

#[test]
fn the_last_sleep_is_cut_to_the_deadline() {
    let mut probe = Script::new(&[]);
    let mut clock = Fake::default();
    let deadline = Duration::from_millis(2500);
    let _ = wait(&mut probe, &mut clock, deadline);
    assert_eq!(clock.now, deadline);
    assert_eq!(clock.sleeps.last(), Some(&Duration::from_millis(500)));
}

#[test]
fn a_marker_without_lemnosd_ready_is_not_a_good_boot() {
    let mut probe = Script::new(&[(false, "os-1\n"); 5]);
    let mut clock = Fake::default();
    let outcome = wait(&mut probe, &mut clock, Duration::from_secs(4));
    assert_eq!(
        outcome,
        Outcome::Late {
            ready: false,
            marker: "os-1\n".into(),
        }
    );
}

#[test]
fn lemnosd_ready_without_a_marker_or_with_a_marker_that_names_no_slot_is_not_good() {
    let mut probe = Script::new(&[(true, ""), (true, "../etc\n")]);
    let mut clock = Fake::default();
    let outcome = wait(&mut probe, &mut clock, Duration::from_secs(2));
    assert_eq!(
        outcome,
        Outcome::Late {
            ready: true,
            marker: "../etc\n".into(),
        }
    );
}

#[test]
fn the_waiter_asks_only_for_readiness_and_the_marker() {
    // Device health is not one of the facts: a failing device changes nothing here, because the
    // probe has no question to ask about devices. The record shows the only two questions.
    let mut probe = Script::new(&[(true, "os-1\n")]);
    let mut clock = Fake::default();
    assert!(matches!(
        wait(&mut probe, &mut clock, Duration::from_secs(300)),
        Outcome::Good { .. }
    ));
    assert_eq!(probe.asked, vec!["ready", "marker"]);
}

#[test]
fn a_good_boot_at_the_start_is_good_at_zero_seconds() {
    let mut probe = Script::new(&[(true, "os-1\n")]);
    let mut clock = Fake::default();
    assert_eq!(
        wait(&mut probe, &mut clock, Duration::from_secs(30)),
        Outcome::Good {
            slot: "os-1".into(),
            after: Duration::ZERO,
        }
    );
    assert!(clock.sleeps.is_empty());
}

#[test]
fn the_real_probe_reads_the_booted_marker_from_the_data_partition() {
    let dir = lemnos_scratch::Scratch::adopt(
        std::env::temp_dir().join(format!("lemnos-good-boot-test-{}", std::process::id())),
    );
    let slots = dir.join("board/slots");
    std::fs::create_dir_all(&slots).unwrap();
    let mut probe = System {
        data: dir.to_path_buf(),
    };
    assert_eq!(probe.booted_marker(), "", "no marker yet");
    std::fs::write(slots.join("booted"), "os-3\n").unwrap();
    assert_eq!(probe.booted_marker(), "os-3");
    // A marker that is a directory, not a file, is not read as a slot.
    std::fs::remove_file(slots.join("booted")).unwrap();
    std::fs::create_dir(slots.join("booted")).unwrap();
    assert_eq!(probe.booted_marker(), "");
}
