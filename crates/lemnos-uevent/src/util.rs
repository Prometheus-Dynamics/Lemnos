use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The entries of `root`, sorted. A missing directory is empty, not an error.
pub(crate) fn read_dir_sorted(root: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(root) {
        Ok(entries) => {
            let mut paths = entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?;
            paths.sort();
            Ok(paths)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}
