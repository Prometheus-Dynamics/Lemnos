//! The USB gadget, end to end, in the QEMU harness (`testing/qemu`).
//!
//! `lemnos-gadget-check` runs as root in the guest, with configfs, FunctionFS and the dummy
//! controller (`dummy_hcd`) the kernel provides:
//!
//! 1. The daemon's own setup: the board file is read and its gadget's serial resolved
//!    (`lemnosd::gadget::resolve`, from the kernel command line), then the root phase makes the
//!    configfs tree (`lemnosd::gadget::privileged_start`).
//! 2. The tree holds the derived NCM addresses and the board serial as the USB serial.
//! 3. The gadget is enabled on `dummy_udc.0` the way the daemon enables it
//!    (`lemnos_linux::FunctionFsGadget`), and waits for the host to configure it. The NCM
//!    function's interface is named from the board's `ifname=lemnos%d` template, and the name the
//!    kernel gave it is read back and checked against `lemnos0` in the guest.
//! 4. The host side, a child process, finds the device in sysfs and reads its descriptors over
//!    usbfs: a vendor-class interface, a CDC NCM pair and a CDC ACM pair.
//! 5. The host claims the vendor interface and sends bytes out of the bulk OUT endpoint; the
//!    gadget reads them and sends them back on the bulk IN endpoint; the host compares.
//!
//! Prints one `ok` or `FAIL` line per check and exits non-zero if any failed. `host` as the
//! first argument runs the host side alone (the child of step 4 and 5).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use lemnos_hal::UsbFunction;
use lemnos_hal::blocks::UsbFunctionEvent;
use lemnos_linux::FunctionFsGadget;
use lemnos_linux_sys::usbfs::UsbfsFile;
use lemnosd::gadget::{resolve, settings_for};
use lemnosd::privilege::{self, KEEP, Request};

mod holder;
mod hub;
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

/// The serial the command line gives, and the addresses it derives for board `lemnos-qemu`.
/// Computed with `hashlib` from the rule in `docs/boards.md`, not with this code.
const SERIAL: &str = "0123456789abcdef";
const HOST_MAC: &str = "e6:41:0a:6e:54:dc";
const DEVICE_MAC: &str = "e6:41:0a:6e:54:dd";

const CONFIGFS: &str = "/sys/kernel/config";
const MOUNTS: &str = "/dev/usb-ffs";
const UDC_CLASS: &str = "/sys/class/udc";
const UDC: &str = "dummy_udc.0";
const GADGET_DIR: &str = "/sys/kernel/config/usb_gadget/lemnos-dummy_udc.0";
/// The user and group the daemon runs as in the check: numbers, so no account database is needed.
const DAEMON_ID: &str = "999";
/// The socket's directory is made in the root phase, owned by the daemon's user.
const SOCKET: &str = "/run/lemnos-check/lemnosd.sock";
/// `CAP_SYS_ADMIN`, the capability the FunctionFS mount needs and the drop must remove.
const CAP_SYS_ADMIN: u32 = 21;

