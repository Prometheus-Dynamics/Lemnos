//! Register jobs on the fake video engine (test module `lemnos_test_regjob`), run with the A733
//! encoder's profile: a good job followed through the fake block, every way a job can name memory
//! or a register tried wrong with the errno required and the registers left as they were, a job
//! that never finishes (ETIMEDOUT, the reset line pulsed, the next job runs), a job started
//! without waiting and collected by WAIT, and an unbind with the session open.
//!
//! Usage: `lemnos-regjob-check /dev/lemnos <device> <unbind> <heap>`. Port of
//! `kernel/test/lemnos_regjob_check.c` (CZ-GUEST-2). The uAPI is laid out in bytes as the C check
//! lays out its structures (`kernel/lemnos_regjob.h`, `kernel/lemnos.h`); the pins of the C file
//! are the constants and assertions below. The job's buffers are the process's own, and the kernel
//! reads and writes them at the addresses the job names, so they never move while a job is in
//! flight. A job that runs in a thread is moved into it and back by `join`.
//! Static musl.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::dma_heap;
use lemnos_linux_sys::guest;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::module;
use lemnos_linux_sys::{ior, iow, iowr};

// lemnos_regjob.h: the requests on the control node and on a session.
const REGJOB_OPEN: u32 = iowr::<[u8; 24]>(b'L', 0xf0);
const IMPORT: u32 = iowr::<[u8; 24]>(b'L', 0xf1);
const RELEASE: u32 = iow::<[u8; 4]>(b'L', 0xf2);
const RUN: u32 = iowr::<[u8; 64]>(b'L', 0xf3);
const INFO: u32 = ior::<[u8; 48]>(b'L', 0xf4);
const WAIT: u32 = iowr::<[u8; 24]>(b'L', 0xf5);
const WRITE: u32 = iow::<[u8; 32]>(b'L', 0xf6);
const EVENT: u32 = ior::<[u8; 16]>(b'L', 0xf7);

// The request numbers the C file pins (its _Static_asserts), and the sizes of the structures they
// carry are the array lengths above.
const _: () = assert!(
    REGJOB_OPEN == 0xc018_4cf0
        && IMPORT == 0xc018_4cf1
        && RELEASE == 0x4004_4cf2
        && RUN == 0xc040_4cf3
        && INFO == 0x8030_4cf4
        && WAIT == 0xc018_4cf5
        && WRITE == 0x4020_4cf6
        && EVENT == 0x8010_4cf7
);

// lemnos_regjob.h: the values the check names.
const PROFILE_A733_VE: u32 = 1;
const ABI: u32 = 1;
const OPEN_CONFIG: u32 = 1 << 0;
const RUN_NOWAIT: u32 = 1 << 0;
const MAX_WRITES: u32 = 1024;
const MAX_ADDRS: u32 = 16;
const MAX_BUFFERS: u32 = 64;
const MAX_TIMEOUT_MS: u32 = 10000;

// struct lemnos_regjob_run (64 bytes), at the offsets the C file pins.
const RUN_WRITES: usize = 0;
const RUN_ADDRS: usize = 8;
const RUN_READS: usize = 16;
const RUN_VALUES: usize = 24;
const RUN_NWRITES: usize = 32;
const RUN_NADDRS: usize = 36;
const RUN_NREADS: usize = 40;
const RUN_START: usize = 44;
const RUN_TIMEOUT: usize = 48;
const RUN_STATUS: usize = 52;
const RUN_FLAGS: usize = 56;
const RUN_RESERVED: usize = 60;
const _: () = assert!(RUN_START == 44 && RUN_STATUS == 52 && RUN_FLAGS == 56 && RUN_RESERVED == 60);

// struct lemnos_regjob_addr (24 bytes) and struct lemnos_regjob_write (8 bytes).
const ADDR_BYTES: usize = 24;
const WRITE_BYTES: usize = 8;
const _: () = assert!(ADDR_BYTES == 24 && WRITE_BYTES == 8);

// The encoder's registers (kernel/rs/regjob_ve.rs).
const MODE: u32 = 0x800;
const TOP_RESET: u32 = 0x804;
const IN_SIZE: u32 = 0xa00;
const IN_STRIDE: u32 = 0xa04;
const IN_CTRL: u32 = 0xa08;
const IN_LUMA: u32 = 0xa78;
const IN_CHROMA: u32 = 0xa7c;
const PARAM: u32 = 0xb04;
const QP: u32 = 0xb08;
const AVC_CTRL: u32 = 0xb14;
const TRIGGER: u32 = 0xb18;
const STATUS: u32 = 0xb1c;
const UNK_BUF: u32 = 0xb60;
const VLE_ADDR: u32 = 0xb80;
const VLE_END: u32 = 0xb84;
const VLE_OFFSET: u32 = 0xb88;
const VLE_MAX: u32 = 0xb8c;
const REF_LUMA: u32 = 0xba0;
const REF_CHROMA: u32 = 0xba4;
const REC_LUMA: u32 = 0xbb0;
const REC_CHROMA: u32 = 0xbb4;
const REF_SLUMA: u32 = 0xbb8;
const REC_SLUMA: u32 = 0xbbc;
const MB_INFO: u32 = 0xbc0;

