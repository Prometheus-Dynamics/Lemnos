//! The guest's first process for a kernel with no loadable modules, lemnos built in. It checks
//! what the kernel command line says and what the built-in module did with it: each parameter
//! reads back what the command line gave it (or its default), the control node exists before any
//! daemon starts, module loading is refused, and the claim checks pass. Prints one result line and
//! powers off.
//!
//! What this proves: the module is built in and initialised at boot, its parameters come from the
//! command line, and the control node and claims work. It does not drive any block: no fake
//! device is in the kernel, so nothing here reaches a bus, a stream or a DMA engine.
//!
//! Port of `testing/qemu/init-builtin.c` (CZ-GUEST). Static musl, so the image needs nothing else.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitCode};

use lemnos_linux_sys::guest;

/// `ENOSYS` on every Linux architecture Lemnos builds for.
const ENOSYS: i32 = libc::ENOSYS;

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Prints one check's line and counts a failure.
fn check(failures: &mut u32, ok: bool, what: &str) {
    say(&format!("{} {what}", if ok { "ok  " } else { "FAIL" }));
    *failures += u32::from(!ok);
}

/// The first line of a parameter file, or "" when it cannot be read.
fn read_parameter(name: &str) -> String {
    fs::read_to_string(format!("/sys/module/lemnos/parameters/{name}"))
        .ok()
        .and_then(|text| text.lines().next().map(str::to_owned))
        .unwrap_or_default()
}

/// The value the command line gave `lemnos.<name>=`, or `fallback` when it gave none.
fn command_line_value(name: &str, fallback: &str) -> String {
    let key = format!("lemnos.{name}=");
    if let Ok(line) = fs::read_to_string("/proc/cmdline") {
        let line = line.lines().next().unwrap_or("");
        // strtok on " " in the C original: empty tokens are skipped.
        for token in line.split(' ').filter(|t| !t.is_empty()) {
            if let Some(value) = token.strip_prefix(&key) {
                return value.to_owned();
            }
        }
    }
    fallback.to_owned()
}

/// Every parameter of the module, with the default it has when the command line gives none.
const PARAMETERS: &[(&str, &str)] = &[
    ("owned", ""),
    ("confined", ""),
    ("unconfined", ""),
    ("pisp_be", ""),
    ("gsdma", ""),
    ("a733_ve", ""),
    ("vip9000", ""),
    ("k230_isp", ""),
    ("bus_buffer", "4096"),
];

fn main() -> ExitCode {
    let _ = guest::mount("devtmpfs", "/dev", "devtmpfs");
    let _ = guest::mount("sysfs", "/sys", "sysfs");
    let _ = guest::mount("proc", "/proc", "proc");
    let _ = guest::dup_console_stdio();
    say("lemnos-builtin: start");

    let mut failures = 0;

    check(
        &mut failures,
        Path::new("/sys/module/lemnos").exists(),
        "lemnos is built into the kernel",
    );
    check(
        &mut failures,
        !Path::new("/lemnos.ko").exists(),
        "no lemnos.ko is in the image",
    );
    // With CONFIG_MODULES off, the kernel has no init_module: it answers ENOSYS.
    check(
        &mut failures,
        guest::init_module_errno() == Some(ENOSYS),
        "the kernel refuses to load a module (no module support)",
    );
    check(
        &mut failures,
        Path::new("/dev/lemnos").exists(),
        "control node exists at boot, before any daemon",
    );

    for &(name, fallback) in PARAMETERS {
        let want = command_line_value(name, fallback);
        let got = read_parameter(name);
        let what = if want.is_empty() {
            format!("{name} takes its default \"{fallback}\"")
        } else {
            format!("{name}={want} from the command line")
        };
        check(&mut failures, got == want, &what);
    }

    let claimed = Command::new("/lemnos_claim_check")
        .arg("/dev/lemnos")
        .status()
        .is_ok_and(|status| status.success());
    check(&mut failures, claimed, "claims pass on the built-in module");

    say(&format!(
        "lemnos-builtin: {} ({failures} failed)",
        if failures > 0 { "FAILED" } else { "passed" }
    ));
    let _ = guest::power_off();
    ExitCode::SUCCESS
}
