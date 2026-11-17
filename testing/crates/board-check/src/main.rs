//! The board's power and thermal check, for a real block, changing as little as it can: the bind
//! and unbind events, the block's clock (its rate and state; turned on and off again only with
//! `toggle`), a thermal zone read beside sysfs, and a watch set around the temperature the zone has
//! now, which must fire at once on the side it is already past.
//!
//! Usage: `lemnos-board-check /dev/lemnos <device> <bind path> <unbind path> <clock or -> <zone>
//! <zone sysfs temp> [toggle]`. The device must be unbound, with the module loaded owning its
//! compatible. Port of `kernel/test/lemnos_board_check.c` (CZ-GUEST-2). It is built so it keeps
//! compiling and runs only on a board (C6); the harness does not run it.

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

const CLOCK: u32 = 0;
const SUPPLY: u32 = 1;
const RESET: u32 = 2;
const ENABLE: u32 = 0;
const DISABLE: u32 = 1;
const STATUS: u32 = 2;
const GET: u32 = 3;
const EVENT_BLOCK_BOUND: u32 = 1;
const EVENT_BLOCK_UNBOUND: u32 = 2;
const EVENT_TEMPERATURE: u32 = 4;
const POWER: u32 = iowr::<[u8; 64]>(b'L', 0xd6);
const THERMAL_READ: u32 = iowr::<[u8; 72]>(b'L', 0xa0);
const THERMAL_WATCH: u32 = iowr::<[u8; 80]>(b'L', 0xa1);
const NAME_MAX: usize = 64;
const POWER_NAME_MAX: usize = 32;

fn check(failures: &mut u32, ok: bool, what: &str) {
    say(&format!("{} {what}", if ok { "ok   " } else { "FAIL " }));
    *failures += u32::from(!ok);
}

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Writes `text` to a sysfs file: 0, or -1.
fn put(path: &str, text: &str) -> i32 {
    let Ok(mut f) = OpenOptions::new().write(true).open(path) else {
        return -1;
    };
    match io::Write::write(&mut f, text.as_bytes()) {
        Ok(n) if n > 0 => 0,
        _ => -1,
    }
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
    index: u32,
    kind: u32,
    value: i64,
}

impl Event {
    fn parse(b: &[u8; 32]) -> Self {
        Self {
            block: get32(b, 0),
            index: get32(b, 4),
            kind: get32(b, 12),
            value: get64(b, 24) as i64,
        }
    }
}

/// `get()` of the C check: a sysfs file's number, or -1 when it cannot be read.
fn get(path: &str) -> i64 {
    let Ok(mut f) = std::fs::File::open(path) else {
        return -1;
    };
    let mut text = [0u8; 32];
    match io::Read::read(&mut f, &mut text[..31]) {
        Ok(n) if n > 0 => atoll(&text),
        _ => -1,
    }
}

