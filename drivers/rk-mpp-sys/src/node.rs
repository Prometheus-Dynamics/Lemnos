//! The service node and what the driver asks of the system: sessions, whether a block sits behind
//! an IOMMU, and a dma-buf's size.

use std::io;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};

use lemnos_driver_rk_mpp::{Block, Device};

use crate::session::ServiceSession;

/// The platform driver that serves `block`, as its source names it (`*_DRIVER_NAME`).
#[must_use]
pub const fn driver_name(block: Block) -> &'static str {
    match block {
        Block::Vdpu1 => "mpp_vdpu1",
        Block::Vdpu2 => "mpp_vdpu2",
        Block::Vepu1 => "mpp_vepu1",
        Block::Vepu2 => "mpp_vepu2",
        Block::JpegDecoder => "mpp_jpgdec",
        Block::JpegEncoder => "mpp_jpgenc",
        // `IEP2_DRIVER_NAME` in `mpp_iep2.c`.
        Block::Iep2 => "mpp-iep2",
    }
}

/// The service node at a path the caller gives, and the sysfs it reads.
#[derive(Clone, Debug)]
pub struct ServiceNode {
    node: PathBuf,
    sysfs: PathBuf,
}

impl ServiceNode {
    /// The node at `node` (`/dev/mpp_service` on the boards that have it), with sysfs at `/sys`.
    /// Nothing is opened.
    pub fn new(node: impl Into<PathBuf>) -> Self {
        Self::with_sysfs(node, "/sys")
    }

    /// The node at `node`, with sysfs mounted at `sysfs`.
    pub fn with_sysfs(node: impl Into<PathBuf>, sysfs: impl Into<PathBuf>) -> Self {
        Self {
            node: node.into(),
            sysfs: sysfs.into(),
        }
    }

    /// The node's path.
    #[must_use]
    pub fn node(&self) -> &Path {
        &self.node
    }

    /// Whether `block` is confined: its platform driver has at least one device bound, and every
    /// device bound has an `iommu_group`. A missing driver directory is `false`.
    pub fn is_confined(&self, block: Block) -> io::Result<bool> {
        let dir = self
            .sysfs
            .join("bus/platform/drivers")
            .join(driver_name(block));
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let mut devices = 0;
        for entry in entries {
            let entry = entry?;
            // The bound devices are the links; `module` is the one link that is not a device.
            if !entry.file_type()?.is_symlink() || entry.file_name() == "module" {
                continue;
            }
            devices += 1;
            if !entry.path().join("iommu_group").exists() {
                return Ok(false);
            }
        }
        Ok(devices > 0)
    }
}

impl Device for ServiceNode {
    type Session = ServiceSession;

    /// A session that may bind to the blocks found confined now.
    fn open(&mut self) -> io::Result<ServiceSession> {
        let allowed: Vec<Block> = Block::ALL
            .into_iter()
            .filter(|&b| self.is_confined(b).unwrap_or(false))
            .collect();
        ServiceSession::open(&self.node, &allowed)
    }

    fn confined(&mut self, block: Block) -> io::Result<bool> {
        self.is_confined(block)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        lemnos_linux_sys::dma_buf::size(fd)
    }
}
