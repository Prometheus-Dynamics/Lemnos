//! The tool against a real service. It holds no code for these devices.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::{parse, parse_lenient};
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::packages::{PackageConfig, PackageFile, PackageMode, PackageReport, add_packages};
use lemnosd::{Config, Service};

struct Running {
    _service: Service,
    socket: PathBuf,
}

fn start() -> Running {
    start_with(PackageReport::default())
}

/// A service that reports `packages` as what became of its driver packages.
fn start_with(packages: PackageReport) -> Running {
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
    let config = Config {
        socket: socket.clone(),
        name: "t".to_string(),
        // Lenient: the two devices that link to each other are a loop, which the strict reader
        // refuses and the daemon starts without (the device that closes it is refused).
        board: parse_lenient(text).unwrap(),
        drivers: vec![entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    };
    let service = Service::start_with_packages(config, packages).unwrap();
    Running {
        _service: service,
        socket,
    }
}

/// Runs the tool. A describe waits for a driver in a tick or a call (see `Host::describe`), so
/// the answer is the answer: there is no asking again.
fn ctl(running: &Running, words: &[&str]) -> Result<String, String> {
    let mut args = vec!["--socket".to_string(), running.socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out)?;
    Ok(String::from_utf8(out).unwrap())
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

#[test]
fn packages_shows_what_was_loaded_and_why_the_rest_was_refused() {
    // A service with packages off says so.
    assert_eq!(
        ctl(&start(), &["packages"]).unwrap(),
        "packages off: no package directory is configured\n"
    );

    // A real scan of a directory holding a file that is no package, in open mode, and beside it
    // a package that loaded.
    let dir = std::env::temp_dir().join(format!("lemnos-ctl-packages-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("junk.so"), "not a shared object").unwrap();
    let mut drivers = vec![entry::<FakeCounter>()];
    let mut report = add_packages(
        &mut drivers,
        &PackageConfig {
            dir: Some(dir.clone()),
            keys: None,
            open: true,
        },
    );
    assert_eq!(report.mode, PackageMode::Open);
    report.files.insert(
        0,
        PackageFile {
            path: "/packages/good.so".into(),
            driver: Some("some-driver".to_string()),
            reason: String::new(),
        },
    );
    let junk = dir.join("junk.so").display().to_string();
    let width = junk.len();
    assert_eq!(
        ctl(&start_with(report), &["packages"]).unwrap(),
        format!(
            "packages open: any package in the directory runs\n\
             {:width$}  loaded   some-driver\n\
             {junk}  refused  not a driver package: not a shared object\n",
            "/packages/good.so"
        )
    );

    // Signed mode with no keys: nothing was looked at, and the reason is there to read.
    let report = add_packages(
        &mut drivers,
        &PackageConfig {
            dir: Some(dir.clone()),
            keys: None,
            open: false,
        },
    );
    let text = ctl(&start_with(report), &["packages"]).unwrap();
    let rows: Vec<&str> = text.lines().collect();
    assert_eq!(
        rows[0],
        "packages signed: only packages signed by a trusted key run"
    );
    assert!(
        rows[1].ends_with("not used: signed mode needs a key file"),
        "{text}"
    );
    assert_eq!(rows.len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

/// Writes `text` to a board file of its own and runs `validate` on it, with no daemon.
fn validate(text: &str) -> Result<String, String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-validate-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    let mut out = Vec::new();
    // No daemon is listening there: validate never connects.
    let args = [
        "--socket".to_string(),
        dir.join("nobody").display().to_string(),
        "validate".to_string(),
        path.display().to_string(),
    ];
    let result = lemnos_ctl::run(&args, &mut out).map(|()| String::from_utf8(out).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
    result
}

const HEAD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";

#[test]
fn validate_shows_the_defaults_with_no_host_section() {
    let text = validate(&format!("{HEAD}[[devices]]\nid = \"a\"\ndriver = \"x\"\n")).unwrap();
    assert!(text.contains("board \"t\", 1 devices"), "{text}");
    assert!(
        text.contains("host: call deadline 2000 ms, first retry 1000 ms, retry cap 8000 ms"),
        "{text}"
    );
    assert!(text.contains("auto-off: none"), "{text}");
}

#[test]
fn validate_shows_what_the_host_section_and_the_devices_set() {
    let text = validate(&format!(
        "{HEAD}[host]\ncall_deadline_ms = 500\nretry_first_ms = 250\nretry_cap_ms = 4000\n\
         [[devices]]\nid = \"a\"\ndriver = \"x\"\nauto_off_after = 3\n\
         [[devices]]\nid = \"b\"\ndriver = \"x\"\n"
    ))
    .unwrap();
    assert!(
        text.contains("host: call deadline 500 ms, first retry 250 ms, retry cap 4000 ms"),
        "{text}"
    );
    assert!(text.contains("auto-off: a after 3\n"), "{text}");
}

#[test]
fn validate_refuses_a_retry_cap_past_eight_seconds_and_says_why() {
    let error = validate(&format!("{HEAD}[host]\nretry_cap_ms = 9000\n")).unwrap_err();
    assert!(error.contains("retry_cap_ms"), "{error}");
    assert!(error.contains("8 s"), "{error}");
}

#[test]
fn validate_refuses_bad_host_settings_by_name() {
    for (keys, named) in [
        ("[host]\ncall_deadline_ms = 0\n", "call_deadline_ms"),
        (
            "[host]\nretry_first_ms = 5000\nretry_cap_ms = 1000\n",
            "retry_first_ms",
        ),
        ("[host]\nauto_off = true\n", "auto_off"),
        (
            "[[devices]]\nid = \"a\"\ndriver = \"x\"\nauto_off_after = 0\n",
            "auto_off_after",
        ),
    ] {
        let error = validate(&format!("{HEAD}{keys}")).unwrap_err();
        assert!(error.contains(named), "{keys}: {error}");
    }
}

#[test]
fn validate_names_a_file_it_cant_read() {
    let mut out = Vec::new();
    let error = lemnos_ctl::run(
        &[
            "validate".to_string(),
            "/nonexistent/board.toml".to_string(),
        ],
        &mut out,
    )
    .unwrap_err();
    assert!(error.starts_with("/nonexistent/board.toml: "), "{error}");
    assert!(lemnos_ctl::run(&["validate".to_string()], &mut out).is_err());
}

/// Counts to what it is asked, one step per millisecond, and ends with the count. Asked for 0,
/// it fails.
#[lemnos_core::contract("counting", version = 1)]
pub trait Counting {
    #[method(1, job)]
    fn count(&mut self, to: u32) -> lemnos_core::Result<lemnos_core::JobId>;
}

#[lemnos_core::driver("counting", provides = [Counting])]
pub struct Counter {
    counting: Option<(u32, u32)>,
}

impl lemnos_core::Lifecycle for Counter {
    type Needs = CounterNeeds;
    const TICKS: bool = true;
    const JOBS: bool = true;

    fn start(_: CounterNeeds, _: &lemnos_core::Config<'_>) -> lemnos_core::Result<Self> {
        Ok(Self { counting: None })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let (done, to) = self.counting.as_mut()?;
        if *done >= *to {
            return None;
        }
        *done += 1;
        Some(now_ns + 1_000_000)
    }

    fn job(
        &mut self,
        _: lemnos_core::JobId,
        result: &mut lemnos_core::Reply<'_>,
    ) -> lemnos_core::JobState {
        use lemnos_core::{Encode, JobState, Status};
        match self.counting {
            Some((_, 0)) => {
                self.counting = None;
                JobState::Finished(Status::INVALID_ARGUMENT)
            }
            Some((done, to)) if done < to => JobState::Running {
                done: u64::from(done),
                total: u64::from(to),
            },
            Some((done, _)) => {
                self.counting = None;
                JobState::Finished(match done.encode(result) {
                    Ok(()) => Status::OK,
                    Err(status) => status,
                })
            }
            None => JobState::Finished(Status::NOT_FOUND),
        }
    }
}

impl Counting for Counter {
    fn count(&mut self, to: u32) -> lemnos_core::Result<lemnos_core::JobId> {
        self.counting = Some((0, to));
        Ok(lemnos_core::JobId(1))
    }
}

fn start_counting() -> (Running, PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-jobs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
        [[devices]]\nid = \"counter\"\ndriver = \"counting\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<Counter>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    (
        Running {
            _service: service,
            socket,
        },
        dir,
    )
}

#[test]
fn a_job_method_runs_as_a_job_with_its_progress() {
    let (running, dir) = start_counting();
    let described = ctl(&running, &["describe", "counter"]).unwrap();
    assert!(described.contains("-> job"), "{described}");
    let text = ctl(&running, &["call", "counter", "count", "20"]).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with("job 0x") && lines[0].ends_with(" started"),
        "{text}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("progress ") && l.ends_with("/20")),
        "{text}"
    );
    assert!(
        lines.last().unwrap().ends_with(" done: 14 00 00 00"),
        "{text}"
    );
    let failed = ctl(&running, &["call", "counter", "count", "0"]).unwrap_err();
    assert!(failed.ends_with("failed: status -22"), "{failed}");
    drop(running);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn validate_refuses_a_link_to_a_device_the_board_does_not_have() {
    let error = validate(&format!(
        "{HEAD}[[devices]]\nid = \"cam0-sensor\"\ndriver = \"ov9782\"\nlink = \"cam0-recevier\"\n"
    ))
    .unwrap_err();
    assert!(error.contains("\"cam0-sensor\""), "{error}");
    assert!(error.contains("\"cam0-recevier\""), "{error}");
    assert!(error.contains("doesn't have"), "{error}");
}

#[test]
fn validate_refuses_a_link_to_the_device_itself() {
    let error = validate(&format!(
        "{HEAD}[[devices]]\nid = \"a\"\ndriver = \"x\"\nlink = \"a\"\n"
    ))
    .unwrap_err();
    assert!(error.contains("linked to itself"), "{error}");
}

#[test]
fn validate_accepts_links_between_devices_the_board_has() {
    validate(&format!(
        "{HEAD}[[devices]]\nid = \"s\"\ndriver = \"x\"\nlink = \"r\"\n\
         [[devices]]\nid = \"r\"\ndriver = \"x\"\n"
    ))
    .unwrap();
}
