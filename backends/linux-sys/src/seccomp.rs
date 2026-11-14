//! A seccomp filter that allows an allowlist of system calls and kills the process on any other.
//!
//! The program is written here, directly, as classic BPF: it checks the architecture, loads the
//! call number and compares it with each allowed number. A call that matches none ends the process
//! with `SECCOMP_RET_KILL_PROCESS`. The filter is installed after `PR_SET_NO_NEW_PRIVS`, so it
//! needs no privilege. Each system call here is one operation, with its `SAFETY` note.

use std::io;

use libc::{sock_filter, sock_fprog};

/// The architecture a filter checks, as `seccomp_data.arch` names it.
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;
/// `seccomp_data`: the call number at offset 0, the architecture at offset 4.
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ALLOW: u32 = 0x7fff_0000;

/// The call numbers a filter can allow, by name: the calls the helper's verbs make. Each constant
/// is the number of the call of the same name.
#[allow(missing_docs, clippy::unnecessary_cast)]
pub mod nr {
    pub const READ: i64 = libc::SYS_read as i64;
    pub const WRITE: i64 = libc::SYS_write as i64;
    pub const CLOSE: i64 = libc::SYS_close as i64;
    pub const OPENAT: i64 = libc::SYS_openat as i64;
    pub const MKDIRAT: i64 = libc::SYS_mkdirat as i64;
    pub const UNLINKAT: i64 = libc::SYS_unlinkat as i64;
    pub const SYMLINKAT: i64 = libc::SYS_symlinkat as i64;
    pub const READLINKAT: i64 = libc::SYS_readlinkat as i64;
    pub const NEWFSTATAT: i64 = libc::SYS_newfstatat as i64;
    pub const FSTAT: i64 = libc::SYS_fstat as i64;
    pub const STATX: i64 = libc::SYS_statx as i64;
    pub const GETDENTS64: i64 = libc::SYS_getdents64 as i64;
    pub const LSEEK: i64 = libc::SYS_lseek as i64;
    pub const FSYNC: i64 = libc::SYS_fsync as i64;
    pub const FCHOWNAT: i64 = libc::SYS_fchownat as i64;
    pub const FCHMODAT: i64 = libc::SYS_fchmodat as i64;
    pub const MOUNT: i64 = libc::SYS_mount as i64;
    pub const UMOUNT2: i64 = libc::SYS_umount2 as i64;
    pub const MMAP: i64 = libc::SYS_mmap as i64;
    pub const MUNMAP: i64 = libc::SYS_munmap as i64;
    pub const MREMAP: i64 = libc::SYS_mremap as i64;
    pub const MPROTECT: i64 = libc::SYS_mprotect as i64;
    pub const BRK: i64 = libc::SYS_brk as i64;
    pub const RT_SIGACTION: i64 = libc::SYS_rt_sigaction as i64;
    pub const RT_SIGPROCMASK: i64 = libc::SYS_rt_sigprocmask as i64;
    pub const RT_SIGRETURN: i64 = libc::SYS_rt_sigreturn as i64;
    pub const SIGALTSTACK: i64 = libc::SYS_sigaltstack as i64;
    pub const CLOCK_GETTIME: i64 = libc::SYS_clock_gettime as i64;
    pub const GETRANDOM: i64 = libc::SYS_getrandom as i64;
    pub const EXIT: i64 = libc::SYS_exit as i64;
    pub const EXIT_GROUP: i64 = libc::SYS_exit_group as i64;
    pub const FCNTL: i64 = libc::SYS_fcntl as i64;
    pub const OPENAT2: i64 = 437;
    pub const FSTATFS: i64 = libc::SYS_fstatfs as i64;
    pub const GETSOCKOPT: i64 = libc::SYS_getsockopt as i64;
    pub const RECVFROM: i64 = libc::SYS_recvfrom as i64;
    pub const SENDTO: i64 = libc::SYS_sendto as i64;
    pub const CLONE: i64 = libc::SYS_clone as i64;
    pub const CLONE3: i64 = libc::SYS_clone3 as i64;
    pub const WAIT4: i64 = libc::SYS_wait4 as i64;
    pub const WAITID: i64 = libc::SYS_waitid as i64;
    pub const CLOCK_NANOSLEEP: i64 = libc::SYS_clock_nanosleep as i64;
    pub const NANOSLEEP: i64 = libc::SYS_nanosleep as i64;
    pub const PRCTL: i64 = libc::SYS_prctl as i64;
    pub const CAPGET: i64 = libc::SYS_capget as i64;
    pub const CAPSET: i64 = libc::SYS_capset as i64;
    pub const GETPID: i64 = libc::SYS_getpid as i64;
    pub const GETPPID: i64 = libc::SYS_getppid as i64;
    pub const GETUID: i64 = libc::SYS_getuid as i64;
    pub const GETEUID: i64 = libc::SYS_geteuid as i64;
    pub const GETGID: i64 = libc::SYS_getgid as i64;
    pub const GETEGID: i64 = libc::SYS_getegid as i64;
    pub const GETGROUPS: i64 = libc::SYS_getgroups as i64;
    pub const SETGROUPS: i64 = libc::SYS_setgroups as i64;
    pub const SETUID: i64 = libc::SYS_setuid as i64;
    pub const SETGID: i64 = libc::SYS_setgid as i64;
    pub const SETRESUID: i64 = libc::SYS_setresuid as i64;
    pub const SETRESGID: i64 = libc::SYS_setresgid as i64;
    pub const FUTEX: i64 = libc::SYS_futex as i64;
    pub const MADVISE: i64 = libc::SYS_madvise as i64;
    pub const SCHED_YIELD: i64 = libc::SYS_sched_yield as i64;
    pub const SECCOMP: i64 = libc::SYS_seccomp as i64;
    pub const RT_SIGQUEUEINFO: i64 = libc::SYS_rt_sigqueueinfo as i64;
    pub const TGKILL: i64 = libc::SYS_tgkill as i64;
    pub const GETTID: i64 = libc::SYS_gettid as i64;
    pub const SET_ROBUST_LIST: i64 = libc::SYS_set_robust_list as i64;
    pub const RSEQ: i64 = libc::SYS_rseq as i64;
    pub const SET_TID_ADDRESS: i64 = libc::SYS_set_tid_address as i64;
    pub const POLL: i64 = libc::SYS_ppoll as i64;
    /// The calls libc makes under their own names on x86_64 (glibc's `mkdir`, `unlink`, `chown`
    /// and the rest are their own system calls there, where aarch64 has only the `*at` forms).
    #[cfg(target_arch = "x86_64")]
    pub mod legacy {
        pub const MKDIR: i64 = libc::SYS_mkdir as i64;
        pub const RMDIR: i64 = libc::SYS_rmdir as i64;
        pub const OPEN: i64 = libc::SYS_open as i64;
        pub const STAT: i64 = libc::SYS_stat as i64;
        pub const LSTAT: i64 = libc::SYS_lstat as i64;
        pub const UNLINK: i64 = libc::SYS_unlink as i64;
        pub const SYMLINK: i64 = libc::SYS_symlink as i64;
        pub const READLINK: i64 = libc::SYS_readlink as i64;
        pub const CHMOD: i64 = libc::SYS_chmod as i64;
        pub const CHOWN: i64 = libc::SYS_chown as i64;
        pub const RENAME: i64 = libc::SYS_rename as i64;
        pub const FORK: i64 = libc::SYS_fork as i64;
        pub const VFORK: i64 = libc::SYS_vfork as i64;
    }
}

