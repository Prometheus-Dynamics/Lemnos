//! GPIO lines through gpiolib and PWM channels through the PWM core, against a simulated chip of
//! each kind.
//!
//! Usage: `lemnos-gpio-check /dev/lemnos <gpio chip number> <simulated line dir> <pwm provider>`.
//! Port of `kernel/test/lemnos_gpio_check.c` (CZ-GUEST-2). Every request is made as the C check
//! makes it: the structure laid out in bytes, so a claim of -1 or a value of 2 reaches the kernel
//! as given, and the kernel's errno is the result.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::time::Duration;

use lemnos_linux_sys::guest;
use lemnos_linux_sys::poll::poll_one;
use lemnos_linux_sys::unix::set_nonblocking;
use lemnos_linux_sys::{iow, iowr};

const EPERM: i32 = libc::EPERM;
const EINVAL: i32 = libc::EINVAL;
const EBUSY: i32 = libc::EBUSY;
const ENODEV: i32 = libc::ENODEV;
const EAGAIN: i32 = libc::EAGAIN;

// gpio (lemnos.h): the flags of a line's configuration.
const OUTPUT: u32 = 1 << 0;
const ACTIVE_LOW: u32 = 1 << 1;
const PULL_UP: u32 = 1 << 2;
const PULL_DOWN: u32 = 1 << 3;
const RISING: u32 = 1 << 7;
const FALLING: u32 = 1 << 8;

const GPIO_REQUEST: u32 = iow::<[u8; 16]>(b'L', 0x98);
const GPIO_CONFIG: u32 = iow::<[u8; 16]>(b'L', 0x99);
const GPIO_GET: u32 = iowr::<[u8; 8]>(b'L', 0x9a);
const GPIO_SET: u32 = iow::<[u8; 8]>(b'L', 0x9b);
const PWM_REQUEST: u32 = iow::<[u8; 56]>(b'L', 0xb8);
const PWM_APPLY: u32 = iow::<[u8; 32]>(b'L', 0xb9);
const PWM_GET: u32 = iowr::<[u8; 32]>(b'L', 0xba);

/// `LEMNOS_NAME_MAX` and the provider field's room (`LEMNOS_NAME_MAX - 16`).
const NAME_MAX: usize = 64;
const PROVIDER_MAX: usize = NAME_MAX - 16;

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

/// The claim: its number as the requests name it (negative: the error the claim got), and the
/// file that holds it, when it got one.
struct Claim {
    fd: i32,
    file: Option<OwnedFd>,
}

impl Claim {
    fn borrow(&self) -> Option<BorrowedFd<'_>> {
        self.file.as_ref().map(AsFd::as_fd)
    }
}

/// `strncpy` of `name` into a name field of `N` bytes, keeping the NUL the field needs.
fn name_bytes(name: &str, room: usize) -> Vec<u8> {
    name.bytes().take(room - 1).collect()
}

struct Checker {
    node: OwnedFd,
    sim: String,
    failures: u32,
}

impl Checker {
    /// One request: 0, or -errno, as the C `call` returns.
    fn call<const N: usize>(&self, request: u32, arg: &mut [u8; N]) -> i32 {
        match guest::ioctl_bytes(self.node.as_fd(), request, arg) {
            Ok(()) => 0,
            Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
        }
    }

    fn claim(&self, name: &str) -> Claim {
        let mut bytes = name_bytes(name, NAME_MAX);
        bytes.truncate(NAME_MAX);
        match guest::claim_raw(self.node.as_fd(), &bytes, 0) {
            Ok(file) => Claim {
                fd: std::os::fd::AsRawFd::as_raw_fd(&file),
                file: Some(file),
            },
            Err(e) => Claim {
                fd: -e.raw_os_error().unwrap_or(libc::EIO),
                file: None,
            },
        }
    }

    fn claim_line(&self, chip: u32, offset: u32) -> Claim {
        self.claim(&format!("gpio-{chip}:{offset}"))
    }

    fn request(&self, c: i32, chip: u32, offset: u32) -> i32 {
        let mut a = [0u8; 16];
        put32(&mut a, 0, c as u32);
        put32(&mut a, 4, chip);
        put32(&mut a, 8, offset);
        self.call(GPIO_REQUEST, &mut a)
    }

