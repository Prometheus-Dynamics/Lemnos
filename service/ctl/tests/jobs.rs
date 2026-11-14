//! A job open on a device shows in `list` and `describe` of the tool, from another session than the
//! one that started it; with no job the rows are what they were.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_fake_devices::FakeProbe;
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
        "lemnos-ctl-jobs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
        [[devices]]\nid = \"probe\"\ndriver = \"fake-probe\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "t".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<FakeProbe>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    Running {
        _service: service,
        socket,
    }
}

fn ctl(socket: &std::path::Path, words: &[&str]) -> Result<String, String> {
    let mut args = vec!["--socket".to_string(), socket.display().to_string()];
    args.extend(words.iter().map(ToString::to_string));
    let mut out = Vec::new();
    lemnos_ctl::run(&args, &mut out)?;
    Ok(String::from_utf8(out).unwrap())
}

#[test]
fn list_and_describe_say_a_job_is_running_and_say_nothing_otherwise() {
    let running = start();
    let before_list = ctl(&running.socket, &["list"]).unwrap();
    let before_describe = ctl(&running.socket, &["describe", "probe"]).unwrap();
    assert!(!before_list.contains("job "), "{before_list}");
    assert!(!before_describe.contains("job 0x"), "{before_describe}");

    // The job is started by a session of its own (the tool's `call` waits for it to end).
    let socket = running.socket.clone();
    let starter =
        std::thread::spawn(move || ctl(&socket, &["call", "probe", "run", "slow", "false"]));

    let deadline = Instant::now() + Duration::from_secs(20);
    let (list, describe) = loop {
        let list = ctl(&running.socket, &["list"]).unwrap();
        if list.contains("job 0x") {
            let describe = ctl(&running.socket, &["describe", "probe"]).unwrap();
            break (list, describe);
        }
        assert!(Instant::now() < deadline, "no job in list:\n{list}");
        std::thread::sleep(Duration::from_millis(5));
    };
    let row = list.lines().find(|l| l.starts_with("probe")).unwrap();
    assert!(
        row.starts_with(before_list.lines().next().unwrap().trim_end()),
        "{row}"
    );
    assert!(
        row.contains("  job 0x") && row.contains(" running"),
        "{row}"
    );
    // The same words in describe, after the items.
    assert!(
        describe.lines().last().unwrap().starts_with("job 0x"),
        "{describe}"
    );

    starter.join().unwrap().unwrap();
    // Over: the rows are as they were.
    assert_eq!(ctl(&running.socket, &["list"]).unwrap(), before_list);
    assert_eq!(
        ctl(&running.socket, &["describe", "probe"]).unwrap(),
        before_describe
    );
}
