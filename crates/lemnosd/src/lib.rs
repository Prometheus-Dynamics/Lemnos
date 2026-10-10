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

mod clients;
mod devices;
pub mod fans;
mod light;
#[cfg(feature = "mock")]
pub mod mock;
mod notify;
#[cfg(feature = "orion")]
pub mod orion;
mod raw;
mod schedule;
mod service;
mod update;

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

/// Whether systemd is stopping the system (a restart or power-off).
pub fn system_stopping() -> bool {
    std::process::Command::new("systemctl")
        .arg("is-system-running")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "stopping")
        .unwrap_or(false)
}
