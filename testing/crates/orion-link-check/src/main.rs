#![allow(clippy::print_stderr)] // the harness reports its steps on stdout, by design
//! The USB link, end to end, in the QEMU harness (`testing/qemu`): the join that no other test
//! makes. The real `lemnosd` makes the gadget on the dummy controller (the default composition, whose
//! first interface is `orion-link`) and lends its bulk endpoints; the real `lemnos-orion` binary,
//! started with the `LEMNOS_ORION_LINKS` line that `lemnos-ctl tree --bridge-env` writes, takes them
//! and offers them to a comm core; and a host session, over the emulated bus (`dummy_hcd` loops the
//! gadget back to a host controller in the same guest), shakes hands with that comm core and
//! exchanges frames with it. The roles:
//!
//! - no argument: the check. It starts the comm core and the daemon, then
//!   1. the bridge with `LEMNOS_ORION_LINKS` unset: it says so once in its log, offers nothing, and
//!      the gadget's description says the pair is not lent (the negative);
//!   2. the bridge with the setting: the gadget is enabled, the bridge logs that it offered the
//!      endpoints, the description says the pair is lent, a host session comes up and exchanges
//!      ping and pong round trips with the node;
//!   3. the controller lets go of the gadget (the `UDC` attribute is cleared): the bridge gives the hold
//!      up and offers again once the gadget is up, and a second host session does the same.
//! - `comm`: the comm core. `host N`: the host session, with N round trips.
//!
//! Nothing here stops a process: the guest powers off when the checks end. Prints one `ok` or
//! `FAIL` line per check and exits non-zero if any failed.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// A line on standard output, which the harness reads.
macro_rules! say {
    ($($arg:tt)*) => {
        { let _ = std::io::Write::write_fmt(&mut std::io::stdout(), format_args!("{}\n", format_args!($($arg)*))); }
    };
}

mod bridge;
mod comm;
mod daemon;
mod host;

use bridge::Bridge;
use daemon::{Lemnosd, enabled, wait_until};

/// The board: the gadget with the default composition (no `interfaces` key), deriving its addresses
/// from the identity device, whose serial the kernel command line carries as `lemnos.serial=`.
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

const DIR: &str = "/run/lemnos-link";
const BOARD_PATH: &str = "/run/lemnos-link/board.toml";
const LEMNOSD_SOCKET: &str = "/run/lemnos-link/lemnosd.sock";
const COMM_SOCKET: &str = "/run/lemnos-link/orion.sock";

pub const UDC: &str = "dummy_udc.0";
pub const GADGET_DIR: &str = "/sys/kernel/config/usb_gadget/lemnos-dummy_udc.0";
/// The link is interface 0 of the default composition; its bulk endpoints are the first `endpoint
/// in` (0x81) and the second (0x02), numbered in the order the composition lists them.
pub const LINK_INTERFACE: u8 = 0;
pub const EP_IN: u8 = 0x81;
pub const EP_OUT: u8 = 0x02;

/// The round trips each host session makes.
const ROUNDS: u64 = 20;
const LONG: Duration = Duration::from_secs(60);
const OFFERED: &str = "link orion-link: offered the endpoints";
const WENT_DOWN: &str = "link orion-link: the gadget went down";
const NO_LINK: &str = "no USB link is configured (LEMNOS_ORION_LINKS is unset): none is offered";
/// The `pair_lent` method of `usb.function/1`.
const PAIR_LENT: u16 = 12;