const WINDOW: usize = 0x1000;
const SLOTS: usize = 11;
const BUF_BYTES: u64 = 64 * 1024;
const SLOT_SPACING: u64 = 4096;
const VLE_BYTES: u64 = 4096;
const MB_W: u32 = 4;
const MB_H: u32 = 2;
/// The start value every good job writes to the start register, last.
const START_VALUE: u32 = 0x8;

/// The address registers of a 64x32 NV12 encode, in the order the job names them.
const SLOT_REGS: [u32; SLOTS] = [
    IN_LUMA, IN_CHROMA, VLE_ADDR, REF_LUMA, REF_CHROMA, REF_SLUMA, REC_LUMA, REC_CHROMA, REC_SLUMA,
    MB_INFO, UNK_BUF,
];
/// What the profile needs for a 64x32 NV12 frame, per address register.
const SLOT_NEED: [u64; SLOTS] = [2048, 1024, 1, 2048, 2048, 512, 2048, 2048, 512, 128, 64];
/// The writes of a good job: the encoder's mode, frame size and stride, and the rest of its state.
const GOOD_WRITES: [(u32, u32); 7] = [
    (MODE, 0x80),
    (IN_SIZE, (MB_W << 16) | MB_H),
    (IN_STRIDE, MB_W << 16),
    (IN_CTRL, 0),
    (PARAM, 0x110),
    (QP, 0x041e1e),
    (VLE_OFFSET, 0),
];

/// `struct lemnos_regjob_info` (48 bytes): the fields the check reads.
#[derive(Clone, Copy)]
struct Info {
    wedged: u32,
    live: u32,
    jobs: u64,
    refused: u64,
    timeouts: u64,
    resets: u64,
}

/// A buffer the session imported: its dma-buf (kept open, as the C check keeps it), its index for
/// the job, its device address and size, and the CPU's mapping of it.
struct Buf {
    _fd: OwnedFd,
    index: u32,
    addr: u64,
    bytes: u64,
    map: Mapping,
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
            writes: vec![0; (MAX_WRITES as usize + 1) * WRITE_BYTES],
            addrs: vec![0; (MAX_ADDRS as usize + 1) * ADDR_BYTES],
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
    thread::sleep(Duration::from_millis(ms));
}

/// Whether descriptor `fd` is open: its entry under /proc/self/fd exists.
fn is_open(fd: i32) -> bool {
    fs::symlink_metadata(format!("/proc/self/fd/{fd}")).is_ok()
}

/// A dma-buf of `len` bytes from the heap, imported by the session and mapped for the CPU.
fn import(heap: &OwnedFd, sess: &OwnedFd, len: u64) -> Option<Buf> {
    let fd = dma_heap::allocate(heap.as_fd(), len).ok()?;
    let mut arg = [0u8; 24];
    put32(&mut arg, 0, fd.as_raw_fd() as u32);
    guest::ioctl_bytes(sess.as_fd(), IMPORT, &mut arg).ok()?;
    let map = Mapping::new(fd.as_fd(), len as usize, 0, true).ok()?;
    Some(Buf {
        _fd: fd,
        index: get32(&arg, 4),
        addr: get64(&arg, 16),
        bytes: get64(&arg, 8),
        map,
    })
}

/// Releases an imported buffer by its index: the errno, 0 for success.
fn release(sess: &OwnedFd, index: u32) -> i32 {
    errno_of(guest::ioctl_bytes(
        sess.as_fd(),
        RELEASE,
        &mut index.to_ne_bytes(),
    ))
}

/// Starts or runs a job on the session: the errno, 0 for success.
fn run_job(sess: &OwnedFd, j: &mut Job) -> i32 {
    errno_of(guest::ioctl_bytes(sess.as_fd(), RUN, &mut j.run))
}

/// Collects a job started without waiting: the errno, and the status the interrupt gave.
fn wait_job(j: &mut Job, sess: &OwnedFd) -> (i32, u32) {
    let mut w = [0u8; 24];
    let nreads = get32(&j.run, RUN_NREADS);
    put64(&mut w, 0, addr_mut(&mut j.values));
    put32(&mut w, 8, nreads);
    let e = errno_of(guest::ioctl_bytes(sess.as_fd(), WAIT, &mut w));
    (e, get32(&w, 12))
}

fn set_nwrites(j: &mut Job, n: u32) {
    put32(&mut j.run, RUN_NWRITES, n);
}

