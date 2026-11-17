//! The registers of a block listed in `unconfined=` and the test-only interrupt path, on a bound
//! block with the module loaded `unconfined=<its compatible> test_irq_inject=1`:
//! - the registers through mmap, the module saying they are mappable only because of
//!   `unconfined=` (read only: region 0's first word);
//! - interrupts: one set pending at the controller reaches `read()` when enabled, waits while
//!   masked and arrives on enable, and the time from the injection to the handler and to user
//!   space, over many.
//!
//! Usage: `lemnos-irq-check /dev/lemnos <device name> [rounds]`. Port of
//! `kernel/test/lemnos_irq_check.c` (CZ-GUEST-2). Static musl.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::time::Duration;

use lemnos_linux_sys::guest;
use lemnos_linux_sys::mmap::{Mapping, page_size};
use lemnos_linux_sys::module;
use lemnos_linux_sys::poll::poll_one;
use lemnos_linux_sys::time::monotonic_ns;

const EINVAL: i32 = libc::EINVAL;
const EVENT_IRQ: u32 = 0;

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

fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn get64(b: &[u8], at: usize) -> u64 {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[at..at + 8]);
    u64::from_ne_bytes(w)
}

/// `struct lemnos_event` (32 bytes): block, index, count, kind, ns, value.
#[derive(Clone, Copy, Default)]
struct Event {
    block: u32,
    index: u32,
    count: u32,
    kind: u32,
    ns: u64,
}

/// One event read from the node, or `None` when the read is not a whole event.
fn read_event(fd: &OwnedFd) -> Option<Event> {
    let mut b = [0u8; 32];
    match guest::read(fd.as_fd(), &mut b) {
        Ok(32) => Some(Event {
            block: get32(&b, 0),
            index: get32(&b, 4),
            count: get32(&b, 8),
            kind: get32(&b, 12),
            ns: get64(&b, 16),
        }),
        _ => None,
    }
}

/// `poll(&p, 1, ms) == 1` on the node: something is ready within `ms`.
fn ready_within(fd: &OwnedFd, ms: u64) -> bool {
    poll_one(fd.as_fd(), libc::POLLIN, Some(Duration::from_millis(ms))).is_ok_and(|r| r != 0)
}

