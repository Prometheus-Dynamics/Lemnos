//! A private mount namespace shaped as the service manager shapes one for a unit: the calls that the
//! QEMU harness's unit launcher (`lemnos-unit-check exec`) needs to carry out `ProtectSystem=strict`,
//! `ReadWritePaths=`, `ProtectKernelTunables=`, `PrivateTmp=` and the unit's runtime directory before
//! it drops to the unit's capability set. Each is one system call. They need `CAP_SYS_ADMIN`, so they
//! run while the launcher is still full root.
//!
//! Nothing in the daemon calls these: the daemon runs in whatever namespace its service manager
//! made. The tests cover only the flag mapping; the calls change the running process's view.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A mount's own flags, as `/proc/self/mountinfo` lists them (its sixth field), which a read-only
/// remount must carry again or the kernel refuses a change it was not asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Kept {
    /// `nosuid`.
    pub no_suid: bool,
    /// `nodev`.
    pub no_dev: bool,
    /// `noexec`.
    pub no_exec: bool,
    /// `noatime`.
    pub no_atime: bool,
    /// `nodiratime`.
    pub no_diratime: bool,
    /// `relatime`.
    pub relatime: bool,
}

impl Kept {
    /// From the comma-separated mount options of a mountinfo line.
    #[must_use]
    pub fn parse(options: &str) -> Self {
        let mut kept = Self::default();
        for option in options.split(',') {
            match option {
                "nosuid" => kept.no_suid = true,
                "nodev" => kept.no_dev = true,
                "noexec" => kept.no_exec = true,
                "noatime" => kept.no_atime = true,
                "nodiratime" => kept.no_diratime = true,
                "relatime" => kept.relatime = true,
                _ => {}
            }
        }
        kept
    }

    fn bits(self) -> libc::c_ulong {
        let mut bits = 0;
        for (on, bit) in [
            (self.no_suid, libc::MS_NOSUID),
            (self.no_dev, libc::MS_NODEV),
            (self.no_exec, libc::MS_NOEXEC),
            (self.no_atime, libc::MS_NOATIME),
            (self.no_diratime, libc::MS_NODIRATIME),
            (self.relatime, libc::MS_RELATIME),
        ] {
            if on {
                bits |= bit;
            }
        }
        bits
    }
}

/// Moves this process into a new mount namespace of its own (`unshare(CLONE_NEWNS)`). Its children
/// inherit it.
///
/// # Errors
///
/// `unshare(2)` fails (`EPERM` without `CAP_SYS_ADMIN`).
pub fn unshare_mounts() -> io::Result<()> {
    // SAFETY: unshare takes a flag word and no pointer.
    check(unsafe { libc::unshare(libc::CLONE_NEWNS) })
}

/// Moves the calling process into the mount namespace of process `pid`: what it mounts, unmounts
/// and sees from then on is that process's, as for a unit's own mounts, which only the unit sees.
/// The caller must be single-threaded and hold `CAP_SYS_ADMIN`.
///
/// # Errors
///
/// `/proc/<pid>/ns/mnt` can't be opened, or `setns(2)` fails.
pub fn enter_mount_namespace(pid: u32) -> io::Result<()> {
    let ns = File::open(format!("/proc/{pid}/ns/mnt"))?;
    // SAFETY: the descriptor is open for the call (`ns` lives to the end of the function), and
    // CLONE_NEWNS names the namespace kind the file is.
    check(unsafe { libc::setns(ns.as_raw_fd(), libc::CLONE_NEWNS) })
}

/// A mount propagation, as `mount(2)` changes it for every mount under `/` (`MS_REC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Propagation {
    /// Mounts from the namespace this one came from still arrive; none made here go back.
    Slave,
    /// Mounts made here reach this namespace's own peers (none outside, after [`Self::Slave`]).
    Shared,
}

/// Sets the propagation of every mount under `/`. The service manager makes a unit's mounts slave
/// before it sets the namespace up and shared after, so a unit's own mounts never reach the host.
///
/// # Errors
///
/// `mount(2)` fails.
pub fn propagate(to: Propagation) -> io::Result<()> {
    let root = c"/";
    let flag = match to {
        Propagation::Slave => libc::MS_SLAVE,
        Propagation::Shared => libc::MS_SHARED,
    };
    // SAFETY: the target is a NUL-terminated string; source, type and data may be null for a
    // propagation change.
    check(unsafe {
        libc::mount(
            core::ptr::null(),
            root.as_ptr(),
            core::ptr::null(),
            libc::MS_REC | flag,
            core::ptr::null(),
        )
    })
}

/// Bind-mounts `path` (and every mount under it) onto itself, so it is a mount of its own whose
/// read-only flag is set apart from its parent's.
///
/// # Errors
///
/// `mount(2)` fails (`ENOENT` when `path` does not exist).
pub fn bind_self(path: &Path) -> io::Result<()> {
    let path = c_path(path)?;
    // SAFETY: source and target are the same NUL-terminated string, alive for the call; type and
    // data may be null for a bind.
    check(unsafe {
        libc::mount(
            path.as_ptr(),
            path.as_ptr(),
            core::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            core::ptr::null(),
        )
    })
}

/// Remounts the mount at `path` read-only or writable (`MS_REMOUNT | MS_BIND`), keeping its own
/// flags `kept`. Only this mount changes, not the filesystem under it or another mount of it.
///
/// # Errors
///
/// `mount(2)` fails.
pub fn remount(path: &Path, read_only: bool, kept: Kept) -> io::Result<()> {
    let path = c_path(path)?;
    let mut flags = libc::MS_REMOUNT | libc::MS_BIND | kept.bits();
    if read_only {
        flags |= libc::MS_RDONLY;
    }
    // SAFETY: the target is a NUL-terminated string alive for the call; source, type and data may
    // be null for a remount.
    check(unsafe {
        libc::mount(
            core::ptr::null(),
            path.as_ptr(),
            core::ptr::null(),
            flags,
            core::ptr::null(),
        )
    })
}

/// Mounts a fresh tmpfs at `path`, mode 1777: a private `/tmp`.
///
/// # Errors
///
/// `mount(2)` fails.
pub fn private_tmp(path: &Path) -> io::Result<()> {
    let path = c_path(path)?;
    let kind = c"tmpfs";
    let data = c"mode=1777";
    // SAFETY: every pointer is a NUL-terminated string alive for the call.
    check(unsafe {
        libc::mount(
            kind.as_ptr(),
            path.as_ptr(),
            kind.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            data.as_ptr().cast(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mounts_own_flags_are_read_and_mapped() {
        let kept = Kept::parse("rw,nosuid,nodev,noexec,relatime");
        assert!(kept.no_suid && kept.no_dev && kept.no_exec && kept.relatime);
        assert!(!kept.no_atime);
        assert_eq!(
            kept.bits(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_RELATIME
        );
        assert_eq!(Kept::parse("ro").bits(), 0);
    }
}
