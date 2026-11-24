//! The lends' END holds across a restart: the host's [`EndKeeper`] over the service manager's file
//! descriptor store. Each hold's write end is kept under `lemnos-end.DEVICE` while the hold lasts,
//! so a restart (or a crash of the main process) leaves the holder's END open and its link going;
//! the next daemon takes it back, and the host decides at the device's first tick whether the hold
//! goes on (docs/daemon.md, "The USB gadget across a restart").
//!
//! A stop of the daemon lets nothing go from the store: the stop is the restart's first half, and
//! the service manager empties the store itself when the unit stops for good. Without a store (a
//! BusyBox init) there is no keeper, and a stop ends every hold.

use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lemnos_host::EndKeeper;

/// `poll`'s `POLLERR`: on a pipe's write end, no read end is open anywhere.
const POLLERR: i16 = 0x008;

/// The start of every name a hold's write end is kept under.
pub const NAME_PREFIX: &str = "lemnos-end.";

/// Whether a stored descriptor's name is a hold's.
#[must_use]
pub fn is_end_name(name: &str) -> bool {
    name.starts_with(NAME_PREFIX)
}

/// The holds' keeper: the store, and what the daemon before kept, until taken.
pub struct EndStore {
    store: Arc<dyn lemnos_linux::FdStore>,
    given: Mutex<Vec<(String, OwnedFd)>>,
    stopping: AtomicBool,
}

impl EndStore {
    /// `given`: the descriptors the service manager passed under [`NAME_PREFIX`] names.
    #[must_use]
    pub fn new(store: Arc<dyn lemnos_linux::FdStore>, given: Vec<(String, OwnedFd)>) -> Arc<Self> {
        Arc::new(Self {
            store,
            given: Mutex::new(given),
            stopping: AtomicBool::new(false),
        })
    }

    /// The daemon is stopping: from now on nothing is let go from the store.
    pub fn stopping(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    /// What the daemon before kept and no device of this board took up: let go of in the store,
    /// with a line for each. Called once every device has been offered its own.
    #[must_use]
    pub fn let_go_untaken(&self) -> Vec<String> {
        let left = std::mem::take(&mut *self.given.lock().unwrap_or_else(PoisonError::into_inner));
        left.into_iter()
            .map(|(name, fd)| {
                drop(fd);
                self.store.let_go(&name);
                format!("{name}: kept by the daemon before for no device of this board; let go")
            })
            .collect()
    }
}

fn name_of(device: &str) -> String {
    format!("{NAME_PREFIX}{device}")
}

impl EndKeeper for EndStore {
    fn keep(&self, device: &str, write: BorrowedFd<'_>) {
        let name = name_of(device);
        if let Err(error) = self.store.keep(&name, write) {
            lemnos_host_warning(&format!(
                "device {device}: the lend's END is not kept in the service manager's store ({error}): a restart ends the hold"
            ));
        }
    }

    fn let_go(&self, device: &str) {
        if !self.stopping.load(Ordering::Acquire) {
            self.store.let_go(&name_of(device));
        }
    }

    fn take(&self, device: &str) -> Option<OwnedFd> {
        let name = name_of(device);
        let mut given = self.given.lock().unwrap_or_else(PoisonError::into_inner);
        let at = given.iter().position(|(kept, _)| *kept == name)?;
        Some(given.swap_remove(at).1)
    }

    fn readers_gone(&self, write: BorrowedFd<'_>) -> bool {
        // A pipe's write end polls POLLERR once no read end is open anywhere.
        lemnos_linux_sys::poll::poll_one(write, 0, Some(Duration::ZERO))
            .is_ok_and(|events| events & POLLERR != 0)
    }
}

fn lemnos_host_warning(text: &str) {
    crate::warning_line(text);
}

/// Descriptors the service manager passed, with their names.
pub type Named = Vec<(String, OwnedFd)>;

/// Splits the descriptors the service manager passed into the holds' and the rest.
#[must_use]
pub fn split(kept: Named) -> (Named, Named) {
    kept.into_iter().partition(|(name, _)| is_end_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::os::fd::AsFd;

    #[derive(Default)]
    struct Store {
        kept: Mutex<Vec<String>>,
        let_go: Mutex<Vec<String>>,
    }

    impl lemnos_linux::FdStore for Store {
        fn keep(&self, name: &str, _fd: BorrowedFd<'_>) -> io::Result<bool> {
            self.kept.lock().unwrap().push(name.to_string());
            Ok(true)
        }
        fn let_go(&self, name: &str) {
            self.let_go.lock().unwrap().push(name.to_string());
        }
        fn barrier(&self) -> Option<OwnedFd> {
            None
        }
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let (read, write) = std::io::pipe().unwrap();
        (OwnedFd::from(read), OwnedFd::from(write))
    }

    #[test]
    fn a_hold_is_kept_by_its_devices_name_and_nothing_is_let_go_while_stopping() {
        let store = Arc::new(Store::default());
        let ends = EndStore::new(store.clone(), Vec::new());
        let (_read, write) = pipe();
        ends.keep("gadget", write.as_fd());
        assert_eq!(*store.kept.lock().unwrap(), ["lemnos-end.gadget"]);
        ends.let_go("gadget");
        ends.stopping();
        ends.let_go("gadget");
        assert_eq!(
            *store.let_go.lock().unwrap(),
            ["lemnos-end.gadget"],
            "once, before the stop"
        );
    }

    #[test]
    fn what_the_daemon_before_kept_is_taken_once_and_the_rest_let_go() {
        let store = Arc::new(Store::default());
        let (_r1, w1) = pipe();
        let (_r2, w2) = pipe();
        let ends = EndStore::new(
            store.clone(),
            vec![
                ("lemnos-end.gadget".into(), w1),
                ("lemnos-end.gone".into(), w2),
            ],
        );
        assert!(ends.take("gadget").is_some());
        assert!(ends.take("gadget").is_none());
        let lines = ends.let_go_untaken();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(*store.let_go.lock().unwrap(), ["lemnos-end.gone"]);
    }

    #[test]
    fn the_readers_are_gone_only_once_every_read_end_has_closed() {
        let ends = EndStore::new(Arc::new(Store::default()), Vec::new());
        let (read, write) = pipe();
        let copy = read.try_clone().unwrap();
        assert!(!ends.readers_gone(write.as_fd()));
        drop(read);
        assert!(!ends.readers_gone(write.as_fd()), "a copy is still open");
        drop(copy);
        assert!(ends.readers_gone(write.as_fd()));
    }

    #[test]
    fn the_holds_names_are_split_from_the_gadgets() {
        let (_r, w) = pipe();
        let (_r2, w2) = pipe();
        let (ends, rest) = split(vec![
            ("lemnos-end.gadget".into(), w),
            ("lemnos-ffs.udc0.ep0".into(), w2),
        ]);
        assert_eq!(ends.len(), 1);
        assert_eq!(rest[0].0, "lemnos-ffs.udc0.ep0");
    }
}
