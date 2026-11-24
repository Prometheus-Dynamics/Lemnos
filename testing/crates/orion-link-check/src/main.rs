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
//!      up and offers again once the gadget is up, and a second host session does the same;
//!   4. the hello sweep: more bounces, with the host's first HELLO at set delays;
//!   5. a stuck driver call: the daemon's lane threads stand still while a host session moves
//!      bytes, the gadget's call runs past its deadline, and the device rebinds with its session
//!      kept: `UDC` stays set, the bridge keeps its offer and the session never breaks; twice in a
//!      row;
//!   6. the controller lets go after the rebinds: the instance that took the session up sees it,
//!      enables the gadget again, and a host session comes up;
//!   7. the board's network link (the gadget's NCM function, to the host's `cdc_ncm` moved into a
//!      network namespace of its own) carries a TCP connection, and the daemon restarts as
//!      `systemctl restart` does, ten times (`lemnos-qemu.link_restarts=N` asks for N), under a
//!      stand-in service manager that keeps the gadget's files in its store: `UDC` is never clear,
//!      the connection never goes a second without an echo, fresh connections are answered, both
//!      interfaces keep their index, and the daemon that comes up takes the gadget up as it was;
//!      Then with the host at work: a restart with a control request to the link's interface sent
//!      while no daemon runs (the next daemon stalls it before it serves anything: the host sees a
//!      stall, not its timeout), and a restart under a paced Orion session (its round trips stall
//!      while no daemon runs and go on after; the longest stall is said);
//!   8. the same through a SIGKILL while the daemon runs and a SIGKILL at each step of the start
//!      that follows (the root phase, its end, the host's start, the gadget's bind, its end, every
//!      bind made, ready), then a host session over the Orion link.
//! - `comm`: the comm core. `host N [HELLO_MS [PACE_MS]]`: the host session, with N round trips.
//!   `netpeer NAME`: the host's end of the network link, in its own namespace.
//!
//! Nothing here stops a process but the daemon in steps 7 and 8, by its PID: the guest powers off
//! when the checks end. Prints one `ok` or
//! `FAIL` line per check and exits non-zero if any failed.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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
mod manager;
mod net;
#[cfg(feature = "trace")]
mod trace;

use bridge::Bridge;
use daemon::{KillAt, Lemnosd, enabled, wait_until};
use manager::Manager;

/// The board: the gadget with the default composition (no `interfaces` key), deriving its addresses
/// from the identity device, whose serial the kernel command line carries as `lemnos.serial=`.
///
/// The gadget's backoff is the product setting `retry_first_ms` / `retry_cap_ms`, set shorter here
/// than its 1 s / 8 s defaults: each bounce below waits out the first step instead of 1, 2, 4, 8 s.
/// The first step stays at 300 ms, three of the bridge's 100 ms samples of the gadget's state
/// (`service/orion/src/links.rs`, the `subscribe` of `STATE`): the gadget is down for at least that
/// long, so the bridge always sees it go down ("the bridge gives the hold up") whatever the
/// emulated host's enumeration takes. The defaults themselves are the driver's unit tests'.
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
config = { manufacturer = "Lemnos", product_name = "Gadget check", identity = "identity", retry_first_ms = 300, retry_cap_ms = 600 }
"#;

