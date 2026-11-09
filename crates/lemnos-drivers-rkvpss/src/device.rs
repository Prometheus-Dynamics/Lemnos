use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use lemnos_linux_sys::rkvpss::{self, Job};

/// The vendor driver's name for the node.
const NAME: &str = "rkvpss-offline";

/// The offline node's path, if this kernel has one.
#[must_use]
pub fn find() -> Option<PathBuf> {
    find_in(Path::new("/sys/class/video4linux"))
}

fn find_in(class: &Path) -> Option<PathBuf> {
    fs::read_dir(class).ok()?.flatten().find_map(|entry| {
        let name = fs::read_to_string(entry.path().join("name")).ok()?;
        (name.trim_end() == NAME).then(|| Path::new("/dev").join(entry.file_name()))
    })
}

/// An open offline scaler.
#[derive(Debug)]
pub struct Rkvpss {
    file: File,
}

impl Rkvpss {
    /// Opens the node at `path`.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    /// Hands the channels in `mask` (see [`rkvpss::Channel::bit`]) to offline mode, leaving the
    /// others as they are. A channel starts on the camera path, so this comes before the first
    /// job. It is refused while the camera path runs and lasts until the last open file closes.
    pub fn select_offline(&self, mask: u8) -> io::Result<()> {
        rkvpss::select_offline(self.file.as_fd(), mask)
    }

    /// Runs `job` and waits for it.
    pub fn run(&self, job: &mut Job<'_>) -> io::Result<()> {
        rkvpss::run(self.file.as_fd(), job)
    }

    /// Lets go of the dma-bufs used so far, which the kernel otherwise keeps attached, and so
    /// holds in memory, until this file closes.
    pub fn release_buffers(&self) -> io::Result<()> {
        rkvpss::release_buffers(self.file.as_fd())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_device_is_an_error() {
        let error = Rkvpss::open("/nonexistent/video9").expect_err("no such device");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn the_node_is_found_by_name() {
        let root = std::env::temp_dir().join(format!("rkvpss-find-{}", std::process::id()));
        for (node, name) in [
            ("video0", "rkisp_mainpath\n"),
            ("video7", "rkvpss-offline\n"),
        ] {
            let dir = root.join(node);
            fs::create_dir_all(&dir).expect("mkdir");
            fs::write(dir.join("name"), name).expect("write");
        }
        let found = find_in(&root);
        fs::remove_dir_all(&root).expect("clean up");
        assert_eq!(found, Some(PathBuf::from("/dev/video7")));
    }

    #[test]
    fn a_kernel_without_the_node_finds_nothing() {
        assert_eq!(find_in(Path::new("/nonexistent/video4linux")), None);
    }
}
