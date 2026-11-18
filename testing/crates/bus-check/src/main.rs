//! Transfers through the kernel's I2C and SPI cores. Expects `i2c-stub` loaded with a chip at
//! 0x30, and the test buses at 40 with SPI device 40.0 bound.
//!
//! Usage: `lemnos-bus-check /dev/lemnos`. Port of `kernel/test/lemnos_bus_check.c` (CZ-GUEST-2).
//! Every request is laid out in bytes as the C check lays out its structures, and the segment
//! pointers are the addresses of this process's buffers, which the kernel reads and writes.
//! Static musl.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;

use lemnos_linux_sys::guest;
use lemnos_linux_sys::{iow, iowr};

const TEST_BUS: u32 = 40;
const STUB_ADDR: u16 = 0x30;
const MEM_ADDR: u16 = 0x50;
const HELD_ADDR: u16 = 0x51;
const BUFFER: usize = 4096;
const MAX_SEGMENTS: u32 = 16;

// lemnos.h: the flags and ioctls the check uses.
const I2C_READ: u32 = 1 << 0;
const I2C_TRANSFER: u32 = iow::<[u8; 24]>(b'L', 0xa8);
const I2C_SMBUS: u32 = iowr::<[u8; 56]>(b'L', 0xa9);
const SPI_TRANSFER: u32 = iow::<[u8; 32]>(b'L', 0xaa);
const SPI_INFO: u32 = iowr::<[u8; 16]>(b'L', 0xab);

// <linux/i2c.h>: SMBus protocols.
const SMBUS_BYTE_DATA: u16 = 2;
const SMBUS_WORD_DATA: u16 = 3;
const SMBUS_I2C_BLOCK_DATA: u16 = 8;

const NAME_MAX: usize = 64;

// The test buses' second SPI device (lemnos_test_bus.c): its name, and the parameter that says which
// chip select it is at.
const TWIN: &str = "spi-lemnos-test-twin";
const TWIN_CS: &str = "/sys/module/lemnos_test_bus/parameters/spi_twin_cs";

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

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_ne_bytes());
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

