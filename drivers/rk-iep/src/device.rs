//! What the driver needs of an open `/dev/mpp_service` file.
//!
//! Issuing the service's call means handing the kernel a list of [`Msg`](crate::mpp::Msg)s whose
//! `data_ptr` members point into this process, which takes `unsafe` code this crate may not hold.
//! No crate on `next` offers it yet; the shared service layer (`drivers/rk-mpp`) is where it
//! belongs, and the IEP takes it through this trait when it lands.

use std::io;
use std::path::{Path, PathBuf};

use crate::job::Job;
use crate::mpp::NODE;
use crate::params::Results;

/// The service node's path, if this kernel has one.
#[must_use]
pub fn find() -> Option<PathBuf> {
    find_at(Path::new(NODE))
}

pub(crate) fn find_at(node: &Path) -> Option<PathBuf> {
    node.exists().then(|| node.to_path_buf())
}

/// An open service file, as the [`Iep`](crate::Iep) uses it.
pub trait Device {
    /// Binds the file to a device type (`CMD_INIT_CLIENT_TYPE`), here
    /// [`DEVICE_IEP2`](crate::mpp::DEVICE_IEP2). `EINVAL` when the kernel has no such block.
    fn init_client(&mut self, device_type: u32) -> io::Result<()>;
    /// Issues [`Job::messages`] as one `IOC_CFG_V1` call, with each `data_ptr` at its
    /// [`Data`](crate::Data): the job's parameters and offsets, and `results`. Returns once the
    /// job is done.
    fn run(&mut self, job: &Job<'_>, results: &mut Results) -> io::Result<()>;
    /// Lets go of every dma-buf the file imported (`CMD_RESET_SESSION`).
    fn release_buffers(&mut self) -> io::Result<()>;
}
