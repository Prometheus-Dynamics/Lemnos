//! What the session needs of an open `/dev/mpp_service` file.
//!
//! Issuing the service's call means handing the kernel a list of [`Msg`](crate::mpp::Msg)s whose
//! `data_ptr` members point into this process, which takes `unsafe` code this crate may not hold.
//! No crate on `next` offers it yet; the shared service layer (`drivers/rk-mpp`) is where it
//! belongs, and this crate takes it through this trait when it lands.

use std::io;
use std::path::{Path, PathBuf};

use crate::mpp::NODE;

/// The service node's path, if this kernel has one.
#[must_use]
pub fn find() -> Option<PathBuf> {
    find_at(Path::new(NODE))
}

pub(crate) fn find_at(node: &Path) -> Option<PathBuf> {
    node.exists().then(|| node.to_path_buf())
}

/// An open service file, as the [`Jpeg`](crate::Jpeg) session uses it.
pub trait Device {
    /// Binds the file to a device type (`CMD_INIT_CLIENT_TYPE`). `EINVAL` when the kernel has no
    /// such block.
    fn init_client(&mut self, device_type: u32) -> io::Result<()>;
    /// The bound block's hardware id (`CMD_QUERY_HW_ID`).
    fn hw_id(&mut self) -> io::Result<u32>;
    /// Lets go of every dma-buf the file imported (`CMD_RESET_SESSION`).
    fn release_buffers(&mut self) -> io::Result<()>;
}
