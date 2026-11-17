//! System calls the QEMU guest programs make that std does not offer: mounts, power off, sync,
//! a raw claim on the control node, the descriptor limit, descriptor flags, signals and the
//! console. The guests are test programs, not drivers, so their few `unsafe` calls live here
//! with the rest of this crate's, one operation per block.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use crate::ioctl::iowr;

/// `NAME_MAX` of the control node's claim: the resource is at most this many bytes.
pub const CLAIM_NAME_MAX: usize = 64;

/// `struct lemnos_claim` from `kernel/lemnos.h`: the resource name, the flags, and the claim's fd
/// the kernel returns.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawClaim {
    resource: [u8; CLAIM_NAME_MAX],
    flags: u32,
    fd: i32,
}

/// `LEMNOS_IOC_CLAIM`, `_IOWR('L', 0xce, struct lemnos_claim)`.
const LEMNOS_IOC_CLAIM: u32 = iowr::<RawClaim>(b'L', 0xce);

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

/// Mounts `fstype` from `source` at `target`, with no flags and no options.
///
/// # Errors
///
/// `mount(2)` fails, or a name holds a NUL.
pub fn mount(source: &str, target: &str, fstype: &str) -> io::Result<()> {
    let source = c_string(source)?;
    let target = c_string(target)?;
    let fstype = c_string(fstype)?;
    // SAFETY: the three strings are NUL-terminated and live across the call; the data argument is
    // null, which mount(2) accepts for filesystems that take no options.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Writes every buffered block of every filesystem to its device.
pub fn sync() {
    // SAFETY: `sync` takes no arguments and cannot fail.
    unsafe { libc::sync() };
}

/// Syncs, then powers the machine off. Returns only when the kernel refuses.
///
/// # Errors
///
/// `reboot(2)` fails.
pub fn power_off() -> io::Result<()> {
    sync();
    // SAFETY: `RB_POWER_OFF` is a plain command with no arguments; the kernel acts on it only
    // for the init process or a privileged caller.
    let rc = unsafe { libc::reboot(libc::RB_POWER_OFF) };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
}

/// The errno of `init_module` called with no image, which is what a kernel without module
/// support answers with `ENOSYS`. `None` when the call did not fail.
pub fn init_module_errno() -> Option<i32> {
    let empty = c_string("").ok()?;
    // SAFETY: `init_module(NULL, 0, "")` reads no memory through its arguments: the image is
    // null and its length zero, and the parameter string is a live NUL-terminated string.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_init_module,
            std::ptr::null::<libc::c_void>(),
            0usize,
            empty.as_ptr(),
        )
    };
    if rc == -1 {
        return last_error().raw_os_error();
    }
    None
}

/// Makes descriptors 0, 1 and 2 the console, as a process started by the kernel as init has them.
///
/// # Errors
///
/// `/dev/console` cannot be opened, or a `dup2` fails.
pub fn dup_console_stdio() -> io::Result<()> {
    let console = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/console")?;
    for target in 0..=2 {
        // SAFETY: both descriptors are open: `console` is held until this function returns, and
        // `dup2` replaces the descriptor `target`, which only ever names the standard streams.
        let rc = unsafe { libc::dup2(console.as_raw_fd(), target) };
        if rc < 0 {
            return Err(last_error());
        }
    }
    Ok(())
}

/// Takes the claim `resource` on the control node `node`, with `flags`, and returns its file.
/// The name is copied as given, zero-filled to [`CLAIM_NAME_MAX`] and not checked, so a test can
/// send the kernel a malformed request; the kernel refuses it with its own errno.
///
/// # Errors
///
/// The kernel refuses the claim; its errno is the error's.
///
/// A `resource` longer than [`CLAIM_NAME_MAX`] is cut to it.
pub fn claim_raw(node: BorrowedFd<'_>, resource: &[u8], flags: u32) -> io::Result<OwnedFd> {
    let mut arg = RawClaim {
        resource: [0; CLAIM_NAME_MAX],
        flags,
        fd: -1,
    };
    let n = resource.len().min(CLAIM_NAME_MAX);
    arg.resource[..n].copy_from_slice(&resource[..n]);
    // SAFETY: `LEMNOS_IOC_CLAIM` is `_IOWR` of `RawClaim`, so the kernel reads and writes one
    // `RawClaim` at the pointer, and `arg` is a live, exclusive `RawClaim` for that.
    let rc = unsafe {
        libc::ioctl(
            node.as_raw_fd(),
            LEMNOS_IOC_CLAIM as _,
            &mut arg as *mut RawClaim,
        )
    };
    if rc < 0 {
        return Err(last_error());
    }
    // SAFETY: the kernel just made `arg.fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(arg.fd) })
}

/// Sets the open-descriptor limit, soft and hard, to `limit`.
///
/// # Errors
///
/// `setrlimit(2)` fails.
pub fn set_nofile_limit(limit: u64) -> io::Result<()> {
    let lim = libc::rlimit {
        rlim_cur: limit,
        rlim_max: limit,
    };
    // SAFETY: `lim` is a live `rlimit` for the call, which only reads it.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Whether `fd` is close-on-exec (`FD_CLOEXEC`).
///
/// # Errors
///
/// `fcntl(2)` fails, for instance on a descriptor that is not open.
pub fn close_on_exec(fd: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: `F_GETFD` takes no argument, and `fd` is a live descriptor for the call.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(last_error());
    }
    Ok(flags & libc::FD_CLOEXEC != 0)
}

/// Sends `signal` to process `pid`. Only a positive pid names one process: 0 and negative
/// values name process groups or every process, and are refused without a call.
///
/// # Errors
///
/// `pid` is not positive (EINVAL), or `kill(2)` fails, for instance when no such process exists.
pub fn kill(pid: libc::pid_t, signal: i32) -> io::Result<()> {
    if pid <= 0 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: `kill` reads no memory; `pid` and `signal` are plain integers.
    let rc = unsafe { libc::kill(pid, signal) };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
}

fn c_string(text: &str) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(text.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a name holds a NUL"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `_IOWR('L', 0xce, struct lemnos_claim)` with the header's 72-byte structure.
    #[test]
    fn the_claim_request_is_the_kernels_number() {
        assert_eq!(size_of::<RawClaim>(), 72);
        assert_eq!(LEMNOS_IOC_CLAIM, 0xC048_4CCE);
    }
}
