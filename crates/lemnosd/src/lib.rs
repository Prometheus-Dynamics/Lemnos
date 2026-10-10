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
//! effect runs (50 Hz), with no allocation per frame. On stop, fans go back
//! to the kernel's control (see [`fans`]).

#![forbid(unsafe_code)]

mod calibration;
mod clients;
mod devices;
pub mod fans;
mod fusion;
mod light;
pub mod looks;
#[cfg(feature = "mock")]
pub mod mock;
mod notify;
#[cfg(feature = "orion")]
pub mod orion;
pub mod presets;
mod raw;
mod schedule;
mod service;
pub mod state;
mod update;
mod workers;

pub use service::{Service, ServiceConfig, ServiceError};

/// The default board definition.
pub const DEFAULT_BOARD: &str = "/etc/lemnos/board.toml";
/// The default socket.
pub use lemnos_ipc::DEFAULT_SOCKET;

/// `raw × 10^exponent` as `f64`, dividing by exact powers of ten.
pub(crate) fn scaled(raw: i32, exponent: i8) -> f64 {
    let scale = 10f64.powi(i32::from(exponent.unsigned_abs()));
    if exponent < 0 {
        f64::from(raw) / scale
    } else {
        f64::from(raw) * scale
    }
}

pub use fans::restore_fans;
pub use light::TEST_LEASE_MS;

/// Whether systemd is stopping the system for a restart (not a power-off),
/// so the reboot ember is left on the ring.
///
/// `systemctl reboot` (with or without `--reboot-argument`, such as
/// `0 tryboot`) starts `reboot.target` and keeps it active while the services
/// stop, so that is asked first. `poweroff.target` and `halt.target` answer
/// "no". Only when neither says (no `systemctl`, or no answer) is
/// `is-system-running`'s "stopping" taken as a restart, as it was before.
pub fn system_rebooting() -> bool {
    let unit_state = |unit: &str| -> String {
        std::process::Command::new("systemctl")
            .args(["is-active", unit])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    if unit_state("reboot.target") == "active" {
        return true;
    }
    if unit_state("poweroff.target") == "active" || unit_state("halt.target") == "active" {
        return false;
    }
    std::process::Command::new("systemctl")
        .arg("is-system-running")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "stopping")
        .unwrap_or(false)
}