/// The calls of the architecture this is built for, for a filter that allows the helper's verbs.
#[must_use]
pub fn calls_for_helper(base: &[i64]) -> Vec<i64> {
    #[allow(unused_mut)]
    let mut calls = base.to_vec();
    #[cfg(target_arch = "x86_64")]
    calls.extend_from_slice(&[
        nr::legacy::MKDIR,
        nr::legacy::RMDIR,
        nr::legacy::OPEN,
        nr::legacy::STAT,
        nr::legacy::LSTAT,
        nr::legacy::UNLINK,
        nr::legacy::SYMLINK,
        nr::legacy::READLINK,
        nr::legacy::CHMOD,
        nr::legacy::CHOWN,
        nr::legacy::RENAME,
    ]);
    calls
}

/// [`calls_for_helper`] and the calls that make a process: on x86_64 `fork` and `vfork` as their own
/// calls (glibc and musl use `clone` on aarch64). For the supervisor, which forks the helper.
#[must_use]
pub fn calls_with_fork(base: &[i64]) -> Vec<i64> {
    #[allow(unused_mut)]
    let mut calls = calls_for_helper(base);
    #[cfg(target_arch = "x86_64")]
    calls.extend_from_slice(&[nr::legacy::FORK, nr::legacy::VFORK]);
    calls
}