const DIR: &str = "/run/lemnos-link";
/// The board file sits outside the socket's directory, as /etc/lemnos/board.toml does on a board: the
/// first daemon hands that directory to its user (mode 0750), and a restarted daemon's root phase,
/// which has no `CAP_DAC_OVERRIDE`, could not read a board file inside it.
const BOARD_DIR: &str = "/run/lemnos-link-board";
const BOARD_PATH: &str = "/run/lemnos-link-board/board.toml";
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
const WENT_DOWN: &str = "link orion-link: the gadget is down or unconfigured; the hold is kept";
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
        Some("netpeer") => net::run_peer(&args.next().unwrap_or_default()),
        Some("host") => {
            let rounds = args.next().and_then(|n| n.parse().ok()).unwrap_or(ROUNDS);
            let hello_after_ms: u64 = args.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let pace_ms: u64 = args.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let ok = host::run(
                rounds,
                Duration::from_millis(hello_after_ms),
                Duration::from_millis(pace_ms),
            );
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
        .and_then(|()| fs::create_dir_all(BOARD_DIR))
        .and_then(|()| fs::write(BOARD_PATH, BOARD))
        .map_err(|error| format!("the board file is written: {error}"))?;
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/lemnos-orion-link-check"));
    let here = me
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);

    // The setting an image gets from `tree --bridge-env`, made here from the same board file.
    let board = lemnos_host::board::parse(BOARD).map_err(|error| format!("the board: {error}"))?;
    let written = lemnos_ctl::bridge_env::text(&board, &lemnos_ctl::driver_manifests())?;
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
    // The service manager the daemon runs under: its notify socket and file descriptor store.
    let manager = Manager::start()?;
    let mut daemon = Lemnosd::start(
        &here.join("lemnosd"),
        Path::new(BOARD_PATH),
        Path::new(LEMNOSD_SOCKET),
        Arc::clone(&manager),
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
    // `pair_lent` gives `None` when the describe or the open is refused. Until M28 a describe was
    // answered `BUSY` while the driver was in a tick or a call, and `.ok()?` turned that into "no
    // value": the early read that failed once in CI fits that, though no run that failed was kept
    // to prove it. The daemon now waits instead of refusing, so a read right after start is
    // definite. The bound stays as a ceiling a passing run does not pay (it ends at the first
    // definite answer) and says what it last saw when there is none.
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
        host_session(&me, 1, 0),
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
    // The controller let go and took the gadget back on the same endpoints: the bridge keeps its offer
    // (no second offer). A new host's HELLO begins a session on the kept stream (C57).
    std::thread::sleep(Duration::from_secs(3));
    check(
        bridge.count(OFFERED) == 1,
        "the bridge keeps its offer across the controller's let-go: the endpoints did not change",
    );
    check(
        wait_until(Duration::from_secs(10), || lent() == Some(true)),
        "the gadget's description says the pair is lent again",
    );
    check(
        host_session(&me, 2, 0),
        "host session 2: up again over the emulated bus, and the round trips with the node",
    );

    // 4. The hello sweep: each bounce, with the host's first HELLO sent a set time after it claims the
    // link. The endpoints are the same after each bounce, so the bridge keeps its offer, and each new
    // host's HELLO begins a session on the kept stream (C57). See `HELLO_SWEEP_MS`.
    for (cycle, &hello_after_ms) in HELLO_SWEEP_MS.iter().enumerate() {
        let went_down = bridge.count(WENT_DOWN) + 1;
        check(
            fs::write(format!("{GADGET_DIR}/UDC"), "\n").is_ok(),
            &format!("bounce {}: the controller lets go of the gadget", cycle + 3),
        );
        check(
            bridge.wait_for(WENT_DOWN, went_down, LONG),
            &format!(
                "bounce {}: the bridge keeps the hold while the gadget is down",
                cycle + 3
            ),
        );
        check(
            wait_until(LONG, enabled),
            &format!("bounce {}: the daemon enables the gadget again", cycle + 3),
        );
        let offers = bridge.count(OFFERED);
        let up = host_session(&me, 3 + cycle as u32, hello_after_ms);
        check(
            bridge.count(OFFERED) == offers,
            &format!(
                "bounce {}: no second offer: the endpoints did not change",
                cycle + 3
            ),
        );
        check(
            up,
            &format!(
                "hello sweep: the host's hello {hello_after_ms} ms after its claim, after the gadget is enabled: session up"
            ),
        );
    }

    // 5. A driver call that does not return in time while the link carries a session. The daemon's
    // lane threads (which run its drivers' ticks and calls) stand still while a host session moves
    // bytes; the gadget's tick runs past its deadline, the host abandons it, and when the call
    // returns the device rebinds. The old instance parks its session and is kept across the
    // rebind, the new one takes the session up, and the holder's hold and copies go on: the
    // controller never lets go of the gadget (`UDC` stays set) and the session never breaks.
    // Twice in a row: the instance that took the session up parks it again for the next one.
    for round in 1..=2 {
        stuck_call_keeps_the_link(check, &me, &daemon, &bridge, &mut lent, round);
    }

    // 6. The instance that took the session up has the live link's state: the controller letting
    // go now is seen by it, as by the first instance in step 3, and the link comes back.
    let went_down = bridge.count(WENT_DOWN) + 1;
    check(
        fs::write(format!("{GADGET_DIR}/UDC"), "\n").is_ok(),
        "after the rebinds: the controller lets go of the gadget",
    );
    check(
        bridge.wait_for(WENT_DOWN, went_down, LONG),
        "after the rebinds: the bridge sees the gadget go down",
    );
    check(
        wait_until(LONG, enabled),
        "after the rebinds: the new instance enables the gadget again",
    );
    check(
        host_session(&me, 12, 0),
        "after the rebinds: a host session comes up again, and the round trips with the node",
    );

    // 7 and 8. The daemon restarts, and is killed at each step of its start, while the board's
    // network link carries a TCP connection.
    keep_through_restarts(check, &me, &mut daemon, &bridge, &manager);
    Ok(())
}

/// What a stopped daemon left of its gadget, said between the stop and the next start: the
/// FunctionFS mounts, every open descriptor of any process into one (its pid, name and path), the
/// gadget's `UDC` and functions, and the USB network devices. Read only.
fn say_left_behind() {
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let ffs: Vec<&str> = mounts
        .lines()
        .filter(|line| line.split(' ').nth(2) == Some("functionfs"))
        .filter_map(|line| line.split(' ').nth(1))
        .collect();
    say!("     left behind: functionfs mounts {ffs:?}");
    let Ok(procs) = fs::read_dir("/proc") else {
        return;
    };
    for proc_entry in procs.flatten() {
        let pid = proc_entry.file_name().to_string_lossy().into_owned();
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy().into_owned();
            if ffs.iter().any(|mount| target.starts_with(mount)) || target.contains("usb-ffs") {
                say!(
                    "     left behind: pid {pid} ({}) fd {} -> {target}",
                    comm.trim(),
                    fd.file_name().to_string_lossy()
                );
            }
        }
    }
    let udc = fs::read_to_string(format!("{GADGET_DIR}/UDC")).unwrap_or_else(|e| format!("<{e}>"));
    let functions: Vec<String> = fs::read_dir(format!("{GADGET_DIR}/functions"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let nets: Vec<String> = fs::read_dir("/sys/class/net")
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name != "lo")
                .collect()
        })
        .unwrap_or_default();
    say!(
        "     left behind: UDC {:?}, functions {functions:?}, net devices {nets:?}",
        udc.trim()
    );
}

