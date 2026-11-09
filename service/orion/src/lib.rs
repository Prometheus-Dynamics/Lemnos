//! `lemnos-orion`: every device lemnosd serves, published to Orion as a resource.
//!
//! Two halves share one [`Shared`]. [`source`] keeps one lemnosd connection on a thread of its
//! own: it reads the device list and each device's description, subscribes to its value items
//! and writes what arrives into the mirror. [`publish`] keeps the Orion session on the async
//! runtime and sends whatever the mirror says has changed. Neither half knows what a device is:
//! the resource, its labels and its statuses come from the device's entry and its description.
//!
//! Each half retries forever with the same backoff. Losing lemnosd removes its devices from Orion
//! until it comes back. Losing Orion makes the next connect publish everything again.

pub mod backoff;
pub mod config;
pub mod mirror;
pub mod publish;
pub mod source;
pub mod values;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use tokio::sync::Notify;

use crate::mirror::Mirror;

/// The name this service gives lemnosd when it connects.
pub const LEMNOSD_NAME: &str = "orion";
/// The name this service gives Orion.
pub const CLIENT_NAME: &str = "lemnos-orion";
/// The Orion provider every device is published under.
pub const PROVIDER_ID: &str = "lemnos";
/// The Orion resource type of every device.
pub const RESOURCE_TYPE: &str = "lemnos.device";

/// The mirror, and the two signals between the halves.
#[derive(Debug)]
pub struct Shared {
    mirror: Mutex<Mirror>,
    /// Raised by the source when the mirror changed. Stores one wake-up if none is waiting.
    wake: Notify,
    stop: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            mirror: Mutex::new(Mirror::new()),
            wake: Notify::new(),
            stop: AtomicBool::new(false),
        }
    }

    /// Runs `f` on the mirror. The lock is held only for `f`.
    pub fn with<R>(&self, f: impl FnOnce(&mut Mirror) -> R) -> R {
        let mut mirror = self.mirror.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut mirror)
    }

    /// Tells the publisher the mirror may have changed.
    pub fn changed(&self) {
        self.wake.notify_one();
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Ends the source's loop. Used by tests; the service runs until it is killed.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

/// One line on stderr. Errors and state changes only.
pub fn log(message: &str) {
    let _ = writeln!(std::io::stderr(), "lemnos-orion: {message}");
}