fn push_write(j: &mut Job, offset: u32, value: u32) {
    let n = get32(&j.run, RUN_NWRITES) as usize;
    put32(&mut j.writes, n * WRITE_BYTES, offset);
    put32(&mut j.writes, n * WRITE_BYTES + 4, value);
    set_nwrites(j, n as u32 + 1);
}

/// Sets the value of the write to `offset`, adding the write if the job has none.
fn set_write(j: &mut Job, offset: u32, value: u32) {
    let n = get32(&j.run, RUN_NWRITES) as usize;
    for i in 0..n {
        if get32(&j.writes, i * WRITE_BYTES) == offset {
            put32(&mut j.writes, i * WRITE_BYTES + 4, value);
            return;
        }
    }
    push_write(j, offset, value);
}

/// Removes the write to `offset`, the last write taking its place.
fn drop_write(j: &mut Job, offset: u32) {
    let n = get32(&j.run, RUN_NWRITES) as usize;
    for i in 0..n {
        if get32(&j.writes, i * WRITE_BYTES) == offset {
            let last = (n - 1) * WRITE_BYTES;
            let (o, v) = (get32(&j.writes, last), get32(&j.writes, last + 4));
            put32(&mut j.writes, i * WRITE_BYTES, o);
            put32(&mut j.writes, i * WRITE_BYTES + 4, v);
            set_nwrites(j, (n - 1) as u32);
            return;
        }
    }
}

fn set_addr_reg(j: &mut Job, k: usize, reg: u32) {
    put32(&mut j.addrs, k * ADDR_BYTES, reg);
}

fn set_addr_buffer(j: &mut Job, k: usize, buffer: u32) {
    put32(&mut j.addrs, k * ADDR_BYTES + 4, buffer);
}

fn set_addr_offset(j: &mut Job, k: usize, offset: u64) {
    put64(&mut j.addrs, k * ADDR_BYTES + 8, offset);
}

fn set_addr_bytes(j: &mut Job, k: usize, bytes: u64) {
    put64(&mut j.addrs, k * ADDR_BYTES + 16, bytes);
}

/// A good 64x32 NV12 encode: each address 4 KiB apart in one buffer.
fn good_job(j: &mut Job, main: &Buf) {
    j.reset();
    for (offset, value) in GOOD_WRITES {
        push_write(j, offset, value);
    }
    for (k, &reg) in SLOT_REGS.iter().enumerate() {
        set_addr_reg(j, k, reg);
        set_addr_buffer(j, k, main.index);
        set_addr_offset(j, k, k as u64 * SLOT_SPACING);
        set_addr_bytes(j, k, if k == 2 { VLE_BYTES } else { SLOT_NEED[k] });
    }
    put32(&mut j.reads, 0, VLE_MAX);
    put32(&mut j.reads, 4, STATUS);
    j.point();
    put32(&mut j.run, RUN_NADDRS, SLOTS as u32);
    put32(&mut j.run, RUN_NREADS, 2);
    put32(&mut j.run, RUN_START, START_VALUE);
    put32(&mut j.run, RUN_TIMEOUT, 3000);
}

/// The harness's state: the control node, the dma-heap, the block's sysfs directory (the fake
/// block's controls), and the count of failed checks.
struct Harness {
    sysdir: String,
    ctl: OwnedFd,
    heap: OwnedFd,
    failures: u32,
}

