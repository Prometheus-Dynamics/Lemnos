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
//! - `daemon-stop`: root 0600, and lemnosd stops itself at the end of its root phase (`LEMNOSD_STOP_AFTER`,
//!   see [`stop_point`]), the daemon hanging before it is healthy. Nothing feeds after the root phase's
//!   feed, and the watchdog must reset the guest one period after that feed;
//! - `daemon-loop`: root 0600, and lemnosd stops itself at the end of its root phase on every start, and is
//!   killed and started again a second later (the unit's restart). Only the boot's first start holds and
//!   feeds; the watchdog must reset the guest one period after that feed. The node is root's so that only a
//!   root-phase hold can feed: a restart's bind could open the node by name, and an open of a running
//!   watchdog by name feeds it as well.
//!
//! The stop modes do not race lemnosd. lemnosd stops itself right after the trace line that ends its stage
//! (the root phase's end, or a device's bind end), so nothing after the stage runs. The guest waits for that
//! stop (`wait_stop`), then reads what lemnosd wrote: the pipe holds all of it, and a line after the stop point
//! is a start that ran past it, which fails the mode.

use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::guest::{self, Waited};
use lemnos_linux_sys::poll::{POLLIN, poll_one};

use crate::{say, since};

const NODE: &str = "/dev/watchdog0";
/// The stand-in manager's notification socket: lemnosd's `NOTIFY_SOCKET`.
const NOTIFY: &str = "/run/fake-manager/notify";
const BOARD_PATH: &str = "/run/wdt-board.toml";
const DAEMON: u32 = 999;
/// How long the guest stays up on lemnosd's feeding: three times the hook's 15 s period.
const STAY_SECONDS: u64 = 45;
/// When `pid1-dead` exits PID 1, after lemnosd has fed the watchdog (its root phase is long over by then).
const PID1_EXIT_AFTER: u64 = 3;
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

/// The device after the watchdog in `daemon-loop-bind`: its bind hangs on the same FIFO as `IDENTITY_HANGS`. The
/// host binds in board order, so the watchdog's bind ends, the host waits in this device's bind (cut at the call
/// deadline, 2 s), and its first tick cannot come before then. The stand-in stops lemnosd at the watchdog's bind end,
/// so the stop always lands before any feed of that start.
const HANG_AFTER_WATCHDOG: &str = r#"
[[devices]]
id = "identity-hang"
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

/// The stand-in manager's descriptor store: the watchdog descriptors lemnosd stored (`FDSTORE=1`), kept
/// until the guest ends, and passed to each start of lemnosd (`LISTEN_FDS`), as systemd's store does
/// across a restart. There is no systemd in this guest, so this is the store's behaviour and no more.
type Store = Arc<Mutex<Vec<(String, OwnedFd)>>>;

/// Starts the manager's notification socket and its store. A message with `FDSTORE=1` keeps the
/// descriptors it carries under its `FDNAME`; any other message is not the manager's to act on.
fn start_manager() -> io::Result<Store> {
    let store: Store = Arc::new(Mutex::new(Vec::new()));
    let _ = fs::create_dir_all("/run/fake-manager");
    let socket = UnixDatagram::bind(NOTIFY)?;
    let kept = Arc::clone(&store);
    thread::spawn(move || {
        let mut buf = [0u8; 512];
        let mut fds = Vec::new();
        loop {
            fds.clear();
            let Ok(read) = lemnos_linux_sys::scm::recv_with_fds(socket.as_fd(), &mut buf, &mut fds)
            else {
                continue;
            };
            let message = String::from_utf8_lossy(&buf[..read]).into_owned();
            if !message.lines().any(|line| line == "FDSTORE=1") {
                continue;
            }
            let name = message
                .lines()
                .find_map(|line| line.strip_prefix("FDNAME="))
                .unwrap_or("unknown")
                .to_owned();
            let count = fds.len();
            if let Ok(mut kept) = kept.lock() {
                kept.extend(fds.drain(..).map(|fd| (name.clone(), fd)));
            }
            say(&format!(
                "user: manager: stored {count} descriptor(s) as {name}"
            ));
        }
    });
    Ok(store)
}

/// The stop point a mode gives lemnosd (`LEMNOSD_STOP_AFTER`), or none. `root-phase` stops it at the end of its root
/// phase, before any device is bound; `bind:watchdog` stops it at the end of the watchdog's bind, healthy or not.
fn stop_point(mode: &str) -> Option<&'static str> {
    match mode {
        "daemon-stop" | "pid1-hang-stop" | "daemon-loop" => Some("root-phase"),
        "daemon-loop-bind" => Some("bind:watchdog"),
        _ => None,
    }
}

