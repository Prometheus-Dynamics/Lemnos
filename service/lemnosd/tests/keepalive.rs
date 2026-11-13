//! The daemon feeds a keep-alive device (a hardware watchdog) only while its liveness is good:
//! the control socket answering. The node is a plain file that counts the feeds.

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::start_board;
use lemnos_driver_hw_watchdog::HwWatchdog;
use lemnos_host::entry;
use lemnos_sim::SimBoard;

fn eventually(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A node directory with `dev/watchdog0`, a plain file, and no sysfs tree.
fn node(name: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("lemnosd-keepalive-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("dev")).unwrap();
    let file = root.join("dev/watchdog0");
    fs::write(&file, b"").unwrap();
    (root, file)
}

fn devices(root: &std::path::Path) -> String {
    format!(
        "[[devices]]\nid = \"dog\"\ndriver = \"hw-watchdog\"\nkeepalive = true\n\
         config = {{ device = \"watchdog0\", dev_root = \"{}\", sys_root = \"{}\" }}\n",
        root.join("dev").display(),
        root.join("sys").display(),
    )
}

#[test]
fn the_daemon_feeds_its_keepalive_while_its_socket_answers_and_stops_when_it_does_not() {
    let (root, file) = node("socket");
    let running = start_board(
        &devices(&root),
        vec![entry::<HwWatchdog>()],
        SimBoard::new(),
        None,
    );

    // The probe answers, the loop turns, and the device is fed.
    eventually(|| fs::metadata(&file).unwrap().len() > 0);

    // The socket stops answering: its path is gone, so the probe cannot connect. Within the
    // socket bound (5 s) the daemon is not healthy, and the device is not fed any more.
    fs::remove_file(&running.socket).unwrap();
    std::thread::sleep(Duration::from_secs(7));
    let stopped = fs::metadata(&file).unwrap().len();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        fs::metadata(&file).unwrap().len(),
        stopped,
        "no feed once the socket is silent"
    );

    drop(running);
    let _ = fs::remove_dir_all(&root);
}
