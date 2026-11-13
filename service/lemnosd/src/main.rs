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

use lemnos_host::Options;
use lemnos_host::board::parse_lenient;
use lemnos_linux::LinuxBoard;
use lemnos_linux::hotplug::BoardHints;
use lemnos_linux_sys::poll::{POLLIN, poll_one};
use lemnos_linux_sys::signal::{SIGINT, SIGTERM, SignalFd};
use lemnos_sim::SimBoard;
use lemnosd::gadget;
use lemnosd::packages::{PackageConfig, add_packages};
use lemnosd::privilege;
use lemnosd::{Config, DEFAULT_BOARD, DEFAULT_SOCKET, Service};

const USAGE: &str = "lemnosd [--sim] [--board PATH] [--socket PATH] [--check] \
     [--packages DIR] [--package-keys FILE] [--package-mode signed|open]";

/// Prints each way the live device tree differs from the board file, from the SoC map the board
/// names (read from `$LEMNOS_SOCS`, `/usr/share/lemnos/socs` by default). Reports only.
fn report_live_tree(board: &lemnos_host::board::BoardFile) {
    let dir = std::env::var("LEMNOS_SOCS").unwrap_or_else(|_| "/usr/share/lemnos/socs".into());
    let Some(soc) = board.board.soc.as_deref() else {
        return;
    };
    let path = format!("{dir}/{soc}.toml");
    let map = match std::fs::read_to_string(&path)
        .map_err(|e| format!("{path}: {e}"))
        .and_then(|text| lemnos_devtree::SocMap::parse(&text))
    {
        Ok(map) => map,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "lemnosd: live tree not checked: {error}");
            return;
        }
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
    let findings = lemnos_devtree::check(board, &map, &live);
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
            FakeAccelerometer, FakeCounter, FakeGyroscope, FakeMagnetometer,
        };
        use lemnos_host::entry;
        drivers.extend([
            entry::<FakeAccelerometer>(),
            entry::<FakeGyroscope>(),
            entry::<FakeMagnetometer>(),
            entry::<FakeCounter>(),
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

/// The machine's hardware, through the kernel module where it's loaded.
fn linux_board() -> LinuxBoard {
    let mut board = LinuxBoard::new();
    if let Err(error) = board.open_module()
        && error.kind() != std::io::ErrorKind::NotFound
    {
        let _ = complain(&format!("not using the kernel module: {error}"));
    }
    board
}

fn main() -> ExitCode {
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
    let text = match std::fs::read_to_string(&board) {
        Ok(text) => text,
        Err(error) => return complain(&format!("{board}: {error}")),
    };
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
    // same reason. `--check` refuses the board instead, as `lemnos-ctl validate` does.
    let mut drivers = drivers();
    let refused = lemnos_host::settings_refusals(&board_file, &drivers);
    if check {
        return match refused.first() {
            Some((_, reason)) => complain(&format!("{board}: {reason}")),
            None => ExitCode::SUCCESS,
        };
    }
    for (_, reason) in &refused {
        let _ = writeln!(std::io::stderr(), "lemnosd: {board}: not started: {reason}");
    }
    // The live device tree against what the board file needs, when the board names a SoC map. A
    // mismatch is reported and the daemon starts anyway: nothing here stops or changes hardware.
    if !sim && board_file.board.soc.is_some() {
        report_live_tree(&board_file);
    }
    // The root phase: as root, the privileged start, then the drop to the requested user. Without
    // root there is nothing to drop, and the daemon says which capabilities it lacks.
    let drop = match privilege::request(user, group) {
        Ok(drop) => drop,
        Err(error) => return complain(&error),
    };
    let mut root = privilege::Started::default();
    match (drop, lemnos_linux_sys::privilege::observe()) {
        (Some(request), Ok(now)) if now.euid == 0 => {
            match privilege::start(&request, &board_file, Path::new(&socket)) {
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
    // A device the root phase could not set up is refused, and the daemon starts without it.
    for line in &root.warnings {
        let _ = writeln!(std::io::stderr(), "lemnosd: {board}: {line}");
    }
    for (_, reason) in &root.refused {
        let _ = writeln!(std::io::stderr(), "lemnosd: {board}: not started: {reason}");
    }
    privilege::refuse(&mut board_file, &root.refused);
    // What the board file calls its hardware, for the backend to say which of it comes and goes.
    // An `[[each]]` entry is told of by its pattern.
    let names: Vec<String> = board_file
        .devices
        .iter()
        .flat_map(|device| device.bus.iter().chain(device.uses.values()))
        .chain(board_file.each.iter().map(|each| &each.pattern))
        .cloned()
        .collect();
    // Before any thread starts, so that every thread has these signals blocked.
    let signals = match SignalFd::new(&[SIGTERM, SIGINT]) {
        Ok(signals) => signals,
        Err(error) => return complain(&format!("signals: {error}")),
    };
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
            Box::new(linux_board())
        },
        options: Options::default(),
    };
    let mut service = match Service::start_with_packages(config, packages) {
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
    ExitCode::SUCCESS
}