/// Starts lemnosd as its unit does, with its error output piped. Its notification socket is the
/// manager's, and it is passed the stored descriptors the way the manager passes them on a restart.
/// With a stop point, lemnosd is told it (`LEMNOSD_STOP_AFTER`) and stops itself there.
fn start_lemnosd(store: &Store, stop: Option<&str>) -> io::Result<Child> {
    let mut command = Command::new("/sbin/lemnos-unit-check");
    command
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
        .env("NOTIFY_SOCKET", NOTIFY)
        .stderr(Stdio::piped());
    if let Some(point) = stop {
        command.env("LEMNOSD_STOP_AFTER", point);
    }
    let passed = {
        let kept = store
            .lock()
            .map_err(|_| io::Error::other("the store is poisoned"))?;
        let mut passed = Vec::with_capacity(kept.len());
        for (name, fd) in kept.iter() {
            passed.push((name.clone(), fd.try_clone()?));
        }
        passed
    };
    lemnos_linux_sys::listen::pass_on(&mut command, passed);
    command.spawn()
}

/// Says one line of lemnosd's stderr on the console, and the piece of the gap it makes, if any. A piece is
/// stamped on CLOCK_MONOTONIC from the hook's last feed; `binds_before` counts the device binds begun so far in
/// this start.
fn relay(line: &str, last: f64, binds_before: &mut u32) {
    say(&format!("lemnosd| {line}"));
    let Some(rest) = line.strip_prefix("lemnosd: start trace: ") else {
        return;
    };
    let Some((stamp, event)) = rest.split_once(' ') else {
        return;
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
        *binds_before += 1;
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
    }
}

/// Whether a stderr line is the trace line that ends `point`'s stage (the line lemnosd stops after).
fn ends_stage(line: &str, point: &str) -> bool {
    let Some(rest) = line.strip_prefix("lemnosd: start trace: ") else {
        return false;
    };
    let Some((_, event)) = rest.split_once(' ') else {
        return false;
    };
    match point.strip_prefix("bind:") {
        Some(id) => event == format!("bind {id} ok") || event == format!("bind {id} failed"),
        None => event.starts_with("root phase done"),
    }
}

/// What a start that was given a stop point came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reached {
    /// Stopped at its stop point, and wrote nothing after it.
    Stopped,
    /// Ended before its stop point.
    Ended,
    /// Ran past its stop point, or stopped without it: said on the console, and the mode fails.
    Ran,
}

/// Waits until lemnosd is stopped (or has ended), then reads what it wrote and says it on the console. The pipe
/// holds everything lemnosd wrote, since a stopped process writes nothing more, so the reads take only what is
/// there (a zero-timeout poll before each): the wait is for the stop, never for the end of the pipe. A line after
/// the stop point, or a stop without it, is said as the start's failure ([`Reached::Ran`]).
fn at_stop_point(
    mode: &str,
    start: u32,
    pid: i32,
    stderr: &mut ChildStderr,
    point: &str,
    last: f64,
) -> Reached {
    let waited = match guest::wait_stop(pid) {
        Ok(waited) => waited,
        Err(e) => {
            say(&format!(
                "user: daemon ({mode}): waiting for lemnosd start {start} failed: {e}"
            ));
            return Reached::Ran;
        }
    };
    let lines = match drain(stderr) {
        Ok(lines) => lines,
        Err(e) => {
            say(&format!(
                "user: daemon ({mode}): reading lemnosd start {start}'s output failed: {e}"
            ));
            return Reached::Ran;
        }
    };
    let mut binds_before = 0;
    for line in &lines {
        relay(line, last, &mut binds_before);
    }
    if waited == Waited::Ended {
        return Reached::Ended;
    }
    let Some(at) = lines.iter().position(|line| ends_stage(line, point)) else {
        say(&format!(
            "user: daemon ({mode}): lemnosd start {start} stopped without its stop point ({point})"
        ));
        return Reached::Ran;
    };
    if let Some(after) = lines.get(at + 1) {
        say(&format!(
            "user: daemon ({mode}): lemnosd start {start} ran past its stop point ({point}): {after}"
        ));
        return Reached::Ran;
    }
    Reached::Stopped
}

/// Reads what is in `stderr` now, without waiting for more: a zero-timeout poll says whether a read would
/// return, so a writer that is still running is never waited for.
fn drain(stderr: &mut ChildStderr) -> io::Result<Vec<String>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if poll_one(stderr.as_fd(), POLLIN, Some(Duration::ZERO))? == 0 {
            break;
        }
        let n = stderr.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The stop modes' end: lemnosd ran past its stop point, which fails the mode. Says why and ends the guest, so
/// the host reads the failure at once.
fn fail_past_stop() -> ! {
    say("user: daemon: powering off after a failed start check (the line above names it)");
    let _ = guest::power_off();
    loop {
        thread::park();
    }
}

