//! A host that never ends a control request's data stage, on the dummy controller: the device's
//! consumer answers the request, and its call returns at once; the daemon, the device and the bulk
//! pair go on serving; the device logs the stage once, by its request, after the control timeout;
//! and the host's next request ends it.
//!
//! The consumer is `lemnos-usb-echo` (its library, on a thread here), which answers the vendor
//! request [`lemnos_usb_echo::LATE`] [`lemnos_usb_echo::LATE_BY`] after it came. The host here gives
//! that request [`HOST_GIVES`], less than that, so it has gone by the time the answer is handed to
//! the kernel: the data stage is queued with no host to take it, which is a host that never ends a
//! stage. Before the stage went off the call, the consumer's `control_in` waited on it, ran out the
//! daemon's call deadline and abandoned the driver (`ran past its ... deadline`).
//!
//! The gadget is taken down at the end, so the check after this one starts on a controller with no
//! gadget on it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_contract_usb_function::UsbFunction;
use lemnos_linux::{Roots, SystemMounts};
use lemnos_linux_sys::usbfs::{ControlSetup, UsbfsFile};

use crate::daemon::{self, Lemnosd};
use crate::{EP_IN, EP_OUT, UDC};

/// The gadget this check makes: the link function alone, a control timeout shorter than the time
/// the check watches, and the data stream the consumer moves the bulk bytes on. No NCM function:
/// the check after this one makes `lemnos0` again, and on the harness's kernel (7.3.5) an NCM
/// function made again after one was removed in the same boot ended in a panic in
/// `register_netdevice`.
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
config = { manufacturer = "Lemnos", product_name = "Stage check", identity = "identity", control_timeout_ms = 300, interfaces = "function ffs\ninterface 0xff 0 0 orion-link\nendpoint in bulk\nendpoint out bulk\n" }

[devices.streams.data]
buffers = 8
buffer_bytes = 8192
job_slots = 16
job_bytes = 22
"#;

const BOARD_PATH: &str = "/run/lemnos-stage-board.toml";
const SOCKET: &str = "/run/lemnos-stage/lemnosd.sock";
const LOG: &str = "/run/lemnos-stage.log";
/// A vendor request to interface 0 whose data goes to the host. bRequest 0x51 is the gadget's
/// Microsoft OS descriptor code, which the kernel answers itself; 0x50 reaches the consumer.
const IN_TO_INTERFACE: u8 = 0xc1;
const PLAIN: u8 = 0x50;
/// The length the host asks for.
const LENGTH: u16 = 6;
/// How long the host gives the late request.
const HOST_GIVES: u32 = 50;
/// How long the check watches after the host gave up: longer than the late answer and the
/// control timeout together.
const WATCH: Duration = Duration::from_millis(1000);
/// What a call to the device may take while the stage is under way. Before, it was the daemon's
/// whole call deadline.
const ANSWERED_WITHIN: Duration = Duration::from_millis(150);

