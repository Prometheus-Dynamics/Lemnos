//! `lemnos-ctl gpu` against the mock service: the state, a command run under
//! a lease, two runs that do not overlap, and the command's exit code.

use lemnosd::mock::{MockHardware, MockLemnosd};
use std::process::{Command, Output};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"
"#;

fn start(tag: &str) -> MockLemnosd {
    let root = std::env::temp_dir().join(format!("lemnos-ctl-gpu-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let drm = root.join("sys/class/drm/renderD128");
    let device = root.join("sys/devices/gpu");
    let driver = root.join("sys/drivers/v3d");
    for p in [&drm, &device, &driver] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::os::unix::fs::symlink(&device, drm.join("device")).unwrap();
    std::os::unix::fs::symlink(&driver, device.join("driver")).unwrap();
    MockLemnosd::start_in(root, BOARD, MockHardware::new()).unwrap()
}

fn ctl(service: &MockLemnosd, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lemnos-ctl"))
        .env("LEMNOSD_SOCKET", service.socket())
        .args(args)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn a_command_runs_under_the_lease_and_its_exit_code_is_kept() {
    let service = start("run");
    let free = ctl(&service, &["gpu"]);
    assert!(free.status.success());
    assert_eq!(stdout(&free), "v3d (v3d, /dev/dri/renderD128)\n  free\n");
    // The inner `gpu status` sees the outer run's lease.
    let me = env!("CARGO_BIN_EXE_lemnos-ctl");
    let held = ctl(
        &service,
        &["gpu", "run", "--label", "bench", "--", me, "gpu", "status"],
    );
    assert!(
        held.status.success(),
        "{}",
        String::from_utf8_lossy(&held.stderr)
    );
    let text = stdout(&held);
    assert!(
        text.contains("exclusive lemnos-ctl") && text.contains("bench") && text.contains("held"),
        "{text}"
    );
    let failed = ctl(&service, &["gpu", "run", "--", "sh", "-c", "exit 3"]);
    assert_eq!(failed.status.code(), Some(3));
    assert!(stdout(&ctl(&service, &["gpu"])).contains("free"));
    service.stop();
}

#[test]
fn a_second_run_waits_for_the_first_or_gives_up() {
    let service = start("wait");
    let me = env!("CARGO_BIN_EXE_lemnos-ctl");
    let mut first = Command::new(me)
        .env("LEMNOSD_SOCKET", service.socket())
        .args(["gpu", "run", "--", "sleep", "1"])
        .spawn()
        .unwrap();
    // Until the first holds it.
    for _ in 0..200 {
        if stdout(&ctl(&service, &["gpu"])).contains("held") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let gave_up = ctl(&service, &["gpu", "run", "--wait", "0.1", "--", "true"]);
    assert!(!gave_up.status.success());
    assert!(
        String::from_utf8_lossy(&gave_up.stderr).contains("not free"),
        "{}",
        String::from_utf8_lossy(&gave_up.stderr)
    );
    // Shared does not get in beside an exclusive holder either; with a wait it
    // runs once the first is done.
    let started = std::time::Instant::now();
    let waited = ctl(
        &service,
        &["gpu", "run", "--shared", "--wait", "10", "--", "true"],
    );
    assert!(waited.status.success());
    assert!(first.wait().unwrap().success());
    assert!(started.elapsed() >= std::time::Duration::from_millis(500));
    service.stop();
}
