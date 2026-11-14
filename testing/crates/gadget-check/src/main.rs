//! The USB gadget, end to end, in the QEMU harness (`testing/qemu`).
//!
//! `lemnos-gadget-check` runs as root in the guest, with configfs, FunctionFS and the dummy
//! controller (`dummy_hcd`) the kernel provides. The daemon is its own process:
//!
//! 1. This check starts `lemnosd` on the board file below, as root, with `--user 999`. The daemon's
//!    own privileged start makes the gadget's configfs tree and drops to uid 999; its gadget driver
//!    then enables the function on `dummy_udc.0`. The check reads the daemon's process status
//!    (`/proc/PID/status`) for the drop, and runs a `probe` child as the daemon's user to see the
//!    FunctionFS mount refused.
//! 2. The tree holds the derived NCM addresses and the board serial as the USB serial. The NCM
//!    interface has the name the board's template gives, and the kernel's name is read back.
//! 3. The host side, a child process, finds the device in sysfs and reads its descriptors over
//!    usbfs: a vendor-class interface, a CDC NCM pair and a CDC ACM pair. It claims the vendor
//!    interface and sends bytes out of the bulk OUT endpoint.
//! 4. A holder takes the bulk pair and END from the daemon through `lemnos-client`, and echoes the
//!    bytes from OUT to IN until the host has its bytes back.
//! 5. The holder checks (`holder.rs`) run on that hold: the host and bus events through the hub,
//!    the gadget's unbind, the daemon's process killed and started again (case 6), and the device
//!    switched off through the client (case 5). END is asserted in each case.
//!
//! Prints one `ok` or `FAIL` line per check and exits non-zero if any failed. `host`, `hub` and
//! `probe` are the roles of the child processes this program starts.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use lemnosd::privilege::KEEP;

mod daemon;
mod holder;
mod hub;
use daemon::{DAEMON_ID, Lemnosd};
use hub::Hub;

/// The board the check runs: the gadget derives its addresses from the identity device, whose
/// serial the kernel command line carries as `lemnos.serial=` (see `testing/qemu/run.sh`).
const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "lemnos-qemu"

[[devices]]
id = "identity"
driver = "system-identity"
config = { serial = "cmdline", serial_key = "lemnos.serial" }

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc:dummy_udc.0" }
config = { manufacturer = "Lemnos", product_name = "Gadget check", identity = "identity" }
"#;

/// Where the board file is written, and the daemon's socket beside it (the daemon makes its
/// socket's directory in its privileged start).
const BOARD_PATH: &str = "/run/lemnos-check/board.toml";
const SOCKET: &str = "/run/lemnos-check/lemnosd.sock";

/// The serial the command line gives, and the addresses it derives for board `lemnos-qemu`.
/// Computed with `hashlib` from the rule in `docs/boards.md`, not with this code.
const SERIAL: &str = "0123456789abcdef";
const HOST_MAC: &str = "e6:41:0a:6e:54:dc";
const DEVICE_MAC: &str = "e6:41:0a:6e:54:dd";

const MOUNTS: &str = "/dev/usb-ffs";
pub const UDC: &str = "dummy_udc.0";
pub const GADGET_DIR: &str = "/sys/kernel/config/usb_gadget/lemnos-dummy_udc.0";
/// `CAP_SYS_ADMIN`, the capability the FunctionFS mount needs and the drop must remove.
const CAP_SYS_ADMIN: u32 = 21;

/// Bulk endpoints: the gadget's IN endpoint is the first `endpoint in` (address 0x81), its OUT
/// endpoint the second (0x02), numbered from 1 in the order the board file lists them.
pub const EP_IN: u8 = 0x81;
pub const EP_OUT: u8 = 0x02;
/// The bytes moved, in packets of the high-speed bulk size.
pub const CHUNK: usize = 512;
const ROUNDS: usize = 8;
/// A line on standard output, which the harness reads.
macro_rules! say {
    ($($arg:tt)*) => {
        { let _ = std::io::Write::write_fmt(&mut std::io::stdout(), format_args!("{}\n", format_args!($($arg)*))); }
    };
}

mod contract;
mod host;