impl Harness {
    fn check(&mut self, ok: bool, what: &str) {
        say(&format!("{} {what}", if ok { "ok  " } else { "FAIL" }));
        self.failures += u32::from(!ok);
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

    fn poke(&mut self, off: u32, value: u32) {
        if !self.sys_put("poke", &format!("{off:x} {value:x}")) {
            say(&format!("FAIL poke {off:x}"));
            self.failures += 1;
        }
    }

    fn stamp(&self, reg: u32, byte: u8) -> bool {
        self.sys_put("stamp", &format!("{reg:x} {byte:x}"))
    }

    fn raise_irq(&mut self) {
        if !self.sys_put("irq", "1") {
            say("FAIL irq");
            self.failures += 1;
        }
    }

    /// The fake block's whole register window into `out`, which must be `WINDOW` bytes.
    fn read_regs(&mut self, out: &mut [u8]) {
        let read = File::open(format!("{}/regs", self.sysdir))
            .is_ok_and(|f| f.read_at(out, 0).is_ok_and(|n| n == WINDOW));
        if !read {
            say("FAIL regs");
            self.failures += 1;
        }
    }

    fn reg(&mut self, off: u32) -> u32 {
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

    /// Waits until the start register holds the start value: everything is written by then.
    fn wait_started(&mut self) -> bool {
        for _ in 0..2000 {
            if self.reg(TRIGGER) == START_VALUE {
                return true;
            }
            sleep_ms(1);
        }
        false
    }

    fn info(&mut self, sess: &OwnedFd) -> Info {
        let mut b = [0xffu8; 48];
        if let Err(e) = guest::ioctl_bytes(sess.as_fd(), INFO, &mut b) {
            let errno = e.raw_os_error().unwrap_or(libc::EIO);
            say(&format!("FAIL info: {}", strerror(errno)));
            self.failures += 1;
        }
        Info {
            wedged: get32(&b, 8),
            live: get32(&b, 12),
            jobs: get64(&b, 16),
            refused: get64(&b, 24),
            timeouts: get64(&b, 32),
            resets: get64(&b, 40),
        }
    }

    /// A job refused with `err`, and no register touched.
    fn refused(&mut self, j: &mut Job, sess: &OwnedFd, err: i32, what: &str) {
        let mut before = vec![0u8; WINDOW];
        let mut after = vec![0u8; WINDOW];
        self.read_regs(&mut before);
        let got = run_job(sess, j);
        self.read_regs(&mut after);
        let text = format!("{what}: {}", strerror(err));
        if got != err {
            say(&format!("     got {}", strerror(got)));
        }
        let same = before == after;
        self.check(got == err && same, &text);
    }
}

/// The session on the block: the profile and window the module reports, or `None` when it won't open.
fn open_session(h: &mut Harness, block: u32) -> Option<Arc<OwnedFd>> {
    let mut o = [0u8; 24];
    put32(&mut o, 0, block);
    let fd = guest::regjob_open(h.ctl.as_fd(), &mut o).ok()?;
    let (profile, window, abi) = (get32(&o, 12), get32(&o, 16), get32(&o, 20));
    h.check(
        profile == PROFILE_A733_VE && window == WINDOW as u32 && abi == ABI,
        "the session runs the encoder's profile on a 4 KiB window",
    );
    Some(Arc::new(fd))
}

/// A raw `LEMNOS_IOC_REGJOB_OPEN` on the control node, with the request's 24 bytes: the errno, 0
/// for a session opened (its descriptor is then left in the request, as the C check leaves it).
fn raw_open(ctl: &OwnedFd, o: &mut [u8; 24]) -> i32 {
    errno_of(guest::ioctl_bytes(ctl.as_fd(), REGJOB_OPEN, o))
}

fn test_one_session(h: &mut Harness, sess: &OwnedFd) {
    let block = module::find_block(h.ctl.as_fd(), "lemnos-test-ve")
        .ok()
        .flatten()
        .unwrap_or(0);
    let mut o = [0u8; 24];
    put32(&mut o, 0, block);
    let e = raw_open(&h.ctl, &mut o);
    h.check(
        e == libc::EBUSY,
        "a second session on the block is refused (EBUSY)",
    );
    put32(&mut o, 4, OPEN_CONFIG);
    let e = raw_open(&h.ctl, &mut o);
    h.check(
        e == libc::EINVAL,
        "a configuration session on a profile with no such role (EINVAL)",
    );
    put32(&mut o, 4, 2);
    let e = raw_open(&h.ctl, &mut o);
    h.check(e == libc::EINVAL, "unknown flags (EINVAL)");
    let e = errno_of(guest::ioctl_bytes(sess.as_fd(), WRITE, &mut [0u8; 32]));
    h.check(
        e == libc::EINVAL,
        "writes outside a job, on a profile whose registers move with its jobs (EINVAL)",
    );
    let e = errno_of(guest::ioctl_bytes(sess.as_fd(), EVENT, &mut [0u8; 16]));
    h.check(e == libc::EINVAL, "events on a frames session (EINVAL)");
    let e = release(sess, 63);
    h.check(
        e == libc::ENOENT,
        "releasing an index never imported (ENOENT)",
    );
}

fn test_good_job(h: &mut Harness, sess: &Arc<OwnedFd>, main: &mut Buf) {
    let mut j = Job::default();
    good_job(&mut j, main);
    h.poke(TRIGGER, 0);
    h.poke(STATUS, 0);
    main.map.as_mut_slice().fill(0);
    let sess_t = Arc::clone(sess);
    let runner = thread::spawn(move || {
        let result = run_job(&sess_t, &mut j);
        (j, result)
    });
    let started = h.wait_started();
    h.check(started, "a good job is written and started");
    let mut ok = true;
    for (k, &reg) in SLOT_REGS.iter().enumerate() {
        let want = main.addr.wrapping_add(k as u64 * SLOT_SPACING);
        if h.reg(reg) != want as u32 {
            say(&format!(
                "     slot {reg:x} holds {:x}, not {want:x}",
                h.reg(reg)
            ));
            ok = false;
        }
        if !h.stamp(reg, (0x40 + k) as u8) {
            ok = false;
        }
    }
    h.check(
        ok,
        "every address register holds the buffer's address plus the job's offset",
    );
    let end = main.addr.wrapping_add(2 * SLOT_SPACING + VLE_BYTES - 1);
    let vle_end = h.reg(VLE_END);
    h.check(
        vle_end == end as u32,
        "the bitstream's end is the module's, from the checked length",
    );
    let vle_max = h.reg(VLE_MAX);
    h.check(
        vle_max == (VLE_BYTES * 8) as u32,
        "the bitstream's size in bits is the module's",
    );
    let stamped = h.stamp(VLE_END, 0x7e);
    h.check(stamped, "the block writes the bitstream's last byte");
    let avc = h.reg(AVC_CTRL);
    h.check((avc & 7) == 7, "the completion interrupt is enabled");
    let (in_size, param) = (h.reg(IN_SIZE), h.reg(PARAM));
    h.check(
        in_size == ((MB_W << 16) | MB_H) && param == 0x110,
        "the job's own registers are written",
    );
    h.poke(STATUS, 1);
    h.raise_irq();
    let (j, result) = runner
        .join()
        .unwrap_or_else(|_| panic!("the job's thread panicked"));
    h.check(result == 0, "the job ends at its interrupt");
    h.check(
        get32(&j.run, RUN_STATUS) == 1,
        "with the status the interrupt gave",
    );
    h.check(
        get32(&j.values, 0) == (VLE_BYTES * 8) as u32,
        "registers are read back after the job",
    );
    let avc = h.reg(AVC_CTRL);
    h.check((avc & 7) == 0, "the handler turned the interrupt off");
    let ok = (0..SLOTS).all(|k| main.map.as_slice()[k * SLOT_SPACING as usize] == 0x40 + k as u8);
    h.check(
        ok,
        "what the block wrote at each address is in the caller's buffer, where the job said",
    );
    let last = main.map.as_slice()[(2 * SLOT_SPACING + VLE_BYTES - 1) as usize];
    h.check(
        last == 0x7e,
        "the bitstream's last byte is the job's last byte",
    );
    let counted = h.info(sess).jobs == 1 && h.info(sess).live == 0;
    h.check(counted, "the job is counted, and nothing is live after it");
}

fn test_refusals(h: &mut Harness, sess: &OwnedFd, main: &Buf) {
    let mut j = Job::default();
    let refused_before = h.info(sess).refused;
    h.poke(TRIGGER, 0);

    // Registers.
    good_job(&mut j, main);
    set_write(&mut j, VLE_ADDR, main.addr as u32);
    h.refused(&mut j, sess, libc::EPERM, "a write to an address register");
    good_job(&mut j, main);
    set_write(&mut j, IN_LUMA, 0);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a write of 0 to an address register",
    );
    good_job(&mut j, main);
    set_write(&mut j, VLE_END, 0xffff_ffff);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a write to the bitstream's end, which the module derives",
    );
    good_job(&mut j, main);
    set_write(&mut j, VLE_MAX, 0xffff_ffff);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a write to the bitstream's size, which the module derives",
    );
    good_job(&mut j, main);
    set_write(&mut j, AVC_CTRL, 7);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a write to the interrupt enables, which are the module's",
    );
    good_job(&mut j, main);
    set_write(&mut j, TOP_RESET, 1);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a write to the top reset register",
    );
    good_job(&mut j, main);
    set_write(&mut j, 0x100, 1);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a write to a register outside the profile",
    );
    good_job(&mut j, main);
    set_write(&mut j, PARAM, 0x1);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a bit the profile doesn't let a caller set",
    );
    good_job(&mut j, main);
    set_write(&mut j, 0x1000, 0);
    h.refused(&mut j, sess, libc::EINVAL, "a register past the window");
    good_job(&mut j, main);
    set_write(&mut j, 0xa02, 0);
    h.refused(&mut j, sess, libc::EINVAL, "a register off its boundary");
    good_job(&mut j, main);
    set_nwrites(&mut j, MAX_WRITES + 1);
    h.refused(
        &mut j,
        sess,
        libc::EINVAL,
        "more writes than a job may have",
    );

    // Addresses.
    good_job(&mut j, main);
    put32(&mut j.run, RUN_NADDRS, (SLOTS - 1) as u32);
    h.refused(&mut j, sess, libc::EINVAL, "an address register left out");
    good_job(&mut j, main);
    set_addr_reg(&mut j, 1, IN_LUMA);
    h.refused(
        &mut j,
        sess,
        libc::EINVAL,
        "an address register named twice",
    );
    good_job(&mut j, main);
    set_addr_reg(&mut j, 0, PARAM);
    h.refused(
        &mut j,
        sess,
        libc::EINVAL,
        "a register that takes no address named as one",
    );
    good_job(&mut j, main);
    set_addr_buffer(&mut j, 0, 40);
    h.refused(
        &mut j,
        sess,
        libc::ENOENT,
        "a buffer the session never imported",
    );
    good_job(&mut j, main);
    set_addr_buffer(&mut j, 0, MAX_BUFFERS);
    h.refused(&mut j, sess, libc::ENOENT, "a buffer index past the table");
    let other = import(&h.heap, sess, 4096);
    h.check(other.is_some(), "a second buffer imported");
    let other_index = other.as_ref().map_or(0, |b| b.index);
    let e = release(sess, other_index);
    h.check(e == 0, "and released");
    good_job(&mut j, main);
    set_addr_buffer(&mut j, 0, other_index);
    h.refused(&mut j, sess, libc::ENOENT, "a buffer the session released");
    good_job(&mut j, main);
    set_addr_offset(&mut j, 0, BUF_BYTES - 1024);
    set_addr_bytes(&mut j, 0, 2048);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "a range running past the buffer's end",
    );
    good_job(&mut j, main);
    set_addr_offset(&mut j, 0, BUF_BYTES);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "a range starting at the buffer's end",
    );
    good_job(&mut j, main);
    set_addr_offset(&mut j, 0, u64::MAX - 100);
    set_addr_bytes(&mut j, 0, 4096);
    h.refused(&mut j, sess, libc::EFAULT, "an offset and length that wrap");
    good_job(&mut j, main);
    set_addr_offset(&mut j, 0, 0);
    set_addr_bytes(&mut j, 0, u64::MAX);
    h.refused(&mut j, sess, libc::EFAULT, "a length that wraps");
    good_job(&mut j, main);
    set_addr_bytes(&mut j, 0, 0);
    h.refused(&mut j, sess, libc::EINVAL, "a length of 0");
    good_job(&mut j, main);
    set_addr_offset(&mut j, 0, 16);
    h.refused(
        &mut j,
        sess,
        libc::EINVAL,
        "an address the register can't take (unaligned)",
    );
    good_job(&mut j, main);
    set_addr_bytes(&mut j, 0, 2047);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "an input plane shorter than the frame the block reads",
    );
    good_job(&mut j, main);
    set_addr_bytes(&mut j, 3, 2047);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "a reference picture shorter than the block reads",
    );
    good_job(&mut j, main);
    set_addr_bytes(&mut j, 10, 63);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "a macroblock buffer shorter than the block writes",
    );
    good_job(&mut j, main);
    set_write(&mut j, IN_STRIDE, 8 << 16);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "a stride that makes the input reach past its range",
    );
    good_job(&mut j, main);
    set_write(&mut j, IN_CTRL, 1 << 29);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "a format whose chroma reaches past its range",
    );
    good_job(&mut j, main);
    set_write(&mut j, VLE_OFFSET, (VLE_BYTES * 8) as u32);
    h.refused(&mut j, sess, libc::EFAULT, "a bitstream offset at its end");

    // Geometry.
    good_job(&mut j, main);
    drop_write(&mut j, IN_SIZE);
    h.refused(&mut j, sess, libc::EINVAL, "a job without its frame size");
    good_job(&mut j, main);
    push_write(&mut j, IN_SIZE, (1 << 16) | 1);
    h.refused(&mut j, sess, libc::EINVAL, "a frame size written twice");
    good_job(&mut j, main);
    set_write(&mut j, IN_STRIDE, 2 << 16);
    h.refused(
        &mut j,
        sess,
        libc::EINVAL,
        "a stride narrower than the frame",
    );
    good_job(&mut j, main);
    set_write(&mut j, IN_SIZE, 0);
    h.refused(&mut j, sess, libc::EINVAL, "an empty frame");
    good_job(&mut j, main);
    set_write(&mut j, IN_CTRL, 2 << 29);
    h.refused(
        &mut j,
        sess,
        libc::EINVAL,
        "an input format the profile doesn't size",
    );
    good_job(&mut j, main);
    set_write(&mut j, MODE, 0);
    h.refused(&mut j, sess, libc::EINVAL, "the encoder not selected");

    // The rest of the job.
    good_job(&mut j, main);
    put32(&mut j.run, RUN_START, 1);
    h.refused(
        &mut j,
        sess,
        libc::EPERM,
        "a start value the profile doesn't allow",
    );
    good_job(&mut j, main);
    put32(&mut j.run, RUN_TIMEOUT, 0);
    h.refused(&mut j, sess, libc::EINVAL, "no timeout");
    good_job(&mut j, main);
    put32(&mut j.run, RUN_TIMEOUT, MAX_TIMEOUT_MS + 1);
    h.refused(&mut j, sess, libc::EINVAL, "a timeout past the limit");
    good_job(&mut j, main);
    put32(&mut j.reads, 0, 0x1000);
    h.refused(&mut j, sess, libc::EINVAL, "a read past the window");
    good_job(&mut j, main);
    put32(&mut j.reads, 0, 0xb05);
    h.refused(&mut j, sess, libc::EINVAL, "a read off its boundary");
    good_job(&mut j, main);
    put64(&mut j.run, RUN_WRITES, 16);
    h.refused(
        &mut j,
        sess,
        libc::EFAULT,
        "writes at an address the caller can't read",
    );
    good_job(&mut j, main);
    put32(&mut j.run, RUN_FLAGS, 2);
    h.refused(&mut j, sess, libc::EINVAL, "an unknown flag");
    good_job(&mut j, main);
    put32(&mut j.run, RUN_RESERVED, 1);
    h.refused(&mut j, sess, libc::EINVAL, "a reserved field set");

    let counted = h.info(sess).refused.wrapping_sub(refused_before) == 43;
    h.check(counted, "every refusal is counted");
    let ran = h.info(sess).jobs == 1;
    h.check(ran, "and none ran");
    let started = h.reg(TRIGGER) == 0;
    h.check(started, "the block was never started");
}