/// `atoll`: optional blanks and sign, then digits.
fn atoll(text: &[u8]) -> i64 {
    let mut i = 0;
    while i < text.len() && text[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < text.len() && (text[i] == b'-' || text[i] == b'+') {
        neg = text[i] == b'-';
        i += 1;
    }
    let mut n: i64 = 0;
    while i < text.len() && text[i].is_ascii_digit() {
        n = n.wrapping_mul(10).wrapping_add(i64::from(text[i] - b'0'));
        i += 1;
    }
    if neg { -n } else { n }
}

struct Board {
    node: OwnedFd,
    block: u32,
}

impl Board {
    fn call<const N: usize>(&self, request: u32, arg: &mut [u8; N]) -> i32 {
        match guest::ioctl_bytes(self.node.as_fd(), request, arg) {
            Ok(()) => 0,
            Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
        }
    }

    /// `power()` of the C check: 0 or -errno; `value` in and out when given.
    fn power(&self, kind: u32, op: u32, name: &str, value: Option<&mut u64>) -> i32 {
        let mut a = [0u8; 64];
        put32(&mut a, 0, self.block);
        put32(&mut a, 4, kind);
        put32(&mut a, 8, op);
        put_name(&mut a, 16, name, POWER_NAME_MAX);
        if let Some(v) = value.as_deref() {
            put64(&mut a, 48, *v);
        }
        let r = self.call(POWER, &mut a);
        if let Some(v) = value {
            *v = get64(&a, 48);
        }
        r
    }

    /// The next event of `kind` within `ms` each, skipping others (16 reads at most).
    fn next_event(&self, kind: u32, ms: u64) -> Option<Event> {
        for _ in 0..16 {
            let ready = poll_one(
                self.node.as_fd(),
                libc::POLLIN,
                Some(Duration::from_millis(ms)),
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
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 8 {
        let _ = writeln!(
            io::stderr(),
            "usage: lemnos_board_check /dev/lemnos <device> <bind> <unbind> <clock or -> <zone> <zone temp file> [toggle]"
        );
        return ExitCode::from(2);
    }
    let name = args[2].as_str();
    let clock = if args[5] == "-" { "" } else { args[5].as_str() };
    let toggle = args.len() > 8 && args[8] == "toggle";
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
    let mut b = Board { node, block: 0 };
    let mut failures = 0;
    let path = format!("/sys/bus/platform/devices/{name}/driver_override");

    check(
        &mut failures,
        put(&path, "lemnos") == 0 && put(&args[3], name) == 0,
        "block bound",
    );
    let bound = b.next_event(EVENT_BLOCK_BOUND, 1000);
    check(&mut failures, bound.is_some(), "event: block bound");
    b.block = module::find_block(b.node.as_fd(), name)
        .ok()
        .flatten()
        .unwrap_or(0);
    let block = b.block;
    check(
        &mut failures,
        block != 0 && bound.is_some_and(|e| e.block == block),
        &format!("for block {block}"),
    );

    let mut v: u64 = 0;
    let r = b.power(CLOCK, GET, clock, Some(&mut v));
    check(
        &mut failures,
        r == 0,
        &format!(
            "clock \"{clock}\" rate: {v} Hz ({})",
            if r == 0 {
                "ok".to_string()
            } else {
                io::Error::from_raw_os_error(-r).to_string()
            }
        ),
    );
    let r = b.power(CLOCK, STATUS, clock, Some(&mut v));
    check(&mut failures, r == 0, &format!("clock status: {v}"));
    if toggle {
        check(
            &mut failures,
            b.power(CLOCK, ENABLE, clock, None) == 0,
            "clock on",
        );
        check(
            &mut failures,
            b.power(CLOCK, STATUS, clock, Some(&mut v)) == 0 && v == 1,
            "status on",
        );
        say("     (read the clock's enable count now)");
        std::thread::sleep(Duration::from_secs(2));
        check(
            &mut failures,
            b.power(CLOCK, DISABLE, clock, None) == 0,
            "clock off",
        );
        check(
            &mut failures,
            b.power(CLOCK, STATUS, clock, Some(&mut v)) == 0 && v == 0,
            "status off",
        );
    }
    check(
        &mut failures,
        b.power(CLOCK, GET, "nope", Some(&mut v)) < 0,
        "an unknown clock is refused",
    );
    check(
        &mut failures,
        b.power(SUPPLY, GET, "nope", Some(&mut v)) < 0,
        "an unknown supply is refused",
    );
    check(
        &mut failures,
        b.power(RESET, STATUS, "nope", Some(&mut v)) < 0,
        "an unknown reset is refused",
    );

    // Thermal, beside sysfs.
    let zone = args[6].as_str();
    let before = get(&args[7]);
    let mut t: i64 = 0;
    let r = b.thermal_read(zone, &mut t);
    let after = get(&args[7]);
    check(
        &mut failures,
        r == 0,
        &format!("zone {zone} reads {t} mC; sysfs before {before}, after {after}"),
    );
    let (lo, hi) = if before < after {
        (before, after)
    } else {
        (after, before)
    };
    check(
        &mut failures,
        t >= lo - 3000 && t <= hi + 3000,
        "within 3 degrees of sysfs",
    );

    // Already above the high limit: the first poll reports it.
    let low_mc = t as i32 - 20_000;
    let high_mc = t as i32 - 10_000;
    let mut wa = [0u8; 80];
    put_name(&mut wa, 0, zone, NAME_MAX);
    put32(&mut wa, 64, low_mc as u32);
    put32(&mut wa, 68, high_mc as u32);
    put32(&mut wa, 72, 50);
    let wr = b.call(THERMAL_WATCH, &mut wa);
    check(
        &mut failures,
        wr == 0,
        &format!("watching {low_mc} to {high_mc} mC"),
    );
    let watch = get32(&wa, 76);
    let high = b.next_event(EVENT_TEMPERATURE, 1000);
    check(
        &mut failures,
        high.is_some_and(|e| e.index == watch && e.value >= i64::from(high_mc)),
        &format!("event: high, {} mC", high.map_or(-1, |e| e.value)),
    );
    let mut none = 0i64;
    check(
        &mut failures,
        b.thermal_read("no-such-zone", &mut none) < 0,
        "an unknown zone is refused",
    );

    check(&mut failures, put(&args[4], name) == 0, "unbound");
    let unbound = b.next_event(EVENT_BLOCK_UNBOUND, 1000);
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
