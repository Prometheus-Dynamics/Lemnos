//! Stops one thread of another process and lets it go again, for the QEMU harness: a driver's call
//! that runs past its deadline because the thread running it stood still. Only that thread stops
//! (`PTRACE_SEIZE` and `PTRACE_INTERRUPT` on its id); the rest of the process runs on. Test support
//! only.

use std::io;

/// A thread this process holds stopped. Let go by [`Held::release`] or on drop, from the thread that
/// took it: the kernel takes a tracee's commands from its tracer thread only.
pub struct Held {
    tid: libc::pid_t,
}

/// Stops thread `tid` and waits until it stands still.
pub fn hold(tid: libc::pid_t) -> io::Result<Held> {
    let null = std::ptr::null_mut::<libc::c_void>();
    // SAFETY: PTRACE_SEIZE takes a thread id and two unused pointer-sized words; no memory is read.
    let seized = unsafe { libc::ptrace(libc::PTRACE_SEIZE, tid, null, null) };
    if seized != 0 {
        return Err(io::Error::last_os_error());
    }
    let held = Held { tid };
    // SAFETY: PTRACE_INTERRUPT on the thread seized above; the two words are unused.
    let interrupted = unsafe { libc::ptrace(libc::PTRACE_INTERRUPT, tid, null, null) };
    if interrupted != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waitpid writes one int through the pointer to a live local.
        let waited = unsafe { libc::waitpid(tid, &mut status, libc::__WALL) };
        if waited == tid {
            return Ok(held);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

impl Held {
    /// Lets the thread run again.
    pub fn release(mut self) -> io::Result<()> {
        let tid = std::mem::replace(&mut self.tid, 0);
        detach(tid)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.tid != 0 {
            let _ = detach(self.tid);
        }
    }
}

fn detach(tid: libc::pid_t) -> io::Result<()> {
    let null = std::ptr::null_mut::<libc::c_void>();
    // SAFETY: PTRACE_DETACH on a stopped tracee of this thread; the address word is unused and the
    // data word (no signal) is zero.
    let detached = unsafe { libc::ptrace(libc::PTRACE_DETACH, tid, null, null) };
    if detached != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// A child shell prints a line every 10 ms: held, it prints nothing; released, it prints
    /// again. (A thread can't trace its own thread group, so the target is another process.)
    #[test]
    fn a_held_thread_stands_still_and_a_released_one_runs_on() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "while :; do echo x; sleep 0.01; done"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tid = libc::pid_t::try_from(child.id()).unwrap();
        let out = child.stdout.take().unwrap();
        let seen = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&seen);
        std::thread::spawn(move || {
            use std::io::BufRead;
            for _ in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        let started = std::time::Instant::now();
        while seen.load(Ordering::Relaxed) < 3 && started.elapsed() < Duration::from_secs(10) {
            std::thread::yield_now();
        }
        let held = hold(tid).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let before = seen.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            seen.load(Ordering::Relaxed),
            before,
            "a held thread printed"
        );
        held.release().unwrap();
        let after = std::time::Instant::now();
        while seen.load(Ordering::Relaxed) <= before && after.elapsed() < Duration::from_secs(10) {
            std::thread::yield_now();
        }
        assert!(
            seen.load(Ordering::Relaxed) > before,
            "a released thread stayed still"
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}