/// The kernel log's lines that hold `needle`, counted from the start of the log (`/dev/kmsg`, one
/// record a read, until none is left); None when the log can't be read.
fn log_lines(needle: &[u8]) -> Option<u32> {
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open("/dev/kmsg")
        .ok()?;
    let mut record = [0u8; 8192];
    let mut lines = 0;
    loop {
        match f.read(&mut record) {
            Ok(0) => return Some(lines),
            Ok(n) => {
                lines += u32::from(record[..n].windows(needle.len()).any(|w| w == needle));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Some(lines),
            // EPIPE: records were overwritten under the reader; the next read goes on from the oldest left.
            Err(e) if e.raw_os_error() == Some(libc::EPIPE) => {}
            Err(_) => return None,
        }
    }
}

/// Writes `text` to a sysfs file: 0, or -errno.
fn put(path: &str, text: &str) -> i32 {
    match OpenOptions::new().write(true).open(path) {
        Ok(mut f) => match f.write(text.as_bytes()) {
            Ok(_) => 0,
            Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
        },
        Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
    }
}

/// The address of a buffer, as the kernel is given it: a pointer the kernel reads or writes.
fn addr(buf: &[u8]) -> u64 {
    buf.as_ptr() as u64
}

/// The address of a buffer the kernel writes into: a read's destination.
fn addr_mut(buf: &mut [u8]) -> u64 {
    buf.as_mut_ptr() as u64
}

/// A claim: its number as the requests name it (negative: the error it got), and the file
/// that holds it, when it got one. Closing it leaves its number in use by nothing, as C does.
struct Claim {
    fd: i32,
    file: Option<OwnedFd>,
}

/// One `struct lemnos_i2c_segment` (16 bytes).
fn i2c_segment(buf: u64, len: u32, flags: u32) -> Vec<u8> {
    let mut s = vec![0u8; 16];
    put64(&mut s, 0, buf);
    put32(&mut s, 8, len);
    put32(&mut s, 12, flags);
    s
}

/// One `struct lemnos_spi_segment` (32 bytes).
fn spi_segment(tx: u64, rx: u64, len: u32, delay_us: u16) -> Vec<u8> {
    let mut s = vec![0u8; 32];
    put64(&mut s, 0, tx);
    put64(&mut s, 8, rx);
    put32(&mut s, 16, len);
    put16(&mut s, 24, delay_us);
    s
}

struct Bus {
    node: OwnedFd,
}

impl Bus {
    /// One request: 0, or -errno.
    fn call<const N: usize>(&self, request: u32, arg: &mut [u8; N]) -> i32 {
        match guest::ioctl_bytes(self.node.as_fd(), request, arg) {
            Ok(()) => 0,
            Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
        }
    }

    fn claim(&self, name: &str) -> Claim {
        let mut bytes: Vec<u8> = name.bytes().take(NAME_MAX - 1).collect();
        bytes.truncate(NAME_MAX);
        match guest::claim_raw(self.node.as_fd(), &bytes, 0) {
            Ok(file) => Claim {
                fd: file.as_raw_fd(),
                file: Some(file),
            },
            Err(e) => Claim {
                fd: -e.raw_os_error().unwrap_or(libc::EIO),
                file: None,
            },
        }
    }

    fn claim_i2c(&self, bus: u32, addr: u16) -> Claim {
        self.claim(&format!("i2c-{bus}:0x{addr:x}"))
    }

    /// `i2c()` of the C check: a transfer of `count` segments at `segments`, as the claim `c`.
    fn i2c(&self, c: i32, bus: u32, addr: u16, segments: u64, count: u32) -> i32 {
        let mut t = [0u8; 24];
        put32(&mut t, 0, c as u32);
        put32(&mut t, 4, bus);
        put16(&mut t, 8, addr);
        put32(&mut t, 12, count);
        put64(&mut t, 16, segments);
        self.call(I2C_TRANSFER, &mut t)
    }

    fn smbus(&self, s: &mut [u8; 56]) -> i32 {
        self.call(I2C_SMBUS, s)
    }

    fn spi(&self, c: i32, cs: u32, mode: u32, segments: u64, count: u32) -> i32 {
        let mut t = [0u8; 32];
        put32(&mut t, 0, c as u32);
        put32(&mut t, 4, TEST_BUS);
        put32(&mut t, 8, cs);
        put32(&mut t, 12, mode);
        put32(&mut t, 16, count);
        put64(&mut t, 24, segments);
        self.call(SPI_TRANSFER, &mut t)
    }
}

/// One `struct lemnos_i2c_smbus` (56 bytes), as the C check fills it in.
fn smbus_request(
    c: i32,
    bus: u32,
    addr: u16,
    read: u8,
    command: u8,
    protocol: u16,
    data: &[u8],
) -> [u8; 56] {
    let mut s = [0u8; 56];
    put32(&mut s, 0, c as u32);
    put32(&mut s, 4, bus);
    put16(&mut s, 8, addr);
    s[12] = read;
    s[13] = command;
    put16(&mut s, 14, protocol);
    s[16..16 + data.len()].copy_from_slice(data);
    s
}

/// The i2c-stub's adapter number, found by name; -1 if it isn't there.
fn stub_bus() -> i32 {
    for n in 0..64 {
        let path = format!("/sys/bus/i2c/devices/i2c-{n}/name");
        let Ok(name) = fs::read(&path) else {
            continue;
        };
        if name.starts_with(b"SMBus stub") {
            return n;
        }
    }
    -1
}

fn check_smbus(bus: &Bus, failures: &mut u32) {
    let stub = stub_bus();
    check(failures, stub >= 0, &format!("i2c-stub is adapter {stub}"));
    let c = bus.claim_i2c(stub as u32, STUB_ADDR);
    let cn = c.fd;
    let sbus = stub as u32;
    let mut s = smbus_request(cn, sbus, STUB_ADDR, 0, 0x10, SMBUS_BYTE_DATA, &[0xa5]);
    check(failures, cn >= 0, "claimed the stub's chip");
    check(failures, bus.smbus(&mut s) == 0, "SMBus write byte");
    let mut s = smbus_request(cn, sbus, STUB_ADDR, 1, 0x10, SMBUS_BYTE_DATA, &[]);
    let r = bus.smbus(&mut s);
    check(
        failures,
        r == 0 && s[16] == 0xa5,
        &format!("SMBus read byte: 0x{:02x}", s[16]),
    );

    let mut s = smbus_request(cn, sbus, STUB_ADDR, 0, 0x20, SMBUS_WORD_DATA, &[0x34, 0x12]);
    check(failures, bus.smbus(&mut s) == 0, "SMBus write word");
    let mut s = smbus_request(cn, sbus, STUB_ADDR, 1, 0x20, SMBUS_WORD_DATA, &[]);
    let r = bus.smbus(&mut s);
    check(
        failures,
        r == 0 && s[16] == 0x34 && s[17] == 0x12,
        "SMBus read word",
    );

    let mut s = smbus_request(
        cn,
        sbus,
        STUB_ADDR,
        0,
        0x30,
        SMBUS_I2C_BLOCK_DATA,
        &[4, 1, 2, 3, 4],
    );
    check(failures, bus.smbus(&mut s) == 0, "SMBus write block");
    let mut s = smbus_request(cn, sbus, STUB_ADDR, 1, 0x30, SMBUS_I2C_BLOCK_DATA, &[4]);
    let r = bus.smbus(&mut s);
    check(
        failures,
        r == 0 && s[16] == 4 && s[17] == 1 && s[20] == 4,
        "SMBus read block",
    );

    // A block past 32 bytes: the same request with the length byte at 33.
    s[16] = 33;
    check(
        failures,
        bus.smbus(&mut s) == -libc::EINVAL,
        "a block past 32 bytes: EINVAL",
    );
    let mut s = smbus_request(cn, sbus, STUB_ADDR, 0, 0, 99, &[]);
    check(
        failures,
        bus.smbus(&mut s) == -libc::EINVAL,
        "an unknown protocol: EINVAL",
    );

    let byte = [0u8; 1];
    let g = i2c_segment(addr(&byte), 1, 0);
    check(
        failures,
        bus.i2c(cn, sbus, STUB_ADDR, addr(&g), 1) == -libc::EOPNOTSUPP,
        "plain transfer on an adapter without them: EOPNOTSUPP",
    );
    drop(c.file);
    let mut s = smbus_request(cn, sbus, STUB_ADDR, 1, 0, SMBUS_BYTE_DATA, &[]);
    check(
        failures,
        bus.smbus(&mut s) == -libc::EPERM,
        "after the claim closes: EPERM",
    );
}

fn check_i2c(bus: &Bus, failures: &mut u32) {
    let big = vec![0u8; BUFFER + 2];
    let c = bus.claim_i2c(TEST_BUS, MEM_ADDR);
    let cn = c.fd;
    let reg_and_data = [0x10u8, 0xde, 0xad, 0xbe, 0xef];
    let reg = [0x10u8];
    let mut got = [0u8; 4];
    let write = i2c_segment(addr(&reg_and_data), 5, 0);
    let write_read = {
        let mut v = i2c_segment(addr(&reg), 1, 0);
        v.extend(i2c_segment(addr_mut(&mut got), 4, I2C_READ));
        v
    };
    check(failures, cn >= 0, "claimed the target");
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&write), 1) == 0,
        "write",
    );
    let r = bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&write_read), 2);
    check(
        failures,
        r == 0 && got[0] == 0xde && got[3] == 0xef,
        &format!(
            "write then read in one transfer: {:02x} {:02x} {:02x} {:02x}",
            got[0], got[1], got[2], got[3]
        ),
    );

    let wrong = bus.claim_i2c(TEST_BUS, 0x52);
    let other = bus.claim("not-a-bus");
    let dupe = c.file.as_ref().and_then(|f| f.try_clone().ok());
    let dupe_fd = dupe.as_ref().map_or(-1, AsRawFd::as_raw_fd);
    let devnull = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open("/dev/null")
        .ok();
    let devnull_fd = devnull.as_ref().map_or(-1, AsRawFd::as_raw_fd);

    check(
        failures,
        bus.i2c(-1, TEST_BUS, MEM_ADDR, addr(&write), 1) == -libc::EPERM,
        "no claim: EPERM",
    );
    check(
        failures,
        bus.i2c(devnull_fd, TEST_BUS, MEM_ADDR, addr(&write), 1) == -libc::EPERM,
        "a file that isn't a claim: EPERM",
    );
    check(
        failures,
        bus.i2c(wrong.fd, TEST_BUS, MEM_ADDR, addr(&write), 1) == -libc::EPERM,
        "a claim on another target: EPERM",
    );
    check(
        failures,
        bus.i2c(other.fd, TEST_BUS, MEM_ADDR, addr(&write), 1) == -libc::EPERM,
        "a claim on something else: EPERM",
    );
    check(
        failures,
        bus.i2c(dupe_fd, TEST_BUS, MEM_ADDR, addr(&write), 1) == 0,
        "a copy of the claim works",
    );
    drop(dupe);
    drop(devnull);
    drop(other.file);
    drop(wrong.file);

    // The module refuses a transfer of no segments itself. The I2C core refuses one too, with the
    // same errno, but it warns as it does; so the errno alone doesn't show the module's guard.
    let warnings = log_lines(b"WARNING:");
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&write), 0) == -libc::EINVAL,
        "no segments: EINVAL",
    );
    let after = log_lines(b"WARNING:");
    check(
        failures,
        warnings.is_some() && after == warnings,
        &format!(
            "no segments: the transfer never reached the I2C core (kernel warnings {warnings:?} -> {after:?})"
        ),
    );
    check(
        failures,
        bus.i2c(-1, TEST_BUS, MEM_ADDR, addr(&write), 0) == -libc::EINVAL,
        "no segments and no claim: EINVAL, before the claim is looked at",
    );
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&write), MAX_SEGMENTS + 1) == -libc::EINVAL,
        "too many segments: EINVAL",
    );
    let huge = i2c_segment(addr(&big), (BUFFER + 1) as u32, 0);
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&huge), 1) == -libc::EMSGSIZE,
        "more than the buffer: EMSGSIZE",
    );
    let mut half = i2c_segment(addr(&big), (BUFFER / 2) as u32, 0);
    half.extend(i2c_segment(addr(&big), (BUFFER / 2) as u32, 0));
    half.extend(i2c_segment(addr(&big), 1, 0));
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&half), 3) == -libc::EMSGSIZE,
        "segments that sum past it: EMSGSIZE",
    );
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&half), 2) == 0,
        "segments that fill it exactly",
    );
    let mut bad = i2c_segment(16, 4, 0);
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&bad), 1) == -libc::EFAULT,
        "a bad pointer: EFAULT",
    );
    put32(&mut bad, 12, 0x8000);
    check(
        failures,
        bus.i2c(cn, TEST_BUS, MEM_ADDR, addr(&bad), 1) == -libc::EINVAL,
        "unknown segment flags: EINVAL",
    );
    drop(c.file);

    let c = bus.claim_i2c(TEST_BUS, HELD_ADDR);
    check(
        failures,
        bus.i2c(c.fd, TEST_BUS, HELD_ADDR, addr(&write), 1) == -libc::EBUSY,
        "an address a kernel driver holds: EBUSY",
    );
    drop(c.file);
    let c = bus.claim_i2c(TEST_BUS, 0x60);
    check(
        failures,
        bus.i2c(c.fd, TEST_BUS, 0x60, addr(&write), 1) == -libc::ENXIO,
        "nothing answers: ENXIO",
    );
    drop(c.file);
    let c = bus.claim_i2c(39, MEM_ADDR);
    check(
        failures,
        bus.i2c(c.fd, 39, MEM_ADDR, addr(&write), 1) == -libc::ENODEV,
        "no such adapter: ENODEV",
    );
    drop(c.file);
    let c = bus.claim("i2c-40:0x80");
    check(
        failures,
        bus.i2c(c.fd, TEST_BUS, 0x80, addr(&write), 1) == -libc::EINVAL,
        "an address past 7 bits: EINVAL",
    );
    drop(c.file);
}