fn test_timeout(h: &mut Harness, sess: &Arc<OwnedFd>, main: &Buf) {
    let mut j = Job::default();
    let before = h.resets();

    good_job(&mut j, main);
    put32(&mut j.run, RUN_TIMEOUT, 200);
    h.poke(TRIGGER, 0);
    h.poke(STATUS, 0);
    let e = run_job(sess, &mut j);
    h.check(
        e == libc::ETIMEDOUT,
        "a job the block never finishes ends with ETIMEDOUT",
    );
    let pulsed = h.resets() == before + 1;
    h.check(pulsed, "and the block's reset line was pulsed");
    let i = h.info(sess);
    h.check(
        i.timeouts == 1 && i.resets == 1 && i.wedged == 0 && i.live == 0,
        "the block was recovered: nothing live, not wedged",
    );
    let avc = h.reg(AVC_CTRL);
    h.check((avc & 7) == 0, "its interrupt is off after the reset");

    // The interrupt the block would have raised arrives late: it belongs to no job.
    h.poke(STATUS, 1);
    h.poke(AVC_CTRL, 7);
    h.raise_irq();
    sleep_ms(20);

    good_job(&mut j, main);
    h.poke(TRIGGER, 0);
    h.poke(STATUS, 0);
    let sess_t = Arc::clone(sess);
    let runner = thread::spawn(move || {
        let result = run_job(&sess_t, &mut j);
        (j, result)
    });
    let started = h.wait_started();
    h.check(started, "the next job starts on the recovered block");
    sleep_ms(50);
    let avc = h.reg(AVC_CTRL);
    h.check((avc & 7) != 0, "a late interrupt didn't finish it");
    h.poke(STATUS, 1);
    h.raise_irq();
    let (j, result) = runner
        .join()
        .unwrap_or_else(|_| panic!("the job's thread panicked"));
    h.check(
        result == 0 && get32(&j.run, RUN_STATUS) == 1,
        "and it finishes at its own interrupt",
    );
}

