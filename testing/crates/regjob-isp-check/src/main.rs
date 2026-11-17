//! Register jobs on the fake ISP (test module `lemnos_test_regjob`), run with the K230 ISP's
//! profile: a capture path that keeps writing its buffers between jobs. Every job here is started
//! without waiting, so the whole check runs on one thread: the block is driven by writes to the
//! fake block's sysfs controls, and the job is collected with WAIT.
//!
//! The first frames switch the block to the caller's buffers at once on an idle block, and at the
//! end of a frame on a running one; buffers stay held until the switch, and stay held when the
//! block did not take the new addresses. Then the refusals; the configuration role beside the
//! frames role (its own table, the shared isp_ctrl merged, the ISP core's interrupt as events, a
//! second session refused); a frame that never comes (ETIMEDOUT, the block reset, the configuration
//! role told); a session closed on a running path (the block is reset before anything is
//! released); module-allocated memory imported into a frames session; and the configuration role
//! alone.
//!
//! Usage: `lemnos-regjob-isp-check /dev/lemnos <device> <heap>`. Port of
//! `kernel/test/lemnos_regjob_isp_check.c` (CZ-GUEST-2). The uAPI is laid out in bytes as the C
//! check lays out its structures (`kernel/lemnos_regjob.h`, `kernel/lemnos.h`). The job's buffers
//! are the process's own, and the kernel reads and writes them at the addresses the job names, so
//! they never move while a job is in flight. Static musl.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::process::ExitCode;
use std::time::Duration;

use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::guest;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::module;
use lemnos_linux_sys::time::monotonic_ns;
use lemnos_linux_sys::{ior, iow, iowr};

// lemnos_regjob.h: the requests on the control node and on a session.
const IMPORT: u32 = iowr::<[u8; 24]>(b'L', 0xf1);
const RELEASE: u32 = iow::<[u8; 4]>(b'L', 0xf2);
const RUN: u32 = iowr::<[u8; 64]>(b'L', 0xf3);
const INFO: u32 = ior::<[u8; 48]>(b'L', 0xf4);
const WAIT: u32 = iowr::<[u8; 24]>(b'L', 0xf5);
const WRITE: u32 = iow::<[u8; 32]>(b'L', 0xf6);
const EVENT: u32 = ior::<[u8; 16]>(b'L', 0xf7);

const _: () = assert!(
    IMPORT == 0xc018_4cf1
        && RELEASE == 0x4004_4cf2
        && RUN == 0xc040_4cf3
        && INFO == 0x8030_4cf4
        && WAIT == 0xc018_4cf5
        && WRITE == 0x4020_4cf6
        && EVENT == 0x8010_4cf7
);

// lemnos_regjob.h: the values the check names.
const PROFILE_K230_ISP: u32 = 2;
const OPEN_CONFIG: u32 = 1 << 0;
const RUN_NOWAIT: u32 = 1 << 0;
const EVENTS: usize = 64;
const EVENT_RESET: u32 = 1 << 31;
const UNLATCHED: u32 = 1 << 31;

// struct lemnos_regjob_run (64 bytes), at the offsets the C check names.
const RUN_WRITES: usize = 0;
const RUN_ADDRS: usize = 8;
const RUN_READS: usize = 16;
const RUN_VALUES: usize = 24;
const RUN_NWRITES: usize = 32;
const RUN_NADDRS: usize = 36;
const RUN_NREADS: usize = 40;
const RUN_START: usize = 44;
const RUN_TIMEOUT: usize = 48;
const RUN_FLAGS: usize = 56;

// The ISP's registers (kernel/lemnos_regjob_k230isp.c).
const ISP_CTRL: u32 = 0x0400;
const ISP_ACQ_PROP: u32 = 0x0404;
const ISP_ACQ_H_SIZE: u32 = 0x0410;
const ISP_AWB_PROP: u32 = 0x0510;
const ISP_IMSC: u32 = 0x05bc;
const ISP_MIS: u32 = 0x05c4;
const ISP_ICR: u32 = 0x05c8;
const MI_CTRL: u32 = 0x1300;
const MI_MP_CTRL: u32 = 0x1310;
const MI_MP_FMT: u32 = 0x1314;
const MI_MP_Y_BASE: u32 = 0x1324;
const MI_MP_Y_SIZE: u32 = 0x1328;
const MI_MP_Y_OFFS: u32 = 0x132c;
const MI_MP_Y_LLENGTH: u32 = 0x1330;
const MI_MP_CB_BASE: u32 = 0x1340;
const MI_MP_CR_BASE: u32 = 0x134c;
const MI_MP_Y_BASE2: u32 = 0x1358;
const MI_MP_Y_BASE_SHD: u32 = 0x1370;
const MI_MP_RAW_BASE: u32 = 0x1394;
const MI_MP_RAW_SIZE: u32 = 0x1398;
const MI_MP_RAW_OFFS: u32 = 0x139c;
const MI_MP_RAW_BASE_SHD: u32 = 0x13b4;
const MI_MP_JDP_BASE: u32 = 0x13c0;
const MI_IMSC: u32 = 0x16c0;
const MI_MIS: u32 = 0x16d0;
const MI_RIS: u32 = 0x16e0;
const AE_MEAN_0: u32 = 0x2614;
const LSC_CTRL: u32 = 0x2200;
const LSC_R_TABLE_ADDR: u32 = 0x2204;
const LSC_R_TABLE_DATA: u32 = 0x2214;
const LSC_XSIZE_0: u32 = 0x2244;
const LSC_TABLE_SEL: u32 = 0x2264;

const WINDOW: usize = 0x9000;
const PLANES: usize = 3; // Y, Cb, Cr
const SLOTS: usize = 4; // and raw
const BUF_BYTES: u64 = 64 * 1024;
const Y_BYTES: u32 = 32768;
const C_BYTES: u32 = 16384;
const FRAME_END: u32 = 1;
const RAW_FRAME_END: u32 = 2;
const WRAP_Y: u32 = 0x100;
const CTRL_ON: u32 = 0x17; // enable, Bayer RGB mode, input formatter
const CTRL_GAMMA: u32 = 0x800; // the configuration role's gamma-out enable
const FRAME_OUT: u32 = 2; // the ISP core's frame-out interrupt

/// The raw output's slot is pointed at the Y plane's range: the YCbCr output doesn't use it.
const PLANE_REG: [u32; SLOTS] = [MI_MP_Y_BASE, MI_MP_CB_BASE, MI_MP_CR_BASE, MI_MP_RAW_BASE];
const PLANE_OFF: [u32; SLOTS] = [0, Y_BYTES, Y_BYTES, 0];
const PLANE_LEN: [u32; SLOTS] = [Y_BYTES, C_BYTES, C_BYTES, Y_BYTES];

/// The writes of a frame job: the ISP on, its acquisition window and the main path's format.
const FRAME_WRITES: [(u32, u32); 5] = [
    (ISP_CTRL, 0x1),
    (ISP_ACQ_PROP, 0),
    (ISP_ACQ_H_SIZE, 256),
    (MI_MP_FMT, 0x0),
    (MI_MP_Y_LLENGTH, 256),
];

