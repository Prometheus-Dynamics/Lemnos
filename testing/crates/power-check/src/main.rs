//! A block's clocks, supplies and resets, and the event stream: bind and unbind, a supply fault,
//! a temperature limit crossed both ways.
//!
//! Usage: `lemnos-power-check /dev/lemnos <device name> <bind path> <unbind path>`. Port of
//! `kernel/test/lemnos_power_check.c` (CZ-GUEST-2). Requests are laid out in bytes as the C check
//! lays out its structures (`struct lemnos_power`, `lemnos_thermal`, `lemnos_thermal_watch`), so a
//! refused value reaches the kernel as given.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::time::Duration;

use lemnos_linux_sys::guest;
use lemnos_linux_sys::iowr;
use lemnos_linux_sys::module;
use lemnos_linux_sys::poll::poll_one;

const EINVAL: i32 = libc::EINVAL;

/// `REGULATOR_EVENT_OVER_CURRENT` from the kernel's regulator consumer header.
const REGULATOR_EVENT_OVER_CURRENT: i64 = 0x02;
const PARAM: &str = "/sys/module/lemnos_test_bus/parameters/";

// lemnos.h: power kinds and operations.
const CLOCK: u32 = 0;
const SUPPLY: u32 = 1;
const RESET: u32 = 2;
const ENABLE: u32 = 0;
const DISABLE: u32 = 1;
const STATUS: u32 = 2;
const GET: u32 = 3;
const SET: u32 = 4;
const PULSE: u32 = 5;

// lemnos.h: event kinds.
const EVENT_BLOCK_BOUND: u32 = 1;
const EVENT_BLOCK_UNBOUND: u32 = 2;
const EVENT_SUPPLY_FAULT: u32 = 3;
const EVENT_TEMPERATURE: u32 = 4;

const POWER: u32 = iowr::<[u8; 64]>(b'L', 0xd6);
const THERMAL_READ: u32 = iowr::<[u8; 72]>(b'L', 0xa0);
const THERMAL_WATCH: u32 = iowr::<[u8; 80]>(b'L', 0xa1);

const NAME_MAX: usize = 64;
const POWER_NAME_MAX: usize = 32;

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Prints one check's line and counts a failure.
fn check(failures: &mut u32, ok: bool, what: &str) {
    say(&format!("{} {what}", if ok { "ok   " } else { "FAIL " }));
    *failures += u32::from(!ok);
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_ne_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_ne_bytes());
}

fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn get64(b: &[u8], at: usize) -> u64 {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[at..at + 8]);
    u64::from_ne_bytes(w)
}

/// `strncpy` into a name field of `room` bytes: at most `room - 1` bytes, the rest zero.
fn put_name(b: &mut [u8], at: usize, name: &str, room: usize) {
    for (i, byte) in name.bytes().take(room - 1).enumerate() {
        b[at + i] = byte;
    }
}

/// One event, as `struct lemnos_event` (32 bytes) lays it out.
#[derive(Clone, Copy)]
struct Event {
    block: u32,
    kind: u32,
    value: i64,
}

impl Event {
    fn parse(b: &[u8; 32]) -> Self {
        Self {
            block: get32(b, 0),
            kind: get32(b, 12),
            value: get64(b, 24) as i64,
        }
    }
}

struct Checker {
    node: OwnedFd,
    block: u32,
}

impl Checker {
    /// One request: 0, or -errno, as the C `ioctl(...) ? -errno : 0` reads it.
    fn call<const N: usize>(&self, request: u32, arg: &mut [u8; N]) -> i32 {
        match guest::ioctl_bytes(self.node.as_fd(), request, arg) {
            Ok(()) => 0,
            Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
        }
    }

    /// `power()` of the C check: 0 or -errno; `value` in and out when given.
    fn power(&self, kind: u32, op: u32, name: &str, value: Option<&mut u64>, value2: u64) -> i32 {
        let mut a = [0u8; 64];
        put32(&mut a, 0, self.block);
        put32(&mut a, 4, kind);
        put32(&mut a, 8, op);
        put_name(&mut a, 16, name, POWER_NAME_MAX);
        if let Some(v) = value.as_deref() {
            put64(&mut a, 48, *v);
        }
        put64(&mut a, 56, value2);
        let r = self.call(POWER, &mut a);
        if let Some(v) = value {
            *v = get64(&a, 48);
        }
        r
    }

