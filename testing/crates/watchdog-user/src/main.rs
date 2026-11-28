//! The userspace stand-in of `run-watchdog.sh` (M36c): what PID 1 becomes after the hook's exec,
//! in place of a slot's lemnosd. Arguments: the mode (`normal` or `stall`) and the time of the
//! hook's last feed, on CLOCK_MONOTONIC.
//!
//!   normal  opens /dev/watchdog (its first open after the handover), reports the gap from the
//!           last feed to its first open and to its first feed, feeds once a second for 45 s, then
//!           powers off. The guest staying up that long is the check; the first-feed gap is checked
//!           against the bound.
//!   daemon, daemon-late, daemon-root, daemon-hang, daemon-stop, daemon-loop: the real lemnosd under its unit (see `daemon.rs`).
//!   stall   never opens /dev/watchdog. Nothing feeds the device after the hook's last feed, so the
//!           watchdog resets the guest within its timeout. The stall is reported before it starts.
//!
//! It stands in for lemnosd only: lemnosd is not in this guest. Static musl, so the image needs
//! nothing else. Port of `testing/qemu/watchdog-user.c` (CZ-GUEST).

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::guest;

mod daemon;
use lemnos_linux_sys::time::monotonic_now;

const STAY_SECONDS: u32 = 45;

pub(crate) fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// [`say`] for the last line before the guest ends (PID 1 exits, or the power goes off): the line is
/// written, then the console is drained (`tcdrain`, TCSBRK with a non-zero argument) before the caller
/// goes on. Without it the kernel's own lines at the end (the exit's stack-depth note, "Attempted to
/// kill init", the power-off) go out synchronously while the rest of this line still waits in the
/// tty, and the guest ends before it is sent: the host reads a cut line, or none. A stdout that is not
/// a terminal has nothing to drain.
pub(crate) fn say_last(line: &str) {
    say(line);
    let out = io::stdout();
    let _ = guest::ioctl_at(out.as_fd(), libc::TCSBRK as u32, 1);
}

/// Seconds on CLOCK_MONOTONIC since the hook's last feed, `last`. A clock that cannot be read
/// counts as zero.
pub(crate) fn since(last: f64) -> f64 {
    monotonic_now().map_or(0.0, |d| d.as_secs_f64()) - last
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map_or("normal", String::as_str);
    // strtod in the C original: a value that does not parse is zero.
    let last = args
        .get(2)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    if mode.starts_with("daemon") || mode.starts_with("pid1-") {
        daemon::run(mode, last);
    }
    if mode == "stall" {
        say(&format!(
            "user: stalled after the handover: /dev/watchdog is not opened; the last feed was {:.3} s ago",
            since(last)
        ));
        loop {
            thread::park();
        }
    }
    let Ok(dog) = OpenOptions::new().write(true).open("/dev/watchdog") else {
        say(&format!(
            "user: the first open of /dev/watchdog failed: {}",
            io::Error::last_os_error()
        ));
        thread::sleep(Duration::from_secs(60));
        return ExitCode::from(1);
    };
    say(&format!(
        "user: first open of /dev/watchdog {:.3} s after the hook's last feed",
        since(last)
    ));
    for s in 0..STAY_SECONDS {
        match (&dog).write(&[0]) {
            Ok(1) => {}
            Ok(_) => say("user: a feed failed: short write"),
            Err(e) => say(&format!("user: a feed failed: {e}")),
        }
        if s == 0 {
            say(&format!(
                "user: first feed of /dev/watchdog {:.3} s after the hook's last feed",
                since(last)
            ));
        }
        thread::sleep(Duration::from_secs(1));
    }
    say_last(&format!(
        "user: fed the watchdog for {STAY_SECONDS} s and the guest stayed up; powering off"
    ));
    let _ = guest::power_off();
    ExitCode::from(1)
}
