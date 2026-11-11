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
use std::path::PathBuf;
use std::process::ExitCode;

use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_linux::LinuxBoard;
use lemnos_linux::hotplug::BoardHints;
use lemnos_linux_sys::poll::{POLLIN, poll_one};
use lemnos_linux_sys::signal::{SIGINT, SIGTERM, SignalFd};
use lemnos_sim::SimBoard;
use lemnosd::packages::{PackageConfig, add_packages};
use lemnosd::{Config, DEFAULT_BOARD, DEFAULT_SOCKET, Service};

const USAGE: &str = "lemnosd [--sim] [--board PATH] [--socket PATH] [--check] \
     [--packages DIR] [--package-keys FILE] [--package-mode signed|open]";

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
    let board_file = match parse(&text) {
        Ok(board_file) => board_file,
        Err(error) => return complain(&format!("{board}: {error}")),
    };
    if check {
        return ExitCode::SUCCESS;
    }
    // What the board file calls its hardware, for the backend to say which of it comes and goes.
    let names: Vec<String> = board_file
        .devices
        .iter()
        .flat_map(|device| device.bus.iter().chain(device.uses.values()))
        .cloned()
        .collect();
    // Before any thread starts, so that every thread has these signals blocked.
    let signals = match SignalFd::new(&[SIGTERM, SIGINT]) {
        Ok(signals) => signals,
        Err(error) => return complain(&format!("signals: {error}")),
    };
    let mut drivers = drivers();
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