fn test_nowait(h: &mut Harness, sess: &OwnedFd, main: &Buf) {
    let mut j = Job::default();
    let mut k = Job::default();
    let jobs = h.info(sess).jobs;
    let before = h.resets();

    let (e, _) = wait_job(&mut j, sess);
    h.check(e == libc::EINVAL, "nothing to collect (EINVAL)");
    good_job(&mut j, main);
    put32(&mut j.run, RUN_FLAGS, RUN_NOWAIT);
    h.poke(TRIGGER, 0);
    h.poke(STATUS, 0);
    let e = run_job(sess, &mut j);
    let started = e == 0 && h.reg(TRIGGER) == START_VALUE;
    h.check(started, "a job started without waiting");
    let (e, _) = wait_job(&mut j, sess);
    h.check(
        e == libc::EAGAIN,
        "is not over until its interrupt (EAGAIN)",
    );
    good_job(&mut k, main);
    let e = run_job(sess, &mut k);
    h.check(
        e == libc::EBUSY,
        "no other job starts before it is collected (EBUSY)",
    );
    put32(&mut j.run, RUN_NREADS, 1);
    let (e, _) = wait_job(&mut j, sess);
    h.check(
        e == libc::EINVAL,
        "it is collected with its own reads (EINVAL)",
    );
    put32(&mut j.run, RUN_NREADS, 2);
    let live = h.info(sess).live == 1;
    h.check(live, "its buffer is live");
    h.poke(STATUS, 1);
    h.raise_irq();
    sleep_ms(20);
    put32(&mut j.values, 0, 0);
    let (e, status) = wait_job(&mut j, sess);
    let collected = e == 0 && status == 1 && get32(&j.values, 0) == (VLE_BYTES * 8) as u32;
    h.check(
        collected,
        "after its interrupt it is collected, with its status and reads",
    );
    let counted = h.info(sess).jobs == jobs.wrapping_add(1) && h.info(sess).live == 0;
    h.check(counted, "counted, and nothing is live");
    let (e, _) = wait_job(&mut j, sess);
    h.check(e == libc::EINVAL, "and only once");

    good_job(&mut j, main);
    put32(&mut j.run, RUN_FLAGS, RUN_NOWAIT);
    put32(&mut j.run, RUN_TIMEOUT, 100);
    h.poke(TRIGGER, 0);
    h.poke(STATUS, 0);
    let e = run_job(sess, &mut j);
    h.check(e == 0, "a job started without waiting that never ends");
    let (e, _) = wait_job(&mut j, sess);
    let running = e == libc::EAGAIN && h.resets() == before;
    h.check(running, "runs until its time is up");
    sleep_ms(150);
    let (e, _) = wait_job(&mut j, sess);
    let timed_out = e == libc::ETIMEDOUT && h.resets() == before + 1 && h.info(sess).live == 0;
    h.check(
        timed_out,
        "then the collection resets the block (ETIMEDOUT) and nothing is live",
    );
    let (e, _) = wait_job(&mut j, sess);
    h.check(e == libc::EINVAL, "and there is nothing more to collect");
}

