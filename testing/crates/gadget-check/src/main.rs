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
use std::time::{Duration, Instant};

use lemnos_linux_sys::usbfs::UsbfsFile;
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
config = { vendor = 0x1209, product = 0x0001, manufacturer = "Lemnos", product_name = "Gadget check", identity = "identity", interfaces = "function ffs\ninterface 0xff 0 0 gadget-check\nendpoint in bulk\nendpoint out bulk\nfunction ncm derive ifname=lemnos%d\nfunction acm\n" }
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
/// The interfaces the host must see, in order: the vendor-class FunctionFS link, then the CDC NCM
/// control and data interfaces, then the CDC ACM control and data interfaces. Class, subclass.
const INTERFACES: [(u8, u8); 5] = [
    (0xff, 0x00),
    (0x02, 0x0d),
    (0x0a, 0x00),
    (0x02, 0x02),
    (0x0a, 0x00),
];

/// A line on standard output, which the harness reads.
macro_rules! say {
    ($($arg:tt)*) => {
        { let _ = std::io::Write::write_fmt(&mut std::io::stdout(), format_args!("{}\n", format_args!($($arg)*))); }
    };
}

fn main() -> ExitCode {
    // The first argument decides which role this process plays; the children are this program.
    let role = std::env::args().nth(1);
    match role.as_deref() {
        Some("hub") => {
            hub::run();
            return ExitCode::SUCCESS;
        }
        Some("host") => {
            return if host_side() {
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

/// The host's side: find the device, check its interfaces, and loop the bulk pair.
fn host_side() -> bool {
    let Some(path) = find_device(Duration::from_secs(60)) else {
        say!("FAIL the host sees the gadget (1209:0001) on the bus");
        return false;
    };
    let usb = match UsbfsFile::open(&path) {
        Ok(usb) => usb,
        Err(error) => {
            say!("FAIL open {}: {error}", path.display());
            return false;
        }
    };
    let mut descriptors = [0u8; 4096];
    let n = usb.read_descriptors(0, &mut descriptors).unwrap_or(0);
    let seen = interfaces(&descriptors[..n]);
    let mut ok = seen == INTERFACES;
    say!(
        "{} the host sees {:02x?} (want {:02x?})",
        if ok { "ok  " } else { "FAIL" },
        seen,
        INTERFACES
    );
    if let Err(error) = usb.claim_interface(0) {
        say!("FAIL claim the vendor interface: {error}");
        return false;
    }
    let mut pattern = [0u8; CHUNK];
    let mut back = [0u8; CHUNK];
    for round in 0..ROUNDS {
        for (i, byte) in pattern.iter_mut().enumerate() {
            *byte = (round * 31 + i) as u8;
        }
        let sent = usb.bulk_out(EP_OUT, &pattern, 10_000);
        let got = usb.bulk_in(EP_IN, &mut back, 10_000);
        if sent.as_ref().ok() != Some(&CHUNK)
            || got.as_ref().ok() != Some(&CHUNK)
            || back != pattern
        {
            say!("FAIL round {round}: sent {sent:?}, received {got:?}");
            ok = false;
            break;
        }
    }
    if ok {
        say!("ok   {ROUNDS} rounds of {CHUNK} bytes went out and came back");
    }
    ok
}

/// The bus path of the device with the gadget's vendor and product, when it is listed.
fn find_device(within: Duration) -> Option<PathBuf> {
    let until = Instant::now() + within;
    loop {
        if let Some(path) = scan() {
            return Some(path);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn scan() -> Option<PathBuf> {
    for entry in fs::read_dir("/sys/bus/usb/devices").ok()?.flatten() {
        let dir = entry.path();
        let text = |name: &str| {
            fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_string())
        };
        if text("idVendor").as_deref() != Some("1209")
            || text("idProduct").as_deref() != Some("0001")
        {
            continue;
        }
        let bus: u32 = text("busnum")?.parse().ok()?;
        let dev: u32 = text("devnum")?.parse().ok()?;
        let path = PathBuf::from(format!("/dev/bus/usb/{bus:03}/{dev:03}"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// The (class, subclass) of each interface setting 0 in the descriptors, in order.
fn interfaces(bytes: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 2 <= bytes.len() {
        let len = usize::from(bytes[at]);
        if len < 2 || at + len > bytes.len() {
            break;
        }
        // Interface descriptor: type 4, alternate setting at 3, class at 5, subclass at 6.
        if bytes[at + 1] == 4 && len >= 9 && bytes[at + 3] == 0 {
            out.push((bytes[at + 5], bytes[at + 6]));
        }
        at += len;
    }
    out
}
