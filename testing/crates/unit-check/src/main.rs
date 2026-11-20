//! lemnosd's privileged start under the packaged unit, in the QEMU guest.
//!
//! The service manager starts lemnosd as root with the unit's capability bounding set and
//! `no_new_privs`, in a runtime directory it made itself, owned by root with the unit's mode. This
//! check does the same without a service manager: it makes the directory, and starts the daemon
//! through its own `exec` role, which drops every capability the unit's bounding set leaves out
//! (with the drop-ins named), sets `no_new_privs` and execs the program, keeping root. The daemon
//! must then reach its socket as its own user and group, and the group must be able to connect.
//!
//! The unit's other sandboxing (the system call filter, the read-only file system) is the service
//! manager's and is not carried out here; the capability set is what the root phase's steps need.
//!
//! Roles, by the first argument:
//! - none: the check.
//! - `exec [--with gadget] -- PROGRAM ARGS…`: become what the unit makes of root, then exec.
//! - `caps`: print this process's effective and bounding sets (the check reads them).
//! - `connect UID GID SOCKET`: drop to those ids and connect; exit 0 connected, 3 refused.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::time::Duration;

use lemnos_daemon_ready::Ready;
use lemnos_linux_sys::privilege::{self, Plan};

mod unit;

/// The runtime directory this check makes, standing for the unit's `/run/<RuntimeDirectory>`.
const DIR: &str = "/run/lemnos-unit";
const SOCKET: &str = "/run/lemnos-unit/lemnosd.sock";
const BOARD_PATH: &str = "/run/lemnos-unit-board.toml";
/// The daemon's ids, as numbers: the guest has no account database. The unit names `lemnos`.
const DAEMON: u32 = 999;
/// Another user in the daemon's group, and one in neither.
const GROUP_MEMBER: u32 = 998;
const STRANGER: u32 = 997;
const READY_WAIT: Duration = Duration::from_secs(20);

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "lemnos-qemu"

[[devices]]
id = "identity"
driver = "system-identity"
config = { serial = "cmdline", serial_key = "lemnos.serial" }
"#;

macro_rules! say {
    ($($arg:tt)*) => {
        { let _ = std::io::Write::write_fmt(&mut std::io::stdout(), format_args!("{}\n", format_args!($($arg)*))); }
    };
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("exec") => exec(&args[2..]),
        Some("caps") => caps(),
        Some("connect") => connect(&args[2..]),
        None => check(),
        Some(other) => {
            say!("lemnos-unit-check: unknown role {other}");
            ExitCode::FAILURE
        }
    }
}

/// The unit files for `--with` names: the base unit, then each drop-in in order.
fn files(with: &[&str]) -> Result<Vec<&'static str>, String> {
    let mut files = vec![unit::BASE];
    for name in with {
        match *name {
            "gadget" => files.push(unit::GADGET),
            other => return Err(format!("no drop-in called {other}")),
        }
    }
    Ok(files)
}