    fn configure(&self, c: i32, flags: u32, value: u32) -> i32 {
        let mut a = [0u8; 16];
        put32(&mut a, 0, c as u32);
        put32(&mut a, 4, flags);
        put32(&mut a, 8, value);
        self.call(GPIO_CONFIG, &mut a)
    }

    /// The line's value, as the request answers it (0 when it fails, as the C `get` leaves it).
    fn get(&self, c: i32) -> (i32, u32) {
        let mut a = [0u8; 8];
        put32(&mut a, 0, c as u32);
        let r = self.call(GPIO_GET, &mut a);
        (r, get32(&a, 4))
    }

    fn set(&self, c: i32, value: u32) -> i32 {
        let mut a = [0u8; 8];
        put32(&mut a, 0, c as u32);
        put32(&mut a, 4, value);
        self.call(GPIO_SET, &mut a)
    }

    /// One simulated line's attribute, read: 1 for "pull-up", 0 for "pull-down", -1 when the
    /// file does not open, else the number at its start (`atoi`), as the C `sim_read`.
    fn sim_read(&self, line: u32, attr: &str) -> i32 {
        let path = format!("{}/sim_gpio{line}/{attr}", self.sim);
        let Ok(mut f) = File::open(&path) else {
            return -1;
        };
        let mut text = [0u8; 8];
        match io::Read::read(&mut f, &mut text[..7]) {
            Ok(n) if n > 0 => {}
            _ => text[0] = b'x',
        }
        if &text[..7] == b"pull-up" {
            1
        } else if text[0] == b'p' {
            0
        } else {
            atoi(&text)
        }
    }

    /// Writes "pull-up" or "pull-down" to a simulated line's pull file: 0, or -1.
    fn sim_pull(&self, line: u32, how: &str) -> i32 {
        let path = format!("{}/sim_gpio{line}/pull", self.sim);
        let Ok(mut f) = OpenOptions::new().write(true).open(&path) else {
            return -1;
        };
        match io::Write::write(&mut f, how.as_bytes()) {
            Ok(n) if n > 0 => 0,
            _ => -1,
        }
    }

