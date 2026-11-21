//! The `daemon` modes: the real lemnosd after the hook's hand-over, started as its unit starts it
//! (`lemnos-unit-check exec`, as 999:999, with the start trace on). It measures the unfed gap in its
//! pieces, each from the hook's last feed on CLOCK_MONOTONIC:
//!
//! - lemnosd's exec, its root phase's feed (the root phase opens the keep-alive watchdog as root and
//!   feeds it at once), and the end of its root phase (the drop included);
//! - the watchdog device's bind, as it begins and ends, and how many binds came before it;
//! - its first tick, the first feed after the drop.
//!
//! Then it stays up for [`STAY_SECONDS`]: the guest staying up is the proof that lemnosd feeds.
//! The node's owner is set first, by mode:
//!
//! - `daemon`: group 999, 0660, as the packaged udev rule leaves it before lemnosd starts;
//! - `daemon-late`: root 0600 at the start, given to the group [`LATE`] later, as a udev that has not
//!   finished its coldplug when lemnosd starts;
//! - `daemon-root`: root 0600 for good, as with no rule: the root phase's open is the only one that
//!   can work, and the bind takes its descriptor;
//! - `daemon-hang`: root 0600, and the device bound ahead of the watchdog hangs in its bind (its
//!   serial is read from a FIFO nobody writes). The host cuts the bind at its call deadline and binds
//!   the watchdog next: a hung bind delays the first tick by that deadline, and the daemon, healthy,
//!   feeds on;
//! - `daemon-stop`: root 0600, and lemnosd is stopped (SIGSTOP) as soon as its root phase has fed, the
//!   daemon hanging before it is healthy. Nothing feeds after the root phase's feed, and the watchdog
//!   must reset the guest one period after that feed;
//! - `daemon-loop`: root 0600, and lemnosd is killed at the end of its root phase on every start and
//!   started again a second later (the unit's restart). Only the boot's first start holds and feeds;
//!   the watchdog must reset the guest one period after that feed. The node is root's so that only a
//!   root-phase hold can feed: the kill can land after a restart's bind, and an open of a running
//!   watchdog by name feeds it as well.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::guest;

use crate::{say, since};

const NODE: &str = "/dev/watchdog0";
const BOARD_PATH: &str = "/run/wdt-board.toml";
const DAEMON: u32 = 999;
/// How long the guest stays up on lemnosd's feeding: three times the hook's 15 s period.
const STAY_SECONDS: u64 = 45;
/// The unit's restart delay (`RestartSec=1s`), for `daemon-loop`.
const RESTART: Duration = Duration::from_secs(1);
/// When udev "finishes" in `daemon-late`, after lemnosd is started.
const LATE: Duration = Duration::from_secs(3);

/// A board as a slot's: one device bound ahead of the watchdog (board order), then the watchdog.
/// [`IDENTITY`] or [`IDENTITY_HANGS`] goes first.
const BOARD_HEAD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "lemnos-qemu-watchdog"
"#;

const IDENTITY: &str = r#"
[[devices]]
id = "identity"
driver = "system-identity"
config = { serial = "cmdline", serial_key = "lemnos.serial" }
"#;

/// The identity read from the FIFO the guest's root carries (`/hang.fifo`, made by the run script):
/// its open blocks until a writer comes, and none does, so the bind never returns.
const IDENTITY_HANGS: &str = r#"
[[devices]]
id = "identity"
driver = "system-identity"
config = { serial = "nvmem", serial_path = "/hang.fifo", serial_length = 8 }
"#;

const WATCHDOG: &str = r#"
[[devices]]
id = "watchdog"
driver = "hw-watchdog"
keepalive = true
uses = { watchdog = "watchdog0" }
config = { timeout_s = 15 }
"#;

fn give_node(uid: u32, gid: u32, mode: u32) -> std::io::Result<()> {
    std::os::unix::fs::chown(NODE, Some(uid), Some(gid))?;
    fs::set_permissions(NODE, fs::Permissions::from_mode(mode))
}

/// Starts lemnosd as its unit does, with its error output piped.
fn start_lemnosd() -> std::io::Result<std::process::Child> {
    Command::new("/sbin/lemnos-unit-check")
        .args([
            "exec",
            "--runtime-dir",
            "/run/lemnos",
            "--",
            "/sbin/lemnosd",
        ])
        .args([
            "--board",
            BOARD_PATH,
            "--socket",
            "/run/lemnos/lemnosd.sock",
        ])
        .args(["--user", "999", "--group", "999"])
        .env("LEMNOSD_START_TRACE", "1")
        .stderr(Stdio::piped())
        .spawn()
}