    /// The next event of `kind`, skipping others, within a second (16 reads at most).
    fn next_event(&self, kind: u32) -> Option<Event> {
        for _ in 0..16 {
            let ready = poll_one(
                self.node.as_fd(),
                libc::POLLIN,
                Some(Duration::from_millis(1000)),
            )
            .is_ok_and(|r| r != 0);
            if !ready {
                return None;
            }
            let mut buf = [0u8; 32];
            match guest::read(self.node.as_fd(), &mut buf) {
                Ok(32) => {}
                _ => return None,
            }
            let e = Event::parse(&buf);
            if e.kind == kind {
                return Some(e);
            }
        }
        None
    }

    fn thermal_read(&self, zone: &str, out: &mut i64) -> i32 {
        let mut a = [0u8; 72];
        put_name(&mut a, 0, zone, NAME_MAX);
        let r = self.call(THERMAL_READ, &mut a);
        *out = get64(&a, 64) as i64;
        r
    }

    fn thermal_watch(&self, zone: &str, low: i32, high: i32, period: u32) -> i32 {
        let mut a = [0u8; 80];
        put_name(&mut a, 0, zone, NAME_MAX);
        put32(&mut a, 64, low as u32);
        put32(&mut a, 68, high as u32);
        put32(&mut a, 72, period);
        self.call(THERMAL_WATCH, &mut a)
    }
}

