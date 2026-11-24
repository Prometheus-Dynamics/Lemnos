//! The kernel state a gadget with an NCM function meets when the USB controller's device goes and
//! comes back: on 7.3 every NCM registration after it fails for the rest of the boot
//! (`kobject_add_internal failed for <ifname> (error: -2 parent: net)`), and a second bind of a
//! function whose registration failed is the kernel's `BUG_ON` in `register_netdevice`, a panic.
//!
//! Run on the link's daemon (`lemnos-orion-link-check`, which leaves it running): its gadget has
//! the default composition, NCM included, and its network device is registered. The controller's
//! platform device is unbound and bound; the gadget's bind is tried again on a fresh NCM function,
//! then without one: the gadget comes up without its network function, and its reason names the
//! kernel's state. The kernel staying up is the
//! harness's own check (a panic ends the guest before its last line).

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use lemnos_client::{DeviceState, Session};

use crate::{GADGET_DIR, daemon};

const SOCKET: &str = "/run/lemnos-link/lemnosd.sock";
const PLATFORM: &str = "/sys/bus/platform/drivers/dummy_udc";
/// The words the gadget's reason carries for the state: written out, shared with no driver text.
const REFUSED: &str =
    "the kernel refuses to register the gadget's network device again in this boot";

pub fn run() -> bool {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        say!("{} poison: {what}", if ok { "ok  " } else { "FAIL" });
        failures += usize::from(!ok);
        ok
    };
    let mut session = match Session::connect(Path::new(SOCKET), "poison-check") {
        Ok(session) => session,
        Err(error) => {
            check(false, &format!("connect to the link's daemon: {error:?}"));
            return false;
        }
    };
    let bound = daemon::wait_for_device(
        &mut session,
        "gadget",
        Duration::from_secs(20),
        |_, _, _| daemon::enabled(),
    );
    check(
        bound == Ok(true) && Path::new(GADGET_DIR).join("functions/ncm.usb0").is_dir(),
        &format!("the link's gadget is bound with its network function ({bound:?})"),
    );
    let mark = log_since(0).len();
    let daemon_log = daemon_stderr();
    let again = fs::write(format!("{PLATFORM}/unbind"), "dummy_udc.0")
        .and_then(|()| fs::write(format!("{PLATFORM}/bind"), "dummy_udc.0"));
    check(
        again.is_ok(),
        &format!("the controller's device goes and comes back ({again:?})"),
    );
    let named = daemon::wait_for_device(
        &mut session,
        "gadget",
        Duration::from_secs(60),
        |_, _, reason| reason.contains(REFUSED),
    );
    let gadget = session.list().ok().and_then(|entries| {
        entries
            .into_iter()
            .find(|e| e.name == "gadget")
            .map(|e| (e.state, e.health, e.reason))
    });
    say!("     poison: gadget {gadget:?}");
    say!(
        "     poison: UDC {:?} functions {:?} configs {:?}",
        fs::read_to_string(format!("{GADGET_DIR}/UDC")),
        fs::read_dir(format!("{GADGET_DIR}/functions"))
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>()),
        fs::read_dir(format!("{GADGET_DIR}/configs/c.1"))
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    for line in log_since(mark) {
        say!("     poison: kernel: {line}");
    }
    if !check(
        named == Ok(true),
        &format!("the gadget's reason names the kernel's state ({named:?})"),
    ) {
        for line in where_the_daemon_waits() {
            say!("     poison: {line}");
        }
        if let Some(log) = &daemon_log {
            let text = log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            for line in String::from_utf8_lossy(&text)
                .lines()
                .filter(|l| !l.contains("SLOT-TICK"))
                .rev()
                .take(80)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                say!("     poison: daemon: {line}");
            }
        }
    }
    let functions = |name: &str| Path::new(GADGET_DIR).join("functions").join(name).is_dir();
    check(
        daemon::enabled()
            && !functions("ncm.usb0")
            && functions("acm.GS0")
            && functions("ffs.dummy_udc.0"),
        "the gadget is bound without its network function: the console and the link still reach the host",
    );
    // The host side sees the device configured with what is left: the link's one interface and
    // the console's two (with the network function there were five).
    let host = || {
        let read = |file: &str| fs::read_to_string(format!("/sys/bus/usb/devices/1-1/{file}"));
        (read("bConfigurationValue"), read("bNumInterfaces"))
    };
    let configured = |seen: &(io::Result<String>, io::Result<String>)| matches!(seen, (Ok(c), Ok(n)) if c.trim() == "1" && n.trim() == "3");
    let until = std::time::Instant::now() + Duration::from_secs(10);
    let mut seen = host();
    while !configured(&seen) && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
        seen = host();
    }
    check(
        configured(&seen),
        &format!("the host has the device configured with the link and the console ({seen:?})"),
    );
    check(
        gadget
            .as_ref()
            .is_some_and(|(state, _, _)| *state == DeviceState::Running),
        "the gadget runs",
    );
    check(
        session
            .list()
            .is_ok_and(|entries| entries.iter().any(|e| e.name == "identity")),
        "the daemon keeps serving",
    );
    failures == 0
}