/// A second device at the bound device's bus and chip select. The SPI core never registers two, so
/// the test buses' second device is told to say it is at chip select 0, and the module is asked to
/// bind it: it must refuse, and the device bound first must stay the one transfers reach.
fn check_spi_twin(
    bus: &Bus,
    failures: &mut u32,
    cn: i32,
    one: &[u8],
    tx: &[u8; 8],
    rx: &mut [u8; 8],
) {
    let over = format!("/sys/bus/spi/devices/{TWIN}/driver_override");
    let bound = format!("/sys/bus/spi/devices/{TWIN}/driver");
    check(
        failures,
        put(TWIN_CS, "0") == 0 && put(&over, "lemnos") == 0,
        "the second device says it is at chip select 0, and may be bound to the module",
    );
    let r = put("/sys/bus/spi/drivers/lemnos/bind", TWIN);
    check(
        failures,
        r == -libc::EBUSY && fs::symlink_metadata(&bound).is_err(),
        &format!("a second device at a bound bus and chip select is refused: EBUSY ({r})"),
    );
    rx.fill(0);
    let r = bus.spi(cn, 0, 0, addr(one), 1);
    let mut info = [0u8; 16];
    put32(&mut info, 0, TEST_BUS);
    let i = bus.call(SPI_INFO, &mut info);
    check(
        failures,
        r == 0 && rx == tx && i == 0,
        &format!("the device bound first still transfers (transfer {r}, info {i})"),
    );
    // Back as it was, whatever happened: unbound, no override, at chip select 1.
    if fs::symlink_metadata(&bound).is_ok() {
        let _ = put("/sys/bus/spi/drivers/lemnos/unbind", TWIN);
    }
    let cleared = put(&over, "\n") == 0 && put(TWIN_CS, "1") == 0;
    put32(&mut info, 4, 1);
    check(
        failures,
        cleared && bus.call(SPI_INFO, &mut info) == -libc::ENODEV,
        "the second device is back at chip select 1, unbound",
    );
}

