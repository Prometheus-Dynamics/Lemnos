//! The guest of `testing/uboot-grant/run.sh`: the daemon's `uboot` record writes its U-Boot environment
//! through the partition the root phase handed over, as the unit's user, and nothing else is reachable.
//!
//! Run as the guest's init (no argument): it mounts the kernel's file systems, makes the disk's nodes the
//! way the packaged udev rule and the kernel make them (root:disk 0660), then runs three checks in order:
//!   1. the unit's user (uid 1000, no supplementary group) is refused by name on the real disk node, on the
//!      environment's partition node and on the boot partition node (`probe`, a child as that user);
//!   2. the daemon (`daemon`, a root child): the root phase hands the environment partition over, the daemon
//!      drops to the user, its `set_default` goes through the handed-over partition, and the disk node is
//!      refused by name after the drop;
//!   3. the environment, read back by the guest's own root read (`reader`): the new default is there.
//!
//! The guest prints `uboot-grant: passed (N checks)` only when every check holds, and powers off.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use lemnos_driver_system_slots::boot::{BootRecord, Restarter};
use lemnos_driver_system_slots::uboot::{self, UbootEnv};
use lemnos_linux_sys::privilege::{Plan, drop_to};
use lemnos_slot_store::uboot_env::{self, FileEnv, Geometry, Place};
use lemnos_uboot_grant_check::{ENV_SIZE, SLOT};

/// The unit's user and group: the daemon's, and the user the probes run as.
const UNIT_UID: u32 = 1000;
/// The disk group udev grants the disk's nodes to (gid 6 on the board's distribution).
const DISK_GROUP: u32 = 6;
const DISK: &str = "/dev/vda";
const BOOT: &str = "/dev/vda1";
const ENV: &str = "/dev/vda2";
/// The errno a refused open gives.
const EACCES: i32 = 13;

fn say(line: &str) {
    let _ = writeln!(io::stdout(), "{line}");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(String::as_str) {
        None => return init(),
        Some("probe") => probe(args.get(2).map_or("", String::as_str)),
        Some("daemon") => daemon().map_err(|why| say(&format!("daemon: {why}"))),
        Some("reader") => reader().map_err(|why| say(&format!("reader: {why}"))),
        Some(other) => {
            say(&format!("unknown mode {other}"));
            Err(())
        }
    };
    if code.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Opens `node` read-write as this process is, and says the errno, or that it opened.
fn probe(node: &str) -> Result<(), ()> {
    let uid = rustix::process::getuid().as_raw();
    match OpenOptions::new().read(true).write(true).open(node) {
        Ok(_) => {
            say(&format!("probe uid {uid} {node} opened"));
            Ok(())
        }
        Err(e) => {
            say(&format!(
                "probe uid {uid} {node} errno {}",
                e.raw_os_error().unwrap_or(0)
            ));
            Ok(())
        }
    }
}

/// The daemon's `uboot` record, as the daemon builds it: the boot disk, the environment's place and the boot partition.
fn record() -> Result<UbootEnv, String> {
    Ok(UbootEnv {
        disk: Ok(PathBuf::from(DISK)),
        place: Place::gpt(ENV_SIZE)?,
        restarter: Restarter {
            systemctl: "/nonexistent/systemctl".into(),
            check: None,
        },
        boot_node: Ok(PathBuf::from(BOOT)),
        partitions: None,
    })
}

/// The daemon's start as root, then its write as the unit's user.
fn daemon() -> Result<(), String> {
    let place = Place::gpt(ENV_SIZE)?;
    let line = uboot::root_phase(&place, &Ok(PathBuf::from(DISK)), &Ok(PathBuf::from(BOOT)));
    say(&format!("root phase: {line}"));
    drop_to(&Plan {
        uid: UNIT_UID,
        gid: UNIT_UID,
        groups: Vec::new(),
        keep: Vec::new(),
    })
    .map_err(|e| format!("the drop to uid {UNIT_UID}: {e}"))?;
    let uid = rustix::process::getuid().as_raw();
    say(&format!("daemon: running as uid {uid}"));
    let mut record = record()?;
    record
        .set_default(SLOT)
        .map_err(|e| format!("set_default {SLOT}: {e}"))?;
    say(&format!("daemon: set_default {SLOT} ok"));
    match FileEnv::open(Path::new(DISK)) {
        Ok(_) => Err("the disk node opened by name after the drop".into()),
        Err(e) => {
            say(&format!(
                "daemon: the disk node by name errno {}",
                e.raw_os_error().unwrap_or(0)
            ));
            Ok(())
        }
    }
}

/// The environment as root reads it back after the daemon has gone: the default and its bay.
fn reader() -> Result<(), String> {
    let (mut dev, bytes) =
        FileEnv::open_sized(Path::new(ENV)).map_err(|e| format!("{ENV}: {e}"))?;
    let geometry = Geometry::in_partition(ENV_SIZE, 0, bytes / 512)?;
    let (_, vars) = uboot_env::load_checked(&mut dev, &geometry)?;
    let get = |name: &str| {
        vars.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    say(&format!(
        "reader: default={} default_bay={}",
        get("default"),
        get("default_bay")
    ));
    if get("default") == SLOT && !get("default_bay").is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the environment reads default {:?}",
            get("default")
        ))
    }
}

