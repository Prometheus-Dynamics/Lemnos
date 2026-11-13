//! The daemon stops cleanly on the signals an init system sends.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";
const PATIENCE: Duration = Duration::from_secs(20);

fn eventually(mut done: impl FnMut() -> bool, what: &str) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < PATIENCE, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn stops_on(signal: &str, backend: &[&str]) {
    let dir = std::env::temp_dir().join(format!(
        "lemnosd-signal-{}-{signal}-{}",
        std::process::id(),
        backend.len()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let board = dir.join("board.toml");
    std::fs::write(&board, BOARD).unwrap();
    let socket = dir.join("sock");
    let mut daemon = Reaped(
        Command::new(env!("CARGO_BIN_EXE_lemnosd"))
            .args(backend)
            .arg("--board")
            .arg(&board)
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut client = None;
    eventually(
        || {
            client = UnixStream::connect(&socket).ok();
            client.is_some()
        },
        "the daemon never listened",
    );
    let sent = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(daemon.0.id().to_string())
        .status()
        .unwrap();
    assert!(sent.success());
    let mut status = None;
    eventually(
        || {
            status = daemon.0.try_wait().unwrap();
            status.is_some()
        },
        "the daemon ignored the signal",
    );
    let status = status.unwrap();
    assert!(status.success(), "{signal}: {status}");
    assert!(!Path::new(&socket).exists(), "{signal} left the socket");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A daemon process that is always reaped: a test that fails before the daemon stops still
/// takes it down, so no daemon outlives the test binary.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
fn sigterm_stops_the_daemon_and_removes_the_socket() {
    stops_on("TERM", &["--sim"]);
}

#[test]
fn sigint_stops_the_daemon_and_removes_the_socket() {
    stops_on("INT", &["--sim"]);
}

#[test]
fn the_real_backend_is_the_default_and_stops_the_same_way() {
    stops_on("TERM", &[]);
}
