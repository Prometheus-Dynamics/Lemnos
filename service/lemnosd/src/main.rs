//! `lemnosd [--sim] [--board PATH] [--socket PATH] [--check]
//!          [--packages DIR] [--package-keys FILE] [--package-mode signed|open]`
//!
//! Driver packages are off unless `--packages` (or `LEMNOSD_PACKAGES`) names a directory. They
//! then load in signed mode, against the keys in `--package-keys` (`LEMNOSD_PACKAGE_KEYS`),
//! unless `--package-mode open` (`LEMNOSD_PACKAGE_MODE`) says any package may run. What was
//! loaded and what was refused goes to the error output, and to `lemnos-ctl packages`. A
//! statically linked build loads none, and says so.

use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use lemnos_host::Options;
use lemnos_host::board::parse_lenient;
use lemnos_linux::LinuxBoard;
use lemnos_linux::SharedHelper;
use lemnos_linux::hotplug::BoardHints;
use lemnos_linux_sys::poll::{POLLIN, poll_one};
use lemnos_linux_sys::signal::{SIGINT, SIGTERM, SignalFd};
use lemnos_sim::SimBoard;
use lemnosd::gadget;
use lemnosd::helper;
use lemnosd::packages::{PackageConfig, add_packages};
use lemnosd::privilege;
use lemnosd::{Config, DEFAULT_BOARD, DEFAULT_SOCKET, Service};

const USAGE: &str = "lemnosd [--sim] [--board PATH] [--socket PATH] [--check] \
     [--packages DIR] [--package-keys FILE] [--package-mode signed|open]";

/// Prints each way the live device tree differs from the board file, from the SoC map the board
/// names (read from `$LEMNOS_SOCS`, `/usr/share/lemnos/socs` by default). Reports only.
fn soc_map(board: &lemnos_host::board::BoardFile) -> Option<lemnos_devtree::SocMap> {
    let dir = std::env::var("LEMNOS_SOCS").unwrap_or_else(|_| "/usr/share/lemnos/socs".into());
    let soc = board.board.soc.as_deref()?;
    let path = format!("{dir}/{soc}.toml");
    match std::fs::read_to_string(&path)
        .map_err(|e| format!("{path}: {e}"))
        .and_then(|text| lemnos_devtree::SocMap::parse(&text))
    {
        Ok(map) => Some(map),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "lemnosd: live tree not checked: {error}");
            None
        }
    }
}

/// Prints each way the live device tree differs from the board file, from its SoC map. Run after the
/// root phase, so a node the root phase gave to the module is checked as it now is.
fn report_live_tree(board: &lemnos_host::board::BoardFile, map: Option<&lemnos_devtree::SocMap>) {
    let Some(map) = map else {
        return;
    };
    let chips: Vec<(String, u32)> = lemnos_linux::gpio_chips(std::path::Path::new("/dev"))
        .into_iter()
        .map(|chip| (chip.label, chip.lines))
        .collect();
    let live = lemnos_devtree::check::Live {
        tree: std::path::Path::new("/proc/device-tree"),
        sys: std::path::Path::new("/sys/bus/platform/devices"),
        gpio_chips: &chips,
        udc: std::path::Path::new("/sys/class/udc"),
        gpio_debug: std::path::Path::new("/sys/kernel/debug/gpio"),
        regulators: std::path::Path::new("/sys/class/regulator"),
    };
    let findings = lemnos_devtree::check(board, map, &live);
    for problem in &findings.problems {
        let _ = writeln!(std::io::stderr(), "lemnosd: live tree: {problem}");
    }
    for note in &findings.notes {
        let _ = writeln!(std::io::stderr(), "lemnosd: live tree note: {note}");
    }
}

fn complain(text: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "lemnosd: {text}");
    ExitCode::FAILURE
}

