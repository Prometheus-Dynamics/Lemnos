//! `lemnos-orion`: every device lemnosd serves, published to Orion as a resource.
//!
//! Three threads share one [`Shared`]. [`source`] keeps one lemnosd connection: it reads the
//! device list and each device's description, subscribes to its value items and writes what
//! arrives into the mirror. [`calls`] keeps a second lemnosd connection and runs what Orion's
//! actions ask of a device. [`publish`] owns the Orion session: it describes each device as a
//! resource with its topics and actions, publishes what the mirror says has changed, and hands
//! the runs of its actions to [`calls`]. None of them knows what a device is: the resource, its
//! labels, its topics' fields and its actions come from the device's entry and description.
//!
//! Each half retries forever with the same backoff. Losing lemnosd withdraws its devices from
//! Orion and ends the Orion session, so their topics say their publisher is gone, until it comes
//! back. Losing Orion connects again and says everything again.

pub mod backoff;
pub mod calls;
pub mod config;
pub mod mirror;
pub mod publish;
pub mod source;
pub mod values;

use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use crate::calls::Done;
use crate::mirror::Mirror;

/// The name this service gives lemnosd when it connects.
pub const LEMNOSD_NAME: &str = "orion";
/// The name the call thread gives lemnosd.
pub const LEMNOSD_CALLS_NAME: &str = "orion-calls";
/// The name this service gives Orion.
pub const CLIENT_NAME: &str = "lemnos-orion";
/// What every device's resource name starts with: `lemnos.<device>`.
pub const RESOURCE_PREFIX: &str = "lemnos.";
/// The kind of every device's resource. Orion keeps it as this service's word.
pub const RESOURCE_KIND: &str = "lemnos.device";

/// The mirror, finished calls, and the signals between the threads.
#[derive(Debug)]
pub struct Shared {
    mirror: Mutex<Mirror>,
    done: Mutex<Vec<Done>>,
    /// Written by the other threads when there is something for the publisher. A byte waiting
    /// there is a wake-up; one is enough, so a full socket is not an error.
    bell: UnixStream,
    /// What the publisher waits on.
    woken: UnixStream,
    stop: AtomicBool,
}

impl Shared {
    /// # Errors
    ///
    /// When the wake-up socket cannot be made.
    pub fn new() -> std::io::Result<Self> {
        let (bell, woken) = UnixStream::pair()?;
        bell.set_nonblocking(true)?;
        woken.set_nonblocking(true)?;
        Ok(Self {
            mirror: Mutex::new(Mirror::new()),
            done: Mutex::new(Vec::new()),
            bell,
            woken,
            stop: AtomicBool::new(false),
        })
    }

    /// Runs `f` on the mirror. The lock is held only for `f`.
    pub fn with<R>(&self, f: impl FnOnce(&mut Mirror) -> R) -> R {
        let mut mirror = self.mirror.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut mirror)
    }

    /// Tells the publisher there may be something for it.
    pub fn changed(&self) {
        // A full socket already holds a wake-up; any other error leaves the publisher to its
        // next tick.
        let _ = (&self.bell).write(&[1]);
    }

    /// A call that ended, for the publisher to answer.
    pub fn finished(&self, done: Done) {
        self.done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(done);
        self.changed();
    }

    /// Takes the calls that ended into `into`.
    pub fn take_done(&self, into: &mut Vec<Done>) {
        into.append(&mut self.done.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// What the publisher polls for a wake-up.
    pub fn bell_fd(&self) -> BorrowedFd<'_> {
        self.woken.as_fd()
    }

    /// Clears the wake-ups that came.
    pub fn quiet(&self) {
        let mut sink = [0u8; 64];
        // `WouldBlock` ends them; any other error is the same as no wake-up.
        while matches!((&self.woken).read(&mut sink), Ok(n) if n > 0) {}
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Ends every loop. Used by tests; the service runs until it is killed.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.changed();
    }
}

/// One line on stderr. Errors and state changes only.
pub fn log(message: &str) {
    let _ = writeln!(std::io::stderr(), "lemnos-orion: {message}");
}
