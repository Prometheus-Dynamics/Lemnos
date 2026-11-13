//! A doorbell wait that a signal interrupts keeps its deadline: the signal does not stretch the
//! wait to the full timeout again. A separate test binary, so its signal handler is the only one.

#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use shm_ring_doorbell::{Doorbell, Wake};

const SIGNAL: libc::c_int = libc::SIGUSR1;

extern "C" fn nothing(_signum: libc::c_int) {}

/// Installs a handler that does nothing and does not restart system calls, so a `poll` on this
/// process returns `EINTR` when the signal reaches its thread.
fn catch_without_restart() {
    // SAFETY: an all-zero `sigaction` is a valid value of the type; its fields are set below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = nothing as *const () as libc::sighandler_t;
    action.sa_flags = 0;
    // SAFETY: `action.sa_mask` is a live, writable `sigset_t`.
    let emptied = unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
    assert_eq!(emptied, 0, "sigemptyset");
    // SAFETY: `action` is initialized and the old action is not asked for.
    let set = unsafe { libc::sigaction(SIGNAL, &raw const action, std::ptr::null_mut()) };
    assert_eq!(set, 0, "sigaction");
}

/// Sends `SIGNAL` to the calling thread after `delay`, from a helper thread. The caller joins
/// the helper before it returns.
fn interrupt_this_thread_after(delay: Duration) -> std::thread::JoinHandle<()> {
    // SAFETY: `pthread_self` has no preconditions. The id travels as an integer: `pthread_t` is
    // a pointer on some targets, which is not `Send`.
    let me = unsafe { libc::pthread_self() } as usize;
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        // SAFETY: `me` is the thread that asked, and it is still waiting: the test joins this
        // helper only after the wait returns.
        unsafe { libc::pthread_kill(me as libc::pthread_t, SIGNAL) };
    })
}

#[test]
fn a_signal_does_not_stretch_a_doorbell_wait() {
    // A 1.5 s wait with a signal at 0.5 s returns at 1.5 s. Restarting the full timeout on the
    // signal would return at 2.0 s, so the bounds sit between the two.
    let mut doorbell = Doorbell::create().expect("doorbell");
    catch_without_restart();
    let signal = interrupt_this_thread_after(Duration::from_millis(500));
    let start = Instant::now();
    let woke = doorbell
        .wait(Some(Duration::from_millis(1500)), || false)
        .expect("wait");
    let took = start.elapsed();
    signal.join().expect("signal thread");
    assert_eq!(woke, Wake::TimedOut, "nothing was rung");
    assert!(
        took >= Duration::from_millis(1500) && took < Duration::from_millis(1750),
        "the wait took {took:?}; a restarted timeout would take about 2 s"
    );
}