    fn check_gpio(&mut self, chip: u32) {
        let mut failures = self.failures;
        let c = self.claim_line(chip, 0);
        let cfd = c.fd;

        check(
            &mut failures,
            self.request(-1, chip, 0) == -EPERM,
            "no claim: EPERM",
        );
        check(
            &mut failures,
            self.request(cfd, chip, 1) == -EPERM,
            "a claim on another line: EPERM",
        );
        check(
            &mut failures,
            self.set(cfd, 1) == -EPERM,
            "a claim with no line requested: EPERM",
        );
        check(
            &mut failures,
            self.request(cfd, chip, 0) == 0,
            "line requested",
        );
        check(
            &mut failures,
            self.request(cfd, chip, 0) == -EBUSY,
            "requested twice on one claim: EBUSY",
        );

        check(
            &mut failures,
            self.configure(cfd, OUTPUT, 1) == 0,
            "output, high",
        );
        check(
            &mut failures,
            self.sim_read(0, "value") == 1,
            "the chip sees 1",
        );
        check(
            &mut failures,
            self.set(cfd, 0) == 0 && self.sim_read(0, "value") == 0,
            "set low: the chip sees 0",
        );
        check(
            &mut failures,
            self.set(cfd, 2) == -EINVAL,
            "a value of 2: EINVAL",
        );
        check(
            &mut failures,
            self.configure(cfd, OUTPUT | ACTIVE_LOW, 1) == 0 && self.sim_read(0, "value") == 0,
            "active low, set 1: the chip sees 0",
        );
        check(
            &mut failures,
            self.get(cfd) == (0, 1),
            "active low reads back 1",
        );

        check(&mut failures, self.configure(cfd, 0, 0) == 0, "input");
        check(
            &mut failures,
            self.set(cfd, 1) == -EPERM,
            "setting an input: EPERM",
        );
        let pulled = self.sim_pull(0, "pull-up") == 0;
        let (r, got) = self.get(cfd);
        check(
            &mut failures,
            pulled && r == 0 && got == 1,
            "pulled up: reads 1",
        );
        let pulled = self.sim_pull(0, "pull-down") == 0;
        let (r, got) = self.get(cfd);
        check(
            &mut failures,
            pulled && r == 0 && got == 0,
            "pulled down: reads 0",
        );
        let set_bias = self.configure(cfd, PULL_UP, 0) == 0;
        let (r, got) = self.get(cfd);
        check(
            &mut failures,
            set_bias && r == 0 && got == 1,
            "bias pull-up: reads 1",
        );
        let set_bias = self.configure(cfd, PULL_DOWN, 0) == 0;
        let (r, got) = self.get(cfd);
        check(
            &mut failures,
            set_bias && r == 0 && got == 0,
            "bias pull-down: reads 0",
        );
        check(
            &mut failures,
            self.configure(cfd, PULL_UP | PULL_DOWN, 0) == -EINVAL,
            "two biases: EINVAL",
        );
        check(
            &mut failures,
            self.configure(cfd, OUTPUT | RISING, 0) == -EINVAL,
            "edges on an output: EINVAL",
        );
        check(
            &mut failures,
            self.configure(cfd, 1 << 20, 0) == -EINVAL,
            "unknown flags: EINVAL",
        );

        // Edges: both directions, in order, stamped, and a read that blocks on nothing.
        check(
            &mut failures,
            self.sim_pull(0, "pull-down") == 0,
            "start low",
        );
        check(
            &mut failures,
            self.configure(cfd, RISING | FALLING, 0) == 0,
            "edges on",
        );
        if let Some(file) = c.borrow() {
            let _ = set_nonblocking(file, true);
        }
        let mut ev = [0u8; 64];
        let Some(file) = c.borrow() else {
            self.failures = failures + 1;
            return;
        };
        let none = guest::read(file, &mut ev);
        check(
            &mut failures,
            none.as_ref().err().and_then(|e| e.raw_os_error()) == Some(EAGAIN),
            "none yet: EAGAIN",
        );
        let rising = self.sim_pull(0, "pull-up") == 0
            && poll_one(file, libc::POLLIN, Some(Duration::from_millis(1000)))
                .is_ok_and(|r| r != 0);
        check(&mut failures, rising, "rising edge seen");
        let n = guest::read(file, &mut ev).map_or(-1, |n| n as i64);
        check(
            &mut failures,
            n == 16 && get32(&ev, 12) == 1 && get64(&ev, 0) != 0,
            &format!("rising: seq {}", get32(&ev, 8)),
        );
        let seq = get32(&ev, 8);

        let falling = self.sim_pull(0, "pull-down") == 0
            && poll_one(file, libc::POLLIN, Some(Duration::from_millis(1000)))
                .is_ok_and(|r| r != 0);
        check(&mut failures, falling, "falling edge seen");
        let n = guest::read(file, &mut ev).map_or(-1, |n| n as i64);
        check(
            &mut failures,
            n == 16 && get32(&ev, 12) == 0 && get32(&ev, 8) == seq.wrapping_add(1),
            &format!("falling: seq {}", get32(&ev, 8)),
        );
        check(
            &mut failures,
            guest::read(file, &mut ev[..1])
                .err()
                .and_then(|e| e.raw_os_error())
                == Some(EINVAL),
            "a read too short: EINVAL",
        );

        drop(c);
        let c = self.claim_line(chip, 0);
        check(
            &mut failures,
            c.fd >= 0 && self.request(c.fd, chip, 0) == 0,
            "after closing, the line can be requested again",
        );
        drop(c);

        let c = self.claim_line(chip, 8);
        check(
            &mut failures,
            self.request(c.fd, chip, 8) == -EINVAL,
            "a line past the chip's end: EINVAL",
        );
        drop(c);
        let c = self.claim_line(chip + 100, 0);
        check(
            &mut failures,
            self.request(c.fd, chip + 100, 0) == -ENODEV,
            "no such chip: ENODEV",
        );
        drop(c);
        self.failures = failures;
    }

