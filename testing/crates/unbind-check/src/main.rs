//! A block unbound while the control node is open: the unbind waits for the DMA buffers still
//! held, register mappings fault afterwards, and requests for the block fail. The module must be
//! loaded with the block in `unconfined=`.
//!
//! Usage: `lemnos-unbind-check /dev/lemnos <device name> <unbind path>`. Port of
//! `kernel/test/lemnos_unbind_check.c` (CZ-GUEST slice 2). The mapping's fault is taken in a
//! forked child that shares the mapping, so a SIGBUS ends the child and not this process.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::guest;
use lemnos_linux_sys::mmap::{Mapping, page_size};
use lemnos_linux_sys::module;
use lemnos_linux_sys::privilege::{self, ChildEnd};

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

fn errno(e: &io::Error) -> Option<i32> {
    e.raw_os_error()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        let _ = writeln!(
            io::stderr(),
            "usage: lemnos_unbind_check /dev/lemnos <device name> <unbind path>"
        );
        return ExitCode::from(2);
    }
    let (node, name, unbind) = (args[1].as_str(), args[2].as_str(), args[3].as_str());
    let fd = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(node)
    {
        Ok(fd) => fd,
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
    let region = module::region(fd.as_fd(), block, 0);
    check(
        &mut failures,
        block != 0 && region.is_ok(),
        &format!("block {block}, region 0"),
    );
    let offset = region.as_ref().map_or(0, |r| r.offset);
    let page = page_size();
    let regs = Mapping::new(fd.as_fd(), page, offset, false);
    let first = regs.as_ref().ok().and_then(|m| m.read32(0));
    check(
        &mut failures,
        first.is_some_and(|v| v != 0),
        "registers mapped and readable",
    );
    let private = guest::private_mapping(fd.as_fd(), page, offset);
    check(
        &mut failures,
        private.as_ref().err().and_then(errno) == Some(libc::EINVAL),
        "a private mapping is refused (EINVAL)",
    );

    let buffer = module::alloc(fd.as_fd(), block, 1 << 16);
    check(&mut failures, buffer.is_ok(), "alloc a buffer");
    let Ok(buffer) = buffer else {
        return finish(node, failures);
    };
    let imported = module::import(fd.as_fd(), block, buffer.fd.as_fd());
    check(&mut failures, imported.is_ok(), "import it");
    check(
        &mut failures,
        module::irq_enable(fd.as_fd(), block, 0).is_ok(),
        "enable irq 0",
    );

    let Ok(child) = privilege::fork() else {
        return finish(node, failures);
    };
    if child == 0 {
        // The copy this process inherited would hold the buffer too.
        drop(buffer);
        let ok = OpenOptions::new()
            .write(true)
            .open(unbind)
            .and_then(|mut u| u.write_all(name.as_bytes()))
            .is_ok();
        guest::exit_now(if ok { 0 } else { 1 });
    }
    let mut buffer = Some(buffer);

    thread::sleep(Duration::from_millis(500));
    check(
        &mut failures,
        matches!(guest::try_wait(child), Ok(None)),
        "the unbind waits while the buffer is held",
    );

    check(
        &mut failures,
        module::block_info(fd.as_fd(), block)
            .err()
            .as_ref()
            .and_then(errno)
            == Some(libc::ENODEV),
        "requests for it: ENODEV",
    );

    // The register mapping, read in a child that shares it: a fault ends that child by SIGBUS.
    let reader = privilege::fork();
    if let Ok(0) = reader {
        match regs.as_ref().ok().and_then(|m| m.read32(0)) {
            Some(v) => {
                say(&format!("FAIL the register mapping still reads 0x{v:08x}"));
                guest::exit_now(0);
            }
            None => guest::exit_now(1),
        }
    }
    if let Ok(reader) = reader {
        match privilege::wait_child(reader) {
            Ok(ChildEnd::Signal(sig)) => check(
                &mut failures,
                sig == libc::SIGBUS,
                &format!("the register mapping faults (signal {sig})"),
            ),
            _ => failures += 1,
        }
    } else {
        failures += 1;
    }

    drop(buffer.take());
    let ended = privilege::wait_child(child);
    check(
        &mut failures,
        matches!(ended, Ok(ChildEnd::Exit(0))),
        "closing the buffer ends the unbind",
    );
    let gone = !matches!(module::find_block(fd.as_fd(), name), Ok(Some(_)));
    check(&mut failures, gone, "the block is gone from the list");
    drop(fd);
    finish(node, failures)
}

/// Prints the summary and returns the exit code: 0 when every check passed.
fn finish(node: &str, failures: u32) -> ExitCode {
    say(&format!("{node}: {failures} failed"));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
