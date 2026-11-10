//! Finding the node, and what the scaler needs of it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lemnos_driver_rkvpss_sys::{Job, NODE_NAME, Node};

/// The offline node's path, if this kernel has one.
#[must_use]
pub fn find() -> Option<PathBuf> {
    find_in(Path::new("/sys/class/video4linux"))
}

pub(crate) fn find_in(class: &Path) -> Option<PathBuf> {
    fs::read_dir(class).ok()?.flatten().find_map(|entry| {
        let name = fs::read_to_string(entry.path().join("name")).ok()?;
        (name.trim_end() == NODE_NAME).then(|| Path::new("/dev").join(entry.file_name()))
    })
}

/// An open offline node, as the [`Scaler`](crate::Scaler) uses it. [`Node`] is the real one.
pub trait Device {
    /// Adds the channels in `mask` to offline mode.
    fn select_offline(&mut self, mask: u8) -> io::Result<()>;
    /// Runs `job` and waits for it.
    fn run(&mut self, job: &mut Job<'_>) -> io::Result<()>;
    /// Lets go of the dma-bufs used so far.
    fn release_buffers(&mut self) -> io::Result<()>;
}

impl Device for Node {
    fn select_offline(&mut self, mask: u8) -> io::Result<()> {
        Node::select_offline(self, mask)
    }

    fn run(&mut self, job: &mut Job<'_>) -> io::Result<()> {
        Node::run(self, job)
    }

    fn release_buffers(&mut self) -> io::Result<()> {
        Node::release_buffers(self)
    }
}