/// Mounts what the checks need, makes the nodes as the packaged rule does, and runs the checks.
fn init() -> ExitCode {
    let _ = rustix::mount::mount(
        "proc",
        "/proc",
        "proc",
        rustix::mount::MountFlags::empty(),
        None::<&std::ffi::CStr>,
    );
    let _ = rustix::mount::mount(
        "sysfs",
        "/sys",
        "sysfs",
        rustix::mount::MountFlags::empty(),
        None::<&std::ffi::CStr>,
    );
    let _ = rustix::mount::mount(
        "devtmpfs",
        "/dev",
        "devtmpfs",
        rustix::mount::MountFlags::empty(),
        None::<&std::ffi::CStr>,
    );
    let mut checks = 0u32;
    let mut failed: Vec<String> = Vec::new();
    for node in [DISK, BOOT, ENV] {
        if !wait_for(node) {
            failed.push(format!("{node} never appeared"));
        }
    }
    if !failed.is_empty() {
        return finish(checks, failed);
    }
    // The packaged rule's grant for the disk's nodes (root:disk 0660): the daemon has no disk group.
    for node in [DISK, BOOT, ENV] {
        let mode = fs::metadata(node).map(|m| m.mode() & 0o7777).unwrap_or(0);
        if let Err(e) = chown(node, Some(0), Some(DISK_GROUP))
            .and_then(|()| fs::set_permissions(node, fs::Permissions::from_mode(0o660)))
        {
            failed.push(format!(
                "{node}: the node's owner and mode: {e} (was {mode:o})"
            ));
        }
    }
    let me = std::env::current_exe().map(|p| p.to_string_lossy().into_owned());
    let Ok(me) = me else {
        failed.push("the guest's own path is unknown".into());
        return finish(checks, failed);
    };
    // 1. The unit's user is refused by name on each real node (the disk, the environment's partition, the boot partition).
    for node in [DISK, ENV, BOOT] {
        checks += 1;
        match run_as_unit(&me, &["probe", node]) {
            Ok(out) if out.contains(&format!("{node} errno {EACCES}")) => say(out.trim()),
            Ok(out) => failed.push(format!("probe {node}: expected EACCES: {out}")),
            Err(why) => failed.push(format!("probe {node}: {why}")),
        }
    }
    // 2. The daemon: the root phase, the drop, the write through the handed-over partition, the disk refused.
    checks += 1;
    match Command::new(&me).arg("daemon").status() {
        Ok(status) if status.success() => {}
        Ok(status) => failed.push(format!("daemon exited with {status}")),
        Err(e) => failed.push(format!("daemon: {e}")),
    }
    // 3. The environment, read back as root.
    checks += 1;
    match Command::new(&me).arg("reader").status() {
        Ok(status) if status.success() => {}
        Ok(status) => failed.push(format!("reader exited with {status}")),
        Err(e) => failed.push(format!("reader: {e}")),
    }
    finish(checks, failed)
}

/// Runs this program as the unit's user, with `args`, and returns what it said.
fn run_as_unit(me: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(me)
        .args(args)
        .uid(UNIT_UID)
        .gid(UNIT_UID)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("exited with {}: {text}", out.status));
    }
    Ok(text)
}

/// Waits for a node to appear (the kernel makes it when the disk is scanned), for a bounded time.
fn wait_for(node: &str) -> bool {
    for _ in 0..200 {
        if Path::new(node).exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn finish(checks: u32, failed: Vec<String>) -> ExitCode {
    rustix::fs::sync();
    if failed.is_empty() {
        say(&format!("uboot-grant: passed ({checks} checks)"));
    } else {
        for why in &failed {
            say(&format!("uboot-grant: failed: {why}"));
        }
        say(&format!(
            "uboot-grant: failed ({} of {checks} checks)",
            failed.len()
        ));
    }
    let _ = rustix::system::reboot(rustix::system::RebootCommand::PowerOff);
    ExitCode::FAILURE
}