    fn check_pwm(&mut self, provider: &str) {
        let mut failures = self.failures;
        let name = format!("pwm-{provider}:0");
        let c = self.claim(&name);
        let cfd = c.fd;

        // A request: claim, channel, provider (the C struct's layout).
        let request_for = |claim: i32, channel: u32| -> [u8; 56] {
            let mut a = [0u8; 56];
            put32(&mut a, 0, claim as u32);
            put32(&mut a, 4, channel);
            let p = provider.as_bytes();
            let n = p.len().min(PROVIDER_MAX - 1);
            a[8..8 + n].copy_from_slice(&p[..n]);
            a
        };
        let state_for = |claim: i32, enabled: u32, period: u64, duty: u64| -> [u8; 32] {
            let mut a = [0u8; 32];
            put32(&mut a, 0, claim as u32);
            put32(&mut a, 4, enabled);
            put64(&mut a, 16, period);
            put64(&mut a, 24, duty);
            a
        };

        let mut wrong = request_for(cfd, 1);
        check(
            &mut failures,
            self.call(PWM_REQUEST, &mut wrong) == -EPERM,
            "a claim on another channel: EPERM",
        );
        let mut r = request_for(cfd, 0);
        check(
            &mut failures,
            self.call(PWM_REQUEST, &mut r) == 0,
            "channel requested",
        );

        let mut s = state_for(cfd, 1, 1_000_000, 250_000);
        check(
            &mut failures,
            self.call(PWM_APPLY, &mut s) == 0,
            "applied 1 ms, 25 percent",
        );
        let mut back = state_for(cfd, 0, 0, 0);
        let got = self.call(PWM_GET, &mut back);
        let (enabled, inverted, period, duty) = (
            get32(&back, 4),
            get32(&back, 8),
            get64(&back, 16),
            get64(&back, 24),
        );
        check(
            &mut failures,
            got == 0 && enabled == 1 && period == 1_000_000 && duty == 250_000 && inverted == 0,
            &format!("state back: {period} ns, {duty} ns, on {enabled}"),
        );
        let mut s = state_for(cfd, 1, 1_000_000, 2_000_000);
        check(
            &mut failures,
            self.call(PWM_APPLY, &mut s) == -EINVAL,
            "duty past period: EINVAL",
        );
        let mut s = state_for(cfd, 2, 1_000_000, 250_000);
        check(
            &mut failures,
            self.call(PWM_APPLY, &mut s) == -EINVAL,
            "enabled of 2: EINVAL",
        );
        let mut s = state_for(-1, 1, 1_000_000, 250_000);
        check(
            &mut failures,
            self.call(PWM_APPLY, &mut s) == -EPERM,
            "no claim: EPERM",
        );
        drop(c);

        let c = self.claim(&name);
        let mut r = request_for(c.fd, 0);
        check(
            &mut failures,
            self.call(PWM_REQUEST, &mut r) == 0,
            "after closing, the channel can be requested again",
        );
        let mut back = state_for(c.fd, 0, 0, 0);
        let got = self.call(PWM_GET, &mut back);
        check(
            &mut failures,
            got == 0 && get32(&back, 4) == 1,
            "and it is still running",
        );
        drop(c);

        let c = self.claim("pwm-no-such-device:0");
        let mut r = request_for(c.fd, 0);
        r[8..8 + "no-such-device".len()].copy_from_slice(b"no-such-device");
        r[8 + "no-such-device".len()] = 0;
        check(
            &mut failures,
            self.call(PWM_REQUEST, &mut r) == -ENODEV,
            "no such provider: ENODEV",
        );
        drop(c);
        self.failures = failures;
    }
}

/// `atoi`: optional blanks and sign, then digits, up to the first byte that is not one.
fn atoi(text: &[u8]) -> i32 {
    let mut i = 0;
    while i < text.len() && text[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < text.len() && (text[i] == b'-' || text[i] == b'+') {
        neg = text[i] == b'-';
        i += 1;
    }
    let mut n: i32 = 0;
    while i < text.len() && text[i].is_ascii_digit() {
        n = n.wrapping_mul(10).wrapping_add(i32::from(text[i] - b'0'));
        i += 1;
    }
    if neg { -n } else { n }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        let _ = writeln!(
            io::stderr(),
            "usage: lemnos_gpio_check /dev/lemnos <chip> <sim dir> <pwm provider>"
        );
        return ExitCode::from(2);
    }
    let node = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&args[1])
    {
        Ok(f) => OwnedFd::from(f),
        Err(e) => {
            let _ = writeln!(io::stderr(), "{}: {e}", args[1]);
            return ExitCode::from(2);
        }
    };
    let chip: u32 = atoi(args[2].as_bytes()) as u32;
    let sim: String = args[3].chars().take(159).collect();
    let mut checker = Checker {
        node,
        sim,
        failures: 0,
    };
    checker.check_gpio(chip);
    checker.check_pwm(&args[4]);
    say(&format!("{}: {} failed", args[1], checker.failures));
    if checker.failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
