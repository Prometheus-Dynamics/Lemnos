//! Small helpers for sysfs attributes.

use crate::SysfsError;
use lemnos_hal::ErrorKind;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Calls `f` with `dir/name`, built on the stack when it fits (a sysfs attribute's path does):
/// a polled read or write then allocates nothing. A longer one is joined on the heap.
pub fn attr<R>(dir: &Path, name: &str, f: impl FnOnce(&Path) -> R) -> R {
    use std::os::unix::ffi::OsStrExt as _;
    const MAX: usize = 256;
    let dir = dir.as_os_str().as_bytes();
    let len = dir.len() + 1 + name.len();
    if len > MAX {
        return f(&Path::new(std::ffi::OsStr::from_bytes(dir)).join(name));
    }
    let mut buf = [0u8; MAX];
    buf[..dir.len()].copy_from_slice(dir);
    buf[dir.len()] = b'/';
    buf[dir.len() + 1..len].copy_from_slice(name.as_bytes());
    f(Path::new(std::ffi::OsStr::from_bytes(&buf[..len])))
}

/// Reads an attribute, trimmed.
pub fn read(path: &Path) -> Result<String, SysfsError> {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| SysfsError::io(path, "read", &e))
}

/// Reads an attribute, or `None` if it does not exist.
pub fn read_optional(path: &Path) -> Result<Option<String>, SysfsError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SysfsError::io(path, "read", &e)),
    }
}

/// Parses an attribute as `T`.
pub fn parse<T: std::str::FromStr>(path: &Path, text: &str) -> Result<T, SysfsError> {
    text.parse().map_err(|_| {
        SysfsError::new(
            ErrorKind::Failed,
            path,
            format!("unexpected contents {text:?}"),
        )
    })
}

/// Bytes a number attribute is read into (and a written value formatted into) on the stack.
const SMALL: usize = 64;

/// Reads and parses a number attribute, through a stack buffer: a read allocates nothing.
pub(crate) fn read_parsed<T: std::str::FromStr>(path: &Path) -> Result<T, SysfsError> {
    let file = fs::File::open(path).map_err(|e| SysfsError::io(path, "read", &e))?;
    parse_file(path, file)
}

fn parse_file<T: std::str::FromStr>(path: &Path, file: fs::File) -> Result<T, SysfsError> {
    let mut buf = [0u8; SMALL];
    parse(path, read_small(path, file, &mut buf)?)
}

/// The attribute's text, trimmed, read into `buf`. Not generic, so each parsed type adds only
/// its `parse`. A number never fills `buf`: text that does, or is not UTF-8, is refused.
fn read_small<'a>(
    path: &Path,
    mut file: fs::File,
    buf: &'a mut [u8; SMALL],
) -> Result<&'a str, SysfsError> {
    use std::io::Read as _;
    let mut len = 0;
    loop {
        match file.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(SysfsError::io(path, "read", &e)),
        }
        if len == SMALL {
            break;
        }
    }
    match std::str::from_utf8(&buf[..len]) {
        Ok(text) if len < SMALL => Ok(text.trim()),
        _ => Err(SysfsError::new(
            ErrorKind::Failed,
            path,
            format!("not a number ({len} bytes or more)"),
        )),
    }
}

/// Reads and parses an attribute, or `None` if it does not exist.
pub(crate) fn read_parsed_optional<T: std::str::FromStr>(
    path: &Path,
) -> Result<Option<T>, SysfsError> {
    match fs::File::open(path) {
        Ok(file) => parse_file(path, file).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SysfsError::io(path, "read", &e)),
    }
}

/// Writes an attribute. A short value (a number: the controls' writes) is formatted on the
/// stack, so a write allocates nothing.
pub fn write(path: &Path, value: impl std::fmt::Display) -> Result<(), SysfsError> {
    write_args(path, format_args!("{value}"))
}

/// [`write`]'s body, once for every value type.
fn write_args(path: &Path, args: std::fmt::Arguments<'_>) -> Result<(), SysfsError> {
    struct Stack {
        buf: [u8; SMALL],
        len: usize,
    }
    impl std::fmt::Write for Stack {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            let end = self.len + s.len();
            self.buf
                .get_mut(self.len..end)
                .ok_or(std::fmt::Error)?
                .copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }
    let mut stack = Stack {
        buf: [0; SMALL],
        len: 0,
    };
    let result = if std::fmt::Write::write_fmt(&mut stack, args).is_ok() {
        fs::write(path, &stack.buf[..stack.len])
    } else {
        fs::write(path, std::fmt::format(args))
    };
    result.map_err(|e| SysfsError::io(path, "write", &e))
}

/// The class devices under `root` (for example `/sys/class/hwmon`), sorted.
pub fn entries(root: &Path) -> Result<Vec<PathBuf>, SysfsError> {
    let mut entries = match fs::read_dir(root) {
        Ok(dir) => dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(SysfsError::io(root, "list", &e)),
    };
    entries.sort();
    Ok(entries)
}

/// Whether the device behind a class device sits at I2C `bus`/`address`:
/// some component of its resolved path is the kernel's `<bus>-<address>`
/// client name (`1-0018`).
pub(crate) fn is_i2c_client(class_device: &Path, bus: u32, address: u16) -> bool {
    let client = format!("{bus}-{address:04x}");
    let resolved = fs::canonicalize(class_device).unwrap_or_else(|_| class_device.to_path_buf());
    let linked = fs::canonicalize(class_device.join("device")).ok();
    [Some(resolved), linked]
        .into_iter()
        .flatten()
        .any(|path| path.components().any(|c| c.as_os_str() == client.as_str()))
}