/// The program for an allowlist: the architecture check, then one comparison per allowed call
/// number, a kill for anything else, and an allow. A call number that is not a `u32` is refused.
///
/// # Errors
///
/// The list is too long for a filter (more than 500 calls), or a number does not fit.
pub fn program(allowed: &[i64]) -> io::Result<Vec<sock_filter>> {
    if allowed.len() > 500 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many calls to allow",
        ));
    }
    let mut prog = Vec::with_capacity(allowed.len() + 6);
    prog.push(stmt(BPF_LD_W_ABS, OFFSET_ARCH));
    // Another architecture is killed: a call's number means something else there.
    prog.push(jump(BPF_JEQ_K, AUDIT_ARCH, 1, 0));
    prog.push(stmt(BPF_RET_K, RET_KILL_PROCESS));
    prog.push(stmt(BPF_LD_W_ABS, OFFSET_NR));
    let n = allowed.len();
    for (i, nr) in allowed.iter().enumerate() {
        let nr = u32::try_from(*nr).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "a call number is not a u32")
        })?;
        // A match jumps over the remaining comparisons, the kill, to the allow at the end.
        let jt = (n - i) as u8;
        prog.push(jump(BPF_JEQ_K, nr, jt, 0));
    }
    prog.push(stmt(BPF_RET_K, RET_KILL_PROCESS));
    prog.push(stmt(BPF_RET_K, RET_ALLOW));
    Ok(prog)
}

fn stmt(code: u16, k: u32) -> sock_filter {
    sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> sock_filter {
    sock_filter { code, jt, jf, k }
}

/// Installs a filter that allows exactly `allowed` for this thread, after setting no-new-privs.
/// Once installed, any other system call from the thread kills the process.
///
/// # Errors
///
/// The program cannot be built, or the kernel refuses the filter.
pub fn install(allowed: &[i64]) -> io::Result<()> {
    let prog = program(allowed)?;
    let len = u16::try_from(prog.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the filter is too long"))?;
    // SAFETY: prctl(PR_SET_NO_NEW_PRIVS, 1) takes no pointer; it only sets a flag on this thread.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let fprog = sock_fprog {
        len,
        filter: prog.as_ptr().cast_mut(),
    };
    // SAFETY: `fprog` points at `prog`, which lives until the call returns; the kernel copies the
    // program during the call, so nothing refers to it afterwards.
    let rc = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER as libc::c_ulong,
            &fprog as *const sock_fprog,
            0,
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// How a forked child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It returned this code, or `101` after a panic.
    Code(i32),
    /// It was killed by this signal (`SIGSYS` for a call the filter refuses).
    Signal(i32),
}

/// Runs `body` in a forked child and reports how the child ended. The child never returns into the
/// caller: it ends with `body`'s code. Used by tests, to check that a filter kills a forbidden call
/// without killing the test process.
///
/// # Errors
///
/// The fork or the wait fails.
pub fn run_forked(body: impl FnOnce() -> i32) -> io::Result<Exit> {
    // SAFETY: fork has no arguments. The child runs only `body` and then `_exit`, never returning
    // into the caller's frames. A panic in `body` is caught so it cannot unwind through the fork.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(101);
        // SAFETY: `_exit` ends the child without running the parent's atexit handlers or
        // destructors, which must not run in a copy of the test process.
        unsafe { libc::_exit(code) }
    }
    let mut status = 0;
    loop {
        // SAFETY: `status` is a valid, writable c_int for the duration of the call.
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc == pid {
            break;
        }
        if rc < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
    if libc::WIFSIGNALED(status) {
        Ok(Exit::Signal(libc::WTERMSIG(status)))
    } else {
        Ok(Exit::Code(libc::WEXITSTATUS(status)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_checks_the_architecture_then_each_call_then_kills() {
        let prog = program(&[nr::READ, nr::WRITE]).unwrap();
        // Architecture, its check, the kill, the number load, two comparisons, kill, allow.
        assert_eq!(prog.len(), 8);
        assert_eq!(prog[1].k, AUDIT_ARCH);
        assert_eq!(prog[2].k, RET_KILL_PROCESS);
        // The first comparison jumps over the second comparison, the kill, to the allow.
        assert_eq!((prog[4].jt, prog[5].jt), (2, 1));
        assert_eq!(prog[6].k, RET_KILL_PROCESS);
        assert_eq!(prog[7].k, RET_ALLOW);
    }

    #[test]
    fn a_number_that_is_not_a_u32_is_refused() {
        assert!(program(&[-1]).is_err());
        assert!(program(&vec![0; 501]).is_err());
    }

    #[test]
    fn a_filtered_child_runs_what_it_allows_and_is_killed_by_what_it_does_not() {
        // The allowed calls run: the child ends with its own code, through allowed exit calls.
        let allowed = run_forked(|| {
            install(&[nr::EXIT, nr::EXIT_GROUP, nr::WRITE, nr::RT_SIGRETURN]).unwrap();
            7
        })
        .unwrap();
        assert_eq!(allowed, Exit::Code(7));
        // `getpid` is not allowed: the kernel kills the child with SIGSYS before the call runs.
        let forbidden = run_forked(|| {
            install(&[nr::EXIT, nr::EXIT_GROUP]).unwrap();
            let _ = std::process::id();
            0
        })
        .unwrap();
        assert_eq!(forbidden, Exit::Signal(libc::SIGSYS));
    }
}
