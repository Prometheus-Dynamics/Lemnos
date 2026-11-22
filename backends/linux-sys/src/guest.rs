//! System calls the QEMU guest programs make that std does not offer: mounts, power off, sync,
//! a raw claim on the control node, the descriptor limit, descriptor flags, signals and the
//! console. The guests are test programs, not drivers, so their few `unsafe` calls live here
//! with the rest of this crate's, one operation per block.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use crate::ioctl::iowr;
use crate::privilege::ChildEnd;

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
    mount_data(source, target, fstype, "")
}

/// Mounts `fstype` from `source` at `target` with `data` as its options (`mount(2)`'s data
/// argument, for example `size=64M` for tmpfs).
///
/// # Errors
///
/// `mount(2)` fails, or a name or the options hold a NUL.
pub fn mount_data(source: &str, target: &str, fstype: &str, data: &str) -> io::Result<()> {
    let source = c_string(source)?;
    let target = c_string(target)?;
    let fstype = c_string(fstype)?;
    let data = c_string(data)?;
    // SAFETY: the four strings are NUL-terminated and live across the call; mount(2) reads the
    // data as a string of options, which may be empty.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            data.as_ptr().cast(),
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

/// Loads the module `fd` names, with `params` as its parameter string, as `finit_module(2)` with
/// no flags.
///
/// # Errors
///
/// `params` holds a NUL, or the kernel refuses the module, with its errno.
pub fn finit_module(fd: BorrowedFd<'_>, params: &str) -> io::Result<()> {
    let params = c_string(params)?;
    // SAFETY: `SYS_finit_module` takes a descriptor, a NUL-terminated parameter string and flags.
    // `fd` is live for the call, `params` outlives it, and the kernel only reads the string.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_finit_module,
            fd.as_raw_fd() as libc::c_long,
            params.as_ptr(),
            0 as libc::c_long,
        )
    };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Unloads the module `name`, with `flags` (`O_NONBLOCK` asks for no wait), as `delete_module(2)`.
///
/// # Errors
///
/// `name` holds a NUL, or the kernel refuses the unload (EBUSY, EAGAIN), with its errno.
pub fn delete_module(name: &str, flags: u32) -> io::Result<()> {
    let name = c_string(name)?;
    // SAFETY: `SYS_delete_module` takes a NUL-terminated module name and flags. `name` outlives
    // the call, and the kernel only reads the string.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_delete_module,
            name.as_ptr(),
            flags as libc::c_long,
        )
    };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
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

/// `LEMNOS_IOC_REGJOB_OPEN`, `_IOWR('L', 0xf0, struct lemnos_regjob_open)`: 24 bytes.
const LEMNOS_IOC_REGJOB_OPEN: u32 = iowr::<[u8; 24]>(b'L', 0xf0);