/// `struct lemnos_regjob_info` (48 bytes): the fields the check reads.
#[derive(Clone, Copy)]
struct Info {
    wedged: u32,
    live: u32,
    jobs: u64,
    refused: u64,
}

/// `struct lemnos_regjob_event` (16 bytes), as the check reads it.
#[derive(Clone, Copy)]
struct Event {
    err: i32,
    status: u32,
    lost: u32,
    at: u64,
}

/// What WAIT gave back: the errno, the status, and the time the handler completed the job.
struct Collected {
    err: i32,
    status: u32,
    at: u64,
}

/// A buffer the session imported: its dma-buf (kept open, as the C check keeps it), its index for
/// the job, its device address, and the CPU's mapping of it (none for module memory).
#[derive(Default)]
struct Buf {
    fd: Option<OwnedFd>,
    index: u32,
    addr: u64,
    map: Option<Mapping>,
}

/// A job as the C check builds it: the writes, addresses, reads and values in buffers the kernel
/// reads and writes, and the run structure that names them. The buffers are sized once, so their
/// addresses hold for the job's whole life.
struct Job {
    writes: Vec<u8>,
    addrs: Vec<u8>,
    reads: Vec<u8>,
    values: Vec<u8>,
    run: [u8; 64],
}

impl Default for Job {
    fn default() -> Self {
        Self {
            writes: vec![0; 16 * 8],
            addrs: vec![0; 16 * 24],
            reads: vec![0; 8 * 4],
            values: vec![0; 8 * 4],
            run: [0; 64],
        }
    }
}

impl Job {
    fn reset(&mut self) {
        self.writes.fill(0);
        self.addrs.fill(0);
        self.reads.fill(0);
        self.values.fill(0);
        self.run = [0; 64];
    }

    /// Names the job's buffers in its run structure, as the kernel reads them at the call.
    fn point(&mut self) {
        let writes = addr(&self.writes);
        let addrs = addr(&self.addrs);
        let reads = addr(&self.reads);
        let values = addr_mut(&mut self.values);
        put64(&mut self.run, RUN_WRITES, writes);
        put64(&mut self.run, RUN_ADDRS, addrs);
        put64(&mut self.run, RUN_READS, reads);
        put64(&mut self.run, RUN_VALUES, values);
    }
}

/// The address of a buffer, as the kernel is given it: a pointer the kernel reads.
fn addr(buf: &[u8]) -> u64 {
    buf.as_ptr() as u64
}

/// The address of a buffer the kernel writes into.
fn addr_mut(buf: &mut [u8]) -> u64 {
    buf.as_mut_ptr() as u64
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

/// The packed u32 values of a request's array, in native byte order.
fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

/// One write, packed as `struct lemnos_regjob_write`.
fn pack(offset: u32, value: u32) -> [u8; 8] {
    let mut w = [0u8; 8];
    put32(&mut w, 0, offset);
    put32(&mut w, 4, value);
    w
}

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The text of an errno, as C's `strerror` gives it (the std form without its error number).
fn strerror(errno: i32) -> String {
    let text = io::Error::from_raw_os_error(errno).to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// The errno a request failed with, or 0 when it succeeded.
fn errno_of(result: io::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => e.raw_os_error().unwrap_or(libc::EIO),
    }
}

/// `atoi` of what the resets attribute holds: a decimal number, with the newline after it.
fn atoi(text: &[u8]) -> i32 {
    std::str::from_utf8(text)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn now_ns() -> u64 {
    monotonic_ns().unwrap_or(0)
}

/// Whether descriptor `fd` is open: its entry under /proc/self/fd exists.
fn is_open(fd: i32) -> bool {
    fs::symlink_metadata(format!("/proc/self/fd/{fd}")).is_ok()
}

/// Closes an owned descriptor: whether it was open and is closed now.
fn close_fd(fd: OwnedFd) -> bool {
    let raw = fd.as_raw_fd();
    let was_open = is_open(raw);
    drop(fd);
    was_open && !is_open(raw)
}

/// Closes an optional owned descriptor (none is not closed).
fn close_opt(fd: Option<OwnedFd>) -> bool {
    fd.is_some_and(close_fd)
}

/// The frames session, which the check holds while it is open.
fn session(s: &Option<OwnedFd>) -> BorrowedFd<'_> {
    match s {
        Some(fd) => fd.as_fd(),
        None => panic!("no frames session is open"),
    }
}

/// The configuration session, which the check holds while it is open.
fn config(s: &Option<OwnedFd>) -> BorrowedFd<'_> {
    match s {
        Some(fd) => fd.as_fd(),
        None => panic!("no configuration session is open"),
    }
}

/// The descriptor number of a buffer, or -1 when it has none.
fn fd_of(b: &Buf) -> i32 {
    b.fd.as_ref().map_or(-1, |f| f.as_raw_fd())
}

/// The buffer's mapped bytes (empty for module memory, which the check does not map).
fn mem(b: &Buf) -> &[u8] {
    match &b.map {
        Some(m) => m.as_slice(),
        None => &[],
    }
}

fn mem_mut(b: &mut Buf) -> &mut [u8] {
    match &mut b.map {
        Some(m) => m.as_mut_slice(),
        None => &mut [],
    }
}

/// A dma-buf of `BUF_BYTES` from the heap, imported by the session and mapped for the CPU, into
/// `b`: whether it was imported.
fn import(heap: BorrowedFd<'_>, sess: BorrowedFd<'_>, b: &mut Buf) -> bool {
    let Ok(fd) = dma_heap::allocate(heap, BUF_BYTES) else {
        return false;
    };
    let mut arg = [0u8; 24];
    put32(&mut arg, 0, fd.as_raw_fd() as u32);
    if guest::ioctl_bytes(sess, IMPORT, &mut arg).is_err() {
        return false;
    }
    let Ok(map) = Mapping::new(fd.as_fd(), BUF_BYTES as usize, 0, true) else {
        return false;
    };
    *b = Buf {
        fd: Some(fd),
        index: get32(&arg, 4),
        addr: get64(&arg, 16),
        map: Some(map),
    };
    true
}

/// Starts a frame job on the session (NOWAIT): the errno, 0 for success.
fn start_job(sess: BorrowedFd<'_>, j: &mut Job) -> i32 {
    errno_of(guest::ioctl_bytes(sess, RUN, &mut j.run))
}

/// Writes and reads at once: the errno, 0 for success. A zero count leaves its pointer null.
fn regs_now(
    fd: BorrowedFd<'_>,
    writes: &[u8],
    nwrites: u32,
    reads: &[u8],
    values: &mut [u8],
    nreads: u32,
) -> i32 {
    let mut r = [0u8; 32];
    put64(&mut r, 0, if nwrites == 0 { 0 } else { addr(writes) });
    put64(&mut r, 8, if nreads == 0 { 0 } else { addr(reads) });
    put64(&mut r, 16, if nreads == 0 { 0 } else { addr_mut(values) });
    put32(&mut r, 24, nwrites);
    put32(&mut r, 28, nreads);
    errno_of(guest::ioctl_bytes(fd, WRITE, &mut r))
}

