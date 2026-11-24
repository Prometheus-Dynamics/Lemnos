//! The guest's side of `testing/qsnap/qsnap.py`. The control port is the virtconsole qsnap adds
//! (`/dev/hvc0`); the payload is the read-only disk with serial `qsnap-payload`, holding a `newc`
//! archive (what `cpio -H newc -o` writes) at its start.
//!
//! - [`ready`]: says `QSNAP READY` and waits for the run's line. The state qsnap keeps is this
//!   wait: every restore of it reads its own run's line here.
//! - [`save`]: asks qsnap to keep the state here. The run that asked reads `QSNAP SAVED <dir>`
//!   ([`Saved::Kept`]); a later restore of that state reads its own run's line instead
//!   ([`Saved::Restored`]).
//! - [`unpack_payload`]: writes the payload's files under a directory. Read only after the
//!   restore: what the guest has read of the disk before is in the kept state.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The control port.
pub const CONTROL: &str = "/dev/hvc0";
/// The payload disk, by the serial qsnap gives it.
pub const PAYLOAD: &str = "/dev/disk/by-id/virtio-qsnap-payload";
/// The serial qsnap gives the payload disk.
pub const PAYLOAD_SERIAL: &str = "qsnap-payload";

/// The line `QSNAP READY` asks for, and the one qsnap answers a save with.
const READY: &str = "QSNAP READY";
const SAVED: &str = "QSNAP SAVED ";

/// What [`save`] read back.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    /// The run that asked: the state is kept under this name.
    Kept(String),
    /// A restore of the kept state: this run's line.
    Restored(String),
}

/// Whether this guest runs under qsnap: its control port is there.
#[must_use]
pub fn under_qsnap() -> bool {
    Path::new(CONTROL).exists()
}

fn exchange(say: &str) -> io::Result<String> {
    let mut port = OpenOptions::new().read(true).write(true).open(CONTROL)?;
    port.write_all(say.as_bytes())?;
    port.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(port).read_line(&mut line)?;
    if line.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the control port closed",
        ));
    }
    Ok(line.trim_end().to_string())
}

/// Says the guest is ready and returns the run's line.
///
/// # Errors
///
/// The control port is not there or closes.
pub fn ready() -> io::Result<String> {
    exchange(READY)
}

/// Asks qsnap to keep the state here, under `name` (no spaces).
///
/// # Errors
///
/// The control port is not there or closes.
pub fn save(name: &str) -> io::Result<Saved> {
    let line = exchange(&format!("QSNAP SAVE {name}"))?;
    Ok(match line.strip_prefix(SAVED) {
        Some(dir) => Saved::Kept(dir.to_string()),
        None => Saved::Restored(line),
    })
}

/// Unpacks the payload disk's archive under `to`. Returns how many files it wrote.
///
/// # Errors
///
/// The disk is not there, or its archive is not a `newc` archive.
pub fn unpack_payload(to: &Path) -> io::Result<usize> {
    let disk = match File::open(PAYLOAD) {
        Ok(disk) => disk,
        Err(_) => File::open(payload_by_serial()?)?,
    };
    unpack(BufReader::with_capacity(1 << 20, disk), to)
}

/// The payload disk found by its serial in sysfs, for a guest with no udev to make the by-id links.
fn payload_by_serial() -> io::Result<std::path::PathBuf> {
    for entry in fs::read_dir("/sys/block")?.flatten() {
        let serial = fs::read_to_string(entry.path().join("serial")).unwrap_or_default();
        if serial.trim() == PAYLOAD_SERIAL {
            return Ok(Path::new("/dev").join(entry.file_name()));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no disk with the payload's serial",
    ))
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("payload: {what}"))
}

fn hex(field: &[u8]) -> io::Result<u64> {
    let text = std::str::from_utf8(field).map_err(|_| bad("a header field is not text"))?;
    u64::from_str_radix(text, 16).map_err(|_| bad("a header field is not hex"))
}

fn skip_to_four(reader: &mut impl Read, read: u64) -> io::Result<()> {
    let pad = (4 - read % 4) % 4;
    let mut sink = [0u8; 3];
    reader.read_exact(&mut sink[..usize::try_from(pad).unwrap_or(0)])
}

/// Unpacks a `newc` archive from `reader` under `to`: directories, regular files and symbolic
/// links, with their modes. Ends at the archive's trailer.
///
/// # Errors
///
/// The archive is cut short or is not `newc`, or a file cannot be written.
pub fn unpack(mut reader: impl Read, to: &Path) -> io::Result<usize> {
    let mut files = 0;
    loop {
        let mut header = [0u8; 110];
        reader.read_exact(&mut header)?;
        if &header[..6] != b"070701" {
            return Err(bad("not a newc archive"));
        }
        let field = |i: usize| hex(&header[6 + i * 8..14 + i * 8]);
        let mode = field(1)?;
        let size = field(6)?;
        let name_len = field(11)?;
        let mut name = vec![0u8; usize::try_from(name_len).map_err(|_| bad("name length"))?];
        reader.read_exact(&mut name)?;
        skip_to_four(&mut reader, 110 + name_len)?;
        let name = String::from_utf8(name).map_err(|_| bad("a name is not text"))?;
        let name = name.trim_end_matches('\0').trim_start_matches("./");
        if name == "TRAILER!!!" {
            return Ok(files);
        }
        if name.split('/').any(|part| part == "..") {
            return Err(bad("a name leaves the directory"));
        }
        let path = to.join(name);
        let mut body = vec![0u8; usize::try_from(size).map_err(|_| bad("size"))?];
        reader.read_exact(&mut body)?;
        skip_to_four(&mut reader, size)?;
        match mode & 0o170_000 {
            0o040_000 => fs::create_dir_all(&path)?,
            0o100_000 => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let _ = fs::remove_file(&path);
                fs::write(&path, &body)?;
                fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(u32::try_from(mode & 0o7777).unwrap_or(0o644)),
                )?;
                files += 1;
            }
            0o120_000 => {
                let target = String::from_utf8(body).map_err(|_| bad("a link is not text"))?;
                let _ = fs::remove_file(&path);
                std::os::unix::fs::symlink(target, &path)?;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