fn test_unbind(h: &mut Harness, sess: Arc<OwnedFd>, main: &Buf, device: &str, unbind: &str) {
    let mut j = Job::default();
    let unbound = match OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(unbind)
    {
        Ok(mut f) => f.write(device.as_bytes()).is_ok_and(|n| n == device.len()),
        Err(_) => false,
    };
    h.check(unbound, "the block unbinds with a session open");
    good_job(&mut j, main);
    let e = run_job(&sess, &mut j);
    h.check(
        e == libc::ENODEV,
        "the session's jobs fail with ENODEV after the unbind",
    );
    let mut imp = [0u8; 24];
    put32(&mut imp, 0, h.heap.as_raw_fd() as u32);
    let e = errno_of(guest::ioctl_bytes(sess.as_fd(), IMPORT, &mut imp));
    h.check(e == libc::ENODEV, "and its imports");
    let raw = sess.as_raw_fd();
    let was_open = is_open(raw);
    drop(sess);
    let closed = was_open && !is_open(raw);
    h.check(closed, "the session closes after the unbind");
}

fn finish(h: &Harness) -> ExitCode {
    if h.failures > 0 {
        say("lemnos_regjob_check: FAILED");
        ExitCode::from(1)
    } else {
        say("lemnos_regjob_check: passed");
        ExitCode::SUCCESS
    }
}