/// One register write on `fd`: 0 or the errno.
fn write1(fd: BorrowedFd<'_>, offset: u32, value: u32) -> i32 {
    regs_now(fd, &pack(offset, value), 1, &[], &mut [0u8; 0], 0)
}

/// The next event of the configuration role.
fn event(fd: BorrowedFd<'_>) -> Event {
    let mut e = [0u8; 16];
    let err = errno_of(guest::ioctl_bytes(fd, EVENT, &mut e));
    Event {
        err,
        status: get32(&e, 0),
        lost: get32(&e, 4),
        at: get64(&e, 8),
    }
}

/// Takes every queued event; how many there were.
fn drain(fd: BorrowedFd<'_>) -> i32 {
    let mut n = 0;
    while event(fd).err == 0 {
        n += 1;
    }
    n
}

/// A frame job into the buffer at `index`: NV12, the ISP on, collected later (NOWAIT).
fn frame_job(j: &mut Job, index: u32) {
    j.reset();
    for (offset, value) in FRAME_WRITES {
        push_write(j, offset, value);
    }
    for (k, &reg) in PLANE_REG.iter().enumerate() {
        set_addr(
            j,
            k,
            reg,
            index,
            u64::from(PLANE_OFF[k]),
            u64::from(PLANE_LEN[k]),
        );
    }
    put32(&mut j.reads, 0, AE_MEAN_0);
    j.point();
    put32(&mut j.run, RUN_NADDRS, SLOTS as u32);
    put32(&mut j.run, RUN_NREADS, 1);
    put32(&mut j.run, RUN_START, 0x1);
    put32(&mut j.run, RUN_TIMEOUT, 3000);
    put32(&mut j.run, RUN_FLAGS, RUN_NOWAIT);
}

/// Appends a write to the job.
fn push_write(j: &mut Job, offset: u32, value: u32) {
    let n = get32(&j.run, RUN_NWRITES) as usize;
    put32(&mut j.writes, n * 8, offset);
    put32(&mut j.writes, n * 8 + 4, value);
    put32(&mut j.run, RUN_NWRITES, n as u32 + 1);
}

fn set_addr(j: &mut Job, k: usize, reg: u32, buffer: u32, offset: u64, bytes: u64) {
    put32(&mut j.addrs, k * 24, reg);
    put32(&mut j.addrs, k * 24 + 4, buffer);
    put64(&mut j.addrs, k * 24 + 8, offset);
    put64(&mut j.addrs, k * 24 + 16, bytes);
}

fn set_addr_buffer(j: &mut Job, k: usize, buffer: u32) {
    put32(&mut j.addrs, k * 24 + 4, buffer);
}

fn set_addr_offset(j: &mut Job, k: usize, offset: u64) {
    put64(&mut j.addrs, k * 24 + 8, offset);
}

fn set_addr_bytes(j: &mut Job, k: usize, bytes: u64) {
    put64(&mut j.addrs, k * 24 + 16, bytes);
}

/// Waits for a started job: the errno, its status and the time the handler completed it.
fn collect_job(sess: BorrowedFd<'_>, j: &mut Job) -> Collected {
    let mut w = [0u8; 24];
    put64(&mut w, 0, addr_mut(&mut j.values));
    put32(&mut w, 8, get32(&j.run, RUN_NREADS));
    let err = errno_of(guest::ioctl_bytes(sess, WAIT, &mut w));
    Collected {
        err,
        status: get32(&w, 12),
        at: get64(&w, 16),
    }
}

/// The check's state: the control node, the dma-heap, the block's sysfs directory (the fake
/// block's controls), the frames and configuration sessions while they are open, the buffers the
/// checks use, and the count of failed checks.
struct Harness {
    sysdir: String,
    ctl: OwnedFd,
    heap: OwnedFd,
    sess: Option<OwnedFd>,
    cfg: Option<OwnedFd>,
    a: Buf,
    b: Buf,
    c: Buf,
    d: Buf,
    failures: Cell<u32>,
}

impl Harness {
    fn check(&self, ok: bool, what: &str) {
        say(&format!("{} {what}", if ok { "ok  " } else { "FAIL" }));
        if !ok {
            self.failures.set(self.failures.get() + 1);
        }
    }

    fn fail(&self, what: &str) {
        say(what);
        self.failures.set(self.failures.get() + 1);
    }