/// Opens a register-job session on `block` of the control node, with `flags`. `arg` is the request's
/// 24 bytes as the kernel reads and writes them: block and flags in, and out the session's
/// descriptor (at byte 8), profile, window and ABI. The session's file is returned owned; the
/// descriptor number left in `arg` is then only a number, not a second owner.
///
/// # Errors
///
/// The kernel refuses the session, with its errno (EBUSY for a second opener, EINVAL for a role the
/// profile does not have).
pub fn regjob_open(node: BorrowedFd<'_>, arg: &mut [u8; 24]) -> io::Result<OwnedFd> {
    ioctl_bytes(node, LEMNOS_IOC_REGJOB_OPEN, arg)?;
    let fd = i32::from_ne_bytes([arg[8], arg[9], arg[10], arg[11]]);
    // SAFETY: the kernel just made `fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `LEMNOS_IOC_PISP_BE_CREATE`, `_IOWR('L', 0xb0, struct lemnos_pisp_be_create)`: 80 bytes.
const LEMNOS_IOC_PISP_BE_CREATE: u32 = iowr::<[u8; 80]>(b'L', 0xb0);

/// Makes a frame stream on a PiSP back-end block of the control node. `arg` is the request's 80
/// bytes as the kernel reads and writes them: `struct lemnos_stream_create` (56 bytes) in, then the
/// back end's out fields. The stream's descriptor (at byte 36) is returned owned.
///
/// # Errors
///
/// The kernel refuses the stream, with its errno.
pub fn pisp_be_create(node: BorrowedFd<'_>, arg: &mut [u8; 80]) -> io::Result<OwnedFd> {
    ioctl_bytes(node, LEMNOS_IOC_PISP_BE_CREATE, arg)?;
    let fd = i32::from_ne_bytes([arg[36], arg[37], arg[38], arg[39]]);
    // SAFETY: the kernel just made `fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `LEMNOS_RP1_CSI2_IOC_STREAM_CREATE`, `_IOWR('L', 0xc1, struct lemnos_stream_create)`: 56 bytes.
const LEMNOS_RP1_CSI2_IOC_STREAM_CREATE: u32 = iowr::<[u8; 56]>(b'L', 0xc1);

/// Makes a stream on a receiver's node, as `LEMNOS_RP1_CSI2_IOC_STREAM_CREATE`. `arg` is the
/// 56-byte `struct lemnos_stream_create`; the stream's descriptor (at byte 36) is returned owned.
///
/// # Errors
///
/// The kernel refuses the stream, with its errno.
pub fn rp1_csi2_stream_create(node: BorrowedFd<'_>, arg: &mut [u8; 56]) -> io::Result<OwnedFd> {
    ioctl_bytes(node, LEMNOS_RP1_CSI2_IOC_STREAM_CREATE, arg)?;
    let fd = i32::from_ne_bytes([arg[36], arg[37], arg[38], arg[39]]);
    // SAFETY: the kernel just made `fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `LEMNOS_IOC_STREAM_CREATE`, `_IOWR('L', 0x80, struct lemnos_stream_create)`: 56 bytes, on the
/// control node.
const LEMNOS_IOC_STREAM_CREATE: u32 = iowr::<[u8; 56]>(b'L', 0x80);

/// `LEMNOS_STREAM_IOC_READER`, `_IOWR('L', 0x87, struct lemnos_stream_reader)`: 48 bytes, on a
/// stream's file.
const LEMNOS_STREAM_IOC_READER: u32 = iowr::<[u8; 48]>(b'L', 0x87);

/// Makes a frame stream on the control node, as `LEMNOS_IOC_STREAM_CREATE`. `arg` is the 56-byte
/// `struct lemnos_stream_create`, and its ring size and pool address come back in it; the stream's
/// descriptor (at byte 36) is returned owned.
///
/// # Errors
///
/// The kernel refuses the stream, with its errno.
pub fn stream_create(node: BorrowedFd<'_>, arg: &mut [u8; 56]) -> io::Result<OwnedFd> {
    ioctl_bytes(node, LEMNOS_IOC_STREAM_CREATE, arg)?;
    let fd = i32::from_ne_bytes([arg[36], arg[37], arg[38], arg[39]]);
    // SAFETY: the kernel just made `fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Gives a stream a reader's marks page, as `LEMNOS_STREAM_IOC_READER`. `arg` is the 48-byte
/// `struct lemnos_stream_reader` with the page's memfd in its first word; the registration's slot
/// and refusal come back in it, and the registration's file (at byte 12) is returned owned.
///
/// # Errors
///
/// The module refuses the page, with its errno.
pub fn stream_reader(stream: BorrowedFd<'_>, arg: &mut [u8; 48]) -> io::Result<OwnedFd> {
    ioctl_bytes(stream, LEMNOS_STREAM_IOC_READER, arg)?;
    let fd = i32::from_ne_bytes([arg[12], arg[13], arg[14], arg[15]]);
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    // SAFETY: the kernel just made `fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `LEMNOS_IOC_GSDMA_CREATE`, `_IOWR('L', 0xe8, struct lemnos_gsdma_create)`: 64 bytes, on the
/// control node.
const LEMNOS_IOC_GSDMA_CREATE: u32 = iowr::<[u8; 64]>(b'L', 0xe8);

/// Makes a graphics DMA stream on the control node, as `LEMNOS_IOC_GSDMA_CREATE`. `arg` is the
/// 64-byte `struct lemnos_gsdma_create`: its stream's ring size and pool address come back in it,
/// with the engine's reach, and the stream's descriptor (at byte 36) is returned owned.
///
/// # Errors
///
/// The kernel refuses the stream, with its errno.
pub fn gsdma_create(node: BorrowedFd<'_>, arg: &mut [u8; 64]) -> io::Result<OwnedFd> {
    ioctl_bytes(node, LEMNOS_IOC_GSDMA_CREATE, arg)?;
    let fd = i32::from_ne_bytes([arg[36], arg[37], arg[38], arg[39]]);
    // SAFETY: the kernel just made `fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `LEMNOS_IOC_VIP_CREATE`, `_IOWR('L', 0xe0, struct lemnos_vip_create)`: 64 bytes, on the control
/// node.
const LEMNOS_IOC_VIP_CREATE: u32 = iowr::<[u8; 64]>(b'L', 0xe0);

/// Makes a VIP9000 frame stream on the control node, as `LEMNOS_IOC_VIP_CREATE`. `arg` is the
/// 64-byte `struct lemnos_vip_create`: its stream's ring size and pool address come back in it,
/// with the block's customer id and MMU kind, and the stream's descriptor (at byte 36) is returned
/// owned.
///
/// # Errors
///
/// The kernel refuses the stream, with its errno.
pub fn vip_create(node: BorrowedFd<'_>, arg: &mut [u8; 64]) -> io::Result<OwnedFd> {
    ioctl_bytes(node, LEMNOS_IOC_VIP_CREATE, arg)?;
    let fd = i32::from_ne_bytes([arg[36], arg[37], arg[38], arg[39]]);
    // SAFETY: the kernel just made `fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Pins the calling thread to CPU `cpu`, or, for `None`, lets it run on every CPU
/// (`sched_setaffinity(2)` on pid 0).
///
/// # Errors
///
/// `sched_setaffinity(2)` fails.
pub fn set_cpu_affinity(cpu: Option<usize>) -> io::Result<()> {
    if cpu.is_some_and(|cpu| cpu >= libc::CPU_SETSIZE as usize) {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: an all-zero `cpu_set_t` is an empty set: its words are plain integers.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for i in 0..libc::CPU_SETSIZE as usize {
        if cpu.is_none_or(|cpu| cpu == i) {
            // SAFETY: `i` is below CPU_SETSIZE, the set's size, so the bit it sets is in `set`.
            unsafe { libc::CPU_SET(i, &mut set) };
        }
    }
    // SAFETY: `set` is a live `cpu_set_t` of the size passed, which the call only reads.
    let rc = unsafe {
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw const set)
    };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
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

/// Asks for a private (copy-on-write) mapping of `len` bytes of `fd` at `offset`. The kernel's
/// refusal is the error; a granted mapping is unmapped again at once.
///
/// # Errors
///
/// `mmap(2)` refuses the mapping, for instance with `EINVAL` for a private one of a device.
pub fn private_mapping(fd: BorrowedFd<'_>, len: usize, offset: u64) -> io::Result<()> {
    // SAFETY: a null hint lets the kernel choose the address; `fd` is live for the call; the
    // result is compared with MAP_FAILED before it is used.
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            fd.as_raw_fd(),
            offset as libc::off_t,
        )
    };
    if raw == libc::MAP_FAILED {
        return Err(last_error());
    }
    // SAFETY: `raw` is the mapping `mmap` just returned, of `len` bytes, and nothing else uses it.
    unsafe { libc::munmap(raw, len) };
    Ok(())
}

/// Asks for a mapping of `len` bytes of `fd` at `offset`, with `prot` and `flags` (the `PROT_*` and
/// `MAP_*` constants). The kernel's refusal is the error; a granted mapping is unmapped again at
/// once. For a check of what the kernel refuses to map, where [`private_mapping`] is too narrow.
///
/// # Errors
///
/// `mmap(2)` refuses the mapping.
pub fn mapping_probe(
    fd: BorrowedFd<'_>,
    len: usize,
    offset: u64,
    prot: i32,
    flags: i32,
) -> io::Result<()> {
    // SAFETY: a null hint lets the kernel choose the address; `fd` is live for the call; the
    // result is compared with MAP_FAILED before it is used.
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            prot,
            flags,
            fd.as_raw_fd(),
            offset as libc::off_t,
        )
    };
    if raw == libc::MAP_FAILED {
        return Err(last_error());
    }
    // SAFETY: `raw` is the mapping `mmap` just returned, of `len` bytes, and nothing else uses it.
    unsafe { libc::munmap(raw, len) };
    Ok(())
}

/// The descriptor limits, soft and hard, as `getrlimit(2)` gives them for `RLIMIT_NOFILE`.
///
/// # Errors
///
/// `getrlimit(2)` fails.
pub fn nofile_limits() -> io::Result<(u64, u64)> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a live `rlimit` for the call, which writes it.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
    if rc != 0 {
        return Err(last_error());
    }
    Ok((lim.rlim_cur, lim.rlim_max))
}

/// Sets the descriptor limits, soft and hard, to `soft` and `hard` (unlike [`set_nofile_limit`],
/// which sets both to one value).
///
/// # Errors
///
/// `setrlimit(2)` fails.
pub fn set_nofile_limits(soft: u64, hard: u64) -> io::Result<()> {
    let lim = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: `lim` is a live `rlimit` for the call, which only reads it.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    if rc != 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Whether child `pid` has ended, without waiting: `None` while it runs.
///
/// # Errors
///
/// `waitpid(2)` fails other than by interruption.
pub fn try_wait(pid: libc::pid_t) -> io::Result<Option<ChildEnd>> {
    let mut status = 0;
    loop {
        // SAFETY: `status` is a writable c_int for the call; WNOHANG returns at once.
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc == 0 {
            return Ok(None);
        }
        if rc == pid {
            break;
        }
        let err = last_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(Some(if libc::WIFSIGNALED(status) {
        ChildEnd::Signal(libc::WTERMSIG(status))
    } else {
        ChildEnd::Exit(libc::WEXITSTATUS(status))
    }))
}

/// What [`wait_stop`] saw of a child: it is stopped, or it has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waited {
    /// It is stopped (a `SIGSTOP`, or another stop): it runs no more until continued or killed.
    Stopped,
    /// It has exited or was killed. Its status is not taken: the child stays a zombie until it is
    /// waited for, so its pid cannot be reused while the caller still holds it.
    Ended,
}

/// Blocks until child `pid` is stopped or has ended, without reaping it (`waitid` with `WNOWAIT`).
/// A stopped child is left stopped; the caller kills it or continues it, then reaps it as usual.
///
/// # Errors
///
/// `waitid(2)` fails other than by interruption (for instance `ECHILD`, when `pid` is not a child).
pub fn wait_stop(pid: libc::pid_t) -> io::Result<Waited> {
    loop {
        // SAFETY: an all-zero `siginfo_t` is a valid out-value: its fields are plain integers.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live, writable `siginfo_t` for the call; P_PID names one child by its pid, and
        // WNOWAIT leaves the child waitable, so nothing is reaped and no other process is named.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &raw mut info,
                libc::WSTOPPED | libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            let err = last_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        let code = info.si_code;
        return Ok(if code == libc::CLD_STOPPED || code == libc::CLD_TRAPPED {
            Waited::Stopped
        } else {
            Waited::Ended
        });
    }
}

/// Ends the calling process with `code` at once, without running its exit handlers: what a
/// forked child does instead of returning into the parent's code.
pub fn exit_now(code: i32) -> ! {
    // SAFETY: `_exit` takes an integer and never returns.
    unsafe { libc::_exit(code) }
}

/// Issues `request` with a uAPI structure the caller laid out in `arg`, `N` bytes, as the request
/// was made from `[u8; N]` (`_IOW`/`_IOWR` of that size), so the kernel reads and writes exactly
/// those bytes. For requests whose arguments a typed wrapper would not pass as given (a claim of
/// -1, a value outside 0 and 1): the kernel's answer is the error.
///
/// # Errors
///
/// The kernel refuses the request; its errno is the error's.
pub fn ioctl_bytes<const N: usize>(
    node: BorrowedFd<'_>,
    request: u32,
    arg: &mut [u8; N],
) -> io::Result<()> {
    // SAFETY: `arg` is a live, exclusive array of `N` bytes, which the request's size field says
    // is the structure the kernel reads (and writes back, for `_IOWR`); `node` is live.
    let rc = unsafe { libc::ioctl(node.as_raw_fd(), request as _, arg.as_mut_ptr()) };
    if rc < 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Issues `request` with `addr` as its argument, whatever is there: for the kernel's answer to an
/// argument it cannot read or write (an address with nothing mapped).
///
/// # Errors
///
/// The kernel refuses the request; its errno is the error's.
pub fn ioctl_at(node: BorrowedFd<'_>, request: u32, addr: usize) -> io::Result<()> {
    // SAFETY: the argument is an integer to this process; only the kernel follows it, with its own
    // checks, and a request that takes no memory it can reach changes nothing here. `node` is live.
    let rc = unsafe { libc::ioctl(node.as_raw_fd(), request as _, addr) };
    if rc < 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Reads up to `buf.len()` bytes from `fd`, as `read(2)`.
///
/// # Errors
///
/// `read(2)` fails, for instance with `EINVAL` for a buffer too short for an event.
pub fn read(fd: BorrowedFd<'_>, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is a live, writable slice of `buf.len()` bytes for the call.
    let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(last_error());
    }
    Ok(n as usize)
}

/// Moves the calling thread to `SCHED_FIFO` at `priority`. `true` when the kernel allowed it;
/// a refusal (no privilege) is not an error for the caller, who runs on as `SCHED_OTHER`.
pub fn set_fifo(priority: i32) -> bool {
    // SAFETY: an all-zero `sched_param` is valid: its fields are plain integers.
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    param.sched_priority = priority;
    // SAFETY: `param` is a live `sched_param` for the call, which only reads it; pid 0 is the
    // calling thread.
    unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) == 0 }
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

    /// `_IOWR('L', 0xf0, struct lemnos_regjob_open)` with the header's 24-byte structure.
    #[test]
    fn the_regjob_open_request_is_the_kernels_number() {
        assert_eq!(LEMNOS_IOC_REGJOB_OPEN, 0xC018_4CF0);
    }
}
