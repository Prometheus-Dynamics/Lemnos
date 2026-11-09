//! `lemnosd [--sim] [--board PATH] [--socket PATH] [--check]`

use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::ExitCode;

use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_linux::LinuxBoard;
use lemnos_linux_sys::poll::{POLLIN, poll_one};
use lemnos_linux_sys::signal::{SIGINT, SIGTERM, SignalFd};
use lemnos_sim::SimBoard;
use lemnosd::{Config, DEFAULT_BOARD, DEFAULT_SOCKET, Service};

const USAGE: &str = "lemnosd [--sim] [--board PATH] [--socket PATH] [--check]";

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

fn main() -> ExitCode {
    let var = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
    let mut board = var("LEMNOSD_BOARD", DEFAULT_BOARD);
    let mut socket = var("LEMNOSD_SOCKET", DEFAULT_SOCKET);
    let mut sim = false;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--board" => board = args.next().unwrap_or_default(),
            "--socket" => socket = args.next().unwrap_or_default(),
            "--sim" => sim = true,
            "--check" => check = true,
            _ => return complain(USAGE),
        }
    }
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
    // Before any thread starts, so that every thread has these signals blocked.
    let signals = match SignalFd::new(&[SIGTERM, SIGINT]) {
        Ok(signals) => signals,
        Err(error) => return complain(&format!("signals: {error}")),
    };
    let service = Service::start(Config {
        socket: PathBuf::from(socket),
        name: board_file.board.id.clone(),
        board: board_file,
        drivers: drivers(),
        backend: if sim {
            Box::new(SimBoard::new())
        } else {
            Box::new(LinuxBoard::new())
        },
        options: Options::default(),
    });
    let service = match service {
        Ok(service) => service,
        Err(error) => return complain(&error.to_string()),
    };
    // A signal is the normal way out, and stopping is what releases the hardware.
    if let Err(error) = wait_for_signal(&signals) {
        let _ = complain(&format!("waiting for a signal: {error}"));
    }
    service.stop();
    ExitCode::SUCCESS
}