/// The check. `true` when every line passed.
pub fn run() -> bool {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        say!("{} stage: {what}", if ok { "ok  " } else { "FAIL" });
        failures += usize::from(!ok);
        ok
    };
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/lemnos-gadget-check"));
    if let Err(error) =
        fs::create_dir_all("/run/lemnos-stage").and_then(|()| fs::write(BOARD_PATH, BOARD))
    {
        say!("FAIL stage: the board file is written: {error}");
        return false;
    }
    let _ = fs::remove_file(LOG);
    let mut lemnosd = match Lemnosd::start_logged(
        &me.with_file_name("lemnosd"),
        Path::new(BOARD_PATH),
        Path::new(SOCKET),
        Path::new(LOG),
    ) {
        Ok(lemnosd) => lemnosd,
        Err(error) => {
            say!("FAIL stage: the daemon starts on the board file: {error}");
            return false;
        }
    };
    let echo = std::thread::spawn(|| {
        lemnos_usb_echo::serve(SOCKET, "gadget").map_err(|stop| stop.to_string())
    });
    let ran = watched(&mut check);
    lemnosd.kill();
    // The consumer ends with the daemon. It is not waited on past a bound: this process ends after
    // the check, and the consumer's end is not what is checked.
    if daemon::wait_until(Duration::from_secs(5), || echo.is_finished()) {
        say!("     stage: the consumer ended: {:?}", echo.join().ok());
    }
    let log = fs::read_to_string(LOG).unwrap_or_default();
    for line in log.lines().filter(|l| l.contains("gadget")) {
        say!("     stage: lemnosd: {line}");
    }
    if ran {
        lines(&log, &mut check);
    }
    let down = lemnos_linux::teardown_gadget(
        &Roots {
            configfs: Path::new("/sys/kernel/config"),
            mounts: Path::new("/dev/usb-ffs"),
            udc_class: Path::new("/sys/class/udc"),
        },
        UDC,
        &mut SystemMounts,
    );
    // The teardown leaves the gadget's own directory and its strings, which configfs removes
    // only with rmdir; the check after this one starts with no gadget at all.
    let gadget = Path::new(crate::GADGET_DIR);
    let languages: Vec<PathBuf> = fs::read_dir(gadget.join("strings"))
        .map(|dir| dir.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let removed = languages
        .iter()
        .try_for_each(fs::remove_dir)
        .and_then(|()| fs::remove_dir(gadget));
    if removed.is_err() {
        let left: Vec<_> = fs::read_dir(gadget)
            .map(|dir| dir.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        say!("     stage: the gadget's directory holds {left:?}");
    }
    check(
        down.is_ok() && removed.is_ok() && !gadget.exists(),
        &format!("the gadget is taken down for the next check ({down:?}, {removed:?})"),
    );
    failures == 0
}

/// The host's side, while the daemon and the consumer run. `true` when the host got as far as the
/// stage: the daemon's lines are checked then.
fn watched(check: &mut impl FnMut(bool, &str) -> bool) -> bool {
    let Some(path) = crate::host::find_device(Duration::from_secs(60)) else {
        check(false, "the host sees the gadget on the bus");
        return false;
    };
    let usb = match UsbfsFile::open(&path) {
        Ok(usb) => usb,
        Err(error) => {
            check(false, &format!("open {}: {error}", path.display()));
            return false;
        }
    };
    // The device is on the bus as soon as it enumerates, a moment before the host sets its
    // configuration: until then usbfs has no interface 0 and a claim gets ENOENT. Only that
    // answer is asked again, for a bounded time; any other is the check's failure at once.
    let configured_by = Instant::now() + Duration::from_secs(10);
    loop {
        match usb.claim_interface(0) {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && Instant::now() < configured_by =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                check(false, &format!("claim the vendor interface: {error}"));
                return false;
            }
        }
    }
    let client = match Client::connect(SOCKET, "stage-check") {
        Ok(client) => client,
        Err(error) => {
            check(false, &format!("connect to the daemon: {error:?}"));
            return false;
        }
    };
    let mut device = match client.open::<dyn UsbFunction>("gadget") {
        Ok(device) => device,
        Err(error) => {
            check(
                false,
                &format!("open the gadget through the daemon: {error:?}"),
            );
            return false;
        }
    };
    let setup = |request| ControlSetup {
        request_type: IN_TO_INTERFACE,
        request,
        value: 0,
        index: 0,
    };
    let mut got = [0u8; LENGTH as usize];
    // The consumer serves once the host has configured the device; the first answer says it does.
    let until = Instant::now() + Duration::from_secs(20);
    let first = loop {
        let answer = usb.control_in(setup(PLAIN), &mut got, 1000);
        if answer.is_ok() || Instant::now() >= until {
            break answer;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !check(
        first.as_ref().ok() == Some(&6) && got == *b"lemnos",
        &format!("a vendor request is answered with the consumer's bytes ({first:?}, {got:02x?})"),
    ) {
        return false;
    }

    // The late request: the host gives up on it before the answer comes.
    let late = usb.control_in(setup(lemnos_usb_echo::LATE), &mut got, HOST_GIVES);
    check(
        late.as_ref()
            .is_err_and(|e| e.raw_os_error() == Some(ETIMEDOUT)),
        &format!("the host gives up on the late request after {HOST_GIVES} ms ({late:?})"),
    );
    let gave_up = Instant::now();
    let mut slowest = Duration::ZERO;
    let mut calls = 0u32;
    let mut refused = None;
    while gave_up.elapsed() < WATCH {
        let started = Instant::now();
        if let Err(error) = device.state() {
            refused.get_or_insert(error);
        }
        slowest = slowest.max(started.elapsed());
        calls += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
    check(
        refused.is_none() && slowest < ANSWERED_WITHIN,
        &format!(
            "while the stage is under way the device answers: {calls} calls, the slowest {slowest:?} (under {ANSWERED_WITHIN:?}), refused {refused:?}"
        ),
    );
    // Fewer bytes than a packet: a short packet ends the consumer's receive.
    let pattern: Vec<u8> = (0..100u32).map(|i| (i * 7) as u8).collect();
    let mut back = vec![0u8; 512];
    let sent = usb.bulk_out(EP_OUT, &pattern, 5000);
    let came = usb.bulk_in(EP_IN, &mut back, 5000);
    check(
        sent.as_ref().ok() == Some(&100)
            && came.as_ref().ok() == Some(&100)
            && back[..100] == pattern[..],
        &format!(
            "the bulk pair echoes while the stage is under way (sent {sent:?}, back {came:?})"
        ),
    );
    // The host's next request ends the stage, and is answered.
    got = [0; LENGTH as usize];
    let next = usb.control_in(setup(PLAIN), &mut got, 5000);
    check(
        next.as_ref().ok() == Some(&6) && got == *b"lemnos",
        &format!("the host's next request is answered ({next:?}, {got:02x?})"),
    );
    // The consumer's answer to it is logged; give the daemon a moment to write the stage's end.
    daemon::wait_until(Duration::from_secs(2), || {
        fs::read_to_string(LOG).is_ok_and(|log| ended_line(&log).is_some())
    });
    true
}

/// The lines the daemon wrote: the stage logged once by its request, its end logged, and no call
/// that ran past its deadline.
fn lines(log: &str, check: &mut impl FnMut(bool, &str) -> bool) {
    let word = u32::from(IN_TO_INTERFACE) << 24
        | u32::from(lemnos_usb_echo::LATE) << 16
        | u32::from(LENGTH);
    let waiting = format!("lemnosd: gadget: {}: {word}", note::STAGE_WAITING);
    let count = log.lines().filter(|l| *l == waiting).count();
    check(
        count == 1,
        &format!("the stage is logged once by its request, {word:#010x} ({count} lines)"),
    );
    check(
        ended_line(log).is_some(),
        &format!("the stage's end is logged ({:?})", ended_line(log)),
    );
    let overrun = log.lines().find(|l| l.contains("ran past its"));
    check(
        overrun.is_none(),
        &format!("no call or tick ran past its deadline ({overrun:?})"),
    );
}

/// The line that says how the stage ended: abandoned by the host's next request, or ended late.
fn ended_line(log: &str) -> Option<&str> {
    log.lines().find(|l| {
        l.starts_with("lemnosd: gadget: ")
            && (l.contains(note::STAGE_ABANDONED) || l.contains(note::STAGE_ENDED_LATE))
    })
}

/// `ETIMEDOUT`, which usbfs gives a control transfer that ran out its timeout.
const ETIMEDOUT: i32 = 110;

/// The driver's note texts, written out: the check shares no text with the driver it checks, so a
/// changed note shows up here as a missing line.
mod note {
    pub const STAGE_WAITING: &str = "a control request's data stage has waited the control timeout for the host to end it; it is left to the host, which ends it with a new request or a reset (request as bmRequestType << 24 | bRequest << 16 | wLength)";
    pub const STAGE_ABANDONED: &str = "the host abandoned a control request's data stage";
    pub const STAGE_ENDED_LATE: &str = "the control data stage reported waiting has ended";
}
