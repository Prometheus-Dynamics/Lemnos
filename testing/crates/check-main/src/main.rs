//! Exercises a block on the control node: info, regions, a DMA buffer (written through its dma-buf
//! mapping and imported back), an import of a dma-buf from a heap, the interrupt mask and read,
//! and the register-mapping policy. Leaves the block's registers untouched.
//!
//! Usage: `lemnos-check /dev/lemnos <device name> [heap]`. Port of `kernel/test/lemnos_check.c`
//! (CZ-GUEST-2). Static musl.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::time::Duration;

use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::guest;
use lemnos_linux_sys::mmap::{Mapping, page_size};
use lemnos_linux_sys::module;
use lemnos_linux_sys::poll::poll_one;
use lemnos_linux_sys::unix::set_nonblocking;

const EINVAL: i32 = libc::EINVAL;
const EBUSY: i32 = libc::EBUSY;
const ENODEV: i32 = libc::ENODEV;
const ENOENT: i32 = libc::ENOENT;
const EPERM: i32 = libc::EPERM;
const EAGAIN: i32 = libc::EAGAIN;
const ENOTTY: i32 = libc::ENOTTY;
const EFAULT: i32 = libc::EFAULT;

/// A request number, as `_IOC(dir, type, nr, size)` makes it: `dir` is 1 for a request the kernel
/// reads, 2 for one it writes back, 3 for both.
const fn request(dir: u32, kind: u8, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | ((kind as u32) << 8) | nr
}

/// The errno of a request made with `N` bytes whose first four are `block`.
fn refused<const N: usize>(fd: &OwnedFd, request: u32, block: u32) -> Option<i32> {
    let mut arg = [0u8; N];
    arg[..4].copy_from_slice(&block.to_ne_bytes());
    guest::ioctl_bytes(fd.as_fd(), request, &mut arg)
        .err()
        .as_ref()
        .and_then(errno)
}

/// The node's request entry: what it refuses before any block or part of the module is asked, and
/// the requests a block of another kind refuses.
fn check_entry(failures: &mut u32, fd: &OwnedFd, block: u32) {
    // LEMNOS_IOC_INFO is _IOWR('L', 0xd0, 32 bytes).
    check(
        failures,
        refused::<32>(fd, request(3, b'M', 0xd0, 32), block) == Some(ENOTTY),
        "a request of another type: ENOTTY",
    );
    check(
        failures,
        refused::<4>(fd, request(1, b'L', 0xd0, 2), block) == Some(ENOTTY),
        "a request smaller than a block number: ENOTTY",
    );
    check(
        failures,
        refused::<8192>(fd, request(3, b'L', 0xd0, 8192), block) == Some(ENOTTY),
        "a request larger than any the node takes: ENOTTY",
    );
    check(
        failures,
        refused::<8>(fd, request(3, b'L', 0x01, 8), block) == Some(ENOTTY),
        "a request the node does not know, for a bound block: ENOTTY",
    );
    check(
        failures,
        refused::<8>(fd, request(3, b'L', 0x01, 8), block.wrapping_add(1000)) == Some(ENODEV),
        "a request the node does not know, for a block not bound: ENODEV",
    );
    let nowhere = guest::ioctl_at(fd.as_fd(), request(3, b'L', 0xd0, 32), 1);
    check(
        failures,
        nowhere.as_ref().err().and_then(errno) == Some(EFAULT),
        "a request at an address with nothing mapped: EFAULT",
    );
    // The requests of the blocks the module drives itself, on a block it does not drive: the PiSP
    // back end (create, version), the graphics DMA engine, the VIP9000 and register jobs.
    let kinds = [
        refused::<80>(fd, request(3, b'L', 0xb0, 80), block),
        refused::<8>(fd, request(3, b'L', 0xb2, 8), block),
        refused::<64>(fd, request(3, b'L', 0xe8, 64), block),
        refused::<64>(fd, request(3, b'L', 0xe0, 64), block),
        refused::<24>(fd, request(3, b'L', 0xf0, 24), block),
    ];
    check(
        failures,
        kinds.iter().all(|e| *e == Some(ENOTTY)),
        &format!("the requests of a kind of block this is not: ENOTTY ({kinds:?})"),
    );
}

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

/// Whether the module was loaded with `test_irq_inject`.
fn injecting() -> bool {
    let mut c = *b"N";
    let read = std::fs::File::open("/sys/module/lemnos/parameters/test_irq_inject")
        .and_then(|mut f| f.read(&mut c));
    if !matches!(read, Ok(1)) {
        c = *b"N";
    }
    c[0] == b'Y'
}