fn main() -> ExitCode {
    // The first argument decides which role this process plays; the children are this program.
    let role = std::env::args().nth(1);
    match role.as_deref() {
        Some("hub") => {
            hub::run();
            return ExitCode::SUCCESS;
        }
        Some("host") => {
            return if host::host_side() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Some("probe") => {
            return if probe() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        _ => {}
    }
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        say!("{} {what}", if ok { "ok  " } else { "FAIL" });
        failures += usize::from(!ok);
        ok
    };

    if let Err(error) =
        fs::create_dir_all("/run/lemnos-check").and_then(|()| fs::write(BOARD_PATH, BOARD))
    {
        say!("FAIL the board file is written: {error}");
        return ExitCode::FAILURE;
    }

    // The host is a separate process, as a host is. It starts before the daemon and runs as root,
    // which the check's host side needs for usbfs. The root child for the holder checks is started
    // here too, and keeps its root.
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/lemnos-gadget-check"));
    let mut host = match Command::new(&me).arg("host").spawn() {
        Ok(child) => child,
        Err(error) => {
            say!("FAIL the host side cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut hub = match Hub::start(&me) {
        Ok(hub) => hub,
        Err(error) => {
            say!("FAIL the root child for the holder checks cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };

    // 0. The controller is not there yet: the platform driver of `dummy_udc.0` lets go of it, so
    // `/sys/class/udc` lists none, and takes it back after the daemon has started (case 1b). The
    // gadget is then made by the helper's retry, not at the daemon's start.
    let late = fs::write("/sys/bus/platform/drivers/dummy_udc/unbind", "dummy_udc.0").is_ok()
        && !Path::new("/sys/class/udc/dummy_udc.0").exists();
    check(
        late,
        "the controller is taken away before the daemon starts (no entry in /sys/class/udc)",
    );

    // 1. The daemon, as its own process: its privileged start, the drop, and the gadget's enable.
    let mut daemon = match Lemnosd::start(
        &me.with_file_name("lemnosd"),
        Path::new(BOARD_PATH),
        Path::new(SOCKET),
    ) {
        Ok(daemon) => daemon,
        Err(error) => {
            say!("FAIL the daemon starts on the board file: {error}");
            return ExitCode::FAILURE;
        }
    };
    check(
        true,
        "the daemon's privileged start ran and it listens on its socket",
    );
    let Some(pid) = daemon.pid() else {
        say!("FAIL the daemon's process id is not known");
        return ExitCode::FAILURE;
    };
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let sets = capability_sets(&status);
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default()
        .to_string();
    check(
        uid == DAEMON_ID,
        "the daemon's process runs as uid 999 after the drop",
    );
    check(
        sets.len() == 5,
        "the daemon's /proc status lists the five capability sets",
    );
    for (name, mask) in &sets {
        check(
            mask & (1u64 << CAP_SYS_ADMIN) == 0,
            &format!("{name} has no CAP_SYS_ADMIN after the drop"),
        );
    }
    let keep: u64 = KEEP.iter().fold(0, |mask, &cap| mask | (1u64 << cap));
    let bounding = sets
        .iter()
        .find(|(name, _)| *name == "CapBnd")
        .map_or(u64::MAX, |(_, m)| *m);
    check(
        bounding & !keep == 0,
        "CapBnd keeps only the capabilities the daemon keeps",
    );
    // 1b. The controller is not there: the daemon runs and serves, and the gadget retries.
    if late {
        let list = |daemon: &Lemnosd| {
            daemon
                .connect()
                .ok()
                .and_then(|mut session| session.list().ok())
                .unwrap_or_default()
        };
        let retrying = daemon::wait_until(Duration::from_secs(20), || {
            list(&daemon)
                .iter()
                .any(|e| e.name == "gadget" && e.reason.starts_with("retrying: attempt "))
        });
        let entries = list(&daemon);
        let gadget = entries.iter().find(|e| e.name == "gadget");
        say!(
            "     gadget while the controller is away: {:?}",
            gadget.map(|e| (&e.state, &e.health, &e.reason))
        );
        check(
            retrying,
            "a gadget whose controller is not there yet is retrying, not refused: `retrying: attempt N, last reason: TEXT, next try in M ms`",
        );
        check(
            gadget.is_some_and(|e| {
                e.reason.contains("last reason: ") && e.reason.contains("next try in ")
            }),
            "the retry text names the reason and the time of the next try",
        );
        check(
            entries
                .iter()
                .any(|e| e.name == "identity" && e.health == lemnos_client::DeviceHealth::Ok),
            "the daemon keeps running and serves another device while the gadget retries",
        );
        check(
            !Path::new(GADGET_DIR).join("functions").exists(),
            "no tree is made while the controller is away",
        );
        let status = |comm: &str| processes(comm).into_iter().next();
        let (helper, parent) = (status("lemnosd-gadget"), status("lemnosd-gsuper"));
        say!("     helper {helper:?}, parent {parent:?}");
        check(
            helper.is_some_and(|(_, uid, caps, ppid)| {
                uid == 0 && caps == 1u64 << CAP_SYS_ADMIN && Some(ppid) == parent.map(|p| p.0)
            }),
            "the helper runs as root with CAP_SYS_ADMIN only, as the child of the root parent",
        );
        check(
            parent.is_some_and(|(_, uid, _, ppid)| uid == 0 && Some(ppid) == daemon.pid()),
            "the root parent runs as root, as the child of the daemon",
        );
        check(
            fs::write("/sys/bus/platform/drivers/dummy_udc/bind", "dummy_udc.0").is_ok(),
            "the controller appears (the platform driver takes dummy_udc.0 back)",
        );
        check(
            daemon::wait_until(Duration::from_secs(30), daemon::enabled),
            "the gadget comes up by retry once the controller is there, with no restart",
        );
        // The serial is readable here (the kernel command line), so this shows the gadget that came
        // up by retry reports `ok` with no serial note and carries the serial; the late-serial case
        // (unreadable at the daemon's start) is the driver's unit test, which a command line cannot
        // stage.
        let up_ok = daemon::wait_until(Duration::from_secs(20), || {
            list(&daemon)
                .iter()
                .any(|e| e.name == "gadget" && e.health == lemnos_client::DeviceHealth::Ok)
        });
        say!(
            "     gadget after the retry: {:?}",
            list(&daemon)
                .iter()
                .find(|e| e.name == "gadget")
                .map(|e| (&e.state, &e.health, &e.reason))
        );
        check(
            up_ok,
            "the gadget made by retry reports health ok, with no serial note",
        );
    }

    // 2. What the tree holds, and what the daemon enabled.
    let read = |path: &str| {
        fs::read_to_string(path)
            .map(|s| s.trim_end_matches(['\n', '\0']).to_string())
            .unwrap_or_default()
    };
    check(
        read(&format!("{GADGET_DIR}/strings/0x409/serialnumber")) == SERIAL,
        "the USB serial is the board serial",
    );
    say!(
        "     ncm.usb0 host_addr {:?}, dev_addr {:?}",
        read(&format!("{GADGET_DIR}/functions/ncm.usb0/host_addr")),
        read(&format!("{GADGET_DIR}/functions/ncm.usb0/dev_addr"))
    );
    check(
        read(&format!("{GADGET_DIR}/functions/ncm.usb0/host_addr")) == HOST_MAC,
        "the NCM host address is derived from the board identity",
    );
    check(
        read(&format!("{GADGET_DIR}/functions/ncm.usb0/dev_addr")) == DEVICE_MAC,
        "the NCM device address is derived from the board identity",
    );
    check(
        daemon::wait_until(Duration::from_secs(20), daemon::enabled),
        "the daemon enables the gadget on the dummy controller",
    );
    check(
        read(&format!("{GADGET_DIR}/functions/ncm.usb0/ifname")) == "lemnos0",
        "the NCM interface is named by the board's template, and the name is read back",
    );
    check(
        Path::new("/sys/class/net/lemnos0").exists(),
        "the guest has a network interface lemnos0",
    );

    // The mount point exists now (the helper made it), and the daemon's user is still refused.
    let refused = Command::new(&me)
        .arg("probe")
        .uid(999)
        .gid(999)
        .status()
        .is_ok_and(|status| status.success());
    check(
        refused,
        "a FunctionFS mount is refused to the daemon's user (EPERM)",
    );

    // 3 and 4. The holder takes the pair and END through the daemon's socket, and echoes the host's
    // bytes; the host side checks them.
    let hold = match holder::Hold::take(&daemon) {
        Ok(hold) => hold,
        Err(error) => {
            say!("FAIL the holder takes the bulk pair and END from the daemon: {error}");
            return ExitCode::FAILURE;
        }
    };
    check(
        true,
        "the holder takes the bulk pair and END from the daemon, through the client",
    );
    let moved = (0..ROUNDS).all(|_| holder::echo_round(&hold));
    check(
        moved,
        "bytes sent to the gadget came back on the IN endpoint",
    );
    let host_ok = host.wait().map(|status| status.success()).unwrap_or(false);
    check(
        host_ok,
        "the host side saw the interfaces and got its bytes back",
    );

    // 5. The holder checks, on the same hold, through the daemon's events.
    holder::run(&mut daemon, &mut hub, hold, &mut check);
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The processes whose name is `comm`: process id, user id, effective capabilities and parent.
fn processes(comm: &str) -> Vec<(u32, u32, u64, u32)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let name = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if name.trim() != comm {
            continue;
        }
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let field = |key: &str| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .map(|rest| rest.trim().to_string())
                .unwrap_or_default()
        };
        let uid = field("Uid:")
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(u32::MAX);
        let caps = u64::from_str_radix(&field("CapEff:"), 16).unwrap_or(u64::MAX);
        let ppid = field("PPid:").parse().unwrap_or(0);
        found.push((pid, uid, caps, ppid));
    }
    found
}

/// The five capability sets of `/proc/PID/status`, as masks: inheritable, permitted, effective,
/// bounding and ambient, in that order.
fn capability_sets(status: &str) -> Vec<(&'static str, u64)> {
    const NAMES: [&str; 5] = ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"];
    NAMES
        .iter()
        .filter_map(|&name| {
            let mask = status
                .lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
                .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok())?;
            Some((name, mask))
        })
        .collect()
}

/// The FunctionFS mount, tried as the daemon's user: refused (EPERM), as the drop must refuse it.
fn probe() -> bool {
    lemnos_linux_sys::mount::functionfs(UDC, &Path::new(MOUNTS).join(UDC), "")
        .err()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
}