/// `daemon-loop` and `daemon-loop-bind`: lemnosd stops itself at its stop point on every start, and is killed
/// there and started again [`RESTART`] later, as `Restart=always` with `RestartSec=1s` does. Only the boot's first
/// start may feed (its root phase's hold); a restart must not, so the watchdog resets the guest one period after
/// that first feed. Never returns.
fn crash_loop(mode: &str, last: f64, store: &Store) -> ! {
    let Some(point) = stop_point(mode) else {
        loop {
            thread::park();
        }
    };
    let mut start = 0u32;
    // The point each start is killed at: after its root phase (`daemon-loop`), or after its watchdog's bind
    // has ended, healthy or not (`daemon-loop-bind`: the bind may have opened the node by name, and a restart's
    // open of a running watchdog feeds it).
    let bind = mode == "daemon-loop-bind";
    loop {
        start += 1;
        let mut child = match start_lemnosd(store, Some(point)) {
            Ok(child) => child,
            Err(e) => {
                say(&format!("user: daemon: lemnosd can't be started: {e}"));
                loop {
                    thread::park();
                }
            }
        };
        let pid = i32::try_from(child.id()).unwrap_or(0);
        let Some(mut stderr) = child.stderr.take() else {
            say("user: daemon: lemnosd's output is not piped");
            fail_past_stop();
        };
        match at_stop_point(mode, start, pid, &mut stderr, point, last) {
            Reached::Stopped => {}
            Reached::Ended => {
                say(&format!(
                    "user: daemon ({mode}): lemnosd start {start} ended before its stop point"
                ));
            }
            Reached::Ran => fail_past_stop(),
        }
        // Killed while stopped (or ended, not yet reaped: its pid is still its own), then reaped.
        let _ = child.kill();
        let _ = child.wait();
        if bind {
            say(&format!(
                "user: daemon ({mode}): lemnosd start {start} killed after its watchdog bind, {:.3} s after the hook's last feed",
                since(last)
            ));
        } else {
            say(&format!(
                "user: daemon ({mode}): lemnosd start {start} killed after its root phase, {:.3} s after the hook's last feed",
                since(last)
            ));
        }
        if since(last) > STAY_SECONDS as f64 {
            say(&format!(
                "user: daemon ({mode}): the guest stayed up through the restarts; powering off"
            ));
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
        "daemon" | "daemon-loop-bind" => give_node(0, DAEMON, 0o660),
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
    let after = if mode == "daemon-loop-bind" {
        HANG_AFTER_WATCHDOG
    } else {
        ""
    };
    if let Err(e) = fs::write(
        BOARD_PATH,
        format!("{BOARD_HEAD}{identity}{WATCHDOG}{after}"),
    ) {
        say(&format!(
            "user: daemon: the board file can't be written: {e}"
        ));
    }
    say(&format!(
        "user: daemon ({mode}): starting lemnosd under the unit, {:.3} s after the hook's last feed",
        since(last)
    ));
    let store = match start_manager() {
        Ok(store) => store,
        Err(e) => {
            say(&format!(
                "user: daemon: the manager's socket can't be made: {e}"
            ));
            loop {
                thread::park();
            }
        }
    };
    if mode == "daemon-loop" || mode == "daemon-loop-bind" {
        crash_loop(mode, last, &store);
    }
    let stop = stop_point(mode);
    let child = start_lemnosd(&store, stop);
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
    let label = mode.to_owned();
    thread::spawn(move || {
        let Some(mut stderr) = stderr else { return };
        if let Some(point) = stop {
            match at_stop_point(&label, 1, pid, &mut stderr, point, last) {
                Reached::Stopped => say(&format!(
                    "user: daemon ({label}): lemnosd stopped after its root phase's feed"
                )),
                Reached::Ended => say(&format!(
                    "user: daemon ({label}): lemnosd ended before its stop point"
                )),
                Reached::Ran => fail_past_stop(),
            }
            return;
        }
        let mut binds_before = 0u32;
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            relay(&line, last, &mut binds_before);
        }
    });
    if mode == "pid1-dead" {
        // PID 1 exits: the kernel panics ("Attempted to kill init") and, with panic=-1, reboots at once.
        // The daemon's feeding ends with the system, not by a timeout of its own.
        thread::sleep(Duration::from_secs(PID1_EXIT_AFTER));
        say("user: daemon (pid1-dead): PID 1 exits now");
        std::process::exit(0);
    }
    if mode.starts_with("pid1-hang") {
        // PID 1 hangs: its main thread blocks for good, reaping nothing. A helper thread ends the run
        // after STAY_SECONDS, with the power-off, so the check can tell a feed that went on from a reset.
        say(&format!(
            "user: daemon ({mode}): PID 1 is hung (its main thread blocks; nothing reaps or waits)"
        ));
        let label = mode.to_owned();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(STAY_SECONDS));
            say(&format!(
                "user: daemon ({label}): the guest stayed up {STAY_SECONDS} s with PID 1 hung; powering off"
            ));
            let _ = guest::power_off();
        });
        loop {
            thread::park();
        }
    }
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
