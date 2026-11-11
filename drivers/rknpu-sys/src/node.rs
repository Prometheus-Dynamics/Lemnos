//! The node: a check that the path is the NPU driver's character device, and sessions on it.

use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::session::NpuSession;
use crate::uapi::DRIVER_NAME;

/// The node at a path the caller gives, and the sysfs it checks it against.
#[derive(Clone, Debug)]
pub struct NpuNode {
    node: PathBuf,
    sysfs: PathBuf,
}

/// glibc's `major()` and `minor()` of a device number.
const fn split(rdev: u64) -> (u64, u64) {
    let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & 0xffff_f000);
    let minor = (rdev & 0xff) | ((rdev >> 12) & 0xffff_ff00);
    (major, minor)
}

impl NpuNode {
    /// The node at `node` (a `/dev/dri/renderD*`), with sysfs at `/sys`. Nothing is opened.
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

    /// Checks, without opening it, that the node is a character device whose device's driver is
    /// the NPU's (`/sys/dev/char/<major>:<minor>/device/driver` names it). `Unsupported`
    /// otherwise: another DRM driver takes the same request numbers to mean something else.
    pub fn check(&self) -> io::Result<()> {
        let meta = std::fs::metadata(&self.node)?;
        if !meta.file_type().is_char_device() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let (major, minor) = split(meta.rdev());
        let driver = self
            .sysfs
            .join(format!("dev/char/{major}:{minor}/device/driver"));
        let target = match std::fs::read_link(&driver) {
            Ok(target) => target,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::from(io::ErrorKind::Unsupported));
            }
            Err(e) => return Err(e),
        };
        if target.file_name().is_some_and(|name| name == DRIVER_NAME) {
            Ok(())
        } else {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }
}

impl NpuNode {
    /// [`Self::check`]s the node, then opens a session on it.
    pub fn open(&self) -> io::Result<NpuSession> {
        self.check()?;
        NpuSession::open(&self.node)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn device_numbers_split_as_glibc_does() {
        assert_eq!(super::split(0xe280), (226, 128));
        assert_eq!(super::split(0x0103), (1, 3));
        // glibc's makedev(0x12345, 0x678).
        let (major, minor) = (0x12345u64, 0x678u64);
        let dev = ((major & 0xfff) << 8)
            | ((major & 0xffff_f000) << 32)
            | (minor & 0xff)
            | ((minor & 0xffff_ff00) << 12);
        assert_eq!(super::split(dev), (major, minor));
    }
}
