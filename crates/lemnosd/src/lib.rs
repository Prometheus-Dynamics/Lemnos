//! `lemnosd`: the board's hardware service.
//!
//! One process owns the board's devices (sensors, fan, LEDs, GPIO), built
//! from a board definition (`lemnos-board`) with the generic drivers, and
//! serves clients over a Unix socket (`lemnos-ipc`): device lists, readings
//! and subscriptions, control writes under each device's write policy, LED
//! intents arbitrated between clients, and events. It is a thin host over
//! the compact device model: it knows no chip. See `docs/system-service.md`.
//!
//! The loop is single-threaded: one `poll` over the socket, the clients and
//! the next device or frame deadline. Lights render only while a fade or an
//! effect runs (50 Hz), with no allocation per frame. On stop, fans with a
//! `restore_mode` go back to the kernel's control.

#![forbid(unsafe_code)]

mod clients;
mod devices;
mod light;
mod notify;
mod service;
mod update;

pub use service::{Service, ServiceConfig, ServiceError};

/// The default board definition.
pub const DEFAULT_BOARD: &str = "/etc/lemnos/board.toml";
/// The default socket.
pub use lemnos_ipc::DEFAULT_SOCKET;
/// The device package's update status file.
pub const DEFAULT_UPDATE_STATUS: &str = "/run/pd-device/update.json";

/// `raw × 10^exponent` as `f64`, dividing by exact powers of ten.
pub(crate) fn scaled(raw: i32, exponent: i8) -> f64 {
    let scale = 10f64.powi(i32::from(exponent.unsigned_abs()));
    if exponent < 0 {
        f64::from(raw) / scale
    } else {
        f64::from(raw) * scale
    }
}

/// Writes each fan's restore mode (for a panic hook or a stop helper).
pub fn restore_fans(targets: &[(std::path::PathBuf, i32)]) {
    for (path, mode) in targets {
        let _ = std::fs::write(path, mode.to_string());
    }
}

/// Whether systemd is stopping the system (a restart or power-off).
pub fn system_stopping() -> bool {
    std::process::Command::new("systemctl")
        .arg("is-system-running")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "stopping")
        .unwrap_or(false)
}
