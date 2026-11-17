//! The PIO interface against the test harness's stand-in for the driver: the allowed operations,
//! every refused index, DMA moved in pieces through the module's buffers, and a transfer that never
//! finishes. The `board` form is for real hardware that others may be using: it takes a free
//! machine, touches no pin, loads a three-instruction loopback and checks words and DMA come back
//! through it, then kills a holder with a transfer stuck on its machine.
//!
//! Usage: `lemnos-pio-check /dev/lemnos` (the harness) or `lemnos-pio-check /dev/lemnos board`.
//! Port of `kernel/test/lemnos_pio_check.c` (CZ-GUEST-2). The uAPI is laid out in bytes as the C
//! check lays out its structures (`kernel/lemnos.h`): `lemnos_pio_open`, `lemnos_pio_call`,
//! `lemnos_pio_config_xfer` and `lemnos_pio_xfer`, and the operation arguments the C file declares
//! (`sm_init`, `sm_word`, `sm_enable`, and the program). The buffers the transfers move are the
//! process's own, and the kernel reads and writes them at the addresses the request names, so they
//! never move while a transfer is in flight.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use lemnos_linux_sys::guest;
use lemnos_linux_sys::module;
use lemnos_linux_sys::privilege::{self, ChildEnd};
use lemnos_linux_sys::{iow, iowr};

const PARAM: &str = "/sys/module/rp1_pio_mock/parameters/";

// lemnos.h: the requests on the control node.
const PIO_OPEN: u32 = iow::<[u8; 4]>(b'L', 0x90);
const PIO_CALL: u32 = iowr::<[u8; 88]>(b'L', 0x91);
const PIO_CONFIG_XFER: u32 = iow::<[u8; 24]>(b'L', 0x92);
const PIO_XFER: u32 = iow::<[u8; 32]>(b'L', 0x93);
const _: () = assert!(
    PIO_OPEN == 0x4004_4c90
        && PIO_CALL == 0xc058_4c91
        && PIO_CONFIG_XFER == 0x4018_4c92
        && PIO_XFER == 0x4020_4c93
);

// lemnos.h: `enum lemnos_pio_op`, in the order the header declares it.
const SM_CLAIM: u32 = 2;
const SM_INIT: u32 = 4;
const SM_SET_ENABLED: u32 = 11;
const SM_PUT: u32 = 13;
const SM_GET: u32 = 14;
const ADD_PROGRAM: u32 = 0;
const GPIO_INIT: u32 = 18;
const OP_COUNT: u32 = 23;

// lemnos.h: the directions, and the size of a call's argument area.
const TO_SM: u32 = 0;
const FROM_SM: u32 = 1;
const ARGS: usize = 72;
const CALL_BYTES: usize = 88;

/// The loopback: pull block; mov isr, osr; push block. No pin is read or driven.
const LOOPBACK: [u16; 3] = [0x80a0, 0xa0c7, 0x8020];

const IN_LEN: u32 = 65536;
const OUT_LEN: u32 = 65536;

const EPERM: i32 = libc::EPERM;
const EBUSY: i32 = libc::EBUSY;
const EINVAL: i32 = libc::EINVAL;
const EFAULT: i32 = libc::EFAULT;
const EIO: i32 = libc::EIO;
const ETIMEDOUT: i32 = libc::ETIMEDOUT;

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Prints one check's line and counts a failure.
fn check(failures: &mut u32, ok: bool, what: &str) {
    say(&format!("{}{what}", if ok { "ok   " } else { "FAIL " }));
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

fn get64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_ne_bytes(v)
}

/// The address of a buffer the kernel reads or writes, as a request names it.
fn addr_of(buf: &mut [u8]) -> u64 {
    buf.as_mut_ptr() as u64
}

/// `strerror`: the text of an errno, as the C check's musl `strerror` gives it.
fn strerror(errno: i32) -> String {
    let text = io::Error::from_raw_os_error(errno).to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

/// The errno a request failed with, as the C code's `-errno`, or 0 when it succeeded.
fn rc_of(result: io::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => -e.raw_os_error().unwrap_or(EIO),
    }
}

fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EIO)
}