/// Opens a register-job session on `block`, with a check that its profile is the encoder's.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        say("usage: lemnos_regjob_check /dev/lemnos <device> <unbind> <heap>");
        return ExitCode::from(2);
    }
    let (node, device, unbind, heap_path) = (&args[1], &args[2], &args[3], &args[4]);

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
        failures: 0,
    };
    let powered = module::power(h.ctl.as_fd(), block, (0, 0), "", (0, 0));
    let refused = matches!(&powered, Err(e) if e.raw_os_error() == Some(libc::ENOTTY));
    h.check(
        refused,
        "the block's clocks and resets are the module's (ENOTTY)",
    );

    let sess = open_session(&mut h, block);
    h.check(sess.is_some(), "a session opens");
    let Some(sess) = sess else {
        return ExitCode::from(1);
    };
    let Some(mut main) = import(&h.heap, &sess, BUF_BYTES) else {
        h.check(false, "a buffer is imported with its size");
        return finish(&h);
    };
    let sized = main.bytes == BUF_BYTES;
    h.check(sized, "a buffer is imported with its size");
    let fits = main.addr != 0
        && main
            .addr
            .checked_add(BUF_BYTES)
            .is_some_and(|end| end <= 1 << 32);
    h.check(fits, "its address fits the block's 32-bit registers");
    test_one_session(&mut h, &sess);
    test_good_job(&mut h, &sess, &mut main);
    test_refusals(&mut h, &sess, &main);
    test_timeout(&mut h, &sess, &main);
    test_nowait(&mut h, &sess, &main);
    let released = release(&sess, main.index) == 0;
    h.check(
        released,
        "a buffer is released once the block is done with it",
    );
    let raw = sess.as_raw_fd();
    let was_open = is_open(raw);
    drop(sess);
    let closed = was_open && !is_open(raw);
    h.check(closed, "the session closes");

    let Some(sess) = open_session(&mut h, block) else {
        h.check(false, "a new session opens after the close");
        return finish(&h);
    };
    h.check(true, "a new session opens after the close");
    match import(&h.heap, &sess, BUF_BYTES) {
        Some(b) => {
            main = b;
            h.check(true, "and imports");
        }
        None => h.check(false, "and imports"),
    }
    test_unbind(&mut h, sess, &main, device, unbind);
    finish(&h)
}