/// Writes `text` to a sysfs-like file: 0, or -1 (the C `put`).
fn put(path: &str, text: &str) -> i32 {
    let Ok(mut f) = OpenOptions::new().write(true).open(path) else {
        return -1;
    };
    match io::Write::write(&mut f, text.as_bytes()) {
        Ok(n) if n > 0 => 0,
        _ => -1,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        let _ = writeln!(
            io::stderr(),
            "usage: lemnos_power_check /dev/lemnos <device> <bind> <unbind>"
        );
        return ExitCode::from(2);
    }
    let name = args[2].as_str();
    let node = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&args[1])
    {
        Ok(f) => OwnedFd::from(f),
        Err(e) => {
            let _ = writeln!(io::stderr(), "{}: {e}", args[1]);
            return ExitCode::from(2);
        }
    };
    let mut c = Checker { node, block: 0 };
    let mut failures = 0;

    // The block was unbound by the previous check: binding is the first event.
    let path = format!("/sys/bus/platform/devices/{name}/driver_override");
    check(
        &mut failures,
        put(&path, "lemnos") == 0 && put(&args[3], name) == 0,
        "block bound",
    );
    let bound = c.next_event(EVENT_BLOCK_BOUND);
    check(&mut failures, bound.is_some(), "event: block bound");
    c.block = module::find_block(c.node.as_fd(), name)
        .ok()
        .flatten()
        .unwrap_or(0);
    let block = c.block;
    check(
        &mut failures,
        block != 0 && bound.is_some_and(|e| e.block == block),
        &format!("for block {block}"),
    );

    // Clock.
    let mut v: u64 = 0;
    check(
        &mut failures,
        c.power(CLOCK, GET, "core", Some(&mut v), 0) == 0 && v == 25_000_000,
        &format!("clock rate: {v} Hz"),
    );
    check(
        &mut failures,
        c.power(CLOCK, ENABLE, "core", None, 0) == 0,
        "clock on",
    );
    check(
        &mut failures,
        c.power(CLOCK, ENABLE, "core", None, 0) == 0,
        "clock on again",
    );
    check(
        &mut failures,
        c.power(CLOCK, STATUS, "core", Some(&mut v), 0) == 0 && v == 1,
        "status on",
    );
    check(
        &mut failures,
        c.power(CLOCK, DISABLE, "core", None, 0) == 0,
        "clock off",
    );
    check(
        &mut failures,
        c.power(CLOCK, STATUS, "core", Some(&mut v), 0) == 0 && v == 0,
        "status off",
    );
    check(
        &mut failures,
        c.power(CLOCK, GET, "nope", Some(&mut v), 0) < 0,
        "an unknown clock is refused",
    );

    // Supply.
    check(
        &mut failures,
        c.power(SUPPLY, ENABLE, "vdd", None, 0) == 0,
        "supply on",
    );
    check(
        &mut failures,
        c.power(SUPPLY, STATUS, "vdd", Some(&mut v), 0) == 0 && v == 1,
        "supply status on",
    );
    v = 1_800_000;
    check(
        &mut failures,
        c.power(SUPPLY, SET, "vdd", Some(&mut v), 1_800_000) == 0 && v == 1_800_000,
        &format!("set 1.8 V: {v} uV"),
    );
    check(
        &mut failures,
        c.power(SUPPLY, GET, "vdd", Some(&mut v), 0) == 0 && v == 1_800_000,
        "reads 1.8 V",
    );
    v = 2_000_000;
    check(
        &mut failures,
        c.power(SUPPLY, SET, "vdd", Some(&mut v), 1_000_000) == -EINVAL,
        "minimum past maximum: EINVAL",
    );
    check(
        &mut failures,
        c.power(SUPPLY, GET, "nope", Some(&mut v), 0) < 0,
        "an unknown supply is refused",
    );
    check(
        &mut failures,
        c.power(SUPPLY, GET, "", Some(&mut v), 0) == -EINVAL,
        "a supply needs a name",
    );

    // A fault on the supply.
    check(
        &mut failures,
        put(&format!("{PARAM}fault"), "1") == 0,
        "the supply reports an over-current",
    );
    let fault = c.next_event(EVENT_SUPPLY_FAULT);
    check(
        &mut failures,
        fault.is_some_and(|e| e.block == block && (e.value & REGULATOR_EVENT_OVER_CURRENT) != 0),
        &format!(
            "event: supply fault, value 0x{:x}",
            fault.map_or(0, |e| e.value as u64)
        ),
    );

    // Reset.
    let r = c.power(RESET, DISABLE, "rst", None, 0);
    check(
        &mut failures,
        r == 0,
        &if r == 0 {
            "reset asserted (ok)".to_string()
        } else {
            format!("reset asserted ({})", io::Error::from_raw_os_error(-r))
        },
    );
    check(
        &mut failures,
        c.power(RESET, STATUS, "rst", Some(&mut v), 0) == 0 && v == 1,
        "status asserted",
    );
    check(
        &mut failures,
        c.power(RESET, ENABLE, "rst", None, 0) == 0,
        "reset released",
    );
    check(
        &mut failures,
        c.power(RESET, STATUS, "rst", Some(&mut v), 0) == 0 && v == 0,
        "status released",
    );
    check(
        &mut failures,
        c.power(RESET, PULSE, "rst", None, 0) == 0,
        "reset pulsed",
    );
    check(
        &mut failures,
        c.power(RESET, 99, "rst", None, 0) == -EINVAL,
        "an unknown op: EINVAL",
    );
    check(
        &mut failures,
        c.power(99, GET, "x", None, 0) == -EINVAL,
        "an unknown kind: EINVAL",
    );

    // Temperature.
    let mut t: i64 = 0;
    check(
        &mut failures,
        c.thermal_read("lemnos-test", &mut t) == 0 && t == 40_000,
        &format!("zone reads {t} mC"),
    );
    check(
        &mut failures,
        c.thermal_watch("lemnos-test", 45_000, 60_000, 10) == 0,
        "watching 45 to 60 degrees",
    );
    check(
        &mut failures,
        put(&format!("{PARAM}temp_mc"), "70000") == 0,
        "the zone heats up",
    );
    let high = c.next_event(EVENT_TEMPERATURE);
    check(
        &mut failures,
        high.is_some_and(|e| e.value == 70_000),
        &format!("event: high, {} mC", high.map_or(0, |e| e.value)),
    );
    check(
        &mut failures,
        put(&format!("{PARAM}temp_mc"), "30000") == 0,
        "the zone cools",
    );
    let low = c.next_event(EVENT_TEMPERATURE);
    check(
        &mut failures,
        low.is_some_and(|e| e.value == 30_000),
        &format!("event: low, {} mC", low.map_or(0, |e| e.value)),
    );
    check(
        &mut failures,
        c.thermal_watch("lemnos-test", 2, 1, 100) == -EINVAL,
        "low past high: EINVAL",
    );
    check(
        &mut failures,
        c.thermal_read("no-such-zone", &mut t) < 0,
        "an unknown zone is refused",
    );

    // Unbinding with the supply on: turned off, and the event arrives.
    check(
        &mut failures,
        put(&args[4], name) == 0,
        "unbound with the supply still on",
    );
    let unbound = c.next_event(EVENT_BLOCK_UNBOUND);
    check(
        &mut failures,
        unbound.is_some_and(|e| e.block == block),
        "event: block unbound",
    );
    check(&mut failures, put(&path, "\n") == 0, "override cleared");

    say(&format!("{}: {failures} failed", args[1]));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