/// The vendor and product numbers of the managed identity, from `contracts/usb-link.toml`.
pub fn identity() -> (u16, u16) {
    let table: toml::Table = include_str!("../../../../contracts/usb-link.toml")
        .parse()
        .expect("contracts/usb-link.toml is TOML");
    let number = |key: &str| {
        table["device"][key]
            .as_integer()
            .and_then(|n| u16::try_from(n).ok())
            .expect("the identity numbers")
    };
    (number("vendor"), number("product"))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("comm") => {
            let socket = args.next().unwrap_or_default();
            let links: Vec<String> = args.collect();
            comm::run(Path::new(&socket), &links);
            return ExitCode::SUCCESS;
        }
        Some("host") => {
            let rounds = args.next().and_then(|n| n.parse().ok()).unwrap_or(ROUNDS);
            let ok = host::run(rounds);
            // The shim's threads sit in transfers: leave without waiting for them.
            std::process::exit(i32::from(!ok));
        }
        _ => {}
    }
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        say!("{} {what}", if ok { "ok  " } else { "FAIL" });
        failures += usize::from(!ok);
        ok
    };
    if let Err(message) = check_link(&mut check) {
        say!("FAIL {message}");
        return ExitCode::FAILURE;
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The checks, in order. An `Err` is a step that could not be set up at all.
fn check_link(check: &mut dyn FnMut(bool, &str) -> bool) -> Result<(), String> {
    fs::create_dir_all(DIR)
        .and_then(|()| fs::write(BOARD_PATH, BOARD))
        .map_err(|error| format!("the board file is written: {error}"))?;
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/lemnos-orion-link-check"));
    let here = me
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);

    // The setting an image gets from `tree --bridge-env`, made here from the same board file.
    let board = lemnos_host::board::parse(BOARD).map_err(|error| format!("the board: {error}"))?;
    let written = lemnos_ctl::bridge_env::text(&board)?;
    let setting = written
        .lines()
        .find(|line| line.starts_with(lemnos_ctl::bridge_env::VARIABLE))
        .map(str::to_owned)
        .ok_or("tree --bridge-env wrote no LEMNOS_ORION_LINKS line for the gadget board")?;
    check(
        setting == "LEMNOS_ORION_LINKS=gadget=orion-link",
        &format!("tree --bridge-env gives {setting:?} for the default composition"),
    );

    // The comm core, which knows the USB link by the composition's own name.
    let _comm = Command::new(&me)
        .arg("comm")
        .arg(COMM_SOCKET)
        .arg("orion-link")
        .spawn()
        .map_err(|error| format!("start the comm core: {error}"))?;
    check(
        wait_until(Duration::from_secs(20), || Path::new(COMM_SOCKET).exists()),
        "the comm core listens on its socket and has a USB link named orion-link",
    );

    // The gadget check runs before this one in the harness and leaves its FunctionFS mount (holding
    // only ep0, from its last case) behind. A daemon started over that does not bind, so this check
    // starts from what a fresh boot has: the gadget unbound and nothing mounted. Both are the
    // harness's to do, as root, and are no step of the daemon's.
    let _ = fs::write(format!("{GADGET_DIR}/UDC"), "\n");
    let _ = lemnos_linux_sys::mount::unmount(Path::new("/dev/usb-ffs/dummy_udc.0"));

    // The daemon: its privileged start makes the gadget, and its driver enables it.
    let daemon = Lemnosd::start(
        &here.join("lemnosd"),
        Path::new(BOARD_PATH),
        Path::new(LEMNOSD_SOCKET),
    )?;
    if !check(
        wait_until(LONG, enabled),
        "the daemon enables the gadget on the dummy controller (default composition)",
    ) {
        diagnose();
    }
    let mut watcher = daemon
        .connect("link-check")
        .map_err(|error| format!("the daemon: {error}"))?;
    let device = watcher
        .find("usb.function/1")
        .map_err(|error| format!("find: {error}"))?
        .ok_or("the daemon lists no usb.function device")?
        .index;
    let mut lent = move || pair_lent(&mut watcher, device);

    // 1. Negative: no setting, no offer, said once.
    let bare = Bridge::start(
        &here.join("lemnos-orion"),
        Path::new(LEMNOSD_SOCKET),
        Path::new(COMM_SOCKET),
        None,
    )?;
    check(
        bare.wait_for(NO_LINK, 1, LONG),
        "without LEMNOS_ORION_LINKS the bridge says no USB link is configured",
    );
    std::thread::sleep(Duration::from_secs(5));
    check(
        bare.count(NO_LINK) == 1 && bare.count(OFFERED) == 0,
        "it says so once and offers nothing",
    );
    // A read can meet the daemon still busy with the bridge's own first reads, so the negative waits
    // for a definite "not lent" and says what it last saw when there is none.
    let mut seen = None;
    if !check(
        wait_until(Duration::from_secs(10), || {
            seen = lent();
            seen == Some(false)
        }),
        "the gadget's description says the pair is not lent",
    ) {
        say!("last read of pair_lent: {seen:?}");
    }

    // 2. The bridge with the setting.
    let bridge = Bridge::start(
        &here.join("lemnos-orion"),
        Path::new(LEMNOSD_SOCKET),
        Path::new(COMM_SOCKET),
        Some(&setting),
    )?;
    check(
        bridge.wait_for(OFFERED, 1, LONG),
        "the bridge logs: link orion-link: offered the endpoints",
    );
    check(
        enabled() && bridge.count(NO_LINK) == 0,
        "the gadget was enabled when the offer was made, and the bridge has a link configured",
    );
    check(
        wait_until(Duration::from_secs(10), || lent() == Some(true)),
        "the gadget's description says the pair is lent",
    );
    check(
        host_session(&me, 1),
        "host session 1: up over the emulated bus, and the round trips with the node",
    );

    // 3. The controller lets go; the link comes back.
    let cleared = fs::write(format!("{GADGET_DIR}/UDC"), "\n").is_ok();
    check(
        cleared,
        "the controller lets go of the gadget (UDC cleared)",
    );
    check(
        bridge.wait_for(WENT_DOWN, 1, LONG),
        "the bridge gives the hold up when the gadget goes down",
    );
    check(
        wait_until(LONG, enabled),
        "the daemon enables the gadget again",
    );
    check(
        bridge.wait_for(OFFERED, 2, LONG),
        "the bridge offers the endpoints again",
    );
    check(
        wait_until(Duration::from_secs(10), || lent() == Some(true)),
        "the gadget's description says the pair is lent again",
    );
    check(
        host_session(&me, 2),
        "host session 2: up again over the emulated bus, and the round trips with the node",
    );
    Ok(())
}