fn drivers() -> Vec<lemnos_host::DriverEntry> {
    #[allow(unused_mut)]
    let mut drivers = lemnosd::builtin::drivers();
    #[cfg(feature = "fake-devices")]
    {
        use lemnos_fake_devices::{
            FakeAccelerometer, FakeCounter, FakeFan, FakeFifoAccelerometer, FakeFifoGyroscope,
            FakeGyroscope, FakeMagnetometer, FakeThermalZone,
        };
        use lemnos_host::entry;
        drivers.extend([
            entry::<FakeAccelerometer>(),
            entry::<FakeGyroscope>(),
            entry::<FakeMagnetometer>(),
            entry::<FakeCounter>(),
            entry::<FakeFifoAccelerometer>(),
            entry::<FakeFifoGyroscope>(),
            entry::<FakeFan>(),
            entry::<FakeThermalZone>(),
        ]);
    }
    drivers
}

fn wait_for_signal(signals: &SignalFd) -> std::io::Result<()> {
    loop {
        poll_one(signals.as_fd(), POLLIN, None)?;
        if signals.read()?.is_some() {
            return Ok(());
        }
    }
}

/// The gadget helper's daemon end, shared by the USB gadgets, with each gadget's controller name
/// and index in the helper.
type GadgetHelper = (Arc<Mutex<helper::Client>>, Vec<(String, u32)>);

/// The machine's hardware, through the kernel module where it's loaded, and the USB gadgets through
/// the gadget helper when the root phase started one.
fn linux_board(gadget_helper: Option<&GadgetHelper>) -> LinuxBoard {
    let mut board = LinuxBoard::new();
    if let Some((client, gadgets)) = gadget_helper {
        let shared: SharedHelper = client.clone();
        board.use_gadget_helper(shared, gadgets.clone());
    }
    if let Err(error) = board.open_module()
        && error.kind() != std::io::ErrorKind::NotFound
    {
        let _ = complain(&format!("not using the kernel module: {error}"));
    }
    board
}

/// The route-A check's seam (feature `testing-boot-roots`, never shipped): the boot chain from
/// `LEMNOSD_TEST_BOOT_ROOTS=CHOSEN:SYS:DEV`, set before the root phase, which reads it first.
#[cfg(feature = "testing-boot-roots")]
fn test_boot_roots() {
    let Some(value) = std::env::var_os("LEMNOSD_TEST_BOOT_ROOTS") else {
        return;
    };
    let value = value.to_string_lossy().into_owned();
    let parts: Vec<&str> = value.split(':').collect();
    if let [chosen, sys, dev] = parts[..] {
        let roots = lemnos_slot_store::boot_node::BootRoots {
            chosen: PathBuf::from(chosen),
            sys: PathBuf::from(sys),
            dev: PathBuf::from(dev),
        };
        if let Ok(mut held) = lemnos_slot_store::boot_node::TEST_ROOTS.lock() {
            *held = Some(roots);
        }
        let _ = writeln!(std::io::stderr(), "lemnosd: test boot roots {value}");
    } else {
        let _ = writeln!(
            std::io::stderr(),
            "lemnosd: LEMNOSD_TEST_BOOT_ROOTS is not CHOSEN:SYS:DEV: {value}"
        );
    }
}

