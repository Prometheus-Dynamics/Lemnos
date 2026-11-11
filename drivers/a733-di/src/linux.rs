//! The deinterlacer's node on Linux: a [`Device`] over `lemnos-driver-a733-di-sys`, which
//! holds the `unsafe`.

use std::io;
use std::path::Path;

use lemnos_driver_a733_di_sys::{Job, Node, Settings, uapi};

use crate::engine::Device;

/// One open of the deinterlacer's node.
#[derive(Debug)]
pub struct LinuxDevice(Node);

impl LinuxDevice {
    /// Opens the node at `path`. A device that doesn't answer its version is refused.
    pub fn open(path: &Path) -> io::Result<Self> {
        Node::open(path).map(Self)
    }
}

impl Device for LinuxDevice {
    fn version(&self) -> io::Result<uapi::Version> {
        self.0.version()
    }

    fn reset(&self) -> io::Result<()> {
        self.0.reset()
    }

    fn configure(&self, settings: &Settings) -> io::Result<()> {
        self.0.configure(settings)
    }

    fn check(&self) -> io::Result<()> {
        self.0.check()
    }

    fn process(&self, job: &Job<'_>) -> io::Result<i32> {
        self.0.process(job)
    }
}
