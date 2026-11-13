//! The daemon, started as the binary is started, with a gadget whose identity device the board does
//! not have: the daemon stays up, names the refused device and its reason on its error output, and
//! the other device runs. `--check` still refuses that board, as `lemnos-ctl validate` does.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(20);
/// A gadget that names an identity device the board does not have, and a device that runs.
const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:udc0\" }\n\
    [devices.config]\nvendor = 4617\nproduct = 1\nidentity = \"nothing\"\n\
    interfaces = \"interface 0xff 0 0 pipe\\nendpoint in bulk\\nendpoint out bulk\\n\"\n\
    [[devices]]\nid = \"good\"\ndriver = \"gpu-lease\"\n";

/// The daemon, killed when the test ends. It is a child of this test, so only its own PID is used.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lemnosd-start-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let board = dir.join("board.toml");
    std::fs::write(&board, BOARD).unwrap();
    dir
}

fn spawn(dir: &Path, extra: &[&str]) -> (Daemon, std::path::PathBuf) {
    let stderr = dir.join("stderr.txt");
    let child = Command::new(env!("CARGO_BIN_EXE_lemnosd"))
        .args(["--sim", "--board"])
        .arg(dir.join("board.toml"))
        .arg("--socket")
        .arg(dir.join("sock"))
        .args(extra)
        .stdin(Stdio::null())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    (Daemon(child), stderr)
}

#[test]
fn a_gadget_with_no_identity_device_is_refused_and_the_daemon_stays_up() {
    let dir = scratch("identity");
    let (mut daemon, stderr) = spawn(&dir, &[]);
    let start = Instant::now();
    while UnixStream::connect(dir.join("sock")).is_err() {
        assert!(start.elapsed() < PATIENCE, "the daemon never listened");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Listening is not enough: a daemon that exited after listening would show here.
    std::thread::sleep(Duration::from_millis(300));
    let alive = daemon.0.try_wait().unwrap().is_none();
    let log = std::fs::read_to_string(&stderr).unwrap();
    drop(daemon);
    assert!(alive, "the daemon exited: {log}");
    assert!(
        log.contains(
            "device \"gadget\": `identity` names nothing, which the board file does not have"
        ),
        "{log}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn check_still_refuses_a_board_with_a_gadget_whose_identity_is_wrong() {
    let dir = scratch("check");
    let (mut daemon, stderr) = spawn(&dir, &["--check"]);
    let start = Instant::now();
    let status = loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < PATIENCE, "--check did not finish");
        std::thread::sleep(Duration::from_millis(10));
    };
    let log = std::fs::read_to_string(&stderr).unwrap();
    assert!(!status.success(), "--check accepted the board: {log}");
    assert!(log.contains("`identity` names nothing"), "{log}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A file that is not a board is the one start failure that refuses the whole file: the daemon exits
/// 1 with the file's name on its error output, and never listens.
#[test]
fn a_file_that_is_not_a_board_stops_the_daemon_at_start_and_names_it() {
    let dir = scratch("not-a-board");
    std::fs::write(dir.join("board.toml"), "this is = not [toml\n").unwrap();
    let (mut daemon, stderr) = spawn(&dir, &[]);
    let start = Instant::now();
    let status = loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < PATIENCE, "the daemon kept running");
        std::thread::sleep(Duration::from_millis(10));
    };
    let log = std::fs::read_to_string(&stderr).unwrap();
    assert_eq!(status.code(), Some(1), "{log}");
    assert!(log.contains("board.toml"), "{log}");
    assert!(
        !dir.join("sock").exists(),
        "a daemon that refused its file listened"
    );
    let _ = std::fs::remove_dir_all(dir);
}