fn check_spi(bus: &Bus, failures: &mut u32) {
    let big = vec![0u8; BUFFER + 1];
    let c = bus.claim("spi-40.0");
    let cn = c.fd;
    let tx = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let mut rx = [0u8; 8];
    let mut rx2 = [0u8; 2];
    let one = spi_segment(addr(&tx), addr_mut(&mut rx), tx.len() as u32, 0);
    let mut two = spi_segment(addr(&tx), 0, 2, 0);
    two.extend(spi_segment(addr(&tx) + 6, addr_mut(&mut rx2), 2, 5));

    check(failures, cn >= 0, "claimed the device");
    let r = bus.spi(cn, 0, 0, addr(&one), 1);
    check(
        failures,
        r == 0 && rx == tx,
        "full duplex: 8 bytes came back",
    );
    let r = bus.spi(cn, 0, 0, addr(&two), 2);
    check(
        failures,
        r == 0 && rx2[0] == 7 && rx2[1] == 8,
        &format!("two segments: {} {}", rx2[0], rx2[1]),
    );
    check(
        failures,
        bus.spi(cn, 0, 3, addr(&one), 1) == 0,
        "mode 3 applied",
    );

    let mut info = [0u8; 16];
    put32(&mut info, 0, TEST_BUS);
    put32(&mut info, 4, 0);
    let r = bus.call(SPI_INFO, &mut info);
    check(
        failures,
        r == 0 && get32(&info, 8) == 3,
        &format!(
            "info: bound, and in the mode last applied ({})",
            get32(&info, 8)
        ),
    );
    put32(&mut info, 4, 1);
    let r = bus.call(SPI_INFO, &mut info);
    check(
        failures,
        r == -libc::ENODEV,
        "info on a device not bound here: ENODEV",
    );
    check(
        failures,
        bus.spi(cn, 0, 0x1000, addr(&one), 1) == -libc::EINVAL,
        "an unknown mode bit: EINVAL",
    );
    check(
        failures,
        bus.spi(-1, 0, 0, addr(&one), 1) == -libc::EPERM,
        "no claim: EPERM",
    );
    let neither = spi_segment(0, 0, 4, 0);
    check(
        failures,
        bus.spi(cn, 0, 0, addr(&neither), 1) == -libc::EINVAL,
        "a length with no buffer: EINVAL",
    );
    check(
        failures,
        bus.spi(cn, 0, 0, addr(&one), 0) == -libc::EINVAL,
        "no segments: EINVAL",
    );
    // The SPI core refuses an empty message with the same errno, so the errno alone doesn't show the
    // module's guard. What does: the module applies a transfer's mode before it hands the message
    // on, and a transfer of no segments must not get that far.
    put32(&mut info, 4, 0);
    let r = bus.call(SPI_INFO, &mut info);
    let mode = get32(&info, 8);
    let r2 = bus.spi(cn, 0, mode ^ 1, addr(&one), 0);
    let r3 = bus.call(SPI_INFO, &mut info);
    check(
        failures,
        r == 0 && r2 == -libc::EINVAL && r3 == 0 && get32(&info, 8) == mode,
        &format!(
            "no segments, another mode: EINVAL ({r2}), and the device keeps its mode ({mode} -> {})",
            get32(&info, 8)
        ),
    );
    check(
        failures,
        bus.spi(-1, 0, 0, addr(&one), 0) == -libc::EINVAL,
        "no segments and no claim: EINVAL, before the claim is looked at",
    );
    let huge = spi_segment(addr(&big), 0, (BUFFER + 1) as u32, 0);
    check(
        failures,
        bus.spi(cn, 0, 0, addr(&huge), 1) == -libc::EMSGSIZE,
        "more than the buffer: EMSGSIZE",
    );
    let exact = spi_segment(addr(&big), 0, BUFFER as u32, 0);
    check(
        failures,
        bus.spi(cn, 0, 0, addr(&exact), 1) == 0,
        "exactly the buffer",
    );
    let bad = spi_segment(16, 0, 4, 0);
    check(
        failures,
        bus.spi(cn, 0, 0, addr(&bad), 1) == -libc::EFAULT,
        "a bad pointer: EFAULT",
    );
    check_spi_twin(bus, failures, cn, &one, &tx, &mut rx);
    drop(c.file);

    let c = bus.claim("spi-40.1");
    check(
        failures,
        bus.spi(c.fd, 1, 0, addr(&one), 1) == -libc::ENODEV,
        "a device not bound here: ENODEV",
    );
    drop(c.file);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let node = args.get(1).map_or("/dev/lemnos", String::as_str);
    let fd = match OpenOptions::new()
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
    let bus = Bus { node: fd };
    let mut failures = 0;
    check_smbus(&bus, &mut failures);
    check_i2c(&bus, &mut failures);
    check_spi(&bus, &mut failures);
    say(&format!("{node}: {failures} failed"));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
