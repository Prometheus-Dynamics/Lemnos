//! Sysfs attribute access. Reads and writes of numbers go through stack buffers, and
//! paths are built once by the driver, so a steady-state call allocates nothing.

use lemnos_core::{ErrorKind, Result, Status, status_of};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Bytes a number attribute is read into, and a written value is formatted into.
const SMALL: usize = 64;

/// The status a failed sysfs call reports.
pub fn io_status(error: &io::Error) -> Status {
    status_of(match error.kind() {
        io::ErrorKind::NotFound => ErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
        io::ErrorKind::TimedOut => ErrorKind::Timeout,
        _ => ErrorKind::Failed,
    })
}

/// Reads and parses a number attribute.
pub fn read_number<T: FromStr>(path: &Path) -> Result<T> {
    let mut file = File::open(path).map_err(|e| io_status(&e))?;
    parse(&mut file)
}

/// Reads and parses a number attribute, or `None` if it does not exist.
pub fn read_number_optional<T: FromStr>(path: &Path) -> Result<Option<T>> {
    match File::open(path) {
        Ok(mut file) => parse(&mut file).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_status(&e)),
    }
}

/// Reads a text attribute, trimmed, or `None` if it does not exist. Not on any steady-state
/// path: it is used when the fan is found.
pub fn read_text_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_status(&e)),
    }
}

fn parse<T: FromStr>(file: &mut File) -> Result<T> {
    let mut buf = [0u8; SMALL];
    let mut len = 0;
    loop {
        match file.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_status(&e)),
        }
        if len == SMALL {
            break;
        }
    }
    // A number never fills the buffer; text that does is refused.
    match std::str::from_utf8(&buf[..len]) {
        Ok(text) if len < SMALL => text
            .trim()
            .parse()
            .map_err(|_| status_of(ErrorKind::Failed)),
        _ => Err(status_of(ErrorKind::Failed)),
    }
}

/// Writes a number attribute, formatted on the stack.
pub fn write_number(path: &Path, value: impl fmt::Display) -> Result<()> {
    struct Stack {
        buf: [u8; SMALL],
        len: usize,
    }
    impl fmt::Write for Stack {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.len + s.len();
            self.buf
                .get_mut(self.len..end)
                .ok_or(fmt::Error)?
                .copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }
    let mut stack = Stack {
        buf: [0; SMALL],
        len: 0,
    };
    fmt::Write::write_fmt(&mut stack, format_args!("{value}"))
        .map_err(|_| status_of(ErrorKind::InvalidInput))?;
    // Never creates the attribute: a missing one fails the write.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| io_status(&e))?;
    io::Write::write_all(&mut file, &stack.buf[..stack.len]).map_err(|e| io_status(&e))
}

/// The entries of a directory, sorted. A missing directory has none.
pub fn entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = match fs::read_dir(dir) {
        Ok(read) => read
            .filter_map(io::Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(io_status(&e)),
    };
    entries.sort();
    Ok(entries)
}

/// Whether a path's last component starts with `prefix`.
pub fn named(path: &Path, prefix: &str) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with(prefix))
}
