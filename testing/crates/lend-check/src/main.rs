//! The dma-buf lend checks, against the fake PiSP back end block (the `lemnos-test-pisp` block of the
//! test harness, module parameter `test_pb_fake_regs`).
//!
//! A real dma-buf from the system heap is lent to a back end stream with
//! `LEMNOS_STREAM_IOC_REGISTER_LENT`. Checked, in order:
//! - a memfd, which is not a dma-buf, is refused with EINVAL and nothing is registered;
//! - a system-heap dma-buf is either one contiguous range (released at once) or refused with EINVAL;
//! - a CMA dma-buf is pinned while registered, a job reads it, and unregistering it after the job
//!   ends is accepted; a second unregister is ENOENT; a registration again with the same generation
//!   is a pin-cache hit (the same device address); a normal stop releases the pin;
//! - a stop with a lent dma-buf still registered, on a wedged block, reports the wedge and keeps the
//!   dma-buf pinned.
//!
//! Usage: `lemnos-lend-check /dev/lemnos <device> <bind> <unbind> <cma heap> <system heap>` (init
//! runs it as `/lemnos_lend_check`). Port of `testing/qemu/lemnos_lend_check.c` (CZ-GUEST-2).
//!
//! The uAPI structures are laid out in bytes, sized as the C structures are: `lemnos.h`'s
//! `lemnos_stream_create` (56 bytes, inside the 80-byte `lemnos_pisp_be_create`),
//! `lemnos_stream_register_lent` (32), `lemnos_stream_unregister` (8), `lemnos_stream_status` (40),
//! `lemnos_pisp_be_status` (24, `lemnos_pisp_be.h`) and `lemnos_pisp_be_job` (120). The configuration
//! is `struct pisp_be_tiles_config` from the kernel's `pisp_be_config.h` (16720 bytes); the offsets
//! below are that header's, counted with the C compiler against the kernel tree's headers. The
//! dma-buf and device addresses the check passes are plain numbers, and the buffers it reads are
//! the kernel's own.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{dma_buf, dma_heap, guest, ioc, ior, iow, iowr, module};

/// The fake block's sysfs directory: `poke`, `irq` and `regs`.
const SYS: &str = "/sys/bus/platform/devices/lemnos-test-pisp";

// lemnos.h: the stream's requests on the stream's file.
const STREAM_KICK: u32 = ioc(0, b'L', 0x81, 0);
const STREAM_REGISTER_LENT: u32 = iowr::<[u8; 32]>(b'L', 0x8a);
const STREAM_UNREGISTER: u32 = iow::<[u8; 8]>(b'L', 0x83);
const STREAM_STATUS: u32 = ior::<[u8; 40]>(b'L', 0x84);
// lemnos_pisp_be.h: the back end's status, on the stream's file.
const PISP_BE_STATUS: u32 = ior::<[u8; 24]>(b'L', 0xb1);
const _: () = assert!(
    STREAM_KICK == 0x0000_4c81
        && STREAM_REGISTER_LENT == 0xc020_4c8a
        && STREAM_UNREGISTER == 0x4008_4c83
        && STREAM_STATUS == 0x8028_4c84
        && PISP_BE_STATUS == 0x8018_4cb1
);

// lemnos.h: the ring's header words (word indices, as the module's ring is read), the frame and
// state values, and the frame's words.
const RING_FRAMES_HEAD: usize = 16;
const RING_FRAMES_TAIL: usize = 24;
const RING_RETURNS_HEAD: usize = 32;
const RING_JOBS_HEAD: usize = 48;
const RING_FRAMES: usize = 64;
const RING_STATE_OPEN: u32 = 0;
const RING_STATE_FAULT: u32 = 2;
const FRAME_WORDS: usize = 6;

// The stream the check makes: a pool of BUFFERS buffers, FRAME_SLOTS frame slots, JOB_SLOTS job
// slots of a job of JOB_BYTES bytes (one length word, then 15 words: 16 words a slot).
const BUFFERS: u32 = 4;
const BUFFER_BYTES: u32 = 256 * 1024;
const FRAME_SLOTS: u32 = 8;
const JOB_SLOTS: u32 = 4;
const JOB_BYTES: u32 = 120;
const JOB_WORDS: usize = 16;
/// The dma-buf the check lends, from the CMA and the system heap alike.
const LENT_BYTES: u64 = 256 * 1024;

