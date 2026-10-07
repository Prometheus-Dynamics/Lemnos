//! `lemnosd [--board PATH] [--socket PATH] [--update-status PATH|off]
//! [--booting-ms N] [--check]`
//!
//! Environment: `LEMNOSD_BOARD`, `LEMNOSD_SOCKET`, `LEMNOSD_UPDATE_STATUS`,
//! `LEMNOSD_BOOTING_MS` (flags win).
#![allow(clippy::print_stdout, clippy::print_stderr)]

use lemnos_board::{BoardDefinition, DriverRegistry, LinuxBuses};
use lemnos_linux_sys::signal::{SIGINT, SIGTERM, SignalFd};
use lemnosd::{DEFAULT_BOARD, DEFAULT_SOCKET, Service, ServiceConfig};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let mut board = std::env::var("LEMNOSD_BOARD").unwrap_or_else(|_| DEFAULT_BOARD.into());
    let mut socket = std::env::var("LEMNOSD_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.into());
    // No built-in path: the unit (or the environment file) names the device
    // package's status file, and without it updates stay off the light.
    let mut update = std::env::var("LEMNOSD_UPDATE_STATUS").unwrap_or_default();
    let mut booting: u64 = std::env::var("LEMNOSD_BOOTING_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_default();
        match arg.as_str() {
            "--board" => board = value(),
            "--socket" => socket = value(),
            "--update-status" => update = value(),
            "--booting-ms" => booting = value().parse().unwrap_or(0),
            "--check" => check = true,
            "--help" | "-h" => {
                println!(
                    "lemnosd [--board PATH] [--socket PATH] [--update-status PATH|off] [--booting-ms N] [--check]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("lemnosd: unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }

    let definition = match BoardDefinition::from_path(&board)
        .and_then(|b| b.validate(&DriverRegistry::builtin()).map(|()| b))
    {
        Ok(definition) => definition,
        Err(error) => {
            eprintln!("lemnosd: {board}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if check {
        println!("{board}: ok ({} devices)", definition.devices.len());
        return ExitCode::SUCCESS;
    }
    // Block SIGTERM/SIGINT before anything else, so the loop sees them.
    let signals = match SignalFd::new(&[SIGTERM, SIGINT]) {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("lemnosd: signals: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut config = ServiceConfig::new(definition, PathBuf::from(&socket));
    config.update_status = (!update.is_empty() && update != "off").then(|| PathBuf::from(&update));
    config.booting_ms = (booting > 0).then_some(booting);
    let mut service = match Service::new(config, Box::new(LinuxBuses::default())) {
        Ok(service) => service.with_systemd(),
        Err(error) => {
            eprintln!("lemnosd: {error}");
            return ExitCode::FAILURE;
        }
    };

    // On a panic, hand fans back to the kernel before aborting.
    let targets: Arc<Mutex<Vec<lemnos_drivers_linux::FanRestore>>> = Arc::default();
    let hook_targets = Arc::clone(&targets);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(targets) = hook_targets.lock() {
            lemnosd::restore_fans(&targets);
        }
        default_hook(info);
    }));

    eprintln!("lemnosd: serving {socket}");
    let mut next_targets = Instant::now();
    loop {
        if Instant::now() >= next_targets {
            if let Ok(mut t) = targets.lock() {
                *t = service.fan_restore_targets();
            }
            next_targets = Instant::now() + Duration::from_secs(5);
        }
        match service.step(Duration::from_secs(1), Some(signals.as_fd())) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                eprintln!("lemnosd: {error}");
                service.shutdown(false);
                return ExitCode::FAILURE;
            }
        }
    }
    let _ = signals.read();
    service.shutdown(lemnosd::system_stopping());
    eprintln!("lemnosd: stopped");
    ExitCode::SUCCESS
}
