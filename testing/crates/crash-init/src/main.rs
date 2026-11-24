//! The crash-log hook of `run-crash.sh` (BB3): the guest's first process. It mounts proc, sysfs
//! and devtmpfs, mounts pstore at /sys/fs/pstore (a systemd image does that itself; this guest has
//! no systemd, so the hook stands in for it), and mounts the state disk (ext4, the whole disk) at
//! /state, which keeps the boot history from one reset to the next. Then it runs the probe, which
//! is the driver's own crash-log record step.
//!
//! Modes, from the command line:
//!   default      the crash run. The first boot (no /state/armed) arms the marker, runs the probe
//!                (its history entry: the boot before the crash), syncs and writes 'c' to
//!                /proc/sysrq-trigger. The panic reboots the guest (panic=1) and QEMU keeps the
//!                RAM, so the next boot finds the record. That boot runs the probe, which stores
//!                the record and clears pstore, and powers off.
//!   lemnos.plain one probe and power off, with no crash.
//!   lemnos.nomount pstore is not mounted (the "not mounted" case; with lemnos.plain).
//!   lemnos.undeclared the probe runs with no pstore (the board declares no crash log).
//!
//! Port of `testing/qemu/crash-init.c` (CZ-GUEST). Static musl, so the initramfs needs nothing
//! else. It is not the watchdog hook (`watchdog-init`).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use lemnos_linux_sys::guest;

const PROBE: &str = "/sbin/crash-probe";
const STATE_DISK: &str = "/dev/nvme0n1";

fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "crash-init: {text}");
    let _ = out.flush();
}

/// Whether `word` is in the first 1023 bytes of the command line, as the C original reads it.
fn cmdline_has(word: &str) -> bool {
    let line = fs::read("/proc/cmdline").unwrap_or_default();
    let line = &line[..line.len().min(1023)];
    String::from_utf8_lossy(line).contains(word)
}

/// Runs the probe; its output goes to the console. Reports its exit status.
fn probe(declared: bool) {
    let mut args = vec!["/state/data", "/state/boot", "/sys/fs/pstore"];
    if !declared {
        args[2] = "--no-pstore";
    }
    // A probe that cannot be started is reported as exit status 127, as the C child reports it.
    let code = match Command::new(PROBE).args(&args).status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => 127,
    };
    say(&format!("probe exited {code}"));
}

fn power_off() -> ! {
    let _ = guest::power_off();
    loop {
        thread::park();
    }
}

fn main() {
    let _ = guest::mount("proc", "/proc", "proc");
    let _ = guest::mount("sysfs", "/sys", "sysfs");
    let _ = guest::mount("devtmpfs", "/dev", "devtmpfs");
    // The command line is read from /proc, so the flags are read after it is mounted.
    let declared = !cmdline_has("lemnos.undeclared");
    let plain = cmdline_has("lemnos.plain");
    if !cmdline_has("lemnos.nomount") {
        let _ = fs::create_dir_all("/sys/fs/pstore");
        if let Err(e) = guest::mount("pstore", "/sys/fs/pstore", "pstore") {
            say(&format!("mount pstore: {e}"));
        }
    }
    // The state disk: the device appears once the driver is up, so wait for it.
    let until = Instant::now() + Duration::from_secs(20);
    while !Path::new(STATE_DISK).exists() && Instant::now() < until {
        thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::create_dir("/state");
    if let Err(e) = guest::mount(STATE_DISK, "/state", "ext4") {
        say(&format!("mount state: {e}"));
        power_off();
    }
    let _ = fs::create_dir("/state/data");
    let _ = fs::create_dir("/state/boot");

    if plain {
        probe(declared);
        power_off();
    }
    if !Path::new("/state/armed").exists() {
        let Ok(mut armed) = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o644)
            .open("/state/armed")
        else {
            say("cannot arm the crash");
            power_off();
        };
        let _ = armed.write_all(b"1\n");
        let _ = armed.sync_all();
        drop(armed);
        say("arming the crash: the first boot");
        probe(declared);
        guest::sync();
        say("crashing the guest (sysrq-c)");
        if let Ok(mut trigger) = OpenOptions::new().write(true).open("/proc/sysrq-trigger") {
            let _ = trigger.write_all(b"c");
        }
        // Not reached: the panic reboots the guest.
        loop {
            thread::park();
        }
    }
    say("the boot after the crash");
    probe(declared);
    say("done");
    power_off();
}