/// The first address past the device's DMA reach: 2^bits, or all of the address space.
fn dma_limit(bits: u32) -> u128 {
    if bits >= 64 {
        u128::from(u64::MAX)
    } else {
        1u128 << bits
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let node = args.get(1).map_or("/dev/lemnos", String::as_str);
    let name = args.get(2).map_or("", String::as_str);
    let heap = args
        .get(3)
        .map_or("/dev/dma_heap/linux,cma", String::as_str);
    let fd: OwnedFd = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
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
    check(
        &mut failures,
        block != 0,
        &format!("block {name} is number {block}"),
    );
    let second = OpenOptions::new().read(true).write(true).open(node);
    check(
        &mut failures,
        second.as_ref().err().and_then(errno) == Some(EBUSY),
        "a second open is refused (EBUSY)",
    );
    drop(second);
    let gone = module::block_info(fd.as_fd(), block.wrapping_add(1000));
    check(
        &mut failures,
        gone.as_ref().err().and_then(errno) == Some(ENODEV),
        "a block not bound: ENODEV",
    );
    check_entry(&mut failures, &fd, block);
    let info = module::block_info(fd.as_fd(), block).ok();
    let (abi, flags, regions, irqs, dma_bits) = info.as_ref().map_or((0, 0, 0, 0, 0), |i| {
        (i.abi, i.flags, i.regions, i.irqs, i.dma_bits)
    });
    check(
        &mut failures,
        info.is_some() && abi == module::ABI,
        &format!(
            "info: abi {abi}, {regions} regions, {irqs} irqs, {dma_bits}-bit DMA, registers {}mappable",
            if flags & module::INFO_MMIO_MAPPABLE != 0 {
                ""
            } else {
                "not "
            }
        ),
    );
    let limit = dma_limit(dma_bits);

    let mut first_offset = 0;
    for i in 0..regions {
        let r = module::region(fd.as_fd(), block, i);
        let ok = r.as_ref().is_ok_and(|r| r.size != 0);
        let (start, size) = r.as_ref().map_or((0, 0), |r| (r.start, r.size));
        check(
            &mut failures,
            ok,
            &format!("region {i}: 0x{start:x}, {size} bytes"),
        );
        if i == 0 {
            first_offset = r.as_ref().map_or(0, |r| r.offset);
        }
    }
    let past = module::region(fd.as_fd(), block, regions);
    check(
        &mut failures,
        past.as_ref().err().and_then(errno) == Some(EINVAL),
        "region past the end: EINVAL",
    );

    let page = page_size();
    let regs = Mapping::new(fd.as_fd(), page, first_offset, false);
    if flags & module::INFO_MMIO_MAPPABLE != 0 {
        check(&mut failures, regs.is_ok(), "registers mapped");
    } else {
        check(
            &mut failures,
            regs.as_ref().err().and_then(errno) == Some(EPERM),
            "registers not mapped (EPERM)",
        );
    }
    check(
        &mut failures,
        flags & module::INFO_MMIO_UNCONFINED == 0 || flags & module::INFO_MMIO_MAPPABLE != 0,
        &format!("unconfined only with mappable: flags 0x{flags:x}"),
    );
    drop(regs);

    // A buffer: written by the CPU through the dma-buf, imported back to the same address.
    let size: u64 = 1 << 20;
    let a = module::alloc(fd.as_fd(), block, size);
    let (a_fd, a_addr, a_size) = a.as_ref().map_or((-1, 0, 0), |a| {
        (std::os::fd::AsRawFd::as_raw_fd(&a.fd), a.addr, a.size)
    });
    check(
        &mut failures,
        a.is_ok() && a_fd >= 0,
        &format!("alloc 1 MiB: dma-buf fd {a_fd} at device address 0x{a_addr:x}"),
    );
    check(
        &mut failures,
        u128::from(a_addr) + u128::from(a_size) <= limit,
        &format!("address below 2^{dma_bits}"),
    );
    let cpu = a
        .as_ref()
        .ok()
        .map(|a| Mapping::new(a.fd.as_fd(), a_size as usize, 0, true));
    check(
        &mut failures,
        cpu.as_ref().is_some_and(Result::is_ok),
        "dma-buf mapped for the CPU",
    );
    if let Some(Ok(mut cpu)) = cpu {
        let buf = cpu.as_mut_slice();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7);
        }
        let bad = buf
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != (*i as u8).wrapping_mul(7))
            .count();
        check(
            &mut failures,
            bad == 0,
            &format!("pattern read back: {bad} bytes differ"),
        );
        drop(cpu);
    }

    // Behind an IOMMU the import is a second mapping: another address, same size.
    // A failed allocation leaves the C check importing descriptor 0 (zeroed): the same here.
    let im = match &a {
        Ok(a) => module::import(fd.as_fd(), block, a.fd.as_fd()).ok(),
        Err(_) => module::import(fd.as_fd(), block, io::stdin().as_fd()).ok(),
    };
    let (im_addr, im_size, im_handle) = im
        .as_ref()
        .map_or((0, 0, 0), |i| (i.addr, i.size, i.handle));
    check(
        &mut failures,
        im.is_some()
            && im_handle != 0
            && im_size == a_size
            && u128::from(im_addr) + u128::from(im_size) <= limit,
        &format!(
            "import of its own buffer: handle {im_handle} at 0x{im_addr:x} ({}), {im_size} bytes",
            if im_addr == a_addr {
                "same address"
            } else {
                "a second mapping"
            }
        ),
    );
    let released = module::release(fd.as_fd(), block, im_handle);
    check(&mut failures, released.is_ok(), "release");
    let again = module::release(fd.as_fd(), block, im_handle);
    check(
        &mut failures,
        again.as_ref().err().and_then(errno) == Some(ENOENT),
        "release again: ENOENT",
    );

    // A dma-buf from a heap, left to the close to release.
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(heap)
    {
        Ok(h) => {
            let len: u64 = 4 << 20;
            let d = dma_heap::allocate(h.as_fd(), len);
            check(&mut failures, d.is_ok(), &format!("heap {heap}: 4 MiB"));
            // The C check imports the heap's fd even when the allocation failed (fd 0 then).
            let source = d.as_ref().map_or_else(
                |_| io::stdin().as_fd().try_clone_to_owned(),
                |d| d.try_clone(),
            );
            let hi = source
                .ok()
                .and_then(|f| module::import(fd.as_fd(), block, f.as_fd()).ok());
            check(
                &mut failures,
                hi.as_ref().is_some_and(|i| i.size >= len),
                &format!(
                    "import of the heap buffer: 0x{:x}, {} bytes (held until close)",
                    hi.as_ref().map_or(0, |i| i.addr),
                    hi.as_ref().map_or(0, |i| i.size)
                ),
            );
        }
        Err(_) => say(&format!("skip no {heap}")),
    }

    // Interrupts start masked: nothing to read, and enabling past the end is refused.
    let quiet = |ms: u64| {
        matches!(
            poll_one(fd.as_fd(), libc::POLLIN, Some(Duration::from_millis(ms))),
            Ok(0)
        )
    };
    check(
        &mut failures,
        quiet(100),
        "no interrupt pending (poll 100 ms)",
    );
    if irqs != 0 {
        check(
            &mut failures,
            module::irq_enable(fd.as_fd(), block, 0).is_ok(),
            "enable irq 0 (an idle block raises none)",
        );
        check(
            &mut failures,
            quiet(100),
            "still none after enabling (poll 100 ms)",
        );
    }
    let past_irq = module::irq_enable(fd.as_fd(), block, irqs);
    check(
        &mut failures,
        past_irq.as_ref().err().and_then(errno) == Some(EINVAL),
        &format!("enable irq {irqs}: EINVAL"),
    );
    // lemnos_irq_check covers injection; a board run may load the module with it on.
    if !injecting() {
        let injected = module::test_irq(fd.as_fd(), block, 0);
        check(
            &mut failures,
            injected.as_ref().err().and_then(errno) == Some(EPERM),
            "test interrupt without test_irq_inject: EPERM",
        );
    }
    let _ = set_nonblocking(fd.as_fd(), true);
    let mut ev = [0u8; 32];
    let none = guest::read(fd.as_fd(), &mut ev);
    check(
        &mut failures,
        none.as_ref().err().and_then(errno) == Some(EAGAIN),
        "read with none pending: EAGAIN",
    );
    let short = guest::read(fd.as_fd(), &mut ev[..31]);
    check(
        &mut failures,
        short.as_ref().err().and_then(errno) == Some(EINVAL),
        "read too short: EINVAL",
    );

    // A mapping holds the file open as a descriptor does: let go of both before reopening.
    drop(a);
    drop(fd);
    let reopened = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(node);
    check(
        &mut failures,
        reopened.is_ok(),
        "reopened after close (imports released)",
    );
    drop(reopened);
    say(&format!("{node}: {failures} failed"));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