/// How many daemon restarts step 7 makes: `lemnos-qemu.link_restarts=N` on the kernel command line,
/// else [`RESTARTS`].
fn restarts_asked() -> u32 {
    fs::read_to_string("/proc/cmdline")
        .ok()
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix(RESTARTS_WORD))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(RESTARTS)
        .max(1)
}

/// The kernel command line's word for the restarts step 7 makes.
const RESTARTS_WORD: &str = "lemnos-qemu.link_restarts=";
const RESTARTS: u32 = 10;
/// The longest the running connection may go without an echo across a restart or a kill. It sends
/// every 50 ms; a link that never went down answers within that, and one that went down and came
/// back takes the host's enumeration and the daemon's start, seconds.
const MAX_GAP: Duration = Duration::from_secs(1);
/// How far past the guest's own longest stall (the loopback reference's longest gap over the
/// same time) the link's longest gap may go: a guest that stood still for a second (the machine
/// under it busy) stalls every connection, the link's with it, and that is not the link's.
const GAP_PAST_STALL: Duration = Duration::from_millis(250);

/// TCP's least retransmission timeout (Linux's `TCP_RTO_MIN`), which doubles at each retry.
const RTO_MIN: Duration = Duration::from_millis(200);

/// Whether the link's longest gap `gap` is the link carrying traffic: under [`MAX_GAP`], or no
/// more than [`GAP_PAST_STALL`] past the guest's own longest stall `stalled`. A guest that stood
/// still past [`MAX_GAP`] can lose a transfer on the link, which loopback never does: TCP sends it
/// again on its retransmission timeouts (from [`RTO_MIN`], doubling), so the link's gap may run on
/// to the first of them at or past the stall. Seen in QEMU with the machine under the guest busy:
/// a 3923 ms stall over loopback, 4976 ms on the link, every other round within 250 ms of loopback.
fn within_gap(gap: Duration, stalled: Duration) -> bool {
    if gap <= MAX_GAP || gap <= stalled + GAP_PAST_STALL {
        return true;
    }
    if stalled <= MAX_GAP {
        return false;
    }
    let mut timeout = RTO_MIN;
    let mut retried = RTO_MIN;
    while retried < stalled {
        timeout *= 2;
        retried += timeout;
    }
    gap <= retried + GAP_PAST_STALL
}
/// What a daemon that took the kept gadget up says, and what one that set it up says instead.
const TAKEN_UP: &str = "(taken up as it was)";
const STORE_KEPT: &str = "kept bound through the restart";
const SET_UP: &str = "(set up and bound)";
const STORE_LET_GO: &str = "the files kept for it were let go";