/// `daemon-loop`: lemnosd dies at the end of its root phase, every start, and is started again
/// [`RESTART`] later, as `Restart=always` with `RestartSec=1s` does. Only the boot's first start may
/// feed (its root phase's hold); a restart must not, so the watchdog resets the guest one period after
/// that first feed. Never returns.
fn crash_loop(last: f64) -> ! {
    let mut start = 0u32;
    loop {
        start += 1;
        let mut child = match start_lemnosd() {
            Ok(child) => child,
            Err(e) => {
                say(&format!("user: daemon: lemnosd can't be started: {e}"));
                loop {
                    thread::park();
                }
            }
        };
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                say(&format!("lemnosd| {line}"));
                if line.contains(" root phase: watchdog fed") {
                    say(&format!(
                        "user: piece: root-phase feed at {:.3} s after the hook's last feed (start {start})",
                        since(last)
                    ));
                }
                if line.contains(" root phase done") {
                    break;
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        say(&format!(
            "user: daemon (daemon-loop): lemnosd start {start} killed after its root phase, {:.3} s after the hook's last feed",
            since(last)
        ));
        if since(last) > STAY_SECONDS as f64 {
            say(
                "user: daemon (daemon-loop): the guest stayed up through the restarts; powering off",
            );
            let _ = guest::power_off();
            loop {
                thread::park();
            }
        }
        thread::sleep(RESTART);
    }
}

/// Runs one `daemon` mode. Never returns while lemnosd feeds: it powers the guest off.
pub fn run(mode: &str, last: f64) -> ! {
    for (dir, opts) in [("/run", "mode=0755"), ("/tmp", "mode=1777")] {
        let _ = fs::create_dir_all(dir);
        if let Err(e) = guest::mount_data("tmpfs", dir, "tmpfs", opts) {
            say(&format!("user: daemon: mount {dir}: {e}"));
        }
    }
    let given = match mode {
        "daemon" => give_node(0, DAEMON, 0o660),
        _ => give_node(0, 0, 0o600),
    };
    if let Err(e) = given {
        say(&format!("user: daemon: setting {NODE}'s owner failed: {e}"));
    }
    let identity = if mode == "daemon-hang" {
        IDENTITY_HANGS
    } else {
        IDENTITY
    };
    if let Err(e) = fs::write(BOARD_PATH, format!("{BOARD_HEAD}{identity}{WATCHDOG}")) {
        say(&format!(
            "user: daemon: the board file can't be written: {e}"
        ));
    }
    say(&format!(
        "user: daemon ({mode}): starting lemnosd under the unit, {:.3} s after the hook's last feed",
        since(last)
    ));
    if mode == "daemon-loop" {
        crash_loop(last);
    }
    let child = start_lemnosd();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            say(&format!("user: daemon: lemnosd can't be started: {e}"));
            loop {
                thread::park();
            }
        }
    };
    if mode == "daemon-late" {
        thread::spawn(|| {
            thread::sleep(LATE);
            match give_node(0, DAEMON, 0o660) {
                Ok(()) => say(&format!(
                    "user: daemon: {NODE} given to the daemon's group (udev late) {:.3} s after start",
                    LATE.as_secs_f64()
                )),
                Err(e) => say(&format!("user: daemon: the late owner change failed: {e}")),
            }
        });
    }
    let stderr = child.stderr.take();
    let pid = i32::try_from(child.id()).unwrap_or(0);
    let stop = mode == "daemon-stop";
    thread::spawn(move || {
        let Some(stderr) = stderr else { return };
        let mut binds_before = 0u32;
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            say(&format!("lemnosd| {line}"));
            let Some(rest) = line.strip_prefix("lemnosd: start trace: ") else {
                continue;
            };
            let Some((stamp, event)) = rest.split_once(' ') else {
                continue;
            };
            let after = stamp.parse::<f64>().map_or(f64::NAN, |t| t - last);
            let piece = if event == "exec" {
                Some("exec".to_string())
            } else if event == "root phase: watchdog fed" {
                Some("root-phase feed".to_string())
            } else if event.starts_with("root phase done") {
                Some("drop".to_string())
            } else if event.starts_with("bind watchdog (") {
                Some(format!(
                    "watchdog bind begins ({binds_before} binds before it)"
                ))
            } else if event.starts_with("bind ") && event.contains(" begins ") {
                binds_before += 1;
                None
            } else if let Some(rest) = event.strip_prefix("bind watchdog ") {
                Some(format!("watchdog bind {rest}"))
            } else if event == "first tick watchdog returned" {
                Some("watchdog first feed".to_string())
            } else {
                None
            };
            if let Some(piece) = piece {
                say(&format!(
                    "user: piece: {piece} at {after:.3} s after the hook's last feed"
                ));
                if stop && piece == "root-phase feed" {
                    // `lemnos-unit-check exec` execs lemnosd: the child is lemnosd.
                    match guest::kill(pid, libc::SIGSTOP) {
                        Ok(()) => say(
                            "user: daemon (daemon-stop): lemnosd stopped after its root phase's feed",
                        ),
                        Err(e) => say(&format!("user: daemon: stopping lemnosd failed: {e}")),
                    }
                }
            }
        }
    });
    thread::sleep(Duration::from_secs(STAY_SECONDS));
    say(&format!(
        "user: daemon ({mode}): the guest stayed up {STAY_SECONDS} s on lemnosd's feeding; powering off"
    ));
    let _ = child.kill();
    let _ = guest::power_off();
    loop {
        thread::park();
    }
}
