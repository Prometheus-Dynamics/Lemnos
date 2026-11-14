//! Directory-relative file operations: a [`Dir`] is a directory opened once, and everything done
//! through it names one entry of that directory, never a path.
//!
//! The gadget helper (`lemnosd-gadget`) runs as root and makes configfs trees from a description
//! fixed at start. It opens the few directories it works in once (`Dir::open_path`, the one place
//! an absolute path is given), and from then on every file it touches is an entry of one of
//! them. An entry's name is checked ([`check_name`]): empty, `.`, `..`, anything with a `/` or a
//! NUL, and anything over 255 bytes is refused before a system call is made. Opening an entry
//! never follows a symbolic link: `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
//! RESOLVE_NO_MAGICLINKS` where the kernel has it (Linux 5.6), and `O_NOFOLLOW` on a single
//! checked name where it does not (`ENOSYS`), which is as strict for one component.
//!
//! Each system call here is one operation with its own `SAFETY` note.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The longest entry name Linux takes.
const NAME_MAX: usize = 255;
/// The most `read` returns: configfs and sysfs attributes are one page at most.
const READ_MAX: usize = 64 * 1024;
/// `openat2`'s system call number, the same on every architecture Linux has it for.
const SYS_OPENAT2: libc::c_long = 437;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
/// `CONFIGFS_MAGIC`, what `fstatfs` says of a mounted configfs.
pub const CONFIGFS_MAGIC: i64 = 0x6265_6570;
/// `FUNCTIONFS_MAGIC`, what `fstatfs` says of a mounted FunctionFS.
pub const FUNCTIONFS_MAGIC: i64 = 0x0a64_7361;

/// Set once the kernel has said it has no `openat2`, so the fallback is taken without asking again.
static NO_OPENAT2: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// What an entry is, not following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link.
    Symlink,
    /// Anything else: a device, a socket, a pipe.
    Other,
}

/// The error for a name that is not one entry: `EINVAL`, with the reason as its text.
fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

/// Checks that `name` is one entry of a directory: not empty, not `.` or `..`, no `/`, no NUL, at
/// most 255 bytes.
///
/// # Errors
///
/// `InvalidInput`, with the reason.
pub fn check_name(name: &str) -> io::Result<()> {
    if name.is_empty() {
        return Err(refused("an entry name is empty"));
    }
    if name == "." || name == ".." {
        return Err(refused("an entry name may not be . or .."));
    }
    if name.contains('/') {
        return Err(refused("an entry name is one name, not a path"));
    }
    if name.contains('\0') {
        return Err(refused("an entry name has a NUL"));
    }
    if name.len() > NAME_MAX {
        return Err(refused("an entry name is too long"));
    }
    Ok(())
}

fn c_name(name: &str) -> io::Result<CString> {
    check_name(name)?;
    CString::new(name).map_err(|_| refused("an entry name has a NUL"))
}

fn last() -> io::Error {
    io::Error::last_os_error()
}

/// A directory, opened once. See the module docs.
#[derive(Debug)]
pub struct Dir {
    fd: OwnedFd,
}