/// Watches the gadget's `UDC` from now on: when it was first seen clear.
struct UdcWatch {
    cleared: Arc<Mutex<Option<Instant>>>,
    watching: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl UdcWatch {
    fn start() -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cleared: Arc<Mutex<Option<Instant>>> = Arc::default();
        let watching = Arc::new(AtomicBool::new(true));
        let (seen, on) = (Arc::clone(&cleared), Arc::clone(&watching));
        let thread = std::thread::spawn(move || {
            while on.load(Ordering::Relaxed) {
                if !enabled() {
                    seen.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get_or_insert_with(Instant::now);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Self {
            cleared,
            watching,
            thread: Some(thread),
        }
    }

    /// Stops watching. When `UDC` was first seen clear, if it was.
    fn end(mut self) -> Option<Instant> {
        self.watching
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *self.cleared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What the link must show after the daemon came back: the indexes it had, and the running
/// connection's echoes.
struct Link<'a> {
    board_index: u32,
    host_index: u32,
    stream: &'a net::Stream,
    /// The same echo over loopback: the guest's own stalls, to tell from the link's.
    reference: &'a net::Stream,
}

/// Steps 7 and 8. The board's network link (the gadget's NCM function, to the host's `cdc_ncm` in a
/// namespace of its own) carries a TCP connection that echoes every 50 ms. Step 7: the daemon
/// restarts as `systemctl restart` does, [`restarts_asked`] times. Step 8: it is killed (SIGKILL),
/// started again, killed at each step of that start ([`KillAt`]), and started once more. Through
/// every one the service manager keeps the gadget's files: `UDC` is never cleared, the connection
/// never goes [`MAX_GAP`] without an echo, fresh connections are answered, both interfaces keep
/// their index, the daemon that comes up takes the gadget up as it was, and takes up the bridge's
/// hold, whose END the store keeps too: the bridge never offers again.
fn keep_through_restarts(
    check: &mut dyn FnMut(bool, &str) -> bool,
    me: &Path,
    daemon: &mut Lemnosd,
    bridge: &Bridge,
    manager: &Manager,
) {
    let stored = manager.names();
    check(
        stored.iter().any(|name| name.ends_with(".record"))
            && stored.iter().any(|name| name.ends_with(".ep0"))
            && stored
                .iter()
                .filter(|name| name.contains(".ep") && !name.ends_with(".ep0"))
                .count()
                >= 2,
        &format!("the service manager keeps the bound gadget's files: {stored:?}"),
    );
    check(
        stored.iter().any(|name| name.starts_with(END_KEPT)),
        &format!("the service manager keeps the bridge's lend's END: {stored:?}"),
    );
    let offers = bridge.count(OFFERED);
    let mut peer = match net::Peer::start(me) {
        Ok(peer) => peer,
        Err(error) => {
            check(
                false,
                &format!("the host's interface in a namespace of its own: {error}"),
            );
            return;
        }
    };
    let (Ok(board_index), Some(host_index)) = (net::index(net::BOARD_IF), peer.host_index()) else {
        check(false, "the link's interfaces have indexes on both ends");
        return;
    };
    let stream = match net::Stream::start() {
        Ok(stream) => stream,
        Err(error) => {
            check(false, &format!("a TCP connection over the link: {error}"));
            return;
        }
    };
    let opened = Instant::now();
    check(
        wait_until(LONG, || stream.echoes_since(opened) >= 10),
        &format!(
            "a TCP connection over the board's {} (index {board_index}) to the host's (index {host_index}): its echoes come back",
            net::BOARD_IF
        ),
    );
    check(
        net::connects(3) == 3,
        "fresh connections over the link are answered",
    );
    let reference = match net::Stream::loopback() {
        Ok(reference) => reference,
        Err(error) => {
            check(
                false,
                &format!("the reference connection over loopback: {error}"),
            );
            return;
        }
    };
    let link = Link {
        board_index,
        host_index,
        stream: &stream,
        reference: &reference,
    };

    // 7, with the host at work: a control request while no daemon runs. It goes first: the
    // request claims the link's interface, which the session below holds from then on.
    control_while_down(check, daemon, &mut peer, &link);

    // 7 and 8 under one Orion session over the link, its round trips paced 100 ms apart: it
    // lives through every restart and kill, and never sees its session lost.
    let Some(session) = HostRun::start(me, 40, 0, 0, PACE_MS) else {
        check(false, "the Orion session over the link starts");
        return;
    };
    check(
        wait_until(LONG, || session.count(host::ROUND) >= 3),
        "an Orion session over the link is up, its round trips under way",
    );
    let went_on = |check: &mut dyn FnMut(bool, &str) -> bool, named: &str, before: usize| {
        check(
            wait_until(LONG, || session.count(host::ROUND) > before + 3),
            &format!(
                "{named}: the Orion session's round trips went on ({} since the stop)",
                session.count(host::ROUND) - before
            ),
        );
    };

    // 7. Restarts.
    for round in 1..=restarts_asked() {
        let named = format!("restart {round}");
        let watch = UdcWatch::start();
        let before = session.count(host::ROUND);
        let stopped = daemon.restart(say_left_behind);
        let Ok((stopped_at, clean)) = stopped else {
            check(
                false,
                &format!(
                    "{named}: the daemon restarts: {}",
                    stopped.err().unwrap_or_default()
                ),
            );
            return;
        };
        check(
            clean,
            &format!("{named}: the daemon stopped on SIGTERM inside the unit's 5 s stop bound"),
        );
        came_back(check, &named, daemon, &mut peer, &link, stopped_at, watch);
        went_on(check, &named, before);
    }

    // 8. A kill while it runs, then a kill at each step of the next start.
    for at in KillAt::ALL {
        let named = format!("kill at {at:?}");
        let watch = UdcWatch::start();
        let before = session.count(host::ROUND);
        match daemon.kill_at(at) {
            Ok((killed_at, last)) => {
                say!("     {named}: the killed start's last line: {last}");
                came_back(check, &named, daemon, &mut peer, &link, killed_at, watch);
                went_on(check, &named, before);
            }
            Err(error) => {
                watch.end();
                check(false, &format!("{named}: {error}"));
                return;
            }
        }
    }
    let trips = session.count(host::ROUND);
    let stall = session.longest_gap(host::ROUND);
    check(
        session.finish(),
        &format!(
            "one Orion session through {} restarts and {} SIGKILLs (each of the running daemon, then of the next start at one of its {} steps): never lost, {trips} round trips; the longest stall the host saw was {} ms",
            restarts_asked(),
            2 * KillAt::ALL.len(),
            KillAt::ALL.len(),
            stall.as_millis()
        ),
    );
    check(
        bridge.count(OFFERED) == offers,
        &format!(
            "the bridge's hold went on through every restart and kill: no second offer ({offers} before, {} after)",
            bridge.count(OFFERED)
        ),
    );
    check(
        host_session(me, 70, 0),
        "after the restarts and kills: a host session comes up, and the round trips with the node",
    );
}

/// The store's name for a lend's END hold.
const END_KEPT: &str = "lemnos-end.";
/// The daemon's line when it takes up the hold its last start lent.
const HOLD_TAKEN_UP: &str =
    "the hold the daemon before lent is taken up: the holder's link goes on";
/// The daemon's line when it could not.
const HOLD_DROPPED: &str = "the hold the daemon before lent is dropped";

/// How long the host gives the control request sent while no daemon runs: past the restart's
/// whole span, so a request left unanswered shows as this timeout, not as a stall.
const CONTROL_WAIT_MS: u32 = 10_000;
/// Linux's `EPIPE`: the device stalled the request.
const EPIPE: i32 = 32;
/// The next daemon's line for a request the host made while no daemon ran.
const REFUSED_WHILE_DOWN: &str = "the host made while no daemon ran was refused (stalled)";

/// A restart with a control request to the link's interface sent while no daemon runs. FunctionFS
/// queues it as a setup event on the stored ep0, and the host waits: the next daemon reads it
/// before it serves anything and stalls it, so the host gets a stall within the restart's span,
/// never its own timeout.
fn control_while_down(
    check: &mut dyn FnMut(bool, &str) -> bool,
    daemon: &mut Lemnosd,
    peer: &mut net::Peer,
    link: &Link,
) {
    let named = "restart with a control request";
    let watch = UdcWatch::start();
    let mut asked = None;
    let stopped = daemon.restart(|| {
        say_left_behind();
        asked = Some(std::thread::spawn(|| {
            host::control_request(CONTROL_WAIT_MS)
        }));
        // On the bus before the next start begins (the restart's own delay follows).
        std::thread::sleep(Duration::from_millis(200));
    });
    let Ok((stopped_at, clean)) = stopped else {
        check(
            false,
            &format!(
                "{named}: the daemon restarts: {}",
                stopped.err().unwrap_or_default()
            ),
        );
        return;
    };
    check(clean, &format!("{named}: the daemon stopped on SIGTERM"));
    came_back(check, named, daemon, peer, link, stopped_at, watch);
    let answer = asked.map(std::thread::JoinHandle::join);
    match answer {
        Some(Ok(Ok((waited, got)))) => {
            let stalled = matches!(&got, Err(error) if error.raw_os_error() == Some(EPIPE));
            check(
                stalled && waited < Duration::from_millis(u64::from(CONTROL_WAIT_MS)),
                &format!(
                    "{named}: the host's request, sent while no daemon ran, was stalled once the next daemon drained ep0, {} ms after it was sent (the host's timeout {CONTROL_WAIT_MS} ms): {got:?}",
                    waited.as_millis()
                ),
            );
        }
        Some(Ok(Err(error))) => {
            check(false, &format!("{named}: the host's request: {error}"));
        }
        _ => {
            check(false, &format!("{named}: the host's request was not sent"));
        }
    }
    check(
        !daemon
            .lines_since(stopped_at, REFUSED_WHILE_DOWN)
            .is_empty(),
        &format!(
            "{named}: the next daemon says it refused the request the host made while none ran"
        ),
    );
}

/// The checks after one restart or kill (`named`), from `since`, the stop.
#[allow(clippy::too_many_arguments)]
fn came_back(
    check: &mut dyn FnMut(bool, &str) -> bool,
    named: &str,
    daemon: &Lemnosd,
    peer: &mut net::Peer,
    link: &Link<'_>,
    since: Instant,
    watch: UdcWatch,
) {
    let up = wait_until(LONG, || !daemon.lines_since(since, TAKEN_UP).is_empty());
    check(
        up && !daemon.lines_since(since, STORE_KEPT).is_empty(),
        &format!("{named}: the daemon that came up took the gadget up from the store, as it was"),
    );
    let set_up = daemon.lines_since(since, SET_UP);
    let let_go = daemon.lines_since(since, STORE_LET_GO);
    check(
        set_up.is_empty() && let_go.is_empty(),
        &format!("{named}: nothing was let go or set up again: {set_up:?} {let_go:?}"),
    );
    let lent = wait_until(LONG, || {
        daemon
            .connect("restart-check")
            .ok()
            .and_then(|mut session| {
                let device = session.find("usb.function/1").ok()??.index;
                pair_lent(&mut session, device)
            })
            == Some(true)
    });
    let taken = daemon.lines_since(since, HOLD_TAKEN_UP);
    let dropped = daemon.lines_since(since, HOLD_DROPPED);
    check(
        lent && !taken.is_empty() && dropped.is_empty(),
        &format!(
            "{named}: the daemon that came up took up the bridge's hold, and the pair is lent: {dropped:?}"
        ),
    );
    // The connection, a second past the daemon's ready.
    std::thread::sleep(Duration::from_secs(1));
    let now = Instant::now();
    let cleared = watch.end();
    check(
        cleared.is_none(),
        &format!(
            "{named}: the gadget stayed bound (UDC never clear){}",
            cleared.map_or_else(String::new, |at| format!(
                ": clear {} ms after the stop",
                at.saturating_duration_since(since).as_millis()
            ))
        ),
    );
    let gap = link.stream.longest_gap(since, now);
    let stalled = link.reference.longest_gap(since, now);
    check(
        link.stream.failed().is_none()
            && link.reference.failed().is_none()
            && within_gap(gap, stalled),
        &format!(
            "{named}: the running connection carried traffic throughout: longest gap {} ms (at most {} ms, or {} ms past the guest's own longest stall, {} ms over loopback, or past TCP's retransmission timeout after a stall over {} ms){}",
            gap.as_millis(),
            MAX_GAP.as_millis(),
            GAP_PAST_STALL.as_millis(),
            stalled.as_millis(),
            MAX_GAP.as_millis(),
            link.stream
                .failed()
                .or_else(|| link.reference.failed())
                .map_or_else(String::new, |why| format!(", ended: {why}"))
        ),
    );
    check(
        net::connects(3) == 3,
        &format!("{named}: fresh connections over the link are answered"),
    );
    let board = net::index(net::BOARD_IF).ok();
    let host = peer.host_index();
    check(
        board == Some(link.board_index) && host == Some(link.host_index),
        &format!(
            "{named}: both interfaces kept their index: the board's {board:?} (was {}), the host's {host:?} (was {})",
            link.board_index, link.host_index
        ),
    );
    let ran_past = daemon.lines_since(since, RAN_PAST);
    check(
        ran_past.is_empty(),
        &format!("{named}: no call of the new daemon ran past its deadline: {ran_past:?}"),
    );
}

/// The round trips of the paced host session in step 5, and the time between them.
const PACED_ROUNDS: u64 = 200;
const PACE_MS: u64 = 100;
/// How long the lane threads stay still after the gadget's call ran past its deadline.
const HELD_ON: Duration = Duration::from_secs(3);
/// The daemon's lines step 5 looks for.
const GADGET: &str = "device gadget (driver usb-gadget)";
const RAN_PAST: &str = "ran past its";
const KEPT: &str = "device gadget (driver usb-gadget): the instance is kept across the rebind";
const LET_GO: &str = "the instance kept across the rebind is let go";
const NOT_TAKEN_UP: &str = "did not take up the holder's endpoints";

fn stuck_call_keeps_the_link(
    check: &mut dyn FnMut(bool, &str) -> bool,
    me: &Path,
    daemon: &Lemnosd,
    bridge: &Bridge,
    lent: &mut dyn FnMut() -> Option<bool>,
    round: u32,
) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let named = |what: &str| format!("stuck call {round}: {what}");

    let threads = daemon.lane_threads();
    check(
        !threads.is_empty(),
        &named(&format!("the daemon's lane threads are found: {threads:?}")),
    );
    let offers = bridge.count(OFFERED);
    let Some(run) = HostRun::start(me, 9 + round, PACED_ROUNDS, 0, PACE_MS) else {
        check(false, &named("the paced host session starts"));
        return;
    };
    check(
        wait_until(LONG, || run.count(host::ROUND) >= 3),
        &named("paced host session: up, and its round trips under way"),
    );

    // The controller's hold on the gadget, sampled from here to the end of the session.
    let bound = Arc::new(AtomicBool::new(enabled()));
    let sampling = Arc::new(AtomicBool::new(true));
    let sampler = {
        let (bound, sampling) = (Arc::clone(&bound), Arc::clone(&sampling));
        std::thread::spawn(move || {
            while sampling.load(Ordering::Relaxed) {
                if !enabled() {
                    bound.store(false, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };

    let ran_past = daemon.count_both(GADGET, RAN_PAST);
    let kept = daemon.count(KEPT);
    let held: Vec<_> = threads
        .iter()
        .filter_map(|&tid| hold_waiting(tid))
        .collect();
    check(
        held.len() == threads.len(),
        &named("every lane thread of the daemon stands still"),
    );
    let before = run.count(host::ROUND);
    // The gadget ticks only when it has something to do, so the check's own call into it is what
    // runs past the deadline: it is answered with an error once the host abandons it.
    let answered = daemon.connect("stuck-call").and_then(|mut session| {
        let mut handle = session.open("gadget", "usb.function/1")?;
        let mut reply = [0u8; 8];
        session.call(&mut handle, PAIR_LENT, &[], &mut reply)
    });
    check(
        answered.is_err(),
        &named(&format!(
            "a call into the stuck gadget is not answered ({answered:?})"
        )),
    );
    check(
        wait_until(Duration::from_secs(15), || {
            daemon.count_both(GADGET, RAN_PAST) > ran_past
        }),
        &named("the gadget's call runs past its deadline and the host abandons it"),
    );
    std::thread::sleep(HELD_ON);
    let during = run.count(host::ROUND);
    check(
        during > before,
        &named(&format!(
            "the holder's copies go on while the driver stands still ({before} round trips, then {during})"
        )),
    );
    // Each is released, whatever an earlier one answered.
    let failed = held
        .into_iter()
        .map(lemnos_linux_sys::test_hold::Held::release)
        .filter(Result::is_err)
        .count();
    let released = failed == 0;
    check(released, &named("the lane threads run again"));
    check(
        daemon.wait_for(KEPT, kept + 1, LONG),
        &named(
            "the call returned: the old instance parked its session and is kept across the rebind",
        ),
    );
    check(
        wait_until(Duration::from_secs(10), || lent() == Some(true)),
        &named("the new instance has the holder's pair: the description says it is lent"),
    );
    let finished = run.finish();
    sampling.store(false, Ordering::Relaxed);
    let _ = sampler.join();
    check(
        finished,
        &named(
            "paced host session: every round trip came back, across the stuck call and the rebind",
        ),
    );
    check(
        bound.load(Ordering::Relaxed),
        &named("the controller never let go of the gadget: UDC stayed set throughout"),
    );
    check(
        daemon.count(LET_GO) == 0 && daemon.count(NOT_TAKEN_UP) == 0,
        &named("the kept instance was never let go early, and the new one took the endpoints up"),
    );
    check(
        bridge.count(OFFERED) == offers,
        &named("the bridge kept its offer: no second offer across the rebind"),
    );
}

/// The futex system call's number: what a thread waiting for work, or for a lock, is in.
#[cfg(target_arch = "x86_64")]
const SYS_FUTEX: &str = "202";
#[cfg(target_arch = "aarch64")]
const SYS_FUTEX: &str = "98";

/// Whether a `/proc/<tid>/syscall` line is a futex wait: the futex call, with an operation that
/// waits (`FUTEX_WAIT` 0 or `FUTEX_WAIT_BITSET` 9, whatever its flags). A futex wake is the same
/// call and no wait: a lane wakes its caller while it still holds its mutex.
fn in_futex_wait(line: &str) -> bool {
    let mut words = line.split(' ');
    if words.next() != Some(SYS_FUTEX) {
        return false;
    }
    let op = words
        .nth(1)
        .and_then(|op| u64::from_str_radix(op.trim_start_matches("0x"), 16).ok());
    matches!(op.map(|op| op & 0x7f), Some(0 | 9))
}

/// Says what a stopped thread's stack holds of its program's code: each word of the stack's first
/// 8 KiB that points into an executable mapping of the daemon's file, as its offset in the file, so
/// the line can be read against the unstripped binary. Its tracer may read its memory; nothing is
/// written. Says nothing when a file can't be read.
fn say_stack(tid: i32, at: &str) {
    use std::os::unix::fs::FileExt;
    let hex = |word: &str| u64::from_str_radix(word.trim_start_matches("0x"), 16).ok();
    let mut words = at.rsplit(' ');
    let (Some(pc), Some(sp)) = (words.next().and_then(hex), words.next().and_then(hex)) else {
        return;
    };
    let Ok(maps) = fs::read_to_string(format!("/proc/{tid}/maps")) else {
        return;
    };
    // (start, end, file offset) of each executable mapping of a file.
    let text: Vec<(u64, u64, u64)> = maps
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (range, perms, offset) = (fields.next()?, fields.next()?, fields.next()?);
            let path = fields.nth(2)?;
            if !perms.contains('x') || !path.starts_with('/') {
                return None;
            }
            let (start, end) = range.split_once('-')?;
            Some((hex(start)?, hex(end)?, hex(offset)?))
        })
        .collect();
    let offset_of = |address: u64| {
        text.iter()
            .find(|(start, end, _)| (*start..*end).contains(&address))
            .map(|(start, _, offset)| address - start + offset)
    };
    let Ok(memory) = fs::File::open(format!("/proc/{tid}/mem")) else {
        return;
    };
    let mut stack = vec![0u8; 8192];
    let Ok(read) = memory.read_at(&mut stack, sp) else {
        return;
    };
    let found: Vec<String> = offset_of(pc)
        .into_iter()
        .chain(
            stack[..read]
                .as_chunks::<8>()
                .0
                .iter()
                .filter_map(|word| offset_of(u64::from_le_bytes(*word))),
        )
        .take(40)
        .map(|offset| format!("{offset:#x}"))
        .collect();
    say!(
        "     lane thread {tid} code offsets, innermost first: {}",
        found.join(" ")
    );
}

/// Stops lane thread `tid` while it waits, and says where it stood (`/proc/<tid>/syscall`: the
/// call's number and arguments, the stack pointer and the program counter).
///
/// A driver's call that hangs holds what the driver took and nothing of the host's: the lane's own
/// mutex is held only by the lane's bookkeeping around the call. A tracer can stop a thread at any
/// instruction, so it can stop one inside that bookkeeping, holding the mutex; the caller's deadline
/// is a timed wait that starts by taking it, and never comes (the host's own test,
/// `a_holder_of_the_lanes_mutex_stops_the_callers_deadline_with_it`). That is not the fault this
/// check stages. So a thread found running is let go and stopped again, until it is found in a
/// wait: waiting for work it holds nothing, and waiting inside a call it holds what a hung driver
/// would. Each time is said, with where it stood.
///
/// One of the daemon's lanes is busy through this check (its condvar counts hundreds of thousands
/// of wakes, three a call), so its thread is as likely to be stopped in the lane's handoff as in
/// its wait. Seen in runs of this check, each resolved against the unstripped daemon:
/// - stopped in a futex wake: the lane's `notify_all` after it stored a result, made with the
///   lane's mutex held (the stop that froze every caller), and the mutex's own unlock wake;
/// - stopped in a futex wait: for work (the lane's condvar), or, just woken, for the lane's mutex
///   while the caller takes its result (a wait on the value 2). In both it holds nothing of the
///   lane's, so those are where it is held.
fn hold_waiting(tid: i32) -> Option<lemnos_linux_sys::test_hold::Held> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let held = match lemnos_linux_sys::test_hold::hold(tid) {
            Ok(held) => held,
            Err(error) => {
                say!("     lane thread {tid} not held: {error}");
                return None;
            }
        };
        let at = fs::read_to_string(format!("/proc/{tid}/syscall"))
            .map(|text| text.trim().to_string())
            .unwrap_or_else(|error| format!("<{error}>"));
        if in_futex_wait(&at) {
            say!("     lane thread {tid} stopped in a wait: {at}");
            say_stack(tid, &at);
            return Some(held);
        }
        say_stack(tid, &at);
        say!("     lane thread {tid} was not in a wait when stopped ({at}): let go, stopped again");
        if let Err(error) = held.release() {
            say!("     lane thread {tid} not let go: {error}");
            return None;
        }
        if Instant::now() >= deadline {
            say!("     lane thread {tid} never found in a wait");
            return None;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// The delays the hello sweep tries, in milliseconds, one bounce each. A short delay sends the host's
/// first HELLO before the bridge has seen the gadget up again.
const HELLO_SWEEP_MS: [u64; 5] = [0, 150, 400, 1000, 2500];

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
fn host_session(me: &Path, number: u32, hello_after_ms: u64) -> bool {
    HostRun::start(me, number, ROUNDS, hello_after_ms, 0).is_some_and(HostRun::finish)
}

/// A host session running as a child, its lines kept as they come.
struct HostRun {
    child: Child,
    lines: Arc<Mutex<Vec<(Instant, String)>>>,
    reader: std::thread::JoinHandle<()>,
}

impl HostRun {
    fn start(
        me: &Path,
        number: u32,
        rounds: u64,
        hello_after_ms: u64,
        pace_ms: u64,
    ) -> Option<Self> {
        let child = Command::new(me)
            .arg("host")
            .arg(rounds.to_string())
            .arg(hello_after_ms.to_string())
            .arg(pace_ms.to_string())
            .stdin(if rounds == 0 {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            say!("FAIL host session {number}: cannot start");
            return None;
        };
        let lines: Arc<Mutex<Vec<(Instant, String)>>> = Arc::default();
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
                            .push((Instant::now(), line));
                    }
                }
            })
        };
        Some(Self {
            child,
            lines,
            reader,
        })
    }

    /// How many lines so far contain `text`.
    fn count(&self, text: &str) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, line)| line.contains(text))
            .count()
    }

    /// The longest time between two lines that contain `text`.
    fn longest_gap(&self, text: &str) -> Duration {
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        let at: Vec<Instant> = lines
            .iter()
            .filter(|(_, line)| line.contains(text))
            .map(|(at, _)| *at)
            .collect();
        at.windows(2)
            .map(|pair| pair[1].duration_since(pair[0]))
            .max()
            .unwrap_or_default()
    }

    /// Waits for the child. True when the session came up, the round trips followed, and the
    /// child exited cleanly. A session started with no count of rounds is told to end first.
    fn finish(mut self) -> bool {
        drop(self.child.stdin.take());
        let status = self.child.wait();
        let _ = self.reader.join();
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        let up = lines
            .iter()
            .position(|(_, l)| l.contains("host session up"));
        let trips = lines
            .iter()
            .position(|(_, l)| l.contains("round trips with the node"));
        status.is_ok_and(|status| status.success())
            && matches!((up, trips), (Some(u), Some(t)) if u < t)
    }
}

#[cfg(test)]
mod hold_tests {
    use super::{SYS_FUTEX, in_futex_wait};