    fn sfd(&self) -> BorrowedFd<'_> {
        session(&self.sess)
    }

    fn cfd(&self) -> BorrowedFd<'_> {
        config(&self.cfg)
    }

    /// Writes `text` to the fake block's attribute `attr`: whether the whole text was written.
    fn sys_put(&self, attr: &str, text: &str) -> bool {
        let path = format!("{}/{attr}", self.sysdir);
        let Ok(mut f) = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)
        else {
            return false;
        };
        f.write(text.as_bytes()).is_ok_and(|n| n == text.len())
    }

    fn poke(&self, off: u32, value: u32) {
        if !self.sys_put("poke", &format!("{off:x} {value:x}")) {
            self.fail(&format!("FAIL poke {off:x}"));
        }
    }

    fn stamp(&self, reg: u32, byte: u8) -> bool {
        self.sys_put("stamp", &format!("{reg:x} {byte:x}"))
    }

    fn sys_irq(&self, line: &str, value: &str) {
        if !self.sys_put(line, value) {
            self.fail(&format!("FAIL {line}"));
        }
    }

    /// The fake block's whole register window into `out`, which must be `WINDOW` bytes.
    fn read_regs(&self, out: &mut [u8]) {
        let mut done = 0;
        if let Ok(f) = File::open(format!("{}/regs", self.sysdir)) {
            // sysfs hands a binary attribute out a page at a time.
            while done < WINDOW {
                match f.read_at(&mut out[done..WINDOW], done as u64) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => done += n,
                }
            }
        }
        if done != WINDOW {
            self.fail("FAIL regs");
        }
    }

    fn reg(&self, off: u32) -> u32 {
        let mut r = vec![0u8; WINDOW];
        self.read_regs(&mut r);
        get32(&r, off as usize)
    }

    /// How many resets the block has had, or -1 when the attribute can't be read.
    fn resets(&self) -> i32 {
        let mut text = [0u8; 31];
        let n = File::open(format!("{}/resets", self.sysdir))
            .and_then(|mut f| f.read(&mut text))
            .unwrap_or(0);
        if n == 0 {
            return -1;
        }
        atoi(&text[..n])
    }

    fn info(&self) -> Info {
        let mut b = [0xffu8; 48];
        if let Err(e) = guest::ioctl_bytes(self.sfd(), INFO, &mut b) {
            let errno = e.raw_os_error().unwrap_or(libc::EIO);
            self.fail(&format!("FAIL info: {}", strerror(errno)));
        }
        Info {
            wedged: get32(&b, 8),
            live: get32(&b, 12),
            jobs: get64(&b, 16),
            refused: get64(&b, 24),
        }
    }

    /// The shadow registers the block takes a buffer's planes from, as it would after the module's
    /// forced update.
    fn shadows(&self, addr: u64) {
        for (k, &off) in PLANE_OFF.iter().take(PLANES).enumerate() {
            let at = MI_MP_Y_BASE_SHD + 12 * k as u32;
            self.poke(at, (addr + u64::from(off)) as u32);
            self.poke(at + 4, PLANE_LEN[k]);
        }
        self.poke(MI_MP_RAW_BASE_SHD, addr as u32);
    }

    /// The block reaches the end of a frame of `output`: its status set, its interrupt raised.
    fn output_end(&self, output: u32, wraps: u32) {
        self.poke(MI_MIS, output);
        self.poke(MI_RIS, output | wraps);
        self.sys_irq("irq", "1");
        sleep_ms(20);
    }

    fn frame_end(&self, wraps: u32) {
        self.output_end(FRAME_END, wraps);
    }

    /// A frame leaves the ISP core: its status set, interrupt 0 raised.
    fn frame_out(&self, settle_ms: u64) {
        self.poke(ISP_MIS, FRAME_OUT);
        self.sys_irq("irq_line", "0");
        sleep_ms(settle_ms);
    }

    fn start(&self, j: &mut Job) -> i32 {
        start_job(self.sfd(), j)
    }

    fn collect(&self, j: &mut Job) -> Collected {
        collect_job(self.sfd(), j)
    }

    /// Releases a buffer by its index on the frames session: the errno, 0 for success.
    fn release_index(&self, index: u32) -> i32 {
        errno_of(guest::ioctl_bytes(
            self.sfd(),
            RELEASE,
            &mut index.to_ne_bytes(),
        ))
    }

    /// A job refused with `err`, and no register touched.
    fn refused(&self, j: &mut Job, err: i32, what: &str) {
        let mut before = vec![0u8; WINDOW];
        let mut after = vec![0u8; WINDOW];
        self.read_regs(&mut before);
        let got = start_job(self.sfd(), j);
        self.read_regs(&mut after);
        if got != err {
            say(&format!("     got {}", strerror(got)));
        }
        let same = before == after;
        self.check(got == err && same, &format!("{what}: {}", strerror(err)));
    }

    /// A write refused with `err` on `fd`, and no register touched.
    fn write_refused(&self, fd: BorrowedFd<'_>, offset: u32, value: u32, err: i32, what: &str) {
        let mut before = vec![0u8; WINDOW];
        let mut after = vec![0u8; WINDOW];
        self.read_regs(&mut before);
        let got = write1(fd, offset, value);
        self.read_regs(&mut after);
        if got != err {
            say(&format!("     got {}", strerror(got)));
        }
        let same = before == after;
        self.check(got == err && same, &format!("{what}: {}", strerror(err)));
    }

    /// Opens a session on the control node with `flags`: the errno, 0 for a session opened (which
    /// is closed again at once).
    fn open_errno(&self, flags: u32, block: u32) -> i32 {
        let mut o = [0u8; 24];
        put32(&mut o, 0, block);
        put32(&mut o, 4, flags);
        match guest::regjob_open(self.ctl.as_fd(), &mut o) {
            Ok(_) => 0,
            Err(e) => e.raw_os_error().unwrap_or(libc::EIO),
        }
    }

    /// Opens the configuration role on `block`, with a check that its profile is the ISP's.
    fn open_config(&self, block: u32) -> Option<OwnedFd> {
        let mut o = [0u8; 24];
        put32(&mut o, 0, block);
        put32(&mut o, 4, OPEN_CONFIG);
        let fd = guest::regjob_open(self.ctl.as_fd(), &mut o).ok()?;
        let (profile, window) = (get32(&o, 12), get32(&o, 16));
        self.check(
            profile == PROFILE_K230_ISP && window == WINDOW as u32,
            "the configuration session runs the ISP's profile",
        );
        Some(fd)
    }

    fn test_stream(&mut self) {
        let mut j = Job::default();
        let mut ok = true;

        // The first frame, on an idle block: switched at once.
        self.shadows(self.a.addr);
        frame_job(&mut j, self.a.index);
        let started = self.start(&mut j) == 0;
        self.check(started, "a first frame starts on the idle block");
        let a_addr = self.a.addr;
        for (k, &base) in PLANE_REG.iter().take(PLANES).enumerate() {
            let want = (a_addr + u64::from(PLANE_OFF[k])) as u32;
            ok &= self.reg(base) == want;
            ok &= self.reg(base + 4) == PLANE_LEN[k] && self.reg(base + 8) == 0;
            ok &= self.reg(MI_MP_Y_BASE2 + 4 * (k as u32)) == want;
        }
        ok &= self.reg(MI_MP_RAW_BASE) == a_addr as u32
            && self.reg(MI_MP_RAW_SIZE) == Y_BYTES
            && self.reg(MI_MP_RAW_OFFS) == 0;
        ok &= self.reg(MI_MP_JDP_BASE) == a_addr as u32
            && self.reg(MI_MP_JDP_BASE + 4) == Y_BYTES
            && self.reg(MI_MP_JDP_BASE + 8) == 0;
        self.check(
            ok,
            "every address register of the main path (Y, Cb, Cr, their second set, raw, JPEG) is the module's, from the checked range",
        );
        let switched = self.reg(MI_MP_CTRL) == 0x38;
        self.check(switched, "the block was switched to them before the start");
        let on = self.reg(MI_CTRL) == 1 && self.reg(MI_IMSC) == (FRAME_END | RAW_FRAME_END);
        self.check(
            on,
            "the main path is on, with both outputs' frame-end interrupts",
        );
        let own = self.reg(ISP_CTRL) == 1 && self.reg(ISP_ACQ_H_SIZE) == 256;
        self.check(own, "the job's own registers are written");
        let held = self.release_index(self.a.index) == libc::EBUSY;
        self.check(held, "the frame's buffer is held");
        mem_mut(&mut self.a).fill(0);
        let stamped = self.stamp(MI_MP_Y_BASE, 0x5a) && self.stamp(MI_MP_CB_BASE, 0x5b);
        self.check(stamped, "the block writes the planes");
        self.poke(AE_MEAN_0, 0x1234);
        let before_ns = now_ns();

        self.frame_end(WRAP_Y);
        let got = self.collect(&mut j);
        let values = get32(&j.values, 0);
        self.check(
            got.err == 0 && got.status == (FRAME_END | WRAP_Y) && values == 0x1234,
            "the frame's end completes it, with the wraps seen and the reads taken then",
        );
        self.check(
            got.at > before_ns && got.at < now_ns(),
            "with the time the handler took it",
        );
        let planes = mem(&self.a);
        self.check(
            planes[0] == 0x5a && planes[Y_BYTES as usize] == 0x5b,
            "what the block wrote is in the buffer",
        );
        let kept = self.info().live == 1 && self.release_index(self.a.index) == libc::EBUSY;
        self.check(kept, "the path keeps writing the buffer: it stays held");

        // The next frame, on the running block: switched only at the frame's end.
        self.poke(MI_MP_CTRL, 0);
        frame_job(&mut j, self.b.index);
        let started = self.start(&mut j) == 0;
        self.check(started, "the next frame starts on the running block");
        let waiting = self.reg(MI_MP_Y_BASE) == self.b.addr as u32 && self.reg(MI_MP_CTRL) == 0;
        self.check(
            waiting,
            "its addresses are written, the switch waits for the frame's end",
        );
        let meanwhile = self.info().live == 2 && self.release_index(self.a.index) == libc::EBUSY;
        self.check(meanwhile, "both frames' buffers are held meanwhile");
        self.shadows(self.b.addr);
        self.frame_end(0);
        let switched = self.reg(MI_MP_CTRL) == 0x38;
        self.check(
            switched,
            "the handler switched the block at the frame's end",
        );
        let got = self.collect(&mut j);
        self.check(
            got.err == 0 && got.status == FRAME_END,
            "and the job completed there",
        );
        let freed = self.info().live == 1 && self.release_index(self.a.index) == 0;
        self.check(freed, "the frame before is free once the block left it");
        let not_free = self.release_index(self.b.index) == libc::EBUSY;
        self.check(not_free, "the frame being written is not");

        // A block that doesn't take the new addresses.
        frame_job(&mut j, self.c.index);
        let started = self.start(&mut j) == 0;
        self.check(started, "a frame whose switch the block won't take");
        self.frame_end(0);
        let got = self.collect(&mut j);
        self.check(
            got.err == 0 && got.status == (FRAME_END | UNLATCHED),
            "completes with LEMNOS_REGJOB_STATUS_UNLATCHED",
        );
        let both = self.info().live == 2
            && self.release_index(self.b.index) == libc::EBUSY
            && self.release_index(self.c.index) == libc::EBUSY;
        self.check(both, "and the block may use either buffer: both stay held");
        frame_job(&mut j, self.d.index);
        let started = self.start(&mut j) == 0;
        self.check(started, "a later frame");
        self.shadows(self.d.addr);
        self.frame_end(0);
        let got = self.collect(&mut j);
        self.check(got.err == 0 && got.status == FRAME_END, "is taken");
        let freed = self.release_index(self.b.index) == 0
            && self.release_index(self.c.index) == 0
            && self.release_index(self.d.index) == libc::EBUSY;
        self.check(freed, "and lets every buffer before it go");
        self.check(self.info().jobs == 4, "four frames counted");

        // The raw output: the same switch, its own enable and frame end, its shadow read back.
        let imported = import(self.heap.as_fd(), session(&self.sess), &mut self.b);
        self.check(imported, "a buffer for raw frames");
        frame_job(&mut j, self.b.index);
        put32(&mut j.run, RUN_START, RAW_FRAME_END);
        let started = self.start(&mut j) == 0;
        self.check(started, "a raw frame starts on the running block");
        let raw =
            self.reg(MI_MP_RAW_BASE) == self.b.addr as u32 && self.reg(MI_MP_RAW_SIZE) == Y_BYTES;
        self.check(raw, "the raw output's base and size are the module's");
        self.shadows(self.b.addr);
        self.output_end(RAW_FRAME_END, 0);
        let got = self.collect(&mut j);
        let raw_on =
            got.err == 0 && got.status == RAW_FRAME_END && self.reg(MI_CTRL) == RAW_FRAME_END;
        self.check(
            raw_on,
            "the raw output's frame end completes it, with the raw output on",
        );
        frame_job(&mut j, self.d.index);
        put32(&mut j.run, RUN_START, RAW_FRAME_END);
        let started = self.start(&mut j) == 0;
        self.check(started, "a raw frame whose switch the block won't take");
        let d = self.d.addr;
        self.poke(MI_MP_Y_BASE_SHD, d as u32);
        self.poke(MI_MP_Y_BASE_SHD + 4, Y_BYTES);
        self.poke(MI_MP_Y_BASE_SHD + 12, (d + u64::from(Y_BYTES)) as u32);
        self.poke(MI_MP_Y_BASE_SHD + 16, C_BYTES);
        self.poke(MI_MP_Y_BASE_SHD + 24, (d + u64::from(Y_BYTES)) as u32);
        self.poke(MI_MP_Y_BASE_SHD + 28, C_BYTES);
        self.output_end(RAW_FRAME_END, 0);
        let got = self.collect(&mut j);
        self.check(
            got.err == 0 && got.status == (RAW_FRAME_END | UNLATCHED),
            "the raw shadow still naming the old buffer: LEMNOS_REGJOB_STATUS_UNLATCHED",
        );
        frame_job(&mut j, self.d.index);
        let started = self.start(&mut j) == 0;
        self.check(started, "back to YCbCr");
        self.shadows(self.d.addr);
        self.frame_end(0);
        let got = self.collect(&mut j);
        let freed =
            got.err == 0 && got.status == FRAME_END && self.release_index(self.b.index) == 0;
        self.check(freed, "taken, and the raw frames' buffer is free");
    }

    fn test_refusals(&mut self) {
        let mut j = Job::default();
        let refused_before = self.info().refused;
        let a_addr = self.a.addr as u32;
        let a_index = self.a.index;
        let d = self.d.index;
        frame_job(&mut j, d);
        push_write(&mut j, MI_MP_Y_BASE, a_addr);
        self.refused(&mut j, libc::EPERM, "a write to a plane's base");
        frame_job(&mut j, d);
        push_write(&mut j, MI_MP_Y_SIZE, 0x1fff_fff0);
        self.refused(
            &mut j,
            libc::EPERM,
            "a write to a plane's size, which the module derives",
        );
        frame_job(&mut j, d);
        push_write(&mut j, MI_MP_Y_OFFS, 0x100);
        self.refused(&mut j, libc::EPERM, "a write to a plane's offset");
        frame_job(&mut j, d);
        push_write(&mut j, MI_MP_Y_BASE2, a_addr);
        self.refused(&mut j, libc::EPERM, "a write to the ping-pong set");
        frame_job(&mut j, d);
        push_write(&mut j, MI_MP_RAW_BASE, a_addr);
        self.refused(&mut j, libc::EPERM, "a write to the raw output's base");
        frame_job(&mut j, d);
        push_write(&mut j, ISP_CTRL, CTRL_GAMMA);
        self.refused(
            &mut j,
            libc::EPERM,
            "a bit of isp_ctrl that is the configuration role's",
        );
        frame_job(&mut j, d);
        push_write(&mut j, ISP_AWB_PROP, 1);
        self.refused(
            &mut j,
            libc::EPERM,
            "a register that is the configuration role's",
        );
        frame_job(&mut j, d);
        push_write(&mut j, MI_MP_CTRL, 0x2);
        self.refused(
            &mut j,
            libc::EPERM,
            "automatic updates, which would switch the block on their own",
        );
        frame_job(&mut j, d);
        push_write(&mut j, MI_CTRL, 0x3);
        self.refused(&mut j, libc::EPERM, "another path's enable");
        frame_job(&mut j, d);
        push_write(&mut j, MI_IMSC, 0xffff_ffff);
        self.refused(
            &mut j,
            libc::EPERM,
            "the interrupt mask, which is the module's",
        );
        frame_job(&mut j, d);
        push_write(&mut j, ISP_IMSC, 0x2);
        self.refused(&mut j, libc::EPERM, "the ISP core's interrupts");
        frame_job(&mut j, d);
        push_write(&mut j, ISP_ACQ_PROP, 1 << 16);
        self.refused(&mut j, libc::EPERM, "the ISP's input from memory");
        frame_job(&mut j, d);
        push_write(&mut j, 0x3d68, 0);
        self.refused(&mut j, libc::EPERM, "the front end's command address");
        frame_job(&mut j, d);
        put32(&mut j.run, RUN_START, 0x3);
        self.refused(
            &mut j,
            libc::EPERM,
            "a start that turns on both outputs at once",
        );
        frame_job(&mut j, d);
        set_addr_bytes(&mut j, 0, 15);
        self.refused(
            &mut j,
            libc::EFAULT,
            "a plane shorter than the size register's grain",
        );
        frame_job(&mut j, d);
        set_addr_offset(&mut j, 1, u64::from(Y_BYTES) + 8);
        self.refused(&mut j, libc::EINVAL, "an unaligned plane");
        frame_job(&mut j, d);
        set_addr_offset(&mut j, 1, BUF_BYTES - 8192);
        self.refused(&mut j, libc::EFAULT, "a plane past its buffer's end");
        frame_job(&mut j, d);
        set_addr_buffer(&mut j, 2, a_index);
        self.refused(&mut j, libc::ENOENT, "a buffer the session released");
        frame_job(&mut j, d);
        put32(&mut j.run, RUN_NADDRS, PLANES as u32);
        self.refused(&mut j, libc::EINVAL, "the raw output's address left out");

        let counted = self.info().refused.wrapping_sub(refused_before) == 19;
        self.check(counted, "every refusal is counted");
        let running = self.info().live == 1 && self.reg(MI_MP_Y_BASE) == self.d.addr as u32;
        self.check(running, "and the running path is as it was");
    }

    fn test_roles(&mut self, block: u32) {
        let mut j = Job::default();
        let e = self.open_errno(0, block);
        self.check(
            e == libc::EBUSY,
            "a second frames session is refused (EBUSY)",
        );
        self.cfg = self.open_config(block);
        let opened = self.cfg.is_some();
        self.check(
            opened,
            "a configuration session opens beside the running frames session",
        );
        let e = self.open_errno(OPEN_CONFIG, block);
        self.check(
            e == libc::EBUSY,
            "a second configuration session is refused (EBUSY)",
        );
        let imsc = self.reg(ISP_IMSC) == FRAME_OUT;
        self.check(imsc, "the ISP core's frame-out interrupt is on for it");

        self.poke(AE_MEAN_0, 0x77);
        let writes = pack(ISP_AWB_PROP, 0xabcd);
        let reads = words(&[ISP_AWB_PROP, AE_MEAN_0]);
        let mut values = [0u8; 8];
        let wrote = regs_now(self.cfd(), &writes, 1, &reads, &mut values, 2) == 0
            && self.reg(ISP_AWB_PROP) == 0xabcd
            && get32(&values, 0) == 0xabcd
            && get32(&values, 4) == 0x77;
        self.check(
            wrote,
            "its writes are made at once, and its reads read after them",
        );

        // Its table, not the frames role's.
        let a_addr = self.a.addr as u32;
        self.write_refused(
            self.cfd(),
            MI_MP_Y_BASE,
            a_addr,
            libc::EPERM,
            "configuration: a plane's base",
        );
        self.write_refused(
            self.cfd(),
            MI_CTRL,
            1,
            libc::EPERM,
            "configuration: the path enable",
        );
        self.write_refused(
            self.cfd(),
            MI_MP_CTRL,
            0x38,
            libc::EPERM,
            "configuration: the forced update",
        );
        self.write_refused(
            self.cfd(),
            ISP_IMSC,
            0,
            libc::EPERM,
            "configuration: the ISP core's interrupt mask",
        );
        self.write_refused(
            self.cfd(),
            ISP_ACQ_H_SIZE,
            64,
            libc::EPERM,
            "configuration: the frames role's acquisition window",
        );
        self.write_refused(
            self.cfd(),
            ISP_CTRL,
            1,
            libc::EPERM,
            "configuration: isp_ctrl's enable",
        );
        self.write_refused(
            self.cfd(),
            ISP_AWB_PROP + 2,
            0,
            libc::EINVAL,
            "configuration: an unaligned register",
        );
        self.write_refused(
            self.cfd(),
            WINDOW as u32,
            0,
            libc::EINVAL,
            "configuration: past the window",
        );
        let unaligned = words(&[2]);
        let e = regs_now(self.cfd(), &[], 0, &unaligned, &mut [0u8; 4], 1);
        self.check(e == libc::EINVAL, "an unaligned read (EINVAL)");
        let mut imp = [0u8; 24];
        put32(&mut imp, 0, fd_of(&self.a) as u32);
        let e = errno_of(guest::ioctl_bytes(self.cfd(), IMPORT, &mut imp));
        self.check(e == libc::EINVAL, "it imports no buffer (EINVAL)");
        frame_job(&mut j, self.d.index);
        let e = errno_of(guest::ioctl_bytes(self.cfd(), RUN, &mut j.run));
        self.check(e == libc::EINVAL, "it runs no job (EINVAL)");
        let released = errno_of(guest::ioctl_bytes(
            self.cfd(),
            RELEASE,
            &mut self.d.index.to_ne_bytes(),
        )) == libc::EINVAL
            && errno_of(guest::ioctl_bytes(self.cfd(), WAIT, &mut [0u8; 24])) == libc::EINVAL;
        self.check(released, "and releases or collects nothing (EINVAL)");
        let e = event(self.sfd());
        self.check(
            e.err == libc::EINVAL,
            "the frames session has no events (EINVAL)",
        );

        // isp_ctrl: both roles' bits, each written by read-modify-write.
        self.poke(ISP_CTRL, 0);
        let on = write1(self.sfd(), ISP_CTRL, CTRL_ON) == 0 && self.reg(ISP_CTRL) == CTRL_ON;
        self.check(on, "the frames role turns the ISP on outside a job");
        let gamma = write1(self.cfd(), ISP_CTRL, CTRL_GAMMA) == 0
            && self.reg(ISP_CTRL) == (CTRL_ON | CTRL_GAMMA);
        self.check(
            gamma,
            "the configuration role's gamma bit leaves the frames role's bits as they were",
        );
        let kept =
            write1(self.sfd(), ISP_CTRL, 0x1) == 0 && self.reg(ISP_CTRL) == (0x1 | CTRL_GAMMA);
        self.check(
            kept,
            "the frames role's write leaves the gamma bit as it was",
        );
        frame_job(&mut j, self.d.index);
        let started = self.start(&mut j) == 0;
        self.check(started, "a frame job writing isp_ctrl");
        self.shadows(self.d.addr);
        self.frame_end(0);
        let got = self.collect(&mut j);
        let gamma_kept = got.err == 0 && self.reg(ISP_CTRL) == (0x1 | CTRL_GAMMA);
        self.check(gamma_kept, "keeps the gamma bit too");
        self.write_refused(
            self.sfd(),
            ISP_AWB_PROP,
            1,
            libc::EPERM,
            "frames, outside a job: a configuration register",
        );
        self.write_refused(
            self.sfd(),
            MI_MP_Y_BASE,
            0,
            libc::EPERM,
            "frames, outside a job: a plane's base",
        );

        // Lens shading, the configuration role's: the enable, a table address, two 12-bit
        // samples to a word, a sector size, and the table select, each with its field bits (the
        // masks of lemnos_regjob_k230isp.c). A value past a field is refused, as the masks say.
        let shade = write1(self.cfd(), LSC_CTRL, 1) == 0 && self.reg(LSC_CTRL) == 1;
        self.check(shade, "lens shading: the enable is written");
        let table =
            write1(self.cfd(), LSC_R_TABLE_ADDR, 153) == 0 && self.reg(LSC_R_TABLE_ADDR) == 153;
        self.check(table, "lens shading: bank 1's address (153)");
        let samples = write1(self.cfd(), LSC_R_TABLE_DATA, 0x00ff_ffff) == 0
            && self.reg(LSC_R_TABLE_DATA) == 0x00ff_ffff;
        self.check(samples, "lens shading: two 12-bit samples to a word");
        let sectors = write1(self.cfd(), LSC_XSIZE_0, 0x03ff_03ff) == 0
            && self.reg(LSC_XSIZE_0) == 0x03ff_03ff;
        self.check(sectors, "lens shading: two 10-bit sector sizes");
        self.write_refused(
            self.cfd(),
            LSC_R_TABLE_DATA,
            0x0100_0000,
            libc::EPERM,
            "lens shading: a bit past the sample pair",
        );
        self.write_refused(
            self.cfd(),
            LSC_XSIZE_0,
            0x0000_0400,
            libc::EPERM,
            "lens shading: a sector size past 10 bits",
        );
        self.write_refused(
            self.cfd(),
            LSC_CTRL,
            2,
            libc::EPERM,
            "lens shading: a bit past the enable",
        );
        self.write_refused(
            self.cfd(),
            LSC_TABLE_SEL,
            2,
            libc::EPERM,
            "lens shading: a table past 1",
        );

        // Events: the ISP core's interrupt, read and cleared by the module.
        let e = event(self.cfd());
        self.check(
            e.err == libc::EAGAIN,
            "no event before the interrupt (EAGAIN)",
        );
        let before_ns = now_ns();
        self.frame_out(20);
        let e = event(self.cfd());
        self.check(
            e.err == 0
                && e.status == FRAME_OUT
                && e.lost == 0
                && e.at > before_ns
                && e.at < now_ns(),
            "a frame leaving the ISP is an event, with its status and time",
        );
        let icr = self.reg(ISP_ICR) == FRAME_OUT;
        self.check(icr, "the module cleared it");
        let once = event(self.cfd()).err == libc::EAGAIN;
        self.check(once, "and it is taken once");
        self.poke(ISP_MIS, 0);
        self.sys_irq("irq_line", "0");
        sleep_ms(20);
        let none = event(self.cfd()).err == libc::EAGAIN;
        self.check(none, "an interrupt with nothing pending is no event");
        for _ in 0..EVENTS + 2 {
            self.frame_out(3);
        }
        let mut ok = true;
        for _ in 0..EVENTS {
            let e = event(self.cfd());
            ok &= e.err == 0 && e.lost == 0;
        }
        self.check(ok, "the queue holds LEMNOS_REGJOB_EVENTS events");
        let dropped = event(self.cfd()).err == libc::EAGAIN;
        self.check(dropped, "and drops the ones past it");
        self.frame_out(20);
        let e = event(self.cfd());
        self.check(
            e.err == 0 && e.lost == 2,
            "the next one says how many were dropped",
        );
        self.frame_end(0);
        let frames = event(self.cfd()).err == libc::EAGAIN;
        self.check(
            frames,
            "the memory interface's interrupt is the frames role's",
        );
        self.poke(ISP_MIS, 0);
    }

    fn test_timeout(&mut self) {
        let mut j = Job::default();
        let before = self.resets();

        let imported = import(self.heap.as_fd(), session(&self.sess), &mut self.a);
        self.check(imported, "a buffer for a frame that never comes");
        frame_job(&mut j, self.a.index);
        put32(&mut j.run, RUN_TIMEOUT, 100);
        let started = self.start(&mut j) == 0;
        self.check(started, "a frame that never comes");
        let got = self.collect(&mut j);
        self.check(got.err == libc::EAGAIN, "is waited for (EAGAIN)");
        sleep_ms(150);
        let got = self.collect(&mut j);
        let timed_out = got.err == libc::ETIMEDOUT && self.resets() == before + 1;
        self.check(
            timed_out,
            "until its time is up: ETIMEDOUT, the block reset",
        );
        let off = self.info().live == 0 && self.reg(MI_CTRL) == 0 && self.reg(MI_IMSC) == 0;
        self.check(off, "the path is off and nothing is live");
        let freed = self.release_index(self.a.index) == 0 && self.release_index(self.d.index) == 0;
        self.check(freed, "every buffer can go");
        let e = event(self.cfd());
        self.check(
            e.err == 0 && e.status == EVENT_RESET,
            "the configuration role hears of the reset (LEMNOS_REGJOB_EVENT_RESET)",
        );
        let kept = self.reg(ISP_IMSC) == FRAME_OUT;
        self.check(kept, "and keeps its interrupt");
        self.frame_out(20);
        let e = event(self.cfd());
        self.check(e.err == 0 && e.status == FRAME_OUT, "which still comes");
    }

    fn test_close_running(&mut self, block: u32) {
        let mut j = Job::default();
        let before = self.resets();

        let imported = import(self.heap.as_fd(), session(&self.sess), &mut self.a);
        self.check(imported, "a buffer for a running path");
        self.shadows(self.a.addr);
        frame_job(&mut j, self.a.index);
        let started = self.start(&mut j) == 0;
        self.check(started, "the path runs again");
        self.frame_end(0);
        let got = self.collect(&mut j);
        let live = got.err == 0 && self.info().live == 1;
        self.check(live, "a frame completes, the path runs on");
        let closed = close_opt(self.sess.take());
        self.check(closed, "the session closes on the running path");
        let reset = self.resets() == before + 1;
        self.check(reset, "the block was reset before its buffer was let go");
        let told = drain(self.cfd()) == 1
            && write1(self.cfd(), ISP_AWB_PROP, 5) == 0
            && self.reg(ISP_AWB_PROP) == 5;
        self.check(told, "the configuration session hears of it, and goes on");
        let mut o = [0u8; 24];
        put32(&mut o, 0, block);
        let reopened = guest::regjob_open(self.ctl.as_fd(), &mut o);
        let ok = reopened.is_ok();
        self.sess = reopened.ok();
        self.check(ok, "a new session opens");
        let idle = self.info().live == 0 && self.info().wedged == 0;
        self.check(idle, "with nothing live");
        close_opt(self.sess.take());
    }

    /// Memory the module allocated for the ISP (LEMNOS_IOC_ALLOC on its block), the way the
    /// capture driver gets its ring: imported into a frames session at the same bus address, and a
    /// frame written into it.
    fn test_module_memory(&mut self, block: u32) {
        let mut j = Job::default();
        let mut o = [0u8; 24];
        put32(&mut o, 0, block);
        let opened = guest::regjob_open(self.ctl.as_fd(), &mut o);
        self.check(opened.is_ok(), "a frames session for the module's memory");
        self.sess = opened.ok();

        let al = module::alloc(self.ctl.as_fd(), block, BUF_BYTES);
        let allocated = al.as_ref().is_ok_and(|m| m.size == BUF_BYTES);
        self.check(allocated, "the module allocates memory for the ISP");
        let Ok(al) = al else {
            return;
        };
        let mut imp = [0u8; 24];
        put32(&mut imp, 0, al.fd.as_raw_fd() as u32);
        let imported = errno_of(guest::ioctl_bytes(self.sfd(), IMPORT, &mut imp)) == 0
            && get64(&imp, 16) == al.addr
            && get64(&imp, 8) == BUF_BYTES;
        self.check(
            imported,
            "it imports into the session at the address the module gave it",
        );
        let mut m = Buf {
            fd: Some(al.fd),
            index: get32(&imp, 4),
            addr: al.addr,
            map: None,
        };
        self.shadows(m.addr);
        frame_job(&mut j, m.index);
        let started = self.start(&mut j) == 0 && self.reg(MI_MP_Y_BASE) == al.addr as u32;
        self.check(started, "a frame into it starts, the base the module's own");
        self.frame_end(0);
        let got = self.collect(&mut j);
        self.check(got.err == 0 && got.status == FRAME_END, "and completes");
        let closed = close_opt(self.sess.take());
        self.check(closed, "the session closes");
        let told = drain(self.cfd()) == 1;
        self.check(
            told,
            "the running path was reset, and the configuration session told",
        );
        let memory = close_opt(m.fd.take());
        self.check(memory, "the memory goes");
    }

    /// The configuration role alone, then closed.
    fn test_config_alone(&mut self, block: u32) {
        let closed = close_opt(self.cfg.take());
        self.check(closed, "the configuration session closes");
        let off = self.reg(ISP_IMSC) == 0;
        self.check(off, "its interrupt goes off with it");
        self.cfg = self.open_config(block);
        let works = self.cfg.is_some()
            && write1(self.cfd(), ISP_AWB_PROP, 9) == 0
            && self.reg(ISP_AWB_PROP) == 9;
        self.check(
            works,
            "a configuration session works with no frames session",
        );
        self.frame_out(20);
        let e = event(self.cfd());
        self.check(e.err == 0 && e.status == FRAME_OUT, "and has its events");
        let mut o = [0u8; 24];
        put32(&mut o, 0, block);
        let beside = guest::regjob_open(self.ctl.as_fd(), &mut o);
        self.check(beside.is_ok(), "a frames session opens beside it");
        drop(beside);
        let drained = drain(self.cfd()) == 0;
        self.check(
            drained,
            "and closing that one, on an idle block, resets nothing",
        );
        close_opt(self.cfg.take());
    }
}