impl Dir {
    /// Opens the directory at `path`, which is given in full here and nowhere else. A symbolic
    /// link as the last component is not followed.
    ///
    /// # Errors
    ///
    /// The path does not name a directory, or cannot be opened.
    pub fn open_path(path: &Path) -> io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let c =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| refused("a NUL in a path"))?;
        // SAFETY: `c` is a NUL-terminated string that outlives the call; the flags take no pointer.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        Self::own(fd)
    }

    fn own(fd: RawFd) -> io::Result<Self> {
        if fd < 0 {
            return Err(last());
        }
        // SAFETY: `fd` is a descriptor the kernel just returned and nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self { fd })
    }

    /// The descriptor's number, for a path of the form `/proc/self/fd/N` (a mount's target).
    #[must_use]
    pub fn proc_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.fd.as_raw_fd()))
    }

    /// `openat` of one checked name, never following a link.
    fn open_at(&self, name: &str, flags: libc::c_int, mode: u32) -> io::Result<RawFd> {
        let c = c_name(name)?;
        if !NO_OPENAT2.load(Ordering::Relaxed) {
            let how = OpenHow {
                flags: (flags | libc::O_CLOEXEC) as u64,
                mode: u64::from(mode),
                resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
            };
            // SAFETY: `c` and `how` outlive the call, and the size passed is `how`'s.
            let fd = unsafe {
                libc::syscall(
                    SYS_OPENAT2,
                    self.fd.as_raw_fd(),
                    c.as_ptr(),
                    &how as *const OpenHow,
                    std::mem::size_of::<OpenHow>(),
                )
            };
            if fd >= 0 {
                return Ok(fd as RawFd);
            }
            let error = last();
            if error.raw_os_error() != Some(libc::ENOSYS) {
                return Err(error);
            }
            NO_OPENAT2.store(true, Ordering::Relaxed);
        }
        // SAFETY: `c` is NUL-terminated and outlives the call; the directory descriptor is open.
        let fd = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode,
            )
        };
        if fd < 0 { Err(last()) } else { Ok(fd) }
    }

    /// The directory `name` of this one.
    ///
    /// # Errors
    ///
    /// A refused name, no such entry, a link, or an entry that is not a directory.
    pub fn open_dir(&self, name: &str) -> io::Result<Self> {
        Self::own(self.open_at(name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?)
    }

    /// A handle on the directory `name` that needs no permission on it (`O_PATH`): enough to ask what
    /// filesystem it is on, and to name it as a mount's target (`proc_path`), not to read it. The
    /// FunctionFS mount point is owned by the daemon's user and closed to everyone else, root without
    /// `CAP_DAC_OVERRIDE` included.
    ///
    /// # Errors
    ///
    /// A refused name, no such entry, a link, or an entry that is not a directory.
    pub fn open_handle(&self, name: &str) -> io::Result<Self> {
        Self::own(self.open_at(name, libc::O_PATH | libc::O_DIRECTORY, 0)?)
    }

    /// Makes the directory `name`, with `mode`.
    ///
    /// # Errors
    ///
    /// A refused name, or the kernel's reason (`EEXIST` when it is there).
    pub fn make_dir(&self, name: &str, mode: u32) -> io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: `c` is NUL-terminated and outlives the call; the directory descriptor is open.
        if unsafe { libc::mkdirat(self.fd.as_raw_fd(), c.as_ptr(), mode as libc::mode_t) } != 0 {
            return Err(last());
        }
        Ok(())
    }

    /// Makes the directory `name` unless it is there.
    ///
    /// # Errors
    ///
    /// A refused name, or the kernel's reason other than `EEXIST`.
    pub fn ensure_dir(&self, name: &str, mode: u32) -> io::Result<()> {
        match self.make_dir(name, mode) {
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Ok(()),
            other => other,
        }
    }

    /// What the entry `name` is, not following a link.
    ///
    /// # Errors
    ///
    /// A refused name, or no such entry (`ENOENT`).
    pub fn kind(&self, name: &str) -> io::Result<Kind> {
        let c = c_name(name)?;
        // SAFETY: `stat` is plain data, all zeros is a valid value of it.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is NUL-terminated and `stat` writable, both alive for the call.
        let rc = unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(last());
        }
        Ok(match stat.st_mode & libc::S_IFMT {
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFREG => Kind::File,
            libc::S_IFLNK => Kind::Symlink,
            _ => Kind::Other,
        })
    }

    /// Whether the entry `name` is there (a link counts, whatever it points at).
    #[must_use]
    pub fn exists(&self, name: &str) -> bool {
        self.kind(name).is_ok()
    }

    /// Whether this directory is on a filesystem whose `f_type` is `magic`.
    ///
    /// # Errors
    ///
    /// `fstatfs` fails.
    pub fn on_filesystem(&self, magic: i64) -> io::Result<bool> {
        // SAFETY: `statfs` is plain data, all zeros is a valid value of it.
        let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `fs` is writable for the call; the descriptor is open.
        if unsafe { libc::fstatfs(self.fd.as_raw_fd(), &mut fs) } != 0 {
            return Err(last());
        }
        Ok(fs.f_type as i64 == magic)
    }

    /// The contents of the file `name`, up to 64 KiB.
    ///
    /// # Errors
    ///
    /// A refused name, a link, no such file, or a read that fails.
    pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        let fd = self.open_at(name, libc::O_RDONLY, 0)?;
        let mut out = vec![0u8; READ_MAX];
        let mut len = 0;
        loop {
            // SAFETY: the pointer and length name the unused part of `out`, writable for the call.
            let n = unsafe { libc::read(fd, out[len..].as_mut_ptr().cast(), out.len() - len) };
            if n < 0 {
                let error = last();
                close(fd);
                return Err(error);
            }
            if n == 0 || len + n as usize == out.len() {
                len += n as usize;
                break;
            }
            len += n as usize;
        }
        close(fd);
        out.truncate(len);
        Ok(out)
    }

    /// Replaces the contents of the file `name` with `data`, making it where it is not there.
    ///
    /// # Errors
    ///
    /// A refused name, a link, or the kernel's reason.
    pub fn write(&self, name: &str, data: &[u8]) -> io::Result<()> {
        let fd = self.open_at(name, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o644)?;
        let mut done = 0;
        while done < data.len() {
            // SAFETY: the pointer and length name the unwritten part of `data`, readable for the call.
            let n = unsafe { libc::write(fd, data[done..].as_ptr().cast(), data.len() - done) };
            if n < 0 {
                let error = last();
                close(fd);
                return Err(error);
            }
            done += n as usize;
        }
        close(fd);
        Ok(())
    }

    /// Removes the directory `name`.
    ///
    /// # Errors
    ///
    /// A refused name, or the kernel's reason.
    pub fn remove_dir(&self, name: &str) -> io::Result<()> {
        self.unlink(name, libc::AT_REMOVEDIR)
    }

    /// Removes the file or link `name`.
    ///
    /// # Errors
    ///
    /// A refused name, or the kernel's reason.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        self.unlink(name, 0)
    }

    fn unlink(&self, name: &str, flags: libc::c_int) -> io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: `c` is NUL-terminated and outlives the call; the directory descriptor is open.
        if unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), flags) } != 0 {
            return Err(last());
        }
        Ok(())
    }

    /// Makes the symbolic link `name`, pointing at `target`. The target is data the kernel stores:
    /// it is not followed here.
    ///
    /// # Errors
    ///
    /// A refused name, or the kernel's reason.
    pub fn symlink(&self, target: &str, name: &str) -> io::Result<()> {
        let c = c_name(name)?;
        let t = CString::new(target).map_err(|_| refused("a NUL in a link target"))?;
        // SAFETY: both strings are NUL-terminated and outlive the call; the descriptor is open.
        if unsafe { libc::symlinkat(t.as_ptr(), self.fd.as_raw_fd(), c.as_ptr()) } != 0 {
            return Err(last());
        }
        Ok(())
    }

    /// Where the link `name` points.
    ///
    /// # Errors
    ///
    /// A refused name, not a link, or no such entry.
    pub fn read_link(&self, name: &str) -> io::Result<String> {
        let c = c_name(name)?;
        let mut buf = [0u8; 4096];
        // SAFETY: `c` is NUL-terminated; `buf` is writable for its length, both alive for the call.
        let n = unsafe {
            libc::readlinkat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n < 0 {
            return Err(last());
        }
        Ok(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }

    /// Gives the file `name` to `uid` and `gid`, not following a link.
    ///
    /// # Errors
    ///
    /// A refused name, or the kernel's reason.
    pub fn set_owner(&self, name: &str, uid: u32, gid: u32) -> io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: `c` is NUL-terminated and outlives the call; the descriptor is open.
        let rc = unsafe {
            libc::fchownat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                uid,
                gid,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 { Err(last()) } else { Ok(()) }
    }

    /// Sets the mode of the regular file `name`. A link is refused: its mode is not the file's.
    ///
    /// # Errors
    ///
    /// A refused name, a link, or the kernel's reason.
    pub fn set_mode(&self, name: &str, mode: u32) -> io::Result<()> {
        let c = c_name(name)?;
        if self.kind(name)? == Kind::Symlink {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        // SAFETY: `c` is NUL-terminated and outlives the call; the descriptor is open.
        if unsafe { libc::fchmodat(self.fd.as_raw_fd(), c.as_ptr(), mode as libc::mode_t, 0) } != 0
        {
            return Err(last());
        }
        Ok(())
    }

    /// The names of the entries of this directory, without `.` and `..`, sorted.
    ///
    /// # Errors
    ///
    /// The directory cannot be read.
    pub fn names(&self) -> io::Result<Vec<String>> {
        // SAFETY: `F_DUPFD_CLOEXEC` takes an integer; the copy is the stream's, closed by `closedir`.
        let copy = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if copy < 0 {
            return Err(last());
        }
        // SAFETY: `copy` is a directory descriptor this call owns; on success the stream owns it.
        let stream = unsafe { libc::fdopendir(copy) };
        if stream.is_null() {
            let error = last();
            close(copy);
            return Err(error);
        }
        // The stream was made from a duplicate, which shares the offset: start from the top.
        // SAFETY: `stream` is the open stream just made.
        unsafe { libc::rewinddir(stream) };
        let mut names = Vec::new();
        loop {
            // SAFETY: `stream` is open; `readdir` returns null at the end or an entry valid until
            // the next call, which is copied out before it.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break;
            }
            // SAFETY: `entry` is non-null and valid until the next `readdir`; this reads its field.
            let field = unsafe { &raw const (*entry).d_name };
            // SAFETY: `d_name` of a returned entry is a NUL-terminated string.
            let name = unsafe { CStr::from_ptr(field.cast()) };
            let name = name.to_string_lossy().into_owned();
            if name != "." && name != ".." {
                names.push(name);
            }
        }
        // SAFETY: `stream` is open, and closed exactly once here (with the descriptor in it).
        unsafe { libc::closedir(stream) };
        names.sort();
        Ok(names)
    }
}

fn close(fd: RawFd) {
    // SAFETY: `fd` is a descriptor this module opened and closes exactly once.
    unsafe { libc::close(fd) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lemnos-confined-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_name_that_is_not_one_entry_is_refused_before_any_call() {
        for bad in ["", ".", "..", "a/b", "/etc", "../x", "x/..", "a\0b"] {
            let error = check_name(bad).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert!(check_name(&"x".repeat(256)).is_err());
        assert!(check_name("lemnos-dummy_udc.0").is_ok());
        let root = scratch("names");
        let dir = Dir::open_path(&root).unwrap();
        assert!(dir.open_dir("..").is_err());
        assert!(dir.write("../escape", b"x").is_err());
        assert!(dir.read("/etc/passwd").is_err());
        assert!(!root.parent().unwrap().join("escape").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_link_is_never_followed_for_a_directory_a_file_or_a_write() {
        let root = scratch("links");
        let outside = scratch("links-outside");
        std::fs::write(outside.join("secret"), b"s").unwrap();
        symlink(&outside, root.join("dir")).unwrap();
        symlink(outside.join("secret"), root.join("file")).unwrap();
        let dir = Dir::open_path(&root).unwrap();
        assert_eq!(
            dir.open_dir("dir").unwrap_err().raw_os_error(),
            Some(libc::ELOOP)
        );
        assert!(dir.read("file").is_err());
        assert!(dir.write("file", b"changed").is_err());
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"s");
        // The link itself is an entry, and is what `kind` says.
        assert_eq!(dir.kind("dir").unwrap(), Kind::Symlink);
        // A link as the root of a `Dir` is not followed either.
        assert!(Dir::open_path(&root.join("dir")).is_err());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn entries_are_made_read_listed_and_removed_through_the_directory() {
        let root = scratch("ops");
        let dir = Dir::open_path(&root).unwrap();
        dir.make_dir("g", 0o755).unwrap();
        assert!(dir.make_dir("g", 0o755).is_err());
        dir.ensure_dir("g", 0o755).unwrap();
        let g = dir.open_dir("g").unwrap();
        g.write("UDC", b"x\n").unwrap();
        assert_eq!(g.read("UDC").unwrap(), b"x\n");
        g.symlink("../g", "back").unwrap();
        assert_eq!(g.read_link("back").unwrap(), "../g");
        assert_eq!(
            g.names().unwrap(),
            vec!["UDC".to_string(), "back".to_string()]
        );
        assert_eq!(g.kind("UDC").unwrap(), Kind::File);
        assert_eq!(dir.kind("g").unwrap(), Kind::Dir);
        assert!(!dir.exists("missing"));
        g.remove_file("back").unwrap();
        g.remove_file("UDC").unwrap();
        dir.remove_dir("g").unwrap();
        assert!(dir.names().unwrap().is_empty());
        assert!(g.proc_path().starts_with("/proc/self/fd/"));
        let _ = std::fs::remove_dir_all(root);
    }
}