fn report(what: &str, v: &mut [u64]) {
    v.sort_unstable();
    let n = v.len();
    say(&format!(
        "     {what}: p50 {:.1} p99 {:.1} max {:.1} us ({n})",
        v[n / 2] as f64 / 1000.0,
        v[n * 99 / 100] as f64 / 1000.0,
        v[n - 1] as f64 / 1000.0
    ));
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let node = args.get(1).map_or("/dev/lemnos", String::as_str);
    let name = args.get(2).map_or("", String::as_str);
    let rounds: usize = args
        .get(3)
        .and_then(|s| s.parse::<i64>().ok())
        .map_or(2000, |n| n.max(0) as usize);

    let fd = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(node)
    {
        Ok(f) => OwnedFd::from(f),
        Err(e) => {
            let _ = writeln!(io::stderr(), "{node}: {e}");
            return ExitCode::from(2);
        }
    };
    let mut failures = 0;

    let block = module::find_block(fd.as_fd(), name)
        .ok()
        .flatten()
        .unwrap_or(0);
    let info = module::block_info(fd.as_fd(), block).ok();
    let (irqs, flags) = info.as_ref().map_or((0, 0), |i| (i.irqs, i.flags));
    check(
        &mut failures,
        block != 0,
        &format!("block {name} is number {block}"),
    );
    check(
        &mut failures,
        info.is_some() && irqs >= 1,
        &format!(
            "info: {irqs} irqs, registers {}mappable",
            if flags & module::INFO_MMIO_MAPPABLE != 0 {
                ""
            } else {
                "not "
            }
        ),
    );
    let wanted = module::INFO_MMIO_MAPPABLE | module::INFO_MMIO_UNCONFINED;
    check(
        &mut failures,
        info.is_some() && flags & wanted == wanted,
        &format!("mappable through unconfined= alone: flags 0x{flags:x}"),
    );

    let page = page_size();
    let region = module::region(fd.as_fd(), block, 0);
    let offset = region.as_ref().map_or(0, |r| r.offset);
    check(
        &mut failures,
        region.is_ok(),
        &format!("region 0 offset 0x{offset:x}"),
    );
    let regs = Mapping::new(fd.as_fd(), page, offset, false);
    check(&mut failures, regs.is_ok(), "region 0 mapped read-only");
    if let Ok(regs) = &regs {
        let v = regs.read32(0).unwrap_or(0);
        check(
            &mut failures,
            v != 0 && v != 0xffff_ffff,
            &format!("region 0, word 0: 0x{v:08x}"),
        );
    }
    drop(regs);

    // Enabled, then injected: one event.
    check(
        &mut failures,
        module::irq_enable(fd.as_fd(), block, 0).is_ok(),
        "enable irq 0",
    );
    let injected = module::test_irq(fd.as_fd(), block, 0);
    check(
        &mut failures,
        injected.is_ok(),
        &match &injected {
            Ok(()) => "inject irq 0 (ok)".to_string(),
            Err(e) => format!("inject irq 0 ({e})"),
        },
    );
    check(&mut failures, ready_within(&fd, 1000), "event within 1 s");
    let mut ev = read_event(&fd).unwrap_or_default();
    check(
        &mut failures,
        read_event_ok(&ev, block) && ev.index == 0 && ev.kind == EVENT_IRQ,
        &format!(
            "read: block {}, irq {}, count {}",
            ev.block, ev.index, ev.count
        ),
    );
    let first = ev.count;

    // Masked since it fired: an injection is not delivered. A level interrupt is not resent on
    // enable (a real device still holds its line, so it fires again); an injected one is gone once
    // the controller took it. Either way the next one after enable arrives.
    check(
        &mut failures,
        module::test_irq(fd.as_fd(), block, 0).is_ok(),
        "inject while masked",
    );
    check(
        &mut failures,
        matches!(
            poll_one(fd.as_fd(), libc::POLLIN, Some(Duration::from_millis(200))),
            Ok(0)
        ),
        "no event while masked (200 ms)",
    );
    check(
        &mut failures,
        module::irq_enable(fd.as_fd(), block, 0).is_ok(),
        "enable",
    );
    if ready_within(&fd, 200) {
        if let Some(e) = read_event(&fd) {
            ev = e;
            say(&format!(
                "     the masked one was resent on enable (an edge interrupt), count {}",
                ev.count
            ));
        } else {
            say("     the masked one was not resent (a level interrupt, its line not held)");
        }
    } else {
        say("     the masked one was not resent (a level interrupt, its line not held)");
    }
    let next = module::irq_enable(fd.as_fd(), block, 0).is_ok()
        && module::test_irq(fd.as_fd(), block, 0).is_ok()
        && ready_within(&fd, 1000)
        && match read_event(&fd) {
            Some(e) => {
                ev = e;
                true
            }
            None => false,
        }
        && ev.count > first;
    check(
        &mut failures,
        next,
        &format!("the next one after enable arrives (count {})", ev.count),
    );
    check(
        &mut failures,
        module::test_irq(fd.as_fd(), block, irqs)
            .err()
            .and_then(|e| e.raw_os_error())
            == Some(EINVAL),
        "inject past the end: EINVAL",
    );

    // Latency, as the back end's own was measured: SCHED_FIFO 10, poll then read.
    let fifo = guest::set_fifo(10);
    let mut to_irq: Vec<u64> = Vec::with_capacity(rounds);
    let mut to_wake: Vec<u64> = Vec::with_capacity(rounds);
    let mut lost = 0;
    let mut i = 0;
    while i < rounds {
        if module::irq_enable(fd.as_fd(), block, 0).is_err() {
            break;
        }
        if ready_within_zero(&fd) && read_event(&fd).is_some() {
            // A stray event from before: redo this round.
            continue;
        }
        let t0 = monotonic_ns().unwrap_or(0);
        if module::test_irq(fd.as_fd(), block, 0).is_err() {
            break;
        }
        let got = ready_within(&fd, 1000);
        let read = if got { read_event(&fd) } else { None };
        let Some(read) = read else {
            lost += 1;
            if lost >= 3 {
                break;
            }
            i += 1;
            continue;
        };
        let t1 = monotonic_ns().unwrap_or(0);
        to_irq.push(read.ns.wrapping_sub(t0));
        to_wake.push(t1.wrapping_sub(t0));
        i += 1;
    }
    let n = to_irq.len();
    check(
        &mut failures,
        n == rounds && lost == 0,
        &format!(
            "{n} of {rounds} injected interrupts read ({}), {lost} lost",
            if fifo { "SCHED_FIFO 10" } else { "SCHED_OTHER" }
        ),
    );
    if n > 0 {
        report("injection to handler", &mut to_irq);
        report("injection to user space", &mut to_wake);
    }
    say(&format!("{node}: {failures} failed"));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `poll(&p, 1, 0) == 1`: an event is ready now.
fn ready_within_zero(fd: &OwnedFd) -> bool {
    poll_one(fd.as_fd(), libc::POLLIN, Some(Duration::ZERO)).is_ok_and(|r| r != 0)
}

/// The event names this block's interrupt zero.
fn read_event_ok(ev: &Event, block: u32) -> bool {
    ev.block == block
}
