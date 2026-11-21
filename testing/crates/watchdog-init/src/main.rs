//! The watchdog hook of `run-watchdog.sh` (M36c): the guest's first process in the `normal` and
//! `stall` modes. It does what slot-init does for the watchdog, and nothing else: it mounts proc,
//! sysfs and devtmpfs, opens /dev/watchdog (an open arms it, and the descriptor is close-on-exec,
//! as std's `OpenOptions` makes it), sets the timeout to 15 s and reads it back, feeds it with byte
//! 0 (never `V`) while it works, and then hands PID 1 to the userspace stand-in by exec, the way
//! switch_root does. Its last feed is made right before the exec, as slot-init's is (no wait
//! between them). The last feed's time goes to the stand-in, which reports the gap from it to its
//! own first feed (run-watchdog.sh checks that gap against the applied period).
//!
//! It does not run slot-init's boot: the slot is not in the guest. Static musl, so the image needs
//! nothing else. Port of `testing/qemu/watchdog-init.c` (CZ-GUEST).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use lemnos_driver_hw_watchdog_sys::{get_timeout, set_timeout};
use lemnos_linux_sys::guest;
use lemnos_linux_sys::time::monotonic_now;

const DOG_SECONDS: u32 = 15;
const FEEDS: u32 = 4;

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The stand-in's mode, from the command line: `lemnos.watchdog=<mode>` (a whole word) or the
/// default `normal`.
fn mode() -> &'static str {
    let line = fs::read("/proc/cmdline").unwrap_or_default();
    let line = &line[..line.len().min(511)];
    let line = String::from_utf8_lossy(line);
    for mode in [
        "stall",
        "daemon-late",
        "daemon-root",
        "daemon-hang",
        "daemon-stop",
        "daemon-loop",
        "daemon",
    ] {
        if line
            .split_whitespace()
            .any(|w| w == format!("lemnos.watchdog={mode}"))
        {
            return mode;
        }
    }
    "normal"
}

/// One feed: a single zero byte. Says so when the write fails.
fn feed(dog: &mut File) {
    match dog.write(&[0]) {
        Ok(1) => {}
        Ok(_) => say("hook: a feed failed: short write"),
        Err(e) => say(&format!("hook: a feed failed: {e}")),
    }
}

fn main() -> ExitCode {
    let _ = guest::mount("proc", "/proc", "proc");
    let _ = guest::mount("sysfs", "/sys", "sysfs");
    let _ = guest::mount("devtmpfs", "/dev", "devtmpfs");
    // std opens with O_CLOEXEC, so the descriptor is closed by the exec below.
    let mut dog = match OpenOptions::new().write(true).open("/dev/watchdog") {
        Ok(dog) => dog,
        Err(e) => {
            say(&format!("hook: NO WATCHDOG: {e}"));
            return ExitCode::from(1);
        }
    };
    let applied = set_timeout(dog.as_fd(), DOG_SECONDS).and_then(|_| get_timeout(dog.as_fd()));
    let applied = match applied {
        Ok(applied) => applied,
        Err(e) => {
            say(&format!("hook: the timeout can't be set: {e}"));
            return ExitCode::from(1);
        }
    };
    say(&format!("hook: watchdog armed: {applied} s"));
    for _ in 0..FEEDS {
        feed(&mut dog);
        thread::sleep(Duration::from_secs(1));
    }
    // The last feed, right before the exec: nothing waits between it and the hand-over.
    feed(&mut dog);
    let last = monotonic_now().map_or(0.0, |d| d.as_secs_f64());
    let when = format!("{last:.6}");
    say(&format!(
        "hook: last feed at {when}, handing over to /sbin/watchdog-user"
    ));
    let e = Command::new("/sbin/watchdog-user")
        .arg0("watchdog-user")
        .arg(mode())
        .arg(&when)
        .exec();
    say(&format!("hook: exec failed: {e}"));
    ExitCode::from(1)
}