fn main() -> ExitCode {
    // The start trace (`LEMNOSD_START_TRACE=1`): the start's steps and each device's bind and first
    // tick, stamped, for the watchdog hand-over's gap to be measured in its pieces.
    let tracing = std::env::var_os("LEMNOSD_START_TRACE").is_some_and(|v| !v.is_empty());
    // The stop point (`LEMNOSD_STOP_AFTER=root-phase` or `bind:<device id>`): this start stops itself right after
    // the trace line that ends that stage, for the QEMU watchdog check. It is honoured only with the trace on.
    let stop_after = std::env::var("LEMNOSD_STOP_AFTER")
        .ok()
        .filter(|v| !v.is_empty());
    if tracing {
        lemnosd::start_trace_on();
        lemnos_host::set_start_trace(lemnosd::start_trace_line);
        if let Some(text) = stop_after {
            let Some(point) = lemnosd::StopAfter::parse(&text) else {
                return complain(&format!(
                    "LEMNOSD_STOP_AFTER is not root-phase or bind:<device id>: {text}"
                ));
            };
            lemnosd::stop_after_on(point);
        }
        lemnosd::start_trace_line("exec");
    } else if stop_after.is_some() {
        let _ = writeln!(
            std::io::stderr(),
            "lemnosd: LEMNOSD_STOP_AFTER is honoured only with LEMNOSD_START_TRACE: not stopping"
        );
    }
    // The host's warnings are journal lines at warning priority, with or without the start trace.
    lemnos_host::set_warning_sink(lemnosd::warning_line);
    let var = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
    let mut board = var("LEMNOSD_BOARD", DEFAULT_BOARD);
    let mut socket = var("LEMNOSD_SOCKET", DEFAULT_SOCKET);
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let mut packages = set("LEMNOSD_PACKAGES").map(PathBuf::from);
    let mut package_keys = set("LEMNOSD_PACKAGE_KEYS").map(PathBuf::from);
    let mut package_mode = var("LEMNOSD_PACKAGE_MODE", "signed");
    let mut user = set("LEMNOSD_USER").and_then(|value| value.into_string().ok());
    let mut group = set("LEMNOSD_GROUP").and_then(|value| value.into_string().ok());
    let mut sim = false;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--board" => board = args.next().unwrap_or_default(),
            "--socket" => socket = args.next().unwrap_or_default(),
            "--packages" => packages = args.next().map(PathBuf::from),
            "--package-keys" => package_keys = args.next().map(PathBuf::from),
            "--package-mode" => package_mode = args.next().unwrap_or_default(),
            "--user" => user = args.next(),
            "--group" => group = args.next(),
            "--sim" => sim = true,
            "--check" => check = true,
            _ => return complain(USAGE),
        }
    }
    let packages = PackageConfig {
        dir: packages,
        keys: package_keys,
        open: match package_mode.as_str() {
            "signed" => false,
            "open" => true,
            _ => return complain(USAGE),
        },
    };
    // The stop signals are blocked from here on, before the board is read, the privilege phase
    // forks anything, or a thread starts. Until this line a SIGTERM takes the process down with
    // the default action (status 143), so a stop that comes during start-up is a crash, not a
    // stop. Pending until `wait_for_signal` reads them, the signal is then handled as any other.
    let signals = match SignalFd::new(&[SIGTERM, SIGINT]) {
        Ok(signals) => signals,
        Err(error) => return complain(&format!("signals: {error}")),
    };
    let text = match std::fs::read_to_string(&board) {
        Ok(text) => text,
        Err(error) => return complain(&format!("{board}: {error}")),
    };
    if check {
        return match lemnosd::check(&text, &drivers()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(reason) => complain(&format!("{board}: {reason}")),
        };
    }
    // A device whose own entry is wrong is kept, refused with its reason: the file is refused only
    // for what nothing in it can run without (see `parse_lenient`).
    let mut board_file = match parse_lenient(&text) {
        Ok(board_file) => board_file,
        Err(error) => return complain(&format!("{board}: {error}")),
    };
    // Each gadget's board serial is read once, here, so the root phase and the daemon agree on it. A
    // gadget whose identity is wrong is refused here, with the reason.
    gadget::resolve(&mut board_file);
    // Each driver's settings check, before anything is made. A device whose settings a driver
    // refuses is not made: the daemon starts without it, and says so here; its state carries the
    // same reason. `--check` refused the board above instead, as `lemnos-ctl validate` does.
    let mut drivers = drivers();
    let refused = lemnos_host::settings_refusals(&board_file, &drivers);
    for (_, reason) in &refused {
        let _ = writeln!(std::io::stderr(), "lemnosd: {board}: not started: {reason}");
    }
    // The board's SoC map: the root phase claims the module's nodes from it, and the live tree is
    // checked against it after that. None under --sim, which touches no hardware.
    let map = if sim { None } else { soc_map(&board_file) };
    // The root phase: as root, the privileged start, then the drop to the requested user. Without
    // root there is nothing to drop, and the daemon says which capabilities it lacks.
    let drop = match privilege::request(user, group) {
        Ok(drop) => drop,
        Err(error) => return complain(&error),
    };
    #[cfg(feature = "testing-boot-roots")]
    test_boot_roots();
    let mut root = privilege::Started::default();
    match (drop, lemnos_linux_sys::privilege::observe()) {
        (Some(request), Ok(now)) if now.euid == 0 => {
            match privilege::start(
                &request,
                &board_file,
                map.as_ref(),
                Path::new(&socket),
                !sim,
            ) {
                Ok(started) => root = started,
                Err(error) => return complain(&error),
            }
        }
        (None, Ok(now)) if now.euid == 0 => {
            let _ =
                complain("running as root; give --user lemnos (LEMNOSD_USER) to drop privileges");
        }
        (_, Ok(now)) => {
            if let Ok(Some(line)) = privilege::lacks(&now) {
                let _ = complain(&line);
            }
        }
        (_, Err(error)) => return complain(&format!("reading the credentials: {error}")),
    }
    if tracing {
        lemnosd::start_trace_line("root phase done (the drop included, when asked)");
    }
    // The live device tree against what the board file needs, when the board names a SoC map, after
    // the root phase's claim. A mismatch is reported and the daemon starts anyway.
    report_live_tree(&board_file, map.as_ref());
    // A device the root phase could not set up is refused, and the daemon starts without it.
    for line in &root.warnings {
        let _ = writeln!(std::io::stderr(), "lemnosd: {board}: {line}");
    }
    for (_, reason) in &root.refused {
        let _ = writeln!(std::io::stderr(), "lemnosd: {board}: not started: {reason}");
    }
    privilege::refuse(&mut board_file, &root.refused);
    // The gadget helper the root phase forked, if the board has a gadget: the daemon's end, with the
    // gadgets' indexes in it.
    let gadget_helper: Option<GadgetHelper> = root.helper.take().map(|client| {
        let gadgets = client.gadgets().to_vec();
        (Arc::new(Mutex::new(client)), gadgets)
    });
    // What the board file calls its hardware, for the backend to say which of it comes and goes.
    // An `[[each]]` entry is told of by its pattern.
    let names: Vec<String> = board_file
        .devices
        .iter()
        .flat_map(|device| device.bus.iter().chain(device.uses.values()))
        .chain(board_file.each.iter().map(|each| &each.pattern))
        .cloned()
        .collect();
    let packages = add_packages(&mut drivers, &packages);
    for line in packages.lines() {
        let _ = complain(&line);
    }
    let config = Config {
        socket: PathBuf::from(socket),
        name: board_file.board.id.clone(),
        board: board_file,
        drivers,
        backend: if sim {
            Box::new(SimBoard::new())
        } else {
            Box::new(linux_board(gadget_helper.as_ref()))
        },
        options: Options::default(),
    };
    if tracing {
        lemnosd::start_trace_line("host start begins: binds in board order");
    }
    let started = Service::start_with_packages(config, packages);
    if tracing {
        lemnosd::start_trace_line("host started: every bind made");
    }
    let mut service = match started {
        Ok(service) => service,
        Err(error) => return complain(&error.to_string()),
    };
    // Hints only: without them devices still bind, when their retries come round.
    if !sim
        && let Some(hints) = BoardHints::open(names.iter().map(String::as_str))
        && let Err(error) = service.watch(Box::new(hints))
    {
        let _ = complain(&format!(
            "not watching for hardware coming and going: {error}"
        ));
    }
    // A signal is the normal way out, and stopping is what releases the hardware.
    if let Err(error) = wait_for_signal(&signals) {
        let _ = complain(&format!("waiting for a signal: {error}"));
    }
    service.stop();
    // Stopping takes the gadgets down: unbound, their trees removed, their mounts unmounted. Then
    // the helper ends with the daemon's end of its socket.
    if let Some((client, _)) = &gadget_helper
        && let Ok(mut client) = client.lock()
    {
        client.teardown_all();
    }
    ExitCode::SUCCESS
}