    #[test]
    fn only_a_futex_wait_is_a_wait() {
        // A private wait and a private bitset wait.
        assert!(in_futex_wait(&format!(
            "{SYS_FUTEX} 0x7f00 0x80 0x2 0x0 0x0 0x0 0x7ffd 0x4011"
        )));
        assert!(in_futex_wait(&format!(
            "{SYS_FUTEX} 0x7f00 0x189 0x2 0x0 0x0 0xffffffff 0x7ffd 0x4011"
        )));
        // A private wake, another call, a thread in no call, and a thread that is running.
        assert!(!in_futex_wait(&format!(
            "{SYS_FUTEX} 0x7f00 0x81 0x1 0x0 0x0 0x0 0x7ffd 0x4011"
        )));
        assert!(!in_futex_wait(
            "0 0x3 0x7f00 0x40 0x0 0x0 0x0 0x7ffd 0x4011"
        ));
        assert!(!in_futex_wait("-1 0x7ffd 0x4011"));
        assert!(!in_futex_wait("running"));
    }
}

#[cfg(test)]
mod gap_tests {
    use super::{GAP_PAST_STALL, MAX_GAP, within_gap};
    use std::time::Duration;

    #[test]
    fn a_link_gap_counts_against_the_link_only_past_the_guests_own_stall() {
        let ms = Duration::from_millis;
        assert!(within_gap(MAX_GAP, ms(0)));
        assert!(
            !within_gap(MAX_GAP + ms(1), ms(50)),
            "a stalled link in a guest that ran"
        );
        assert!(
            within_gap(ms(1368), ms(1300)),
            "the whole guest stood still"
        );
        assert!(!within_gap(ms(900) + GAP_PAST_STALL + ms(1), ms(900)));
        // A guest that stood still past a second: up to the retransmission timeout at or past it
        // (retries at 200, 600, 1400, 3000, 6200 ms).
        assert!(within_gap(ms(1400) + GAP_PAST_STALL, ms(1300)));
        assert!(!within_gap(ms(1400) + GAP_PAST_STALL + ms(1), ms(1300)));
        assert!(within_gap(ms(4976), ms(3923)), "the QEMU run under load");
        assert!(!within_gap(ms(6200) + GAP_PAST_STALL + ms(1), ms(3923)));
    }
}
