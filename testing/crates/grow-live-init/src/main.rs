#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The GROW2 guest's first process (test only, never shipped): the QEMU check's init, in Rust.
//!
//! It mounts proc, sysfs and devtmpfs, waits for the disk's partition 2 (the data partition), mounts it
//! at `/data` (ext4, `errors=remount-ro`), and runs `/lemnos-grow-host` with the watchdog timeout and
//! the run's limit from the kernel command line (`lemnos.wd=<s> lemnos.max=<s>`). It then unmounts
//! `/data` and powers off. A reset from the host (the power-cut case) or a watchdog reset starts this
//! again from the top.
//!
//! The mounts and the unmount go through `lemnos-linux-sys`, the one crate that holds the system calls;
//! the power-off is the kernel's sysrq trigger, which needs no call of its own.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use lemnos_linux_sys::rootfs::{self, MountFlags};

/// The value after `key` on the command line, up to the next space.
fn cmdline_value(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    line[at..].split([' ', '\n']).next().map(str::to_string)
}

fn say(text: &str) {
    println!("grow-init: {text}");
}

/// Writes what is cached, then powers the machine off through the sysrq trigger.
fn power_off() -> ! {
    rootfs::sync();
    say("powering off");
    // `1` enables every sysrq function; `o` is power off. Both are kernel-side and need no call here.
    let _ = fs::write("/proc/sys/kernel/sysrq", "1");
    let _ = fs::write("/proc/sysrq-trigger", "o");
    loop {
        sleep(Duration::from_secs(3600));
    }
}

fn main() {
    for dir in ["/proc", "/sys", "/dev", "/data"] {
        let _ = fs::create_dir_all(dir);
    }
    let flags = MountFlags::default();
    for (source, target, fstype) in [
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
        ("devtmpfs", "/dev", "devtmpfs"),
    ] {
        let _ = rootfs::mount(Path::new(source), Path::new(target), fstype, flags, "");
    }
    say("start");
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let wd = cmdline_value(&cmdline, "lemnos.wd=").unwrap_or_else(|| "15".into());
    let max = cmdline_value(&cmdline, "lemnos.max=").unwrap_or_else(|| "120".into());
    for _ in 0..300 {
        if Path::new("/dev/nvme0n1p2").exists() {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    if let Err(error) = rootfs::mount(
        Path::new("/dev/nvme0n1p2"),
        Path::new("/data"),
        "ext4",
        flags,
        "errors=remount-ro",
    ) {
        println!("grow-init: mount /data failed: {error}");
        power_off();
    }
    say("/data mounted");
    match Command::new("/lemnos-grow-host")
        .args(["/dev/nvme0n1", "/data", &wd, &max])
        .status()
    {
        Ok(status) => println!("grow-init: harness exited {}", status.code().unwrap_or(-1)),
        Err(error) => println!("grow-init: harness did not start: {error}"),
    }
    rootfs::sync();
    match rootfs::umount(Path::new("/data")) {
        Ok(()) => say("/data unmounted"),
        Err(error) => println!("grow-init: umount /data failed: {error}"),
    }
    power_off();
}
