//! The node on a device: this crate's [`Device`] and [`Session`] on `lemnos-driver-rknpu-sys`'s
//! [`NpuNode`] and [`NpuSession`], which hold the `unsafe` and check every request again, and
//! the node a board names, found by the driver's name when it names none.

use std::io;
use std::path::{Path, PathBuf};

use lemnos_driver_rknpu_sys::uapi::DRIVER_NAME;
use lemnos_driver_rknpu_sys::{NpuNode, NpuSession};

use crate::claim::{Device, Session};
use crate::request::{Create, Object, Query, Run};

impl Session for NpuSession {
    fn query(&mut self, query: Query) -> io::Result<u32> {
        NpuSession::query(self, query)
    }

    fn create(&mut self, request: Create) -> io::Result<Object> {
        NpuSession::create(self, request)
    }

    fn memory(&mut self, handle: u32) -> Option<&mut [u8]> {
        NpuSession::memory(self, handle)
    }

    fn destroy(&mut self, handle: u32) -> io::Result<()> {
        NpuSession::destroy(self, handle)
    }

    fn submit(&mut self, run: &Run) -> io::Result<()> {
        NpuSession::submit(self, run)
    }
}

impl Device for NpuNode {
    type Session = NpuSession;

    fn open(&mut self) -> io::Result<NpuSession> {
        NpuNode::open(self)
    }
}

impl<S: Session + ?Sized> Session for Box<S> {
    fn query(&mut self, query: Query) -> io::Result<u32> {
        (**self).query(query)
    }

    fn create(&mut self, request: Create) -> io::Result<Object> {
        (**self).create(request)
    }

    fn memory(&mut self, handle: u32) -> Option<&mut [u8]> {
        (**self).memory(handle)
    }

    fn destroy(&mut self, handle: u32) -> io::Result<()> {
        (**self).destroy(handle)
    }

    fn submit(&mut self, run: &Run) -> io::Result<()> {
        (**self).submit(run)
    }
}

/// The render node under `sysfs`'s `class/drm` whose device is bound to the NPU's driver, as
/// its path under `/dev/dri`. `None` when there is none, or more than one.
#[must_use]
pub fn find_in(sysfs: &Path) -> Option<PathBuf> {
    let mut found = None;
    for entry in std::fs::read_dir(sysfs.join("class/drm")).ok()?.flatten() {
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            continue;
        };
        if !text.starts_with("renderD") {
            continue;
        }
        let Ok(driver) = std::fs::read_link(entry.path().join("device/driver")) else {
            continue;
        };
        if driver.file_name().is_some_and(|n| n == DRIVER_NAME) {
            if found.is_some() {
                return None;
            }
            found = Some(Path::new("/dev/dri").join(text));
        }
    }
    found
}
