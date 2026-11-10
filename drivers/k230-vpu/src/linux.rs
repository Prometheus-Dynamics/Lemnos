//! The block's node on Linux: a [`Device`] that opens it through `lemnos-driver-m2m-decode`'s
//! [`LinuxNode`] and measures dma-bufs with `lemnos-linux-sys`.

use std::io;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};

use lemnos_driver_m2m_decode::LinuxNode;
use lemnos_linux_sys::dma_buf;

use crate::claim::Device;
use crate::uapi::DRIVER;

/// The codec block's `/dev/video*` node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinuxDevice {
    path: PathBuf,
}

impl LinuxDevice {
    /// The node at `path`. Nothing is opened until a claim.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The first memory-to-memory node whose driver is [`DRIVER`], if this kernel has one.
    pub fn find() -> io::Result<Option<Self>> {
        Ok(LinuxNode::find(DRIVER)?.map(Self::new))
    }

    /// The node's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Device for LinuxDevice {
    type Node = LinuxNode;

    fn open(&mut self) -> io::Result<LinuxNode> {
        LinuxNode::open(&self.path)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}