// lemnos_pisp_be.h: the job's device addresses (13 of them), and the one the output is.
const ADDRS: usize = 13;
const ADDR_OUTPUT0: usize = 7;

// The fake block's registers.
const BE_STATUS: u32 = 0x10;
const BE_BATCH: u32 = 0x14;
const BE_INT_STATUS: u32 = 0x1c;
/// Longer than the module's job timeout.
const WEDGE_WAIT: Duration = Duration::from_millis(1400);

const W: u16 = 64;
const H: u16 = 32;
const STRIDE: i32 = 192;
/// `PISP_BE_RGB_ENABLE_INPUT | PISP_BE_RGB_ENABLE_OUTPUT0`.
const RGB_INPUT_OUTPUT0: u32 = 0x0004_0001;
/// `PISP_IMAGE_FORMAT_THREE_CHANNEL | PISP_IMAGE_FORMAT_BPS_8`.
const IMAGE_FORMAT: u32 = 0x4000_0000;

// `struct pisp_be_tiles_config` (16720 bytes): `config` at 0, `tiles[64]` at 6476 (160 bytes each),
// `num_tiles` at 16716. Within `config`: `global` at 112 (`rgb_enables` at 4 of it), `input_format`
// at 124, `output_format[0].image` at 6324. Within `pisp_tile`: `input_width` at 16,
// `input_height` at 18, `output_width[0]` at 132, `output_height[0]` at 136. A `pisp_image_format_config`
// is 16 bytes: width at 0, height at 2, format at 4, stride at 8.
const CONFIG_BYTES: usize = 16720;
const CFG_RGB_ENABLES: usize = 116;
const CFG_INPUT_FORMAT: usize = 124;
const CFG_OUTPUT0_IMAGE: usize = 6324;
const CFG_NUM_TILES: usize = 16716;
const TILE0: usize = 6476;
const TILE_INPUT_WIDTH: usize = TILE0 + 16;
const TILE_INPUT_HEIGHT: usize = TILE0 + 18;
const TILE_OUTPUT_WIDTH0: usize = TILE0 + 132;
const TILE_OUTPUT_HEIGHT0: usize = TILE0 + 136;

const EINVAL: i32 = libc::EINVAL;
const ENOENT: i32 = libc::ENOENT;
const ETIMEDOUT: i32 = libc::ETIMEDOUT;

/// Prints one line of output, unbuffered as the C check's `printf` is.
fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// `strerror`: the text of an errno, as the C check's musl `strerror` gives it.
fn strerror(errno: i32) -> String {
    let text = io::Error::from_raw_os_error(errno).to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
}

/// `errno` as it stands now.
fn last_errno() -> i32 {
    errno_of(&io::Error::last_os_error())
}

/// The descriptor number of an optional owned descriptor, or -1 for none (as the C code keeps it).
fn raw(fd: &Option<OwnedFd>) -> i32 {
    fd.as_ref().map_or(-1, AsRawFd::as_raw_fd)
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_ne_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_ne_bytes());
}