fn open_node(path: &str) -> io::Result<OwnedFd> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
        .map(OwnedFd::from)
}

/// A claim on `name`, as the C `claim()` gives it: the claim's number (negative errno when refused)
/// and the file that holds it, which closes the claim when dropped.
fn claim(node: &OwnedFd, name: &str) -> (i32, Option<OwnedFd>) {
    match module::claim(node.as_fd(), name) {
        Ok(fd) => (fd.as_raw_fd(), Some(fd)),
        Err(e) => (-errno_of(&e), None),
    }
}

/// `LEMNOS_IOC_PIO_OPEN`: binds a client to `claim`. 0 or -errno.
fn open_client(node: &OwnedFd, claim: i32) -> i32 {
    let mut o = claim.to_ne_bytes();
    rc_of(guest::ioctl_bytes(node.as_fd(), PIO_OPEN, &mut o))
}

/// The C `call()`: `rc` is 0 or -errno, `result` the operation's return value, `args` the argument
/// area as the kernel left it.
struct Reply {
    rc: i32,
    result: i64,
    args: [u8; ARGS],
}

/// `LEMNOS_IOC_PIO_CALL`: runs `op` for `claim` with `args` (at most 72 bytes; the rest is zero).
fn pio_call(node: &OwnedFd, claim: i32, op: u32, args: &[u8]) -> Reply {
    let mut p = [0u8; CALL_BYTES];
    put32(&mut p, 0, claim as u32);
    put32(&mut p, 4, op);
    p[16..16 + args.len()].copy_from_slice(args);
    let rc = rc_of(guest::ioctl_bytes(node.as_fd(), PIO_CALL, &mut p));
    let mut out = [0u8; ARGS];
    out.copy_from_slice(&p[16..]);
    Reply {
        rc,
        result: get64(&p, 8) as i64,
        args: out,
    }
}

/// `LEMNOS_IOC_PIO_CONFIG_XFER`: sets a machine's DMA up. 0 or -errno.
fn config_xfer(node: &OwnedFd, claim: i32, sm: u32, dir: u32, size: u32, count: u32) -> i32 {
    let mut x = [0u8; 24];
    put32(&mut x, 0, claim as u32);
    put32(&mut x, 4, sm);
    put32(&mut x, 8, dir);
    put32(&mut x, 12, size);
    put32(&mut x, 16, count);
    rc_of(guest::ioctl_bytes(node.as_fd(), PIO_CONFIG_XFER, &mut x))
}

/// `LEMNOS_IOC_PIO_XFER`: moves `len` bytes at `data` (a caller address) through the machine.
/// 0 or -errno.
fn xfer(
    node: &OwnedFd,
    claim: i32,
    sm: u32,
    dir: u32,
    data: u64,
    len: u32,
    timeout_ms: u32,
) -> i32 {
    let mut x = [0u8; 32];
    put32(&mut x, 0, claim as u32);
    put32(&mut x, 4, sm);
    put32(&mut x, 8, dir);
    put32(&mut x, 12, len);
    put64(&mut x, 16, data);
    put32(&mut x, 24, timeout_ms);
    rc_of(guest::ioctl_bytes(node.as_fd(), PIO_XFER, &mut x))
}

/// `param()` of the C check: the module parameter's integer, or -999 when it cannot be read.
fn param(name: &str) -> i32 {
    let mut text = [0u8; 15];
    let n = File::open(format!("{PARAM}{name}"))
        .and_then(|mut f| f.read(&mut text))
        .unwrap_or(0);
    parse_int(&text[..n]).unwrap_or(-999)
}

