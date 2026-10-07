use lemnos_hal::{ErrorKind, HalError};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// A failed kernel-interface operation: what was attempted, on which path,
/// and how it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsError {
    kind: ErrorKind,
    path: PathBuf,
    detail: String,
}

impl SysfsError {
    pub fn new(kind: ErrorKind, path: impl Into<PathBuf>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.into(),
            detail: detail.into(),
        }
    }

    pub(crate) fn io(path: &Path, action: &str, error: &io::Error) -> Self {
        Self::new(
            ErrorKind::from_io(error.kind()),
            path,
            format!("{action}: {error}"),
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl HalError for SysfsError {
    fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for SysfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.path.display(),
            self.kind,
            self.detail
        )
    }
}

impl std::error::Error for SysfsError {}
