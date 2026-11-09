use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::path::Path;

use lemnos_linux_sys::g2d::{self, HardwareVersion, Job};

/// The BSP kernel's device node.
const DEVICE: &str = "/dev/g2d";

/// An open G2D engine.
#[derive(Debug)]
pub struct G2d {
    file: File,
}

impl G2d {
    /// Opens `/dev/g2d`.
    pub fn open() -> io::Result<Self> {
        Self::open_path(DEVICE)
    }

    /// Opens the engine at `path`.
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    /// The engine's and the chip's versions.
    pub fn version(&self) -> io::Result<HardwareVersion> {
        g2d::query_version(self.file.as_fd())
    }

    /// Runs `job` and waits for it.
    pub fn run(&self, job: &mut Job<'_>) -> io::Result<()> {
        g2d::run(self.file.as_fd(), job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_device_is_an_error() {
        let error = G2d::open_path("/nonexistent/g2d").expect_err("no such device");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