/// Bulk endpoints: the gadget's IN endpoint is the first `endpoint in` (address 0x81), its OUT
/// endpoint the second (0x02), numbered from 1 in the order the board file lists them.
const EP_IN: u8 = 0x81;
const EP_OUT: u8 = 0x02;
/// The bytes moved, in packets of the high-speed bulk size.
const CHUNK: usize = 512;
const ROUNDS: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(10);
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
    if role.as_deref() == Some("hub") {
        hub::run();
        return ExitCode::SUCCESS;
    }
    if role.as_deref() == Some("host") {
        return if host_side() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        say!("{} {what}", if ok { "ok  " } else { "FAIL" });
        failures += usize::from(!ok);
        ok
    };

    // The board file, with the gadget's serial read once, as the daemon does before its start.
    let mut board = match lemnos_host::board::parse(BOARD) {
        Ok(board) => board,
        Err(error) => {
            say!("FAIL the board file parses: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !check(resolve(&mut board).is_ok(), "the board serial is resolved") {
        return ExitCode::FAILURE;
    }

    // The host is a separate process, as a host is. It starts before the daemon's privileged start
    // and runs as root, which the check's host side needs for usbfs.
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/lemnos-gadget-check"));
    let mut host = match Command::new(&me).arg("host").spawn() {
        Ok(child) => child,
        Err(error) => {
            say!("FAIL the host side cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };

    // The root child for the holder checks: port controls, resets, configurations and mounts. It
    // is started here, before the daemon drops to its user, and keeps its root.
    let mut hub = match Hub::start(&me) {
        Ok(hub) => hub,
        Err(error) => {
            say!("FAIL the root child for the holder checks cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };

    // 1. The daemon's own privileged start: the root phase, then the drop to the daemon's user,
    // keeping KEEP, and the verification after the drop. This is what lemnosd runs.
    let request = Request {
        user: DAEMON_ID.to_string(),
        group: Some(DAEMON_ID.to_string()),
    };
    if let Err(reason) = privilege::start(&request, &board, Path::new(SOCKET)) {
        say!("FAIL the daemon's privileged start: {reason}");
        list("/sys/class/udc");
        list("/sys/kernel/config/usb_gadget/lemnos-dummy_udc.0/functions");
        list("/dev/usb-ffs/dummy_udc.0");
        return ExitCode::FAILURE;
    }
    check(true, "the root phase ran and the daemon dropped to uid 999");

    // 1b. What the drop left. The status file is the kernel's own account of the sets.
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let sets = capability_sets(&status);
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default()
        .to_string();
    check(
        uid == DAEMON_ID,
        "the process runs as uid 999 after the drop",
    );
    check(
        sets.len() == 5,
        "/proc/self/status lists the five capability sets",
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
    // A driver in the same process reaches mount(2), but without the capability it is refused.
    let refused = lemnos_linux_sys::mount::functionfs(UDC, &Path::new(MOUNTS).join(UDC), "")
        .err()
        .map(|error| error.kind());
    check(
        refused == Some(std::io::ErrorKind::PermissionDenied),
        "a FunctionFS mount is refused after the drop (EPERM)",
    );

    // 2. What the tree holds.
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

    // 3. The daemon's enable, on the dummy controller.
    let Some((set, functions)) = board
        .devices
        .iter()
        .find(|d| d.id == "gadget")
        .map(|device| settings_for(&board.board.id, device))
        .and_then(Result::ok)
    else {
        say!("FAIL the gadget's settings are read back");
        return ExitCode::FAILURE;
    };
    check(
        functions.as_slice().len() == 3,
        "three functions: ffs, ncm, acm",
    );
    let mut gadget = match FunctionFsGadget::new(
        Path::new(CONFIGFS),
        Path::new(MOUNTS),
        Path::new(UDC_CLASS),
        UDC,
    ) {
        Some(gadget) => gadget,
        None => {
            say!("FAIL {UDC} is not a controller name");
            return ExitCode::FAILURE;
        }
    };
    if !check(
        gadget.enable(set.bytes()).is_ok(),
        "the gadget is enabled on the dummy controller",
    ) {
        return ExitCode::FAILURE;
    }

    // The NCM interface has the name the board's template gives, and the kernel's choice is
    // read back: the first free `lemnos<N>` on a fresh guest is `lemnos0`.
    let mut name = [0u8; 16];
    let named = gadget
        .interface_name(&mut name)
        .ok()
        .flatten()
        .map(|len| String::from_utf8_lossy(&name[..len]).into_owned());
    check(
        named.as_deref() == Some("lemnos0"),
        "the NCM interface is named by the board's template, and the name is read back",
    );
    check(
        Path::new("/sys/class/net/lemnos0").exists(),
        "the guest has a network interface lemnos0",
    );

    // 4 and 5. The host runs beside us, and we serve the bulk pair until it is done.
    let configured = wait_configured(&mut gadget);
    check(configured, "the host configured the gadget");
    let moved = configured && serve(&mut gadget);
    check(
        moved,
        "bytes sent to the gadget came back on the IN endpoint",
    );
    let host_ok = host.wait().map(|status| status.success()).unwrap_or(false);
    check(
        host_ok,
        "the host side saw the interfaces and got its bytes back",
    );
    // 6. The holder checks: a process holding the bulk pair sees each event, and the gadget is
    // torn down by the last case.
    holder::run(&mut gadget, &mut hub, &mut check);
    let _ = gadget.disable();
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Reads events until the host chooses the configuration, or the wait runs out.
/// The five capability sets of `/proc/self/status`, as masks: inheritable, permitted, effective,
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

fn wait_configured(gadget: &mut FunctionFsGadget) -> bool {
    let until = Instant::now() + Duration::from_secs(60);
    while Instant::now() < until {
        match gadget.next_event() {
            Ok(Some(UsbFunctionEvent::Configured)) => return true,
            Ok(_) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return false,
        }
    }
    false
}

/// Moves each round's chunk from the OUT endpoint to the IN endpoint, and says whether every
/// chunk moved in full. The host checks the bytes themselves.
fn serve(gadget: &mut FunctionFsGadget) -> bool {
    let mut buffer = [0u8; CHUNK];
    for _ in 0..ROUNDS {
        let got = match gadget.read(EP_OUT, &mut buffer, TIMEOUT) {
            Ok(n) => n,
            Err(error) => {
                say!("FAIL the gadget reads the OUT endpoint: {error:?}");
                return false;
            }
        };
        if got != CHUNK {
            say!("FAIL the gadget read {got} bytes from OUT, not {CHUNK}");
            return false;
        }
        match gadget.write(EP_IN, &buffer[..got], TIMEOUT) {
            Ok(n) if n == got => {}
            other => {
                say!("FAIL the gadget writes the IN endpoint: {other:?}");
                return false;
            }
        }
    }
    true
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

/// The names in a directory, for a failure's diagnostics.
fn list(dir: &str) {
    let names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_else(|error| vec![error.to_string()]);
    say!("     {dir}: {names:?}");
}