/// What the kernel shows of the gadget, for a step that did not come up.
fn diagnose() {
    let text = |path: &str| {
        fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|error| format!("<{error}>"))
    };
    say!(
        "     UDC attribute {:?}",
        text(&format!("{GADGET_DIR}/UDC"))
    );
    say!(
        "     /sys/class/udc/{UDC}/state {:?}",
        text(&format!("/sys/class/udc/{UDC}/state"))
    );
    fn walk(dir: &Path, depth: usize) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            say!("     tree: {}", entry.path().display());
            if depth > 0 && entry.path().is_dir() {
                walk(&entry.path(), depth - 1);
            }
        }
    }
    walk(Path::new(GADGET_DIR), 3);
    for line in text("/proc/mounts")
        .lines()
        .filter(|l| l.contains("functionfs"))
    {
        say!("     mount: {line}");
    }
    if let Ok(entries) = fs::read_dir("/dev/usb-ffs/dummy_udc.0") {
        let names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        say!("     /dev/usb-ffs/dummy_udc.0: {names:?}");
    }
}

/// Whether the gadget's description says the pair is lent, read through its item and method.
fn pair_lent(session: &mut lemnos_client::Session, device: u32) -> Option<bool> {
    let description = session.describe(device).ok()?;
    let item = pair_lent_method(&description)?;
    if item != PAIR_LENT {
        return None;
    }
    let mut handle = session.open("gadget", "usb.function/1").ok()?;
    let mut reply = [0u8; 8];
    let len = session.call(&mut handle, PAIR_LENT, &[], &mut reply).ok()?;
    (len == 1).then(|| reply[0] != 0)
}

/// The method number of the description's `pair_lent` item.
fn pair_lent_method(description: &lemnos_client::DeviceDescription) -> Option<u16> {
    description
        .items
        .iter()
        .find(|item| item.name == "pair_lent")
        .map(|item| item.method)
}

/// Runs the host role as a child, with its lines on this check's output. True when the session came
/// up, in that order the round trips followed, and the child exited cleanly.
fn host_session(me: &Path, number: u32) -> bool {
    let child = Command::new(me)
        .arg("host")
        .arg(ROUNDS.to_string())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        say!("FAIL host session {number}: cannot start");
        return false;
    };
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let stdout = child.stdout.take();
    let reader = {
        let lines = Arc::clone(&lines);
        std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    say!("{line}");
                    lines
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(line);
                }
            }
        })
    };
    let status = child.wait();
    let _ = reader.join();
    let lines = lines.lock().unwrap_or_else(PoisonError::into_inner);
    let up = lines.iter().position(|l| l.contains("host session up"));
    let trips = lines
        .iter()
        .position(|l| l.contains("round trips with the node"));
    status.is_ok_and(|status| status.success())
        && matches!((up, trips), (Some(u), Some(t)) if u < t)
}
