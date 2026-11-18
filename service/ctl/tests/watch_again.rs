//! `lemnos-ctl watch` on a device that answers `try again` (AGAIN): it says so once on stderr,
//! stops at its bound with a non-zero exit, and a device that answers AGAIN for a while and then
//! gives values prints exactly the values. The fake counter's `again_until_ms` (AGAIN for that
//! long after the bind, as a sensor powering up) and `again_from_ms` (AGAIN from then on, as a
//! sensor that stops answering) make each case; the bounds are short so the tests are fast.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_fake_devices::FakeCounter;
use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_host::entry;
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

struct Running {
    _service: Service,
    socket: PathBuf,
}

/// A service with one fake counter per `(name, config)` line.
fn start(devices: &[(&str, &str)]) -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-watch-again-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let socket = dir.join("sock");
    let mut text =
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n".to_string();
    for (name, config) in devices {
        text.push_str(&format!(
            "[[devices]]\nid = \"{name}\"\ndriver = \"fake-counter\"\n[devices.config]\n{config}\n"
        ));
    }
    let config = Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    };
    let service = Service::start(config).unwrap();
    Running {
        _service: service,
        socket,
    }
}

/// What one run of the tool printed and how it ended, and how long it took.
struct Watched {
    out: String,
    err: String,
    result: Result<(), String>,
    took: Duration,
}

fn watch(running: &Running, words: &[&str]) -> Watched {
    let mut args = vec!["--socket".to_string(), running.socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let mut out = Vec::new();
    let mut err = Vec::new();
    let started = Instant::now();
    let result = lemnos_ctl::run_with_stderr(&args, &mut out, &mut err);
    Watched {
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
        result,
        took: started.elapsed(),
    }
}

#[test]
fn a_device_that_only_answers_again_ends_at_the_bound_with_no_values() {
    // `never` answers AGAIN from the bind on, and never gives a value.
    let running = start(&[("never", "again_from_ms = 0")]);
    let watched = watch(
        &running,
        &[
            "watch",
            "never",
            "count",
            "--period-ms",
            "1",
            "--wait-ms",
            "400",
        ],
    );
    assert_eq!(
        watched.result.unwrap_err(),
        "never count: no value in 400 ms; the device kept answering \"try again\""
    );
    assert_eq!(watched.out, "", "a script reading stdout sees no values");
    // The one notice, at the start of the wait: the device, the item, and the bound.
    assert_eq!(
        watched.err,
        "never count: the device answered \"try again\"; waiting up to 400 ms \
         (--wait-ms N sets the wait, --wait-forever waits without one)\n"
    );
    assert!(
        watched.took >= Duration::from_millis(400),
        "gave up before the bound: {:?}",
        watched.took
    );
    assert!(
        watched.took < Duration::from_secs(5),
        "did not stop at the bound: {:?}",
        watched.took
    );
}

#[test]
fn again_for_less_than_the_bound_then_values_prints_the_same_values() {
    // `warm` answers AGAIN for the first 300 ms, then counts from 1 as it always does.
    let running = start(&[("warm", "again_until_ms = 300")]);
    let watched = watch(
        &running,
        &["watch", "warm", "count", "--period-ms", "1", "--count", "3"],
    );
    watched.result.unwrap();
    assert_eq!(watched.out, "count = 1\ncount = 2\ncount = 3\n");
}

#[test]
fn the_wait_option_changes_the_bound() {
    let running = start(&[("never", "again_from_ms = 0")]);
    let short = watch(
        &running,
        &[
            "watch",
            "never",
            "count",
            "--period-ms",
            "1",
            "--wait-ms",
            "200",
        ],
    );
    assert_eq!(
        short.result.unwrap_err(),
        "never count: no value in 200 ms; the device kept answering \"try again\""
    );
    let longer = watch(
        &running,
        &[
            "watch",
            "never",
            "count",
            "--period-ms",
            "1",
            "--wait-ms",
            "700",
        ],
    );
    assert_eq!(
        longer.result.unwrap_err(),
        "never count: no value in 700 ms; the device kept answering \"try again\""
    );
    assert!(
        longer.took > short.took,
        "{:?} vs {:?}",
        longer.took,
        short.took
    );
    // The option is checked before the device is asked: a bound of 0, or both options, is refused.
    let zero = watch(&running, &["watch", "never", "count", "--wait-ms", "0"]);
    assert!(zero.result.unwrap_err().contains("--wait-ms 0"));
    let both = watch(
        &running,
        &[
            "watch",
            "never",
            "count",
            "--wait-ms",
            "5",
            "--wait-forever",
        ],
    );
    assert!(both.result.unwrap_err().contains("give one of them"));
}

#[test]
fn values_then_again_past_the_bound_says_how_many_values_it_gave() {
    // `late` counts from 1 for 600 ms, then answers AGAIN from then on.
    let running = start(&[("late", "again_from_ms = 600")]);
    let watched = watch(
        &running,
        &[
            "watch",
            "late",
            "count",
            "--period-ms",
            "1",
            "--wait-ms",
            "300",
        ],
    );
    let message = watched.result.unwrap_err();
    let prefix = "late count: no value in 300 ms after ";
    assert!(message.starts_with(prefix), "{message}");
    let seen: usize = message[prefix.len()..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(seen >= 1, "{message}");
    assert!(
        message.ends_with(" values; the device kept answering \"try again\""),
        "{message}"
    );
    // Exactly the values it gave, in order, and nothing for the AGAIN.
    let expected: String = (1..=seen).map(|n| format!("count = {n}\n")).collect();
    assert_eq!(watched.out, expected);
    // One notice for the wait, when it started.
    assert_eq!(
        watched.err.matches("try again").count(),
        1,
        "{}",
        watched.err
    );
}

#[test]
fn read_of_a_device_that_only_answers_again_exits_non_zero_as_watch_does() {
    // `read` is one call, so its answer is the refusal: a non-zero exit, as `watch` ends with.
    let running = start(&[("never", "again_from_ms = 0")]);
    let watched = watch(&running, &["read", "never", "count"]);
    let message = watched.result.unwrap_err();
    assert!(message.starts_with("refused (-11)"), "{message}");
    assert_eq!(watched.out, "");
}
