//! What a bench needs from the scheduler, and the proof that a path makes no system call.

/// How a body run by [`without_system_calls`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemCalls {
    /// The body ran to the end and returned `true`: it made no system call.
    None,
    /// The body made a system call and was killed for it.
    Made,
    /// The body returned `false`, or the filter could not be installed.
    Failed,
}

/// Runs `body` in a child process that is killed on any system call, and says how it ended.
///
/// The child is a `fork` of this process with a seccomp filter that allows only `exit`. It
/// shares nothing writable with the parent except shared mappings, so `body` should work on
/// those: a ring, a doorbell. `body` must not allocate, lock or print: in a forked child of a
/// threaded process those can hang, and all of them are system calls anyway.
pub fn without_system_calls(body: impl FnOnce() -> bool) -> SystemCalls {
    let allow_exit_only = [
        // Load the system call number.
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_exit as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_KILL_PROCESS,
        },
    ];
    let program = libc::sock_fprog {
        len: allow_exit_only.len() as u16,
        filter: allow_exit_only.as_ptr().cast_mut(),
    };
    // SAFETY: `fork` has no preconditions. The child below only makes raw system calls and runs
    // `body` under the contract above, never returning into the caller's frames.
    let child = unsafe { libc::fork() };
    if child < 0 {
        return SystemCalls::Failed;
    }
    if child == 0 {
        // SAFETY: a plain `prctl` with integer arguments.
        let no_new_privs = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        // SAFETY: `program` and the filter it points at outlive the call, which only reads them.
        let filtered = no_new_privs == 0
            && unsafe {
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &raw const program,
                )
            } == 0;
        let code = if !filtered {
            2
        } else if body() {
            0
        } else {
            1
        };
        // SAFETY: a raw `exit` of this thread, the child's only one; the only system call the
        // filter allows. It does not return.
        unsafe { libc::syscall(libc::SYS_exit, code) };
        // SAFETY: not reached; `abort` is a backstop that never returns either.
        unsafe { libc::abort() };
    }
    let mut status = 0;
    // SAFETY: `status` is a valid place for the kernel to write one int.
    if unsafe { libc::waitpid(child, &raw mut status, 0) } != child {
        return SystemCalls::Failed;
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        SystemCalls::None
    } else if libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGSYS {
        SystemCalls::Made
    } else {
        SystemCalls::Failed
    }
}

/// Pins the calling thread to `cpu`. `false` if that CPU can't be used.
pub fn pin_current_thread(cpu: usize) -> bool {
    if cpu >= libc::CPU_SETSIZE as usize {
        return false;
    }
    // SAFETY: plain old data; all zeroes is an empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `cpu` is below `CPU_SETSIZE`, so the bit is inside the set.
    unsafe { libc::CPU_SET(cpu, &mut set) };
    // SAFETY: the call reads `size_of::<cpu_set_t>()` bytes of `set`, which is that size.
    unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &raw const set) == 0 }
}

/// How long the calling thread has run on a CPU, in nanoseconds.
#[must_use]
pub fn thread_cpu_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid place to write one `timespec`.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut time) } != 0 {
        return 0;
    }
    (time.tv_sec as u64).saturating_mul(1_000_000_000) + time.tv_nsec as u64
}
