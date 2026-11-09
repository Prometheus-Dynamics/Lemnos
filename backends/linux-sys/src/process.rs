//! Starting a program and waiting for it (`posix_spawnp`, `waitpid`): what a
//! small command-line tool needs, at a fraction of the size of std's
//! `Command` (about 40 KB less in `lemnos-ctl`). The program inherits the
//! environment, the working directory and the standard streams; descriptors
//! opened with `O_CLOEXEC` (all of std's) are not passed on.

use std::ffi::{CString, OsStr, c_char};
use std::io;
use std::os::unix::ffi::OsStrExt;

unsafe extern "C" {
    /// The process environment (POSIX).
    static environ: *const *mut c_char;
}

/// How a program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It exited with this code.
    Code(i32),
    /// A signal ended it.
    Signal(i32),
}

impl Exit {
    /// Exited with code 0.
    pub fn success(self) -> bool {
        self == Self::Code(0)
    }
}

/// A started program. Wait for it ([`wait`](Self::wait) or
/// [`try_wait`](Self::try_wait)), or it stays a zombie until this process
/// exits.
#[derive(Debug)]
pub struct Child {
    pid: libc::pid_t,
    exit: Option<Exit>,
}

/// Starts `argv[0]`, looked up in `PATH` when it has no `/`, with `argv` as
/// its arguments.
pub fn spawn<S: AsRef<OsStr>>(argv: &[S]) -> io::Result<Child> {
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no program"));
    }
    let args = argv
        .iter()
        .map(|a| CString::new(a.as_ref().as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "an argument holds a NUL"))?;
    let mut ptrs: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr().cast_mut()).collect();
    ptrs.push(std::ptr::null_mut());
    let mut pid: libc::pid_t = 0;
    // SAFETY: `args` (the strings `ptrs` points into) outlives the call, `ptrs`
    // is null-terminated, null file actions and attributes are allowed, and
    // `environ` is the process's null-terminated environment.
    let r = unsafe {
        libc::posix_spawnp(
            &raw mut pid,
            ptrs[0],
            std::ptr::null(),
            std::ptr::null(),
            ptrs.as_ptr(),
            environ,
        )
    };
    if r != 0 {
        return Err(io::Error::from_raw_os_error(r));
    }
    Ok(Child { pid, exit: None })
}

fn decode(status: libc::c_int) -> Exit {
    if libc::WIFEXITED(status) {
        Exit::Code(libc::WEXITSTATUS(status))
    } else {
        Exit::Signal(libc::WTERMSIG(status))
    }
}

impl Child {
    /// The process id.
    pub fn id(&self) -> u32 {
        self.pid as u32
    }

    fn reap(&mut self, flags: libc::c_int) -> io::Result<Option<Exit>> {
        if let Some(exit) = self.exit {
            return Ok(Some(exit));
        }
        loop {
            let mut status: libc::c_int = 0;
            // SAFETY: `status` is a valid out-pointer; `pid` is this child's.
            let r = unsafe { libc::waitpid(self.pid, &raw mut status, flags) };
            if r == self.pid {
                self.exit = Some(decode(status));
                return Ok(self.exit);
            }
            if r == 0 {
                return Ok(None);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    /// How it ended, if it has.
    pub fn try_wait(&mut self) -> io::Result<Option<Exit>> {
        self.reap(libc::WNOHANG)
    }

    /// Waits for it to end.
    pub fn wait(&mut self) -> io::Result<Exit> {
        self.reap(0)?
            .ok_or_else(|| io::Error::other("waitpid returned without a status"))
    }
}