/// What `sscanf("%d")` reads from the start of `text`: leading white space, an optional sign, digits.
fn parse_int(text: &[u8]) -> Option<i32> {
    let mut i = 0;
    while i < text.len() && text[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = i < text.len() && text[i] == b'-';
    if i < text.len() && (text[i] == b'-' || text[i] == b'+') {
        i += 1;
    }
    let start = i;
    let mut v: i32 = 0;
    while i < text.len() && text[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add(i32::from(text[i] - b'0'));
        i += 1;
    }
    if i == start {
        return None;
    }
    Some(if negative { -v } else { v })
}

/// `set_sticky()` of the C check: 0 when the stand-in took the value, -1 when it did not.
fn set_sticky(value: &str) -> i32 {
    let wrote = OpenOptions::new()
        .write(true)
        .open(format!("{PARAM}sticky_sm"))
        .and_then(|mut f| f.write(value.as_bytes()))
        .is_ok_and(|n| n > 0);
    if wrote { 0 } else { -1 }
}

/// A `u16[8]` with its first element `first` and the rest zero, as the C check declares its masks.
fn u16x8(first: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    put16(&mut b, 0, first);
    b
}

/// `struct { uint16_t sm; uint8_t blocking, rsvd; uint32_t data; }` (8 bytes).
fn sm_word(sm: u16, blocking: u8, data: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    put16(&mut b, 0, sm);
    b[2] = blocking;
    put32(&mut b, 4, data);
    b
}

/// `struct { uint16_t n, origin, instrs[32]; }` (68 bytes), a program to add.
struct Program {
    n: u16,
    origin: u16,
    instrs: [u16; 32],
}

impl Program {
    fn bytes(&self) -> [u8; 68] {
        let mut b = [0u8; 68];
        put16(&mut b, 0, self.n);
        put16(&mut b, 2, self.origin);
        for (i, v) in self.instrs.iter().enumerate() {
            put16(&mut b, 4 + 2 * i, *v);
        }
        b
    }
}

/// `struct sm_init` (20 bytes).
fn sm_init(
    sm: u16,
    initial_pc: u16,
    clkdiv: u32,
    execctrl: u32,
    shiftctrl: u32,
    pinctrl: u32,
) -> [u8; 20] {
    let mut b = [0u8; 20];
    put16(&mut b, 0, sm);
    put16(&mut b, 2, initial_pc);
    put32(&mut b, 4, clkdiv);
    put32(&mut b, 8, execctrl);
    put32(&mut b, 12, shiftctrl);
    put32(&mut b, 16, pinctrl);
    b
}

/// `struct sm_enable` (4 bytes).
fn sm_enable(mask: u16, enable: u8) -> [u8; 4] {
    let mut b = [0u8; 4];
    put16(&mut b, 0, mask);
    b[2] = enable;
    b
}

/// The machine's claim: the claim's number, its file (which closes it when dropped), and the
/// control node the claim is made on. The file drops before the node, as the C code closes them.
struct Holder {
    _claim_file: Option<OwnedFd>,
    claim: i32,
    node: OwnedFd,
}

/// Opens the node, claims pio-0, opens a client, takes a free machine and loads the loopback on it,
/// running. `sm` and `origin` say where (they keep the values they had when setup stopped). An
/// error is the errno the failing step left, or 0 when the step failed without one.
fn board_setup(node_path: &str, sm: &mut i32, origin: &mut i32) -> Result<Holder, i32> {
    let node = open_node(node_path).map_err(|e| errno_of(&e))?;
    let (c, claim_file) = claim(&node, "pio-0");
    if c < 0 {
        return Err(-c);
    }
    let rc = open_client(&node, c);
    if rc != 0 {
        return Err(-rc);
    }
    let holder = Holder {
        _claim_file: claim_file,
        claim: c,
        node,
    };

    let any = [0u8; 16];
    let r = pio_call(&holder.node, c, SM_CLAIM, &any);
    if r.rc != 0 || !(0..=3).contains(&r.result) {
        return Err(-r.rc);
    }
    *sm = r.result as i32;

    let mut program = Program {
        n: 3,
        origin: 0xffff,
        instrs: [0; 32],
    };
    program.instrs[..LOOPBACK.len()].copy_from_slice(&LOOPBACK);
    let r = pio_call(&holder.node, c, ADD_PROGRAM, &program.bytes());
    if r.rc != 0 || r.result < 0 {
        return Err(-r.rc);
    }
    *origin = r.result as i32;

    // clkdiv 1.0; wrap from origin+2 back to origin; shift right, no autopush or autopull;
    // pinctrl 0: no pin in any group.
    let execctrl = (((*origin + 2) as u32) << 12) | ((*origin as u32) << 7);
    let init = sm_init(
        *sm as u16,
        *origin as u16,
        1u32 << 16,
        execctrl,
        (1u32 << 18) | (1u32 << 19),
        0,
    );
    let r = pio_call(&holder.node, c, SM_INIT, &init);
    if r.rc != 0 || r.result < 0 {
        return Err(-r.rc);
    }
    let enable = sm_enable(1u16.wrapping_shl(*sm as u32), 1);
    let r = pio_call(&holder.node, c, SM_SET_ENABLED, &enable);
    if r.rc != 0 || r.result < 0 {
        return Err(-r.rc);
    }
    Ok(holder)
}

/// The board form's child: holds a machine with a transfer stuck on it, reports the machine and
/// program offset on the pipe, and sends until it is killed. Its exit status says where it stopped.
fn board_child(node_path: &str, mut pipe: io::PipeWriter, out: &mut [u8]) -> i32 {
    let mut sm = -1;
    let mut origin = -1;
    let Ok(h) = board_setup(node_path, &mut sm, &mut origin) else {
        return 3;
    };
    if config_xfer(&h.node, h.claim, sm as u32, TO_SM, 4096, 4) != 0 {
        return 3;
    }
    let mut msg = [0u8; 8];
    msg[..4].copy_from_slice(&sm.to_ne_bytes());
    msg[4..].copy_from_slice(&origin.to_ne_bytes());
    if !matches!(pipe.write(&msg), Ok(8)) {
        return 4;
    }
    xfer(
        &h.node,
        h.claim,
        sm as u32,
        TO_SM,
        addr_of(out),
        OUT_LEN,
        60000,
    );
    5
}

/// The board form: a free machine and the loopback on it, words and DMA back through it, then a
/// holder killed with a transfer stuck on its machine, after which the machine is free again.
fn board(node_path: &str) -> ExitCode {
    let mut failures = 0;
    let mut result: i64 = -1;
    let mut sm = -1;
    let mut origin = -1;

    let setup = board_setup(node_path, &mut sm, &mut origin);
    check(
        &mut failures,
        setup.is_ok(),
        &format!("a free machine ({sm}) running the loopback at offset {origin}"),
    );
    let h = match setup {
        Ok(h) => h,
        Err(errno) => {
            say(&format!("{node_path}: setup failed ({})", strerror(errno)));
            return ExitCode::from(1);
        }
    };

    let mut words_ok = true;
    let mut times: Vec<u64> = Vec::with_capacity(2000);
    for i in 0..2000u32 {
        let data = 0x5a00_0000 | i;
        let t0 = Instant::now();

        let w = sm_word(sm as u16, 1, data);
        let r = pio_call(&h.node, h.claim, SM_PUT, &w);
        words_ok &= r.rc == 0;

        let back_req = sm_word(sm as u16, 1, 0);
        let r = pio_call(&h.node, h.claim, SM_GET, &back_req);
        result = r.result;
        words_ok &= r.rc == 0 && get32(&r.args, 4) == data;
        times.push(t0.elapsed().as_nanos() as u64);
    }
    times.sort_unstable();
    check(
        &mut failures,
        words_ok,
        "2000 words came back through the machine",
    );
    say(&format!(
        "measure put+get one word: median {} ns  p99 {} ns",
        times[1000], times[1980]
    ));

    check(
        &mut failures,
        config_xfer(&h.node, h.claim, sm as u32, TO_SM, 4096, 4) == 0
            && config_xfer(&h.node, h.claim, sm as u32, FROM_SM, 4096, 4) == 0,
        "DMA configured both ways",
    );
    let mut outbuf = vec![0u8; 65536];
    let mut inbuf = vec![0u8; 65536];
    for (i, b) in outbuf.iter_mut().enumerate() {
        *b = (i * 7 + 3) as u8;
    }
    for _ in 0..3 {
        inbuf.fill(0);
        let rx_addr = addr_of(&mut inbuf);
        let tx_addr = addr_of(&mut outbuf);
        let t0 = Instant::now();

        let node = &h.node;
        let claim = h.claim;
        let (tx_ret, rx_ret) = thread::scope(|s| {
            let rx = s.spawn(move || xfer(node, claim, sm as u32, FROM_SM, rx_addr, IN_LEN, 2000));
            let tx = s.spawn(move || xfer(node, claim, sm as u32, TO_SM, tx_addr, OUT_LEN, 2000));
            let tx_ret = tx.join().unwrap_or(-EIO);
            let rx_ret = rx.join().unwrap_or(-EIO);
            (tx_ret, rx_ret)
        });
        let t = t0.elapsed();

        check(
            &mut failures,
            tx_ret == 0 && rx_ret == 0 && inbuf == outbuf,
            &format!(
                "64 KiB out and back by DMA, in 4 KiB pieces: {}, {}, {} us",
                strerror(-tx_ret),
                strerror(-rx_ret),
                t.as_micros()
            ),
        );
    }
    let out_addr = addr_of(&mut outbuf);
    check(
        &mut failures,
        xfer(&h.node, h.claim, sm as u32, TO_SM, 16, 4, 0) == -EFAULT,
        "a bad pointer: EFAULT",
    );
    // Nothing reads the machine now: once its two FIFOs are full the send stalls.
    check(
        &mut failures,
        xfer(&h.node, h.claim, sm as u32, TO_SM, out_addr, 4096, 50) == -ETIMEDOUT,
        "a send nothing drains ends: ETIMEDOUT",
    );
    check(
        &mut failures,
        xfer(&h.node, h.claim, sm as u32, TO_SM, out_addr, 16, 50) == -EIO,
        "and the channel refuses after: EIO",
    );
    check(
        &mut failures,
        config_xfer(&h.node, h.claim, sm as u32, TO_SM, 4096, 4) == 0,
        "reconfiguring clears it",
    );
    drop(h);

    // A holder killed with a transfer stuck on its machine.
    let Ok((reader, writer)) = io::pipe() else {
        return ExitCode::from(2);
    };
    let Ok(child) = privilege::fork() else {
        return ExitCode::from(2);
    };
    if child == 0 {
        let code = board_child(node_path, writer, &mut outbuf);
        guest::exit_now(code);
    }
    drop(writer);

    let mut msg_bytes = [0xffu8; 8];
    let got = matches!(guest::read(reader.as_fd(), &mut msg_bytes), Ok(8));
    let msg = [
        i32::from_ne_bytes([msg_bytes[0], msg_bytes[1], msg_bytes[2], msg_bytes[3]]),
        i32::from_ne_bytes([msg_bytes[4], msg_bytes[5], msg_bytes[6], msg_bytes[7]]),
    ];
    check(
        &mut failures,
        got,
        &format!(
            "a child holds machine {}, program at {}, sending",
            msg[0], msg[1]
        ),
    );
    thread::sleep(Duration::from_millis(300));
    let _ = guest::kill(child, libc::SIGKILL);
    let killed = matches!(privilege::wait_child(child), Ok(ChildEnd::Signal(_)));
    check(&mut failures, killed, "the child was killed mid-transfer");

    let mut nsm = -1;
    let mut norigin = -1;
    let second = board_setup(node_path, &mut nsm, &mut norigin);
    check(
        &mut failures,
        second.is_ok(),
        &format!("a new holder got a machine ({nsm}) and program space ({norigin})"),
    );
    if let Ok(h) = second {
        // Its own machine is claimed already; the child's must be free unless it is the same.
        let claimed = if msg[0] == nsm {
            true
        } else {
            let r = pio_call(
                &h.node,
                h.claim,
                SM_CLAIM,
                &u16x8(1u32.wrapping_shl(msg[0] as u32) as u16),
            );
            result = r.result;
            r.rc == 0
        };
        check(
            &mut failures,
            claimed,
            &format!(
                "the killed child's machine {} can be claimed: {result}",
                msg[0]
            ),
        );
        drop(h);
    }
    say(&format!("{node_path} board: {failures} failed"));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// The harness form's child: a holder that takes machine 1 with a transfer stuck on it, and is
/// killed by the parent. Its exit status says where it stopped.
fn hold_machine(node_path: &str, out: &mut [u8]) -> i32 {
    let Ok(node) = open_node(node_path) else {
        return 3;
    };
    let (c, _claim_file) = claim(&node, "pio-0");
    if c < 0 || open_client(&node, c) != 0 || config_xfer(&node, c, 1, TO_SM, 4096, 4) != 0 {
        return 3;
    }
    xfer(&node, c, 1, TO_SM, addr_of(out), 16, 60000);
    4
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let node_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/dev/lemnos".to_owned());
    if args.get(2).is_some_and(|a| a == "board") {
        return board(&node_path);
    }
    let node = match open_node(&node_path) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = writeln!(io::stderr(), "{node_path}: {}", strerror(errno_of(&e)));
            return ExitCode::from(2);
        }
    };

    let mut failures = 0;
    let mut outbuf = vec![0u8; 10000];
    let mut inbuf = vec![0u8; 10000];

    check(
        &mut failures,
        open_client(&node, -1) == -EPERM,
        "no claim: EPERM",
    );
    let (wrong, wrong_file) = claim(&node, "not-pio");
    check(
        &mut failures,
        open_client(&node, wrong) == -EPERM,
        "the wrong claim: EPERM",
    );
    drop(wrong_file);

    let (c, claim_file) = claim(&node, "pio-0");
    check(
        &mut failures,
        pio_call(&node, c, SM_CLAIM, &[0u8; 16]).rc == -EPERM,
        "an operation before opening: EPERM",
    );
    check(&mut failures, open_client(&node, c) == 0, "client opened");
    check(
        &mut failures,
        open_client(&node, c) == -EBUSY,
        "opened twice: EBUSY",
    );
    check(
        &mut failures,
        param("clients") == 1,
        "the driver has one client",
    );

    let r = pio_call(&node, c, SM_CLAIM, &u16x8(1));
    check(&mut failures, r.rc == 0, "machine claimed");

    let mut program = Program {
        n: 2,
        origin: 0xffff,
        instrs: [0; 32],
    };
    let r = pio_call(&node, c, ADD_PROGRAM, &program.bytes());
    let result = r.result;
    check(
        &mut failures,
        r.rc == 0 && result == 5,
        &format!("program added at offset {result}"),
    );

    let word = sm_word(0, 0, 41);
    let r = pio_call(&node, c, SM_PUT, &word);
    check(&mut failures, r.rc == 0, "word put");
    let r = pio_call(&node, c, SM_GET, &word);
    let back = get32(&r.args, 4);
    check(
        &mut failures,
        r.rc == 0 && back == 42,
        &format!("word got back through the args: {back}"),
    );

    // Every index the driver would have to trust.
    check(
        &mut failures,
        pio_call(&node, c, SM_INIT, &u16x8(4)).rc == -EINVAL,
        "machine 4: EINVAL",
    );
    check(
        &mut failures,
        pio_call(&node, c, SM_CLAIM, &u16x8(16)).rc == -EINVAL,
        "a mask past 4 machines: EINVAL",
    );
    check(
        &mut failures,
        pio_call(&node, c, GPIO_INIT, &u16x8(28)).rc == -EINVAL,
        "GPIO 28: EINVAL",
    );
    program.n = 33;
    check(
        &mut failures,
        pio_call(&node, c, ADD_PROGRAM, &program.bytes()).rc == -EINVAL,
        "a program of 33 instructions: EINVAL",
    );
    program.n = 0;
    check(
        &mut failures,
        pio_call(&node, c, ADD_PROGRAM, &program.bytes()).rc == -EINVAL,
        "an empty program: EINVAL",
    );
    check(
        &mut failures,
        pio_call(&node, c, OP_COUNT, &u16x8(1)).rc == -EINVAL,
        "an operation past the table: EINVAL",
    );

    // DMA.
    check(
        &mut failures,
        config_xfer(&node, c, 4, 0, 4096, 4) == -EINVAL,
        "DMA on machine 4: EINVAL",
    );
    check(
        &mut failures,
        config_xfer(&node, c, 0, 2, 4096, 4) == -EINVAL,
        "a direction of 2: EINVAL",
    );
    check(
        &mut failures,
        config_xfer(&node, c, 0, 0, 0, 4) == -EINVAL,
        "a buffer of 0 bytes: EINVAL",
    );
    check(
        &mut failures,
        config_xfer(&node, c, 0, 0, 1 << 19, 4) == -EINVAL,
        "a buffer past 256 KiB: EINVAL",
    );
    check(
        &mut failures,
        config_xfer(&node, c, 0, 0, 4096, 65) == -EINVAL,
        "more than 64 buffers: EINVAL",
    );
    let out_addr = addr_of(&mut outbuf);
    check(
        &mut failures,
        xfer(&node, c, 0, 0, out_addr, 4, 0) == -EINVAL,
        "a transfer before configuring: EINVAL",
    );
    check(
        &mut failures,
        config_xfer(&node, c, 0, 0, 4096, 4) == 0 && config_xfer(&node, c, 0, 1, 4096, 4) == 0,
        "DMA configured",
    );
    for (i, b) in outbuf.iter_mut().enumerate() {
        *b = (i * 3) as u8;
    }
    // The stand-in keeps only the last piece: send and read one piece's worth.
    let out_addr = addr_of(&mut outbuf);
    check(
        &mut failures,
        xfer(&node, c, 0, 0, out_addr, 4096, 0) == 0,
        "sent one buffer",
    );
    let in_addr = addr_of(&mut inbuf);
    check(
        &mut failures,
        xfer(&node, c, 0, 1, in_addr, 4096, 0) == 0 && inbuf[..4096] == outbuf[..4096],
        "the same bytes came back",
    );
    let out_addr = addr_of(&mut outbuf);
    check(
        &mut failures,
        xfer(&node, c, 0, 0, out_addr, 10000, 0) == 0,
        "sent more than a buffer, in pieces",
    );
    check(
        &mut failures,
        xfer(&node, c, 0, 0, 16, 4, 0) == -EFAULT,
        "a bad pointer: EFAULT",
    );
    check(
        &mut failures,
        param("last_addr_nonzero") == 0,
        "the driver was never given an address",
    );

    // A transfer that never finishes: refused from then on, until reconfigured.
    check(
        &mut failures,
        set_sticky("1") == 0 && config_xfer(&node, c, 1, 0, 4096, 4) == 0,
        "machine 1 will hang",
    );
    let out_addr = addr_of(&mut outbuf);
    check(
        &mut failures,
        xfer(&node, c, 1, 0, out_addr, 16, 20) == -ETIMEDOUT,
        "the wait ends: ETIMEDOUT",
    );
    check(
        &mut failures,
        xfer(&node, c, 1, 0, out_addr, 16, 20) == -EIO,
        "a stuck channel refuses: EIO",
    );
    check(
        &mut failures,
        set_sticky("-1") == 0
            && config_xfer(&node, c, 1, 0, 4096, 4) == 0
            && xfer(&node, c, 1, 0, out_addr, 16, 0) == 0,
        "reconfiguring clears it",
    );

    drop(claim_file);
    check(
        &mut failures,
        param("clients") == 0,
        "closing the claim closed the client",
    );
    drop(node);

    // A holder killed while its transfer hangs, with a long timeout: it ends at once.
    check(
        &mut failures,
        set_sticky("1") == 0,
        "machine 1 will hang again",
    );
    let child = match privilege::fork() {
        Ok(pid) => pid,
        Err(_) => return ExitCode::from(2),
    };
    if child == 0 {
        let code = hold_machine(&node_path, &mut outbuf);
        guest::exit_now(code);
    }

    thread::sleep(Duration::from_millis(200));
    let t0 = Instant::now();
    let _ = guest::kill(child, libc::SIGKILL);
    let killed = matches!(privilege::wait_child(child), Ok(ChildEnd::Signal(_)));
    let ms = t0.elapsed().as_millis();
    check(
        &mut failures,
        killed && ms < 2000,
        &format!("a holder killed mid-transfer ends at once: {ms} ms"),
    );
    check(
        &mut failures,
        set_sticky("-1") == 0 && param("clients") == 0,
        "and its client is closed",
    );
    say(&format!("{node_path}: {failures} failed"));
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