fn put_i32(b: &mut [u8], at: usize, v: i32) {
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

/// The first place `needle` occurs in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// What `strtol` reads from the start of `text` in base 10: white space, an optional sign, digits.
/// Zero when there are no digits.
fn parse_long(text: &[u8]) -> i64 {
    let mut i = 0;
    while i < text.len() && text[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = text.get(i) == Some(&b'-');
    if matches!(text.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut v: i64 = 0;
    while i < text.len() && text[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add(i64::from(text[i] - b'0'));
        i += 1;
    }
    if negative { -v } else { v }
}

/// The check's state: the failures counted, the CMA heap, and the block under test.
struct Harness {
    failures: u32,
    heap: OwnedFd,
    block: u32,
}

impl Harness {
    /// `CHECK`: a failed check prints its expression, as the C macro stringifies it.
    fn check(&mut self, ok: bool, what: &str) {
        if !ok {
            say(&format!("FAIL {what}"));
            self.failures += 1;
        }
    }

    /// A failure printed with the text the C code gave it.
    fn fail(&mut self, line: &str) {
        say(&format!("FAIL {line}"));
        self.failures += 1;
    }
}

/// Writes `bytes` to the sysfs file at `path`: true when it opened and took every byte.
fn write_node(path: &str, bytes: &[u8]) -> bool {
    OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
        .and_then(|mut f| f.write(bytes))
        .is_ok_and(|n| n == bytes.len())
}

/// Writes `off value` to the fake block's `poke` file.
fn poke(h: &mut Harness, off: u32, value: u32) {
    let text = format!("{off:x} {value:x}");
    if !write_node(&format!("{SYS}/poke"), text.as_bytes()) {
        h.fail(&format!("poke {off:x}"));
    }
}

/// Raises the fake block's interrupt.
fn raise_irq(h: &mut Harness) {
    if !write_node(&format!("{SYS}/irq"), b"1") {
        h.fail("irq");
    }
}

/// Reads the fake block's registers into `buf`, from its `regs` file. When the file does not open,
/// nothing is read and nothing is reported.
fn read_regs(h: &mut Harness, buf: &mut [u8; 64]) {
    let Ok(f) = File::open(format!("{SYS}/regs")) else {
        return;
    };
    let mut at = 0;
    while at < buf.len() {
        match f.read_at(&mut buf[at..], at as u64) {
            Ok(got) if got > 0 => at += got,
            _ => {
                h.fail("regs");
                break;
            }
        }
    }
}

/// A stream the fake back end runs: its file, its mappings, and the producer's own counters.
struct Stream {
    ring: Mapping,
    configs: Mapping,
    fd: OwnedFd,
    _pool: OwnedFd,
    pool_addr: u64,
    returns_at: usize,
    jobs_at: usize,
    jobs_head: u64,
    frames_tail: u64,
    returns_head: u64,
    /// What the fake hardware says it has done.
    hw_started: u8,
    hw_done: u8,
}

/// The ring's words, as atomics. Empty only for a mapping that is not whole words, which opening
/// the stream refuses.
fn words(ring: &Mapping) -> &[AtomicU64] {
    ring.as_atomic_words().unwrap_or(&[])
}

/// Opens a stream on the fake back end block, with a pool from the CMA heap.
fn stream_open(h: &mut Harness, ctl: &OwnedFd) -> Option<Stream> {
    let pool =
        dma_heap::allocate(h.heap.as_fd(), u64::from(BUFFERS) * u64::from(BUFFER_BYTES)).ok()?;
    let mut c = [0u8; 80];
    // `struct lemnos_stream_create`: block, flags, buffers, buffer_bytes, frame_slots, job_slots,
    // job_bytes, pool_fd, doorbell_fd (-1), then fd, ring_bytes and pool_addr out.
    put32(&mut c, 0, h.block);
    put32(&mut c, 8, BUFFERS);
    put32(&mut c, 12, BUFFER_BYTES);
    put32(&mut c, 16, FRAME_SLOTS);
    put32(&mut c, 20, JOB_SLOTS);
    put32(&mut c, 24, JOB_BYTES);
    put_i32(&mut c, 28, pool.as_raw_fd());
    put_i32(&mut c, 32, -1);
    // Out: `config_slot_bytes` (56), `hw_version` (60), `config_mmap_offset` (64), `config_bytes` (72).
    let fd = guest::pisp_be_create(ctl.as_fd(), &mut c).ok()?;
    let ring = Mapping::new(fd.as_fd(), get64(&c, 40) as usize, 0, true).ok()?;
    let configs = Mapping::new(fd.as_fd(), get64(&c, 72) as usize, get64(&c, 64), true).ok()?;
    ring.as_atomic_words()?;
    // The fake block's counters carry on across streams, as a real one's do.
    let mut regs = [0u8; 64];
    read_regs(h, &mut regs);
    let batch = get32(&regs, BE_BATCH as usize);
    let returns_at = RING_FRAMES + FRAME_SLOTS as usize * FRAME_WORDS;
    Some(Stream {
        ring,
        configs,
        fd,
        _pool: pool,
        pool_addr: get64(&c, 48),
        returns_at,
        jobs_at: returns_at + FRAME_SLOTS as usize,
        jobs_head: 0,
        frames_tail: 0,
        returns_head: 0,
        hw_done: batch as u8,
        hw_started: (batch >> 8) as u8,
    })
}

/// `struct lemnos_stream_register_lent`'s fields out.
#[derive(Clone, Copy)]
struct Lent {
    region: u32,
    addr: u64,
    size: u64,
}

/// `LEMNOS_STREAM_IOC_REGISTER_LENT` for `fd` at `generation`: the outcome, and what the kernel left.
fn lend(st: &Stream, fd: i32, generation: u32) -> (io::Result<()>, Lent) {
    let mut a = [0u8; 32];
    put_i32(&mut a, 0, fd);
    put32(&mut a, 24, generation);
    let res = guest::ioctl_bytes(st.fd.as_fd(), STREAM_REGISTER_LENT, &mut a);
    (
        res,
        Lent {
            region: get32(&a, 4),
            addr: get64(&a, 8),
            size: get64(&a, 16),
        },
    )
}

/// A lend that must succeed: says what it got when it doesn't.
fn must_lend(h: &mut Harness, st: &Stream, fd: i32, generation: u32, what: &str) -> Option<Lent> {
    let (res, lent) = lend(st, fd, generation);
    match res {
        Ok(()) => Some(lent),
        Err(e) => {
            h.fail(&format!(
                "{what}: REGISTER_LENT returned -1: {}",
                strerror(errno_of(&e))
            ));
            None
        }
    }
}

fn unregister(st: &Stream, region: u32) -> io::Result<()> {
    let mut u = [0u8; 8];
    put32(&mut u, 0, region);
    guest::ioctl_bytes(st.fd.as_fd(), STREAM_UNREGISTER, &mut u)
}

/// `KICK`: the errno the kick failed with, or 0.
fn kick(st: &Stream) -> i32 {
    match guest::ioctl_bytes(st.fd.as_fd(), STREAM_KICK, &mut [0u8; 0]) {
        Ok(()) => 0,
        Err(e) => errno_of(&e),
    }
}

struct StreamStatus {
    state: u32,
    fault: i32,
}

fn stream_status(st: &Stream) -> Option<StreamStatus> {
    let mut s = [0u8; 40];
    guest::ioctl_bytes(st.fd.as_fd(), STREAM_STATUS, &mut s).ok()?;
    Some(StreamStatus {
        state: get32(&s, 0),
        fault: get32(&s, 4) as i32,
    })
}

struct PispStatus {
    wedged: u32,
}

fn pisp_status(st: &Stream) -> Option<PispStatus> {
    let mut b = [0u8; 24];
    guest::ioctl_bytes(st.fd.as_fd(), PISP_BE_STATUS, &mut b).ok()?;
    Some(PispStatus {
        wedged: get32(&b, 4),
    })
}

/// The config of one RGB888 image in and out, one tile, and the job's addresses: the input at
/// `input` (a lent region's address), the output at `output` (in the pool).
fn cfg_lent(input: u64, output: u64) -> ([u8; CONFIG_BYTES], [u64; ADDRS]) {
    fn image(c: &mut [u8], at: usize) {
        put16(c, at, W);
        put16(c, at + 2, H);
        put32(c, at + 4, IMAGE_FORMAT);
        put_i32(c, at + 8, STRIDE);
    }

    let mut c = [0u8; CONFIG_BYTES];
    let mut addr = [0u64; ADDRS];
    // bayer_enables stays 0.
    put32(&mut c, CFG_RGB_ENABLES, RGB_INPUT_OUTPUT0);
    image(&mut c, CFG_INPUT_FORMAT);
    image(&mut c, CFG_OUTPUT0_IMAGE);
    put32(&mut c, CFG_NUM_TILES, 1);
    put16(&mut c, TILE_INPUT_WIDTH, W);
    put16(&mut c, TILE_INPUT_HEIGHT, H);
    put16(&mut c, TILE_OUTPUT_WIDTH0, W);
    put16(&mut c, TILE_OUTPUT_HEIGHT0, H);
    addr[0] = input;
    addr[ADDR_OUTPUT0] = output;
    (c, addr)
}

/// Posts a job of `identity` whose input is `input`, config slot 0.
fn post_job(st: &mut Stream, identity: u64, input: u64) {
    let (cfg, addr) = cfg_lent(input, st.pool_addr);
    st.configs.write_bytes(0, &cfg);
    // `struct lemnos_pisp_be_job`: identity, buffer 0, config 0, then the addresses.
    let mut job = [0u8; JOB_BYTES as usize];
    put64(&mut job, 0, identity);
    for (i, a) in addr.iter().enumerate() {
        put64(&mut job, 16 + 8 * i, *a);
    }
    post(st, &job);
}

/// Puts `job` as the next slot of the ring.
fn post(st: &mut Stream, job: &[u8; JOB_BYTES as usize]) {
    let slot = (st.jobs_head as usize) & (JOB_SLOTS as usize - 1);
    let at = st.jobs_at + slot * JOB_WORDS;
    let ring = words(&st.ring);
    ring[at].store(u64::from(JOB_BYTES), Ordering::Relaxed);
    for i in 0..JOB_WORDS - 1 {
        ring[at + 1 + i].store(get64(job, 8 * i), Ordering::Relaxed);
    }
    st.jobs_head += 1;
    fence(Ordering::Release);
    ring[RING_JOBS_HEAD].store(st.jobs_head, Ordering::Relaxed);
}

/// The fake hardware says it started and finished one job.
fn complete_job(h: &mut Harness, st: &mut Stream, started: u8, done: u8) {
    st.hw_started = st.hw_started.wrapping_add(started);
    st.hw_done = st.hw_done.wrapping_add(done);
    poke(
        h,
        BE_BATCH,
        u32::from(st.hw_done) | (u32::from(st.hw_started) << 8),
    );
    poke(h, BE_INT_STATUS, 1);
    raise_irq(h);
}

/// The next frame's buffer, and its identity, or -1 if none arrives in time. Gives the buffer back.
fn wait_frame(st: &mut Stream) -> (i32, u64) {
    let mut identity = 0;
    for _ in 0..1000 {
        let ring = words(&st.ring);
        let head = ring[RING_FRAMES_HEAD].load(Ordering::Acquire);
        if head != st.frames_tail {
            let at =
                RING_FRAMES + (st.frames_tail as usize & (FRAME_SLOTS as usize - 1)) * FRAME_WORDS;
            let word = ring[at].load(Ordering::Relaxed);
            let fence_word = ring[at + 4].load(Ordering::Relaxed);
            identity = ring[at + 2].load(Ordering::Relaxed);
            st.frames_tail += 1;
            ring[RING_FRAMES_TAIL].store(st.frames_tail, Ordering::Release);
            let buffer = word as u32;
            let ret_at = st.returns_at + (st.returns_head as usize & (FRAME_SLOTS as usize - 1));
            ring[ret_at].store(u64::from(buffer) | (fence_word << 32), Ordering::Relaxed);
            st.returns_head += 1;
            ring[RING_RETURNS_HEAD].store(st.returns_head, Ordering::Release);
            return (buffer as i32, identity);
        }
        thread::sleep(Duration::from_millis(2));
    }
    (-1, identity)
}

/// The dma-buf's reference count, from its fdinfo: above 1 means the kernel still holds it. -1 when
/// the descriptor has no fdinfo.
fn dmabuf_count(fd: i32) -> i64 {
    let Ok(mut f) = File::open(format!("/proc/self/fdinfo/{fd}")) else {
        return -1;
    };
    let mut text = [0u8; 255];
    let n = match f.read(&mut text) {
        Ok(n) if n > 0 => n,
        _ => return -1,
    };
    match find(&text[..n], b"count:") {
        Some(p) => parse_long(&text[p + 6..]),
        None => -1,
    }
}

/// Whether the fdinfo of `fd` names `name` as its exporter: "exp_name:\t<name>" for a system heap
/// buffer, say.
fn exported_by(fd: i32, name: &str) -> bool {
    let Ok(mut f) = File::open(format!("/proc/self/fdinfo/{fd}")) else {
        return false;
    };
    let mut text = [0u8; 511];
    let n = match f.read(&mut text) {
        Ok(n) if n > 0 => n,
        _ => return false,
    };
    find(&text[..n], format!("exp_name:\t{name}\n").as_bytes()).is_some()
}

/// The module refuses a memfd, which is not a dma-buf, and registers nothing.
fn refuse_memfd(h: &mut Harness, ctl: &OwnedFd) {
    let m = dma_buf::memfd(c"lend-refused").ok();
    h.check(m.is_some(), "m >= 0");
    let st = stream_open(h, ctl);
    h.check(st.is_some(), "!stream_open(&st)");
    let Some(st) = st else {
        return;
    };
    let (res, a) = lend(&st, raw(&m), 1);
    let r = if res.is_ok() { 0 } else { -1 };
    let errno = match &res {
        Err(e) => errno_of(e),
        Ok(()) => last_errno(),
    };
    if r == -1 && errno == EINVAL {
        say(&format!(
            "ok   memfd refused: {} (not a dma-buf), nothing registered",
            strerror(errno)
        ));
    } else {
        h.fail(&format!(
            "memfd lend: returned {r} errno {} (want -1 {})",
            strerror(errno),
            strerror(EINVAL)
        ));
    }
    h.check(r == -1 && errno == EINVAL, "r == -1 && errno == EINVAL");
    h.check(a.addr == 0 && a.size == 0, "a.addr == 0 && a.size == 0");
    h.check(
        matches!(stream_status(&st), Some(s) if s.state == RING_STATE_OPEN),
        "!ioctl(st.fd, LEMNOS_STREAM_IOC_STATUS, &status) && status.state == LEMNOS_RING_STATE_OPEN",
    );
    drop(m);
    drop(st);
}

/// The system heap: the kernel lends a dma-buf only as one contiguous DMA range, and a system heap
/// buffer is not one in general. A registration that succeeds is released at once; one that is
/// refused is refused with EINVAL and nothing is held.
fn system_heap(h: &mut Harness, ctl: &OwnedFd, system: &str) {
    let sys_heap = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(system)
        .ok();
    let sys = sys_heap
        .as_ref()
        .and_then(|heap| dma_heap::allocate(heap.as_fd(), LENT_BYTES).ok());
    h.check(sys.is_some(), "sys >= 0");
    h.check(
        exported_by(raw(&sys), "system"),
        "exported_by(sys, \"system\")",
    );
    let st = stream_open(h, ctl);
    h.check(st.is_some(), "!stream_open(&st)");
    let Some(st) = st else {
        return;
    };
    let (res, a) = lend(&st, raw(&sys), 1);
    match res {
        Ok(()) => {
            say(&format!(
                "ok   system-heap dma-buf is one contiguous range here: addr 0x{:x} size {}",
                a.addr, a.size
            ));
            h.check(
                unregister(&st, a.region).is_ok(),
                "!unregister(&st, a.region)",
            );
        }
        Err(e) => {
            let errno = errno_of(&e);
            h.check(errno == EINVAL, "r == -1 && errno == EINVAL");
            say(&format!(
                "ok   system-heap dma-buf refused: {} (not one contiguous DMA range), nothing registered",
                strerror(errno)
            ));
        }
    }
}

/// A CMA dma-buf, one contiguous range: lent, read by a job, released on finish; the same generation
/// hits the pin cache; a normal stop releases it.
fn lend_release(h: &mut Harness, ctl: &OwnedFd) {
    let lent = dma_heap::allocate(h.heap.as_fd(), LENT_BYTES).ok();
    let lent_fd = raw(&lent);
    h.check(lent.is_some(), "lent >= 0");
    let before = dmabuf_count(lent_fd);
    let st = stream_open(h, ctl);
    h.check(st.is_some(), "!stream_open(&st)");
    let Some(mut st) = st else {
        return;
    };
    let Some(a) = must_lend(h, &st, lent_fd, 1, "lend cma") else {
        return;
    };
    h.check(
        a.addr != 0 && a.size >= LENT_BYTES,
        "a.addr != 0 && a.size >= LENT_BYTES",
    );
    let held = dmabuf_count(lent_fd);
    h.check(held > before, "held > before");
    say(&format!(
        "ok   lent a CMA dma-buf: addr 0x{:x} size {}, count {before} -> {held}",
        a.addr, a.size
    ));

    post_job(&mut st, 5, a.addr);
    h.check(kick(&st) == 0, "!kick(&st)");
    complete_job(h, &mut st, 1, 1);
    let (buffer, identity) = wait_frame(&mut st);
    h.check(buffer == 0 && identity == 5, "buffer == 0 && identity == 5");

    // The job ended: the lent region may be let go.
    h.check(
        unregister(&st, a.region).is_ok(),
        "!unregister(&st, a.region)",
    );
    let again = unregister(&st, a.region);
    h.check(
        again.err().and_then(|e| e.raw_os_error()) == Some(ENOENT),
        "unregister(&st, a.region) == -1 && errno == ENOENT",
    );
    // Unregistered, the module keeps the pin for its generation.
    h.check(
        dmabuf_count(lent_fd) > before,
        "dmabuf_count(lent) > before",
    );

    // The same generation again: a pin-cache hit, the same device address.
    if let Some(b) = must_lend(h, &st, lent_fd, 1, "lend cma again") {
        h.check(b.addr == a.addr, "b.addr == a.addr");
        h.check(
            unregister(&st, b.region).is_ok(),
            "!unregister(&st, b.region)",
        );
    }

    // A normal stop: idle and unwedged, so the stream closes and the pin is released.
    h.check(
        pisp_status(&st).is_some_and(|p| p.wedged == 0),
        "!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 0",
    );
    drop(st);
    let after = dmabuf_count(lent_fd);
    h.check(after == before, "after == before");
    say(&format!(
        "ok   a job finished, the lend was released, a normal stop released the pin: count {after}"
    ));
    drop(lent);
}

/// A stop with a lent dma-buf still registered on a wedged block: the wedge is reported, and the
/// dma-buf stays pinned.
fn lend_wedge(h: &mut Harness, ctl: &OwnedFd) {
    let lent = dma_heap::allocate(h.heap.as_fd(), LENT_BYTES).ok();
    let lent_fd = raw(&lent);
    h.check(lent.is_some(), "lent >= 0");
    let before = dmabuf_count(lent_fd);
    let st = stream_open(h, ctl);
    h.check(st.is_some(), "!stream_open(&st)");
    let Some(mut st) = st else {
        return;
    };
    let Some(a) = must_lend(h, &st, lent_fd, 1, "lend cma for the wedge") else {
        return;
    };
    post_job(&mut st, 7, a.addr);
    h.check(kick(&st) == 0, "!kick(&st)");
    // Busy, never finishing: the module times the job out and wedges the block.
    poke(h, BE_STATUS, 2);
    thread::sleep(WEDGE_WAIT);
    h.check(
        matches!(
            stream_status(&st),
            Some(s) if s.state == RING_STATE_FAULT && s.fault == ETIMEDOUT
        ),
        "!ioctl(st.fd, LEMNOS_STREAM_IOC_STATUS, &status) && status.state == LEMNOS_RING_STATE_FAULT && status.fault == ETIMEDOUT",
    );
    // The wedge flag, read before the stream stops.
    h.check(
        pisp_status(&st).is_some_and(|p| p.wedged == 1),
        "!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 1",
    );
    drop(st);
    let pinned = dmabuf_count(lent_fd);
    h.check(pinned > before, "pinned > before");
    say(&format!(
        "ok   wedged stop: wedge flag 1, lent dma-buf kept pinned, count {before} -> {pinned}"
    ));
    drop(lent);
}

/// Unbinds and binds the block: the fake block's registers and wedge go back to a fresh block.
fn rebind(h: &mut Harness, bind: &str, unbind: &str, device: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(unbind)
    {
        let wrote = f.write(device.as_bytes()).is_ok_and(|n| n == device.len());
        h.check(
            wrote,
            "write(fd, device, strlen(device)) == (ssize_t)strlen(device)",
        );
    }
    poke(h, BE_BATCH, 0);
    poke(h, BE_STATUS, 0);
    poke(h, BE_INT_STATUS, 0);
    h.check(
        write_node(bind, device.as_bytes()),
        "fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device)",
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 {
        say(
            "usage: lemnos_lend_check /dev/lemnos <device> <bind> <unbind> <cma heap> <system heap>",
        );
        return ExitCode::from(2);
    }
    let (node, device, bind, unbind, heap_path, system) =
        (&args[1], &args[2], &args[3], &args[4], &args[5], &args[6]);

    let ctl = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(node)
        .map(OwnedFd::from);
    let heap = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(heap_path)
        .map(OwnedFd::from);
    let (ctl, heap) = match (ctl, heap) {
        (Ok(ctl), Ok(heap)) => (ctl, heap),
        (ctl, heap) => {
            // The errno the last open left: the heap's when it failed, else the node's.
            let errno = match &heap {
                Err(e) => errno_of(e),
                Ok(_) => ctl.as_ref().err().map_or(0, errno_of),
            };
            say(&format!(
                "FAIL open {node}/{heap_path}: {}",
                strerror(errno)
            ));
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
        failures: 0,
        heap,
        block,
    };
    refuse_memfd(&mut h, &ctl);
    system_heap(&mut h, &ctl, system);
    lend_release(&mut h, &ctl);
    lend_wedge(&mut h, &ctl);
    // The wedge is the block's: a fresh block is what the next check gets.
    drop(ctl);
    rebind(&mut h, bind, unbind, device);
    if h.failures > 0 {
        say(&format!("FAILED: {} checks", h.failures));
        ExitCode::from(1)
    } else {
        say("ok   lemnos_lend_check: all checks passed");
        ExitCode::SUCCESS
    }
}
