//! `lemnosd [--board PATH] [--socket PATH] [--update-status PATH|off]
//! [--booting-ms N] [--check] [--sim [--sim-script PATH] [--sim-root DIR]]`
//!
//! Environment: `LEMNOSD_BOARD`, `LEMNOSD_SOCKET`, `LEMNOSD_UPDATE_STATUS`,
//! `LEMNOSD_BOOTING_MS` (flags win). With `--sim` (feature `sim`, the simulated
//! board of `docs/system-service.md`): `LEMNOSD_SIM`, `LEMNOSD_SIM_SCRIPT` and
//! `LEMNOSD_SIM_ROOT`.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use lemnos_board::{BoardDefinition, Buses, DriverRegistry, LinuxBuses};
use lemnos_linux_sys::signal::{SIGHUP, SIGINT, SIGTERM, SignalFd};
use lemnosd::looks::{DEFAULT_LOOKS_DIR, DEFAULT_OVERRIDE_DIR};
use lemnosd::state::DEFAULT_STATE_DIR;
use lemnosd::{DEFAULT_BOARD, DEFAULT_SOCKET, Service, ServiceConfig};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
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
    // The look files: a read-only directory, and a writable one (`looks save`
    // writes there). `off` (or an empty value) turns either off.
    let looks_dir = std::env::var("LEMNOSD_LOOKS_DIR").unwrap_or_else(|_| DEFAULT_LOOKS_DIR.into());
    let looks_override =
        std::env::var("LEMNOSD_LOOKS_OVERRIDE_DIR").unwrap_or_else(|_| DEFAULT_OVERRIDE_DIR.into());
    // Settings that persist across restarts (power switches with `persist`).
    let state_dir = std::env::var("LEMNOSD_STATE_DIR").unwrap_or_else(|_| DEFAULT_STATE_DIR.into());
    // The simulated board (feature `sim`): buses from a script, not the host.
    let mut sim = env_flag("LEMNOSD_SIM");
    let mut sim_script = std::env::var("LEMNOSD_SIM_SCRIPT").unwrap_or_default();
    let mut sim_root = std::env::var("LEMNOSD_SIM_ROOT").unwrap_or_default();
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
            "--sim" => sim = true,
            "--sim-script" => sim_script = value(),
            "--sim-root" => sim_root = value(),
            "--help" | "-h" => {
                println!(
                    "lemnosd [--board PATH] [--socket PATH] [--update-status PATH|off] [--booting-ms N] [--check] [--sim [--sim-script PATH] [--sim-root DIR]]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("lemnosd: unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }

    let mut definition = match BoardDefinition::from_path(&board)
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
    // Block SIGTERM/SIGINT/SIGHUP before anything else, so the loop sees them
    // (SIGHUP re-reads the look files).
    let signals = match SignalFd::new(&[SIGTERM, SIGINT, SIGHUP]) {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("lemnosd: signals: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A simulated board keeps its files under the sim root (unless the
    // environment names a directory for them), so it runs without a board.
    let mut sim_root_owned = false;
    let sim_dirs = if sim {
        if sim_root.is_empty() {
            sim_root = std::env::temp_dir()
                .join(format!("lemnosd-sim-{}", std::process::id()))
                .display()
                .to_string();
            sim_root_owned = true;
        }
        Some(Path::new(&sim_root).to_path_buf())
    } else {
        None
    };
    // Without the feature there is no guard to keep.
    #[cfg_attr(not(feature = "sim"), allow(unused_variables))]
    let (sim_guard, buses) = match start_buses(
        &mut definition,
        sim_dirs.as_deref().map(|root| (sim_script.as_str(), root)),
    ) {
        Ok(hardware) => hardware,
        Err(error) => {
            eprintln!("lemnosd: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut config = ServiceConfig::new(definition, PathBuf::from(&socket));
    config.update_status = (!update.is_empty() && update != "off").then(|| PathBuf::from(&update));
    config.booting_ms = (booting > 0).then_some(booting);
    config.looks_dir = off_or_path(&looks_dir);
    config.looks_override_dir = off_or_path(&looks_override);
    config.state_dir = off_or_path(&state_dir);
    if let Some(root) = &sim_dirs {
        if std::env::var_os("LEMNOSD_STATE_DIR").is_none() {
            config.state_dir = Some(root.join("state"));
        }
        if std::env::var_os("LEMNOSD_LOOKS_OVERRIDE_DIR").is_none() {
            config.looks_override_dir = Some(root.join("looks.d"));
        }
        if std::env::var_os("LEMNOSD_CALIBRATION_DIR").is_none() {
            config.calibration_dir = root.join("calibration");
        }
    }
    let mut service = match Service::new(config, buses) {
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
            Ok(true) => {
                // A reload (SIGHUP) keeps the loop going; anything else stops it.
                if let Ok(Some(SIGHUP)) = signals.read() {
                    service.reload_looks();
                } else {
                    break;
                }
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!("lemnosd: {error}");
                service.shutdown(false);
                return ExitCode::FAILURE;
            }
        }
    }
    let _ = signals.read();
    service.shutdown(lemnosd::system_rebooting());
    // The simulated board's plant stops before its files are removed.
    #[cfg(feature = "sim")]
    drop(sim_guard);
    if sim_root_owned {
        let _ = std::fs::remove_dir_all(&sim_root);
    }
    eprintln!("lemnosd: stopped");
    ExitCode::SUCCESS
}

/// The buses the service runs on, and the simulated board that feeds them
/// (kept alive by the caller). Without a simulation, the board's Linux device
/// nodes.
#[cfg(feature = "sim")]
fn start_buses(
    definition: &mut BoardDefinition,
    sim: Option<(&str, &Path)>,
) -> Result<(Option<lemnosd::sim::Sim>, Box<dyn Buses>), String> {
    let Some((script_path, root)) = sim else {
        return Ok((None, Box::new(LinuxBuses::default())));
    };
    let script = if script_path.is_empty() {
        lemnosd::sim::Script::from_toml_str("format = \"lemnos.sim\"\nschema_version = 1\n")
            .map_err(|e| e.to_string())?
    } else {
        lemnosd::sim::Script::from_path(Path::new(script_path)).map_err(|e| e.to_string())?
    };
    let (sim, buses) =
        lemnosd::sim::Sim::start(definition, &script, root).map_err(|e| e.to_string())?;
    eprintln!("lemnosd: simulated board (sim root {})", root.display());
    Ok((Some(sim), buses))
}

/// Without the `sim` feature there is no simulated board to start.
#[cfg(not(feature = "sim"))]
fn start_buses(
    definition: &mut BoardDefinition,
    sim: Option<(&str, &Path)>,
) -> Result<(Option<()>, Box<dyn Buses>), String> {
    let _ = definition;
    match sim {
        None => Ok((None, Box::new(LinuxBuses::default()))),
        Some(_) => {
            Err("--sim needs a build with the sim feature (cargo build --features sim)".into())
        }
    }
}

/// A boolean environment setting: set and not `0`, `off` or empty.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !matches!(v.as_str(), "" | "0" | "off" | "false"))
}

/// A directory from an environment value; `off` or empty is none.
fn off_or_path(value: &str) -> Option<PathBuf> {
    (!value.is_empty() && value != "off").then(|| PathBuf::from(value))
}