fn exec(args: &[String]) -> ExitCode {
    let Some(split) = args.iter().position(|a| a == "--") else {
        say!("lemnos-unit-check exec: no -- before the program");
        return ExitCode::FAILURE;
    };
    let (opts, program) = (&args[..split], &args[split + 1..]);
    let mut with = Vec::new();
    let mut it = opts.iter();
    while let Some(opt) = it.next() {
        match (opt.as_str(), it.next()) {
            ("--with", Some(name)) => with.push(name.as_str()),
            _ => {
                say!("lemnos-unit-check exec: unknown option {opt}");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some((prog, rest)) = program.split_first() else {
        say!("lemnos-unit-check exec: no program");
        return ExitCode::FAILURE;
    };
    let set = match files(&with).and_then(|files| {
        if unit::last(&files, "NoNewPrivileges") != Some("yes") {
            return Err(
                "the unit does not set NoNewPrivileges=yes, which this role always sets".into(),
            );
        }
        unit::bounding_set(&files)
    }) {
        Ok(set) => set,
        Err(error) => {
            say!("lemnos-unit-check exec: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Root to root: the ids and groups stay; the bounding, effective and permitted sets become the
    // unit's, and no_new_privs is set. What the service manager gives a unit with no User=.
    let groups = match privilege::observe() {
        Ok(now) => now.groups,
        Err(error) => {
            say!("lemnos-unit-check exec: read the process state: {error}");
            return ExitCode::FAILURE;
        }
    };
    let plan = Plan {
        uid: 0,
        gid: 0,
        groups,
        keep: set,
    };
    if let Err(error) = privilege::drop_to(&plan) {
        say!("lemnos-unit-check exec: take the unit's capability set: {error}");
        return ExitCode::FAILURE;
    }
    let error = Command::new(prog).args(rest).exec();
    say!("lemnos-unit-check exec: exec {prog}: {error}");
    ExitCode::FAILURE
}

fn caps() -> ExitCode {
    match privilege::observe() {
        Ok(now) => {
            say!(
                "uid {} effective {:#x} bounding {:#x} no_new_privs {}",
                now.euid,
                now.effective,
                now.bounding,
                now.no_new_privs
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            say!("lemnos-unit-check caps: {error}");
            ExitCode::FAILURE
        }
    }
}

fn connect(args: &[String]) -> ExitCode {
    let [uid, gid, socket] = args else {
        say!("lemnos-unit-check connect: want UID GID SOCKET");
        return ExitCode::FAILURE;
    };
    let (Ok(uid), Ok(gid)) = (uid.parse(), gid.parse()) else {
        say!("lemnos-unit-check connect: ids are numbers");
        return ExitCode::FAILURE;
    };
    let plan = Plan {
        uid,
        gid,
        groups: vec![gid],
        keep: Vec::new(),
    };
    if let Err(error) = privilege::drop_to(&plan) {
        say!("lemnos-unit-check connect: drop to {uid}:{gid}: {error}");
        return ExitCode::FAILURE;
    }
    match UnixStream::connect(socket) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return ExitCode::from(3);
        }
        Err(error) => {
            say!("lemnos-unit-check connect: {socket}: {error}");
            return ExitCode::FAILURE;
        }
        Ok(_) => {}
    }
    match lemnos_client::Session::connect(socket, "unit-check").and_then(|mut s| s.list()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            say!("lemnos-unit-check connect: the session as {uid}:{gid}: {error:?}");
            ExitCode::FAILURE
        }
    }
}

/// One daemon started through the `exec` role, killed by its PID when dropped.
struct Daemon(Option<Child>);

impl Daemon {
    fn start(me: &Path) -> Result<Self, String> {
        let _ = fs::remove_file(SOCKET);
        let ready = Ready::new()?;
        let mut command = Command::new(me);
        command
            .arg("exec")
            .arg("--")
            .arg(me.with_file_name("lemnosd"))
            .args(["--board", BOARD_PATH, "--socket", SOCKET])
            .args([
                "--user",
                &DAEMON.to_string(),
                "--group",
                &DAEMON.to_string(),
            ]);
        ready.give(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("start lemnosd: {error}"))?;
        match ready.wait(&mut child, READY_WAIT) {
            Ok(()) => Ok(Self(Some(child))),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The owner, group and mode bits of `path`.
fn stat(path: &str) -> Option<(u32, u32, u32)> {
    fs::metadata(path)
        .ok()
        .map(|m| (m.uid(), m.gid(), m.mode() & 0o7777))
}

fn check() -> ExitCode {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        say!("{} {what}", if ok { "ok  " } else { "FAIL" });
        failures += usize::from(!ok);
        ok
    };
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/lemnos-unit-check"));
    let base = [unit::BASE];
    let set = unit::bounding_set(&base).unwrap_or_default();
    let mode =
        unit::last(&base, "RuntimeDirectoryMode").and_then(|m| u32::from_str_radix(m, 8).ok());
    check(
        unit::last(&base, "RuntimeDirectory") == Some("lemnos") && mode == Some(0o750),
        "the unit makes the runtime directory lemnos with mode 0750",
    );
    check(
        unit::last(&base, "ExecStart").is_some_and(|e| e.contains("--user lemnos --group lemnos")),
        "the unit starts the daemon as root with --user and --group",
    );
    check(
        !set.contains(&3) && !set.is_empty(),
        "the unit's bounding set leaves out CAP_FOWNER",
    );

    // The stand-in drops what the unit drops: a process it starts has exactly the unit's set.
    let want: u64 = set.iter().map(|&c| 1u64 << c).sum();
    let seen = Command::new(&me)
        .args(["exec", "--"])
        .arg(&me)
        .arg("caps")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let names: Vec<&str> = set.iter().map(|&c| unit::name(c)).collect();
    check(
        seen.trim() == format!("uid 0 effective {want:#x} bounding {want:#x} no_new_privs true"),
        &format!(
            "a program started as the unit starts it holds only {} ({})",
            names.join(" "),
            seen.trim()
        ),
    );

    // The guest's root has no /run until something makes it.
    if let Err(error) = fs::create_dir_all("/run").and_then(|()| fs::write(BOARD_PATH, BOARD)) {
        check(false, &format!("the board file is written: {error}"));
        return ExitCode::FAILURE;
    }
    // Run as a user and group connects; the result is the role's exit code.
    let connects = |uid: u32, gid: u32| {
        Command::new(&me)
            .args(["connect", &uid.to_string(), &gid.to_string(), SOCKET])
            .status()
            .ok()
            .and_then(|s| s.code())
    };
    let daemon_ids = (DAEMON, DAEMON, 0o750);

    // 1. A fresh start: the directory as the service manager makes it, root's, mode 0750.
    let _ = fs::remove_dir_all(DIR);
    let made = fs::create_dir(DIR)
        .and_then(|()| fs::set_permissions(DIR, fs::Permissions::from_mode(0o750)));
    check(
        made.is_ok() && stat(DIR) == Some((0, 0, 0o750)),
        "the runtime directory is made as the service manager makes it (root:root 0750)",
    );
    match Daemon::start(&me) {
        Ok(daemon) => {
            check(
                true,
                "lemnosd starts under the unit's bounding set on a fresh runtime directory and says it is ready",
            );
            check(
                stat(DIR) == Some(daemon_ids),
                &format!(
                    "the runtime directory is the daemon's, 0750 ({:?})",
                    stat(DIR)
                ),
            );
            check(
                stat(SOCKET).is_some_and(|(u, g, _)| (u, g) == (DAEMON, DAEMON)),
                "the socket is made by the daemon's user after the drop",
            );
            check(
                connects(DAEMON, DAEMON) == Some(0),
                "the daemon's user connects and lists",
            );
            check(
                connects(GROUP_MEMBER, DAEMON) == Some(0),
                "another user in the daemon's group connects and lists",
            );
            check(
                connects(STRANGER, STRANGER) == Some(3),
                "a user outside the group is refused at the directory",
            );
            drop(daemon);
        }
        Err(error) => {
            check(
                false,
                &format!(
                    "lemnosd starts under the unit's bounding set on a fresh runtime directory: {error}"
                ),
            );
        }
    }

    // 2. A start on the directory the last daemon left (the daemon's, 0750): nothing to change.
    check(
        Daemon::start(&me).is_ok() && stat(DIR) == Some(daemon_ids),
        "lemnosd starts again on the directory the last one left",
    );

    // 3. A directory the daemon's user owns with another mode: taken back, set, handed over.
    let set_mode = fs::set_permissions(DIR, fs::Permissions::from_mode(0o700));
    check(
        set_mode.is_ok() && Daemon::start(&me).is_ok() && stat(DIR) == Some(daemon_ids),
        &format!(
            "lemnosd starts on a directory its user owns at 0700, and leaves it 0750 ({:?})",
            stat(DIR)
        ),
    );

    let _ = fs::remove_dir_all(DIR);
    let _ = fs::remove_file(BOARD_PATH);
    say!(
        "lemnos-unit-check: {} ({failures} failed)",
        if failures == 0 { "passed" } else { "FAILED" }
    );
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