fn finish(h: &Harness) -> ExitCode {
    if h.failures.get() > 0 {
        say("lemnos_regjob_isp_check: FAILED");
        ExitCode::from(1)
    } else {
        say("lemnos_regjob_isp_check: passed");
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        say("usage: lemnos_regjob_isp_check /dev/lemnos <device> <heap>");
        return ExitCode::from(2);
    }
    let (node, device, heap_path) = (&args[1], &args[2], &args[3]);

    let ctl = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(node);
    let heap = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(heap_path);
    let (ctl, heap) = match (ctl, heap) {
        (Ok(c), Ok(h)) => (OwnedFd::from(c), OwnedFd::from(h)),
        (c, h) => {
            // The heap's errno if it failed, as the C check reports it last.
            let errno = h
                .err()
                .or(c.err())
                .and_then(|e| e.raw_os_error())
                .unwrap_or(0);
            say(&format!("FAIL open: {}", strerror(errno)));
            return ExitCode::from(1);
        }
    };
    let block = module::find_block(ctl.as_fd(), device)
        .ok()
        .flatten()
        .unwrap_or(0);
    if block == 0 {
        say(&format!("FAIL no block {device}"));
        return ExitCode::from(1);
    }
    let mut h = Harness {
        sysdir: format!("/sys/bus/platform/devices/{device}"),
        ctl,
        heap,
        sess: None,
        cfg: None,
        a: Buf::default(),
        b: Buf::default(),
        c: Buf::default(),
        d: Buf::default(),
        failures: Cell::new(0),
    };

    let mut o = [0u8; 24];
    put32(&mut o, 0, block);
    match guest::regjob_open(h.ctl.as_fd(), &mut o) {
        Ok(fd) => {
            h.sess = Some(fd);
            h.check(true, "a session opens");
        }
        Err(_) => {
            h.check(false, "a session opens");
            return finish(&h);
        }
    }
    h.check(
        get32(&o, 12) == PROFILE_K230_ISP && get32(&o, 16) == WINDOW as u32,
        "it runs the ISP's profile on a 36 KiB window",
    );
    let imported = import(h.heap.as_fd(), session(&h.sess), &mut h.a)
        && import(h.heap.as_fd(), session(&h.sess), &mut h.b)
        && import(h.heap.as_fd(), session(&h.sess), &mut h.c)
        && import(h.heap.as_fd(), session(&h.sess), &mut h.d);
    h.check(imported, "four buffers imported");
    let quiet = h.reg(MI_CTRL) == 0 && h.reg(MI_IMSC) == 0 && h.reg(MI_MP_CTRL) == 0;
    h.check(quiet, "the bound block is quiet, with no automatic update");

    h.test_stream();
    h.test_refusals();
    h.test_roles(block);
    h.test_timeout();
    h.test_close_running(block);
    h.test_module_memory(block);
    h.test_config_alone(block);
    finish(&h)
}