/// The kernel log's records from the `skip`th on, their text only, the last 60 at most.
fn log_since(skip: usize) -> Vec<String> {
    let Ok(mut f) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open("/dev/kmsg")
    else {
        return Vec::new();
    };
    let mut record = [0u8; 8192];
    let mut lines = Vec::new();
    loop {
        match f.read(&mut record) {
            Ok(0) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&record[..n]);
                let text = text.split_once(';').map_or(&*text, |(_, t)| t);
                lines.push(text.lines().next().unwrap_or("").to_string());
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.raw_os_error() == Some(libc::EPIPE) => {}
            Err(_) => break,
        }
    }
    if skip == 0 {
        return lines;
    }
    let from = skip.min(lines.len()).max(lines.len().saturating_sub(60));
    lines.split_off(from)
}

/// Where each thread of the daemon and its helpers is in the kernel: its name, state, `wchan` and
/// kernel stack. Said when the gadget's reason never names the kernel's state, so a job that waits
/// shows what it waits on (the link's daemon logs to a pipe that ended with the link check).
fn where_the_daemon_waits() -> Vec<String> {
    let mut lines = Vec::new();
    let Ok(procs) = fs::read_dir("/proc") else {
        return lines;
    };
    for entry in procs.flatten() {
        let pid = entry.file_name().to_string_lossy().into_owned();
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if !comm.starts_with("lemnosd") {
            continue;
        }
        if comm.trim() == "lemnosd" {
            // The FunctionFS mounts as the daemon sees them, and what each holds.
            let mounts = fs::read_to_string(format!("/proc/{pid}/mountinfo")).unwrap_or_default();
            for mount in mounts.lines().filter(|l| l.contains("functionfs")) {
                lines.push(format!("pid {pid} mount {mount}"));
                if let Some(point) = mount.split(' ').nth(4) {
                    let names = fs::read_dir(format!("/proc/{pid}/root{point}")).map(|d| {
                        d.flatten()
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                    });
                    lines.push(format!("    holds {names:?}"));
                }
            }
        }
        let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for task in tasks.flatten() {
            let dir = task.path();
            let read = |file: &str| fs::read_to_string(dir.join(file)).unwrap_or_default();
            let state = read("stat");
            let state = state
                .rsplit_once(") ")
                .map_or("", |(_, rest)| &rest[..1.min(rest.len())]);
            lines.push(format!(
                "pid {pid} task {} {} state {state} wchan {}",
                task.file_name().to_string_lossy(),
                read("comm").trim(),
                read("wchan").trim()
            ));
            for frame in read("stack").lines().take(12) {
                lines.push(format!("    {frame}"));
            }
        }
    }
    lines
}

/// The daemon's error output, read from here on, on a thread of its own: the link check that
/// started it, and read it, has ended, so its lines go nowhere unless something reads them. The
/// thread reads all the time: a reader that only read at the end would let the pipe fill, and a
/// daemon whose writes block stops answering calls.
fn daemon_stderr() -> Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>> {
    for entry in fs::read_dir("/proc").ok()?.flatten() {
        let pid = entry.file_name().to_string_lossy().into_owned();
        if pid.bytes().all(|b| b.is_ascii_digit())
            && fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "lemnosd")
        {
            let mut pipe = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC)
                .open(format!("/proc/{pid}/fd/2"))
                .ok()?;
            let kept = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let into = std::sync::Arc::clone(&kept);
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    into.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .extend_from_slice(&chunk[..n]);
                }
            });
            return Some(kept);
        }
    }
    None
}
