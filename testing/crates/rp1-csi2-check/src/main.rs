//! The rp1-csi2 receiver on the fake block in the test module: frames reach the stream with every
//! programmed address taken from the pool, a bad START or a hostile interrupt status is refused, and
//! stopping (STOP, closing either file, unbinding) waits for the channel's DMA before anything is
//! released. The same on a stream whose buffers are dma-bufs of their own: the channel is armed with
//! each buffer's own address, writes there, and follows a swap.
//!
//! Usage: `lemnos-rp1-csi2-check /dev/lemnos <block> <csi2 device> <unbind> <heap>` (init runs it as
//! `/lemnos_rp1_csi2_check`), or `wedge <csi2 device> <unbind> <heap>`, or `wedge-own <csi2 device>
//! <unbind> <heap>`.
//!
//! Port of `kernel/test/lemnos_rp1_csi2_check.c` (CZ-GUEST-5), in the shared-memory ring form of
//! `kernel/test/shm_ring.h` (I2b2-6). Every ok and FAIL line keeps the C's text and order, including
//! the nested INFO lines the C prints when a message's arguments call `info()`.
//!
//! Departures from the C, each on purpose:
//! - A FAIL line's "(line N)" is not printed: the C's `CHECK` prints its condition only.
//! - Where the C would go on with a stream or descriptor that never opened, the Rust prints the FAIL
//!   line and gives up the rest of that test; the summary still prints. The C would use a zeroed
//!   stream or a -1 descriptor.
//! - Ring, image and pool words outside their mapping read as 0 and writes there are dropped, where
//!   the C would read or write past the mapping. No check reaches one in a passing run.
//! - Descriptors are closed by drop, in the C's order; the mappings go before the descriptors.
//! - The frame's pair, time and end words are not read beyond the fields the checks use.
//! - The fake's FIRE of the all-ones status goes through the same ioctl; no separate helper.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::time::Duration;

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{descriptor, dma_buf, dma_heap, guest, module, time};
use lemnos_linux_sys::{ioc, ior, iow, iowr};

// lemnos.h: the stream's requests, on a stream's file and on the control node.
const KICK: u32 = ioc(0, b'L', 0x81, 0);
const BUFFER: u32 = iowr::<[u8; 24]>(b'L', 0x86);

// lemnos_rp1_csi2.h: the receiver's requests, on the receiver's node.
const RP1_INFO: u32 = ior::<[u8; 56]>(b'L', 0xc0);
const RP1_START: u32 = iow::<[u8; 48]>(b'L', 0xc2);
const RP1_STOP: u32 = ioc(0, b'L', 0xc3, 0);
const RP1_FRAME_STARTS: u32 = ior::<[u8; 24]>(b'L', 0xc4);
const RP1_EMBEDDED: u32 = iow::<[u8; 16]>(b'L', 0xc5);
const RP1_CLAIM: u32 = iow::<[u8; 8]>(b'L', 0xc6);

// The fake receiver's requests (testing/qemu/test_block/lemnos_test_csi2.h), on /dev/lemnos-test-csi2.
const TCSI_FIRE: u32 = iow::<u32>(b'T', 1);
const TCSI_PEEK: u32 = ior::<[u8; 32]>(b'T', 2);
const TCSI_BUSY: u32 = iow::<u32>(b'T', 3);
const TCSI_LINES: u32 = iow::<u32>(b'T', 8);
const TCSI_PEEK_EMB: u32 = ior::<[u8; 32]>(b'T', 11);
const TCSI_BUSY_LEFT: u32 = ior::<u32>(b'T', 13);
const TCSI_BLANK: u32 = iow::<u32>(b'T', 14);
const TCSI_LATE_ACK: u32 = iow::<u32>(b'T', 15);

const TCSI_FS: u32 = 0x1;
const TCSI_FE_ACK: u32 = 0x100;
const TCSI_EMB_FS: u32 = 0x2;
const TCSI_EMB_FE_ACK: u32 = 0x200;

// lemnos_rp1_csi2.h.
const ABI: u32 = 1;
const INFO_WEDGED: u32 = 1 << 0;
const UNPACK16: u32 = 1 << 0;
const PART_CSI2: u32 = 1 << 0;
const EMBEDDED_DT: u32 = 0x12;
const EMBEDDED_MAX: u32 = 65520;

// lemnos.h: the ring's end word (LEMNOS_RING_STATE_*).
const RING_STATE_OPEN: u64 = 0;
const RING_STATE_CLOSED: u64 = 1;

// kernel/test/shm_ring.h (layout 2): the word numbers of the frame ring, the pool page and the job block.
const SHM_W_HEAD: usize = 8;
const SHM_W_END: usize = 10;
const SHM_INDEX: usize = 17;
const SHM_SLOT_WORDS: usize = 10;
const SHM_ENTRY: usize = 4;
const SHM_TICKETS: usize = 8;
const SHM_READERS: usize = 64;
const SHM_JOB_HEAD: usize = 2;
const SHM_JOB_SLOTS: usize = 4;

// The check's own sizes (the C's #defines).
const BUFFERS: u32 = 4;
const BUFFER_BYTES: u32 = 8192;
const STRIDE_POOL: u64 = 8192;
const FRAME_SLOTS: u32 = 8;
const JOB_SLOTS: usize = 4;
/// `sizeof(struct lemnos_stream_job)`: identity, buffer, count and four ranges.
const JOB_BYTES: usize = 80;
/// A job slot: its length word, then the job's words.
const JOB_WORDS: usize = 1 + JOB_BYTES / 8;
const EMB_BYTES: u32 = 256;

// A frame that fits a buffer with room to spare: stride * height is its size.
const F_STRIDE: u32 = 512;
const F_HEIGHT: u32 = 4;
const F_SIZE: u32 = F_STRIDE * F_HEIGHT;

/// The checks that failed so far.
static FAILURES: AtomicU32 = AtomicU32::new(0);

fn failures() -> u32 {
    FAILURES.load(Ordering::Relaxed)
}

/// Writes text to stdout and flushes, unbuffered as the C check's `printf` is.
fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// `CHECK(cond, ...)`: the prefix, then the message (whose arguments may print, as the C's do), then
/// the line. The condition is evaluated before the prefix, as the C's `ok_` is.
macro_rules! check {
    ($cond:expr, $($arg:tt)+) => {{
        let ok_: bool = $cond;
        say(if ok_ { "ok   " } else { "FAIL " });
        if !ok_ {
            FAILURES.fetch_add(1, Ordering::Relaxed);
        }
        let msg = format!($($arg)+);
        say(&msg);
        say("\n");
    }};
}

fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
}

/// The C's `-errno` result of a request: 0, or the negated errno.
fn ret(r: io::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => -errno_of(&e),
    }
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

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn open_rdwr(path: &str) -> Option<OwnedFd> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
        .ok()
        .map(OwnedFd::from)
}

/// `lrc_line_us`: the time for one line of `bytes` on `lanes` lanes at `mbps`, rounded up, in us.
fn lrc_line_us(bytes: u64, lanes: u32, mbps: u32) -> u64 {
    let rate = u64::from(lanes) * u64::from(mbps);
    (bytes * 8).div_ceil(rate)
}

// ---------------------------------------------------------------------------------------------
// The test context: the receiver's node and the fake's node (the C's globals `ctl` and `hw`), and
// the pool's CPU mapping.
// ---------------------------------------------------------------------------------------------

struct Ctx {
    ctl: Option<OwnedFd>,
    hw: Option<OwnedFd>,
    pool_cpu: Option<Mapping>,
}

fn ctl_fd(c: &Ctx) -> Option<BorrowedFd<'_>> {
    c.ctl.as_ref().map(|f| f.as_fd())
}

fn hw_fd(c: &Ctx) -> Option<BorrowedFd<'_>> {
    c.hw.as_ref().map(|f| f.as_fd())
}

/// A request on a descriptor that may not be open: an absent one fails with EBADF, as `ioctl(-1)` does.
fn ioc_on<const N: usize>(
    fd: Option<BorrowedFd<'_>>,
    request: u32,
    arg: &mut [u8; N],
) -> io::Result<()> {
    match fd {
        Some(fd) => guest::ioctl_bytes(fd, request, arg),
        None => Err(io::Error::from_raw_os_error(libc::EBADF)),
    }
}

/// A `u32` argument to a request on the fake's node, by value (as the C passes `&u32`).
fn hw_u32(c: &Ctx, request: u32, value: u32) -> io::Result<()> {
    let mut b = value.to_ne_bytes();
    ioc_on(hw_fd(c), request, &mut b)
}

/// `CLAIM` the receiver part on `fd`: 0, or -errno.
fn claim_csi2(fd: Option<BorrowedFd<'_>>) -> i32 {
    let mut b = [0u8; 8];
    put32(&mut b, 0, PART_CSI2);
    ret(ioc_on(fd, RP1_CLAIM, &mut b))
}

/// The receiver's INFO. Filled with 0xff first, as the C's memset, and read back only on success.
#[derive(Clone, Copy)]
struct Info {
    abi: u32,
    flags: u32,
    streaming: u32,
    frames: u64,
    written: u64,
    dropped: u64,
    errors: u64,
}

fn info(c: &Ctx) -> Info {
    let mut b = [0xffu8; 56];
    check!(ioc_on(ctl_fd(c), RP1_INFO, &mut b).is_ok(), "INFO");
    Info {
        abi: get32(&b, 0),
        flags: get32(&b, 12),
        streaming: get32(&b, 16),
        frames: get64(&b, 24),
        written: get64(&b, 32),
        dropped: get64(&b, 40),
        errors: get64(&b, 48),
    }
}

/// The frame-start count (`struct lemnos_rp1_csi2_frame_starts`).
#[derive(Clone, Copy)]
struct FrameStarts {
    frames: u64,
    latest_ns: u64,
    valid: u32,
}

fn frame_starts(c: &Ctx) -> FrameStarts {
    let mut b = [0xffu8; 24];
    check!(
        ioc_on(ctl_fd(c), RP1_FRAME_STARTS, &mut b).is_ok(),
        "FRAME_STARTS"
    );
    FrameStarts {
        frames: get64(&b, 0),
        latest_ns: get64(&b, 8),
        valid: get32(&b, 16),
    }
}

/// The fake's registers (`struct tcsi_regs`), memset to 0xff as the C's.
#[derive(Clone, Copy)]
struct Regs {
    addr0: u32,
    addr1: u32,
    length: u32,
    stride: u32,
    ctrl: u32,
    frame_size: u32,
}

fn regs_of(b: &[u8; 32]) -> Regs {
    Regs {
        addr0: get32(b, 0),
        addr1: get32(b, 4),
        length: get32(b, 8),
        stride: get32(b, 12),
        ctrl: get32(b, 16),
        frame_size: get32(b, 20),
    }
}

fn peek(c: &Ctx) -> Regs {
    let mut b = [0xffu8; 32];
    let _ = ioc_on(hw_fd(c), TCSI_PEEK, &mut b);
    regs_of(&b)
}

fn peek_emb(c: &Ctx) -> Regs {
    let mut b = [0xffu8; 32];
    let _ = ioc_on(hw_fd(c), TCSI_PEEK_EMB, &mut b);
    regs_of(&b)
}

/// The address the channel has armed: `addr1 << 36 | addr0 << 4`.
fn armed_addr(c: &Ctx) -> u64 {
    let r = peek(c);
    u64::from(r.addr1) << 36 | u64::from(r.addr0) << 4
}

/// Freezes the counter for `lines` of the busy window: a blanking gap, the DMA still busy.
fn blank(c: &Ctx, lines: u32) {
    let _ = hw_u32(c, TCSI_BLANK, lines);
}

fn busy(c: &Ctx, lines: u32) {
    let _ = hw_u32(c, TCSI_BUSY, lines);
}

/// The lines the fake still has to write after FORCE: 0 once the block's DMA is done.
fn busy_left(c: &Ctx) -> u32 {
    let mut b = u32::MAX.to_ne_bytes();
    check!(
        ioc_on(hw_fd(c), TCSI_BUSY_LEFT, &mut b).is_ok(),
        "fake: lines in flight read"
    );
    u32::from_ne_bytes(b)
}

/// Fires a status word and waits until the handler has seen it.
fn fire(c: &Ctx, status: u32) {
    let frames = info(c).frames;
    let mut b = status.to_ne_bytes();
    check!(
        ioc_on(hw_fd(c), TCSI_FIRE, &mut b).is_ok(),
        "fire {status:x}"
    );
    for _ in 0..400 {
        sleep_ms(1);
        if info(c).frames != frames || status & TCSI_FS == 0 {
            break;
        }
    }
    sleep_ms(3);
}

/// The receiver's START argument (`struct lemnos_rp1_csi2_start`, 48 bytes).
type StartArg = [u8; 48];

fn good_start(stream_fd: i32) -> StartArg {
    let mut a = [0u8; 48];
    put_i32(&mut a, 0, stream_fd);
    put32(&mut a, 4, 2);
    put32(&mut a, 8, 800);
    put32(&mut a, 12, 0);
    put32(&mut a, 16, 0x2b);
    put32(&mut a, 20, 16);
    put32(&mut a, 24, F_HEIGHT);
    put32(&mut a, 28, F_STRIDE);
    put32(&mut a, 32, F_SIZE);
    a
}

fn start(c: &Ctx, a: &StartArg) -> i32 {
    let mut b = *a;
    ret(ioc_on(ctl_fd(c), RP1_START, &mut b))
}

fn stop(c: &Ctx) -> i32 {
    let mut b = [0u8; 0];
    ret(ioc_on(ctl_fd(c), RP1_STOP, &mut b))
}

/// `ask_embedded`: the embedded stream for the next START, or -errno.
fn ask_embedded(c: &Ctx, fd: i32, bytes: u32, reserved0: u32) -> i32 {
    let mut b = [0u8; 16];
    put_i32(&mut b, 0, fd);
    put32(&mut b, 4, bytes);
    put32(&mut b, 8, reserved0);
    ret(ioc_on(ctl_fd(c), RP1_EMBEDDED, &mut b))
}

// ---------------------------------------------------------------------------------------------
// A frame stream: its ring, its consumer's marks, and the consumer's view of the frames.
// ---------------------------------------------------------------------------------------------

/// `struct shm_marks`: the consumer's page of hold marks, its memfd and its registration.
#[derive(Default)]
struct Marks {
    page: Option<Mapping>,
    _reader: Option<OwnedFd>,
    _fd: Option<OwnedFd>,
    version: u64,
}

/// `struct stream`: the ring is dropped first, then the marks, then the descriptor, as the C's
/// `stream_close` does.
struct Stream {
    ring: Mapping,
    marks: Marks,
    fd: OwnedFd,
    pool_addr: u64,
    control: usize,
    jobs_at: usize,
    jobs_head: u64,
    frames_tail: u64,
}

/// A frame the module published, as the consumer reads it.
#[derive(Clone, Copy, Default)]
struct Frame {
    buffer: u32,
    bytes: u32,
    sequence: u64,
    identity: u64,
}

/// `shm_page_at`: the pool page's first word, after the frame ring.
fn shm_page_at(frame_slots: u32) -> usize {
    SHM_INDEX + frame_slots as usize + frame_slots as usize * SHM_SLOT_WORDS
}

/// `shm_control`: the control block's first word, after the pool page's tickets and cut-off words.
fn shm_control(frame_slots: u32, buffers: u32) -> usize {
    shm_page_at(frame_slots) + SHM_TICKETS + buffers as usize + SHM_READERS
}

fn words(m: &Mapping) -> &[AtomicU64] {
    m.as_atomic_words().unwrap_or(&[])
}

/// A ring word, acquire. A word past the ring reads as 0.
fn load_at(words: &[AtomicU64], at: usize) -> u64 {
    words.get(at).map_or(0, |w| w.load(Ordering::Acquire))
}

/// A ring word, release. A word past the ring is not written.
fn store_at(words: &[AtomicU64], at: usize, v: u64) {
    if let Some(w) = words.get(at) {
        w.store(v, Ordering::Release);
    }
}

/// `shm_marks_open`: makes the page and registers it on the stream. False, with nothing made, when
/// the page or the module refuses it.
fn marks_open(m: &mut Marks, stream: BorrowedFd<'_>, buffers: u32) -> bool {
    let bytes = (1 + 2 * u64::from(buffers)) * 8;
    let Ok(fd) = dma_buf::sealed_memfd(c"lemnos-marks", bytes) else {
        return false;
    };
    // The C adds the seal set at once: shrink, grow and seal. sealed_memfd adds the first two.
    if descriptor::add_seals(
        fd.as_fd(),
        libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
    )
    .is_err()
    {
        return false;
    }
    let Ok(page) = Mapping::new(fd.as_fd(), bytes as usize, 0, true) else {
        return false;
    };
    if let Some(w) = words(&page).first() {
        w.store(2 << 56, Ordering::Relaxed);
    }
    let mut arg = [0u8; 48];
    put_i32(&mut arg, 0, fd.as_raw_fd());
    let Ok(reader) = guest::stream_reader(stream, &mut arg) else {
        return false;
    };
    *m = Marks {
        page: Some(page),
        _reader: Some(reader),
        _fd: Some(fd),
        version: 0,
    };
    true
}

/// `shm_marks_set`: one mark, with the version odd while it changes, as shm/ring-holds does.
fn marks_set(m: &mut Marks, buffer: u32, mark: u64) {
    const MASK: u64 = (1 << 56) - 1;
    let Some(page) = m.page.as_ref() else {
        return;
    };
    let w = words(page);
    m.version = (m.version + 1) & MASK;
    store_at(w, 0, 2 << 56 | m.version);
    store_at(w, 1 + buffer as usize, mark);
    m.version = (m.version + 1) & MASK;
    store_at(w, 0, 2 << 56 | m.version);
}

/// `shm_frame_read`: frame `seq`, if the module has published it: 1 with `f` filled, 0 while it
/// is not out yet, -1 when its slot holds another frame.
fn frame_read(ring: &[AtomicU64], frame_slots: u32, seq: u64, f: &mut Frame) -> i32 {
    if load_at(ring, SHM_W_HEAD) <= seq {
        return 0;
    }
    let slot = (load_at(
        ring,
        SHM_INDEX + (seq & u64::from(frame_slots - 1)) as usize,
    ) >> 16) as usize;
    let at = SHM_INDEX + frame_slots as usize + slot.wrapping_mul(SHM_SLOT_WORDS);
    let stamp = load_at(ring, at);
    if stamp != (seq.wrapping_add(1)) << 1 {
        return -1;
    }
    let pair = load_at(ring, at + SHM_ENTRY + 1);
    let sequence = load_at(ring, at + SHM_ENTRY + 2);
    let identity = load_at(ring, at + SHM_ENTRY + 3);
    let bytes = load_at(ring, at + SHM_ENTRY + 4);
    if load_at(ring, at) != stamp {
        return -1;
    }
    f.buffer = pair as u32;
    f.bytes = bytes as u32;
    f.sequence = sequence;
    f.identity = identity;
    1
}

/// `stream_open`: a stream on the receiver, with a pool (or none: each buffer a dma-buf of its own).
fn stream_open(c: &Ctx, pool: Option<&OwnedFd>) -> Result<Stream, i32> {
    let mut arg = [0u8; 56];
    put32(&mut arg, 4, if pool.is_none() { 1 } else { 0 });
    put32(&mut arg, 8, BUFFERS);
    put32(&mut arg, 12, BUFFER_BYTES);
    put32(&mut arg, 16, FRAME_SLOTS);
    put32(&mut arg, 20, JOB_SLOTS as u32);
    put32(&mut arg, 24, JOB_BYTES as u32);
    put_i32(&mut arg, 28, pool.map_or(-1, |p| p.as_raw_fd()));
    put_i32(&mut arg, 32, -1);
    let fd = guest::rp1_csi2_stream_create(ctl_fd(c).ok_or(-libc::EBADF)?, &mut arg)
        .map_err(|e| -errno_of(&e))?;
    let ring_bytes = get64(&arg, 40) as usize;
    let pool_addr = get64(&arg, 48);
    let ring = Mapping::new(fd.as_fd(), ring_bytes, 0, true).map_err(|e| -errno_of(&e))?;
    let control = shm_control(FRAME_SLOTS, BUFFERS);
    Ok(Stream {
        ring,
        marks: Marks::default(),
        fd,
        pool_addr,
        control,
        jobs_at: control + SHM_JOB_SLOTS,
        jobs_head: 0,
        frames_tail: 0,
    })
}

/// The ring's words of a stream, as atomics.
fn ring_words(s: &Stream) -> &[AtomicU64] {
    words(&s.ring)
}

/// `hold_buffer`: the consumer's mark on `buffer`, the page made on first use. An error when it
/// can't be made.
fn hold_buffer(s: &mut Stream, buffer: u32) -> Result<(), ()> {
    if s.marks.page.is_none() && !marks_open(&mut s.marks, s.fd.as_fd(), BUFFERS) {
        return Err(());
    }
    marks_set(&mut s.marks, buffer, 1);
    Ok(())
}

/// `pop_frame`: the next frame, if one is out. It is the consumer's until `give_back`, so its
/// buffer is held.
fn pop_frame(s: &mut Stream, f: &mut Frame) -> bool {
    let mut sf = Frame::default();
    if frame_read(ring_words(s), FRAME_SLOTS, s.frames_tail, &mut sf) != 1 {
        return false;
    }
    *f = sf;
    s.frames_tail += 1;
    if hold_buffer(s, f.buffer).is_err() {
        say(&format!(
            "FAIL the consumer could not mark buffer {}\n",
            f.buffer
        ));
    }
    true
}

/// The consumer is done with a frame's buffer: its mark comes off, so a job may name it.
fn give_back(s: &mut Stream, buffer: u32) {
    if s.marks.page.is_some() {
        marks_set(&mut s.marks, buffer, 0);
    }
}

/// `post`: a job in the next slot, then the consumer's head.
fn post(s: &mut Stream, job: &[u8; JOB_BYTES]) {
    let at = s.jobs_at + (s.jobs_head & (JOB_SLOTS as u64 - 1)) as usize * JOB_WORDS;
    let w = words(&s.ring);
    store_at(w, at, JOB_BYTES as u64);
    for i in 0..JOB_BYTES / 8 {
        store_at(w, at + 1 + i, get64(job, 8 * i));
    }
    s.jobs_head += 1;
    fence(Ordering::Release);
    store_at(w, s.control + SHM_JOB_HEAD, s.jobs_head);
}

/// A job, as the consumer posts it: identity, buffer and the count (no ranges).
fn job_bytes(identity: u64, buffer: u32) -> [u8; JOB_BYTES] {
    let mut j = [0u8; JOB_BYTES];
    put64(&mut j, 0, identity);
    put32(&mut j, 8, buffer);
    j
}

fn buffer_addr(s: &Stream, b: u32) -> u64 {
    s.pool_addr + u64::from(b) * STRIDE_POOL
}

/// `expect_armed`: buffer `b` is what the channel has armed, at its address with the frame's size.
fn expect_armed(c: &Ctx, s: &Stream, b: u32, what: &str) {
    let r = peek(c);
    let a = u64::from(r.addr1) << 36 | u64::from(r.addr0) << 4;
    check!(
        a == buffer_addr(s, b) && r.length == F_SIZE >> 4 && r.stride == F_STRIDE >> 4,
        "{what}: buffer {b} armed (addr {a:x} length {} stride {})",
        r.length,
        r.stride
    );
}

/// `stream_close`: the ring, the marks, then the descriptor, as the drop order of `Stream`.
fn stream_close(s: Stream) {
    drop(s);
}

fn pool_byte(c: &Ctx, at: usize) -> u8 {
    c.pool_cpu
        .as_ref()
        .and_then(|m| m.as_slice().get(at).copied())
        .unwrap_or(0)
}

fn pool_zero(c: &mut Ctx) {
    if let Some(m) = c.pool_cpu.as_mut() {
        m.as_mut_slice().fill(0);
    }
}

// ---------------------------------------------------------------------------------------------
// The tests, in the C's order.
// ---------------------------------------------------------------------------------------------

/// Bad starts: each refused, with the receiver left alone and able to run.
fn test_bad_starts(c: &Ctx, pool: &OwnedFd) {
    // (name, byte offset in struct lemnos_rp1_csi2_start, value)
    const BAD: [(&str, usize, u32); 20] = [
        ("lanes 0", 4, 0),
        ("lanes too many", 4, 3),
        ("mbps low", 8, 79),
        ("mbps high", 8, 1501),
        ("vc", 12, 4),
        ("dt 0", 16, 0),
        ("dt 64", 16, 64),
        ("width 0", 20, 0),
        ("width huge", 20, 0x10000),
        ("height 0", 24, 0),
        ("height huge", 24, 0x10000),
        ("stride 0", 28, 0),
        ("stride odd", 28, 17),
        ("stride*height past size", 28, 1024),
        ("size 0", 32, 0),
        ("size odd", 32, 2049),
        ("size past buffer", 32, 8192 + 16),
        ("size past buffer, huge", 32, 0xfffffff0),
        ("flags", 36, 2),
        ("reserved", 40, 1),
    ];

    let Ok(s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let a = good_start(-1);
    check!(
        start(c, &a) == -libc::EBADF,
        "a bad descriptor is refused (EBADF)"
    );
    let a = good_start(ctl_fd(c).map_or(-1, |f| f.as_raw_fd()));
    check!(
        start(c, &a) == -libc::EINVAL,
        "a file that isn't a stream is refused (EINVAL)"
    );

    for (name, off, value) in BAD {
        let mut a = good_start(s.fd.as_raw_fd());
        put32(&mut a, off, value);
        // The rest stays valid, so the field under test is what's refused.
        check!(start(c, &a) == -libc::EINVAL, "refused (EINVAL): {name}");
        check!(info(c).streaming == 0, "not streaming after: {name}");
    }
    let mut a = good_start(s.fd.as_raw_fd());
    put32(&mut a, 36, UNPACK16);
    put32(&mut a, 20, F_STRIDE); // needs a stride of width * 2
    check!(
        start(c, &a) == -libc::EINVAL,
        "refused: UNPACK16 stride too small"
    );

    stream_close(s);
}

/// A stream on another block can't feed this receiver.
fn test_foreign_stream(c: &Ctx, pool: &OwnedFd, block_name: &str) {
    let lem = open_rdwr("/dev/lemnos");
    check!(lem.is_some(), "control node opened");
    let block = lem
        .as_ref()
        .and_then(|l| module::find_block(l.as_fd(), block_name).ok().flatten())
        .unwrap_or(0);
    check!(block != 0, "found the plain block");

    let mut arg = [0u8; 56];
    put32(&mut arg, 0, block);
    put32(&mut arg, 8, BUFFERS);
    put32(&mut arg, 12, BUFFER_BYTES);
    put32(&mut arg, 16, FRAME_SLOTS);
    put32(&mut arg, 20, JOB_SLOTS as u32);
    put32(&mut arg, 24, JOB_BYTES as u32);
    put_i32(&mut arg, 28, pool.as_raw_fd());
    put_i32(&mut arg, 32, -1);
    let made = match lem.as_ref() {
        Some(l) => guest::stream_create(l.as_fd(), &mut arg).ok(),
        None => None,
    };
    check!(made.is_some(), "stream on another block made");
    // The C passes the descriptor the call returned (zero when none came back).
    let fd = made.as_ref().map_or(0, |f| f.as_raw_fd());
    let a = good_start(fd);
    check!(
        start(c, &a) == -libc::EXDEV,
        "a stream made on another block is refused (EXDEV)"
    );
    drop(made);
    drop(lem);
}

/// Frames: START arms, FS latches, FE publishes, and the data is where the kernel said.
fn test_frames(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let mut f = Frame::default();
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    expect_armed(c, &s, 0, "after START");
    let r = peek(c);
    check!(
        r.ctrl & 1 != 0 && (r.ctrl >> 7 & 63) == 0x2b,
        "channel enabled with the data type (ctrl {:x})",
        r.ctrl
    );
    check!(
        r.frame_size == (F_HEIGHT << 16 | 16),
        "frame size register {:x}",
        r.frame_size
    );
    check!(info(c).streaming != 0, "streaming");

    pool_zero(c);
    fire(c, TCSI_FS);
    expect_armed(c, &s, 1, "after frame start");
    fire(c, TCSI_FE_ACK);
    let i = info(c);
    check!(
        i.frames == 1 && i.written == 1,
        "frame written (frames {} written {})",
        i.frames,
        i.written
    );
    check!(pop_frame(&mut s, &mut f), "frame published");
    check!(
        f.buffer == 0 && f.bytes == F_SIZE && f.sequence == 0,
        "frame: buffer {} bytes {} sequence {}",
        f.buffer,
        f.bytes,
        f.sequence
    );
    let seen = pool_byte(c, 0);
    check!(
        seen != 0 && pool_byte(c, F_SIZE as usize - 1) == seen,
        "the channel wrote the frame into its buffer (byte {seen:x})"
    );
    check!(
        pool_byte(c, F_SIZE as usize) == 0,
        "nothing past the frame's bytes"
    );
    check!(
        pool_byte(c, STRIDE_POOL as usize) == 0,
        "the next buffer untouched"
    );
    give_back(&mut s, f.buffer);

    // Now hold every buffer: 1 and 2 go through, 3 is armed. The one after that has nothing to arm:
    // the frame start is dropped and no register is written.
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f) && f.buffer == 1,
        "frame on buffer 1"
    );
    check!(
        pop_frame(&mut s, &mut f) && f.buffer == 2,
        "frame on buffer 2"
    );
    expect_armed(c, &s, 3, "after two frames");
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f) && f.buffer == 3,
        "frame on buffer 3"
    );
    check!(info(c).dropped == 0, "no drops so far");
    expect_armed(c, &s, 0, "the returned buffer is armed");
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f) && f.buffer == 0,
        "frame on the returned buffer"
    );
    let r = peek(c);
    fire(c, TCSI_FS);
    let i = info(c);
    check!(
        i.dropped == 1,
        "a frame start with no buffer armed is dropped ({})",
        i.dropped
    );
    check!(
        peek(c).addr0 == r.addr0,
        "and the armed address is left alone"
    );
    check!(!pop_frame(&mut s, &mut f), "and no frame comes of it");

    stream_close(s);
    check!(
        info(c).streaming == 0,
        "closing the stream stopped the receiver"
    );
}

/// The frame-start count: not valid before START and after STOP; since START it counts the starts,
/// and the latest one's time is CLOCK_MONOTONIC at its own start. A published frame's sequence is
/// the count at its start, less one.
fn test_frame_starts(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let mut f = Frame::default();
    let fs = frame_starts(c);
    check!(
        fs.valid == 0 && fs.frames == 0 && fs.latest_ns == 0,
        "before START the count is not valid (valid {})",
        fs.valid
    );
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    let fs = frame_starts(c);
    check!(
        fs.valid != 0 && fs.frames == 0 && fs.latest_ns == 0,
        "after START the count is valid and empty (valid {} frames {})",
        fs.valid,
        fs.frames
    );
    let t0 = time::monotonic_ns().unwrap_or(0);
    fire(c, TCSI_FS);
    let t1 = time::monotonic_ns().unwrap_or(0);
    let fs = frame_starts(c);
    check!(
        fs.valid != 0 && fs.frames == 1,
        "one start counted (frames {})",
        fs.frames
    );
    check!(
        fs.latest_ns >= t0 && fs.latest_ns <= t1,
        "its time is between the CLOCK_MONOTONIC reads around it ({})",
        fs.latest_ns
    );
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f) && f.sequence == fs.frames.wrapping_sub(1),
        "the frame's sequence is the count less one (sequence {})",
        f.sequence
    );
    check!(stop(c) == 0, "STOP");
    let fs = frame_starts(c);
    check!(fs.valid == 0, "after STOP the count is not valid");
    stream_close(s);
}

/// Nothing armed, no count. With every buffer held by the consumer the channel has no address and
/// makes no frame start, so the count cannot move and is not offered (`valid` zero), where it would
/// read as a sensor gone quiet. Giving a buffer back arms the channel again, and the count is valid
/// again with the starts it had.
fn test_frame_starts_while_starved(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let mut f = [Frame::default(); BUFFERS as usize];
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    let fs = frame_starts(c);
    check!(fs.valid != 0, "armed after START: the count is valid");
    // The consumer holds every buffer: each frame is read and not given back.
    for (n, fr) in f.iter_mut().enumerate() {
        fire(c, TCSI_FS);
        fire(c, TCSI_FE_ACK);
        check!(pop_frame(&mut s, fr), "frame {n} published and held");
    }
    let fs = frame_starts(c);
    check!(
        fs.valid == 0,
        "every buffer held: nothing is armed, the count is not valid (valid {})",
        fs.valid
    );
    sleep_ms(20);
    let fs = frame_starts(c);
    check!(fs.valid == 0, "and it stays so while they are held");
    give_back(&mut s, f[0].buffer);
    sleep_ms(20);
    let fs = frame_starts(c);
    check!(
        fs.valid != 0 && fs.frames == u64::from(BUFFERS),
        "a buffer given back arms the channel: the count is valid again with its starts (valid {} frames {})",
        fs.valid,
        fs.frames
    );
    for fr in f.iter().skip(1) {
        give_back(&mut s, fr.buffer);
    }
    check!(stop(c) == 0, "STOP");
    stream_close(s);
}

/// A frame cut short: its frame start and end came, but the channel wrote one line of it (seen on
/// the board when a start catches a frame mid-way). It is a frame gone wrong, not published; the
/// next whole frame is.
fn test_short_frame(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let mut f = Frame::default();
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    fire(c, TCSI_FS);
    let mut one = 1u32.to_ne_bytes();
    check!(
        ioc_on(hw_fd(c), TCSI_LINES, &mut one).is_ok(),
        "the next frame has one line"
    );
    fire(c, TCSI_FE_ACK);
    let i = info(c);
    check!(
        !pop_frame(&mut s, &mut f),
        "a frame with one line of {F_HEIGHT} isn't published"
    );
    check!(
        i.frames == 1 && i.written == 0 && i.errors == 1,
        "it's an error (frames {} written {} errors {})",
        i.frames,
        i.written,
        i.errors
    );
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f) && f.buffer == 1 && f.bytes == F_SIZE && f.sequence == 1,
        "the next whole frame is (buffer {} sequence {})",
        f.buffer,
        f.sequence
    );
    check!(info(c).written == 1, "and counted written");
    stream_close(s);
}

/// Hostile interrupt status: whatever the block says, only bounded, kernel-held work is done.
fn test_hostile_hw(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let mut f = Frame::default();
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    // A frame end with no frame start: nothing to complete, nothing published.
    fire(c, TCSI_FE_ACK);
    sleep_ms(5);
    check!(
        !pop_frame(&mut s, &mut f),
        "a frame end with no frame start publishes nothing"
    );
    let i = info(c);
    check!(i.frames == 0 && i.written == 0, "and counts nothing");

    // A start before the last frame's end: the old frame is flagged bad, not published.
    fire(c, TCSI_FS);
    fire(c, TCSI_FS);
    let i = info(c);
    check!(
        i.frames == 2 && i.errors == 1,
        "a frame start with one open is an error (frames {} errors {})",
        i.frames,
        i.errors
    );
    check!(!pop_frame(&mut s, &mut f), "the bad frame isn't published");

    // All-ones status, repeatedly: only the bits it means, counts move by one each.
    let frames = info(c).frames;
    for _ in 0..3 {
        let mut all = u32::MAX.to_ne_bytes();
        check!(
            ioc_on(hw_fd(c), TCSI_FIRE, &mut all).is_ok(),
            "fire all ones"
        );
        sleep_ms(5);
    }
    let i = info(c);
    check!(
        i.frames <= frames + 6,
        "frames bounded by interrupts seen ({})",
        i.frames
    );
    while pop_frame(&mut s, &mut f) {
        check!(
            f.buffer < BUFFERS && f.bytes <= F_SIZE,
            "published frames are bounded"
        );
    }
    stream_close(s);
}

/// Job-driven: the next buffer comes from the job; a broken frame comes back with 0 bytes.
fn test_jobs(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let mut f = Frame::default();
    post(&mut s, &job_bytes(0xabc, 2));
    check!(
        ioc_on(Some(s.fd.as_fd()), KICK, &mut [0u8; 0]).is_ok(),
        "job accepted"
    );
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    expect_armed(c, &s, 2, "job names buffer 2");
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    check!(pop_frame(&mut s, &mut f), "frame published");
    check!(
        f.buffer == 2 && f.identity == 0xabc && f.bytes == F_SIZE,
        "the job's identity came back unchanged (buffer {} identity {:x})",
        f.buffer,
        f.identity
    );

    // The stream is job-driven now: no job, no buffer armed for this start.
    fire(c, TCSI_FS);
    let i = info(c);
    check!(
        i.dropped == 1,
        "a frame start with no job is dropped ({})",
        i.dropped
    );
    // Nothing is armed for a job-driven stream with no job: no count to give.
    let fs = frame_starts(c);
    check!(
        fs.valid == 0,
        "with no job armed the count is not valid (valid {})",
        fs.valid
    );

    // A job whose frame goes wrong comes back with 0 bytes, identity intact.
    give_back(&mut s, f.buffer);
    post(&mut s, &job_bytes(0xdef, 3));
    fire(c, TCSI_FE_ACK); // that frame end (no buffer) lets the job be armed
    fire(c, TCSI_FS);
    // The frame on buffer 3 started at this count (nothing is armed now, so the ioctl gives none).
    let bad = info(c).frames;
    fire(c, TCSI_FS); // a second start: the frame on buffer 3 went wrong
    check!(
        pop_frame(&mut s, &mut f) && f.buffer == 3 && f.bytes == 0 && f.identity == 0xdef,
        "a bad frame on a job comes back with 0 bytes (buffer {} bytes {} identity {:x})",
        f.buffer,
        f.bytes,
        f.identity
    );
    check!(
        f.sequence == bad.wrapping_sub(1),
        "and its sequence is its own start's, one below the count ({}, count {})",
        f.sequence,
        bad
    );
    stream_close(s);
}

/// A frame is left in flight: `lines` the fake will still write after FORCE. The action must not
/// return while any of them is still due, so the fake reports none left when it does. The fake's
/// count is its own state, not a clock, so this holds however long the host takes to run the fake's
/// timer.
fn drained_by(c: &mut Ctx, what: &str, lines: u32, action: impl FnOnce(&mut Ctx)) {
    busy(c, lines);
    let before = busy_left(c);
    check!(
        before == lines,
        "{what}: {lines} lines in flight going in ({before})"
    );
    action(c);
    let left = busy_left(c);
    check!(
        left == 0,
        "{what}: returned with {left} of {lines} lines still in flight"
    );
}

/// The stop window scales with the line time. A 4096 px RAW12 line on one lane at 80 Mbit/s takes
/// 615 us, so its window is 3 lines, 1845 us: 10 reads. START takes it, and STOP waits for it.
fn test_window(c: &mut Ctx, pool: &OwnedFd) {
    let line = lrc_line_us(6144, 1, 80);
    check!(
        line == 615,
        "a 4096 px RAW12 line on one lane at 80 Mbit/s takes {line} us"
    );
    let Ok(s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let mut a = good_start(s.fd.as_raw_fd());
    put32(&mut a, 4, 1);
    put32(&mut a, 8, 80);
    put32(&mut a, 20, 4096);
    put32(&mut a, 24, 1);
    put32(&mut a, 28, 6144);
    put32(&mut a, 32, 6144);
    check!(
        start(c, &a) == 0,
        "START takes the 4096 px RAW12 line at one lane, 80 Mbit/s"
    );
    fire(c, TCSI_FS);
    drained_by(
        c,
        "STOP waits for the frame end after FORCE, slow link",
        100,
        |c| {
            check!(stop(c) == 0, "STOP");
        },
    );
    check!(info(c).streaming == 0, "stopped");
    busy(c, 0);
    stream_close(s);
}

/// Pins the thread to CPU 0: the fake's timers run there (see the fake).
fn pin_to_cpu0() {
    let ok = guest::set_cpu_affinity(Some(0)).is_ok();
    check!(ok, "this thread pinned to CPU 0");
}

fn test_stop_waits(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    fire(c, TCSI_FS);
    drained_by(c, "STOP waits for DMA, frame in flight", 800, |c| {
        check!(stop(c) == 0, "STOP");
    });
    check!(info(c).streaming == 0, "stopped");
    busy(c, 0);
    // Every buffer came back, including the two the stopped frame held: START again and all four
    // must go through with nothing dropped.
    check!(start(c, &a) == 0, "START again on the same stream");
    {
        let mut f = Frame::default();
        let (mut seen, mut got) = (0u32, 0u32);

        for _ in 0..4 {
            fire(c, TCSI_FS);
            fire(c, TCSI_FE_ACK);
            if pop_frame(&mut s, &mut f) {
                seen |= 1u32 << f.buffer;
                got += 1;
                give_back(&mut s, f.buffer);
            }
        }
        check!(
            got == 4 && seen == 0xf && info(c).dropped == 0,
            "stopping gave every buffer back ({got} frames, buffers {seen:x}, {} dropped)",
            info(c).dropped
        );
    }

    // Stop again, then close: the DMA wait also holds for a stream's close.
    drained_by(c, "STOP again waits for DMA", 400, |c| {
        check!(stop(c) == 0, "STOP");
    });
    busy(c, 0);
    check!(start(c, &a) == 0, "START");
    fire(c, TCSI_FS);
    drained_by(
        c,
        "closing the stream with a frame in flight waits for DMA",
        600,
        move |c| {
            stream_close(s);
            let _ = c;
        },
    );
    check!(
        info(c).streaming == 0,
        "closing the stream stopped the receiver"
    );
    busy(c, 0);
}

fn test_stop_nothing_in_flight(c: &mut Ctx, pool: &OwnedFd) {
    let Ok(s) = stream_open(c, Some(pool)) else {
        check!(false, "stop idle: stream made");
        return;
    };
    check!(true, "stop idle: stream made");
    let mut f = Frame::default();
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "stop idle: START");
    check!(
        stop(c) == 0,
        "stop idle: STOP with nothing in flight returns"
    );
    let i = info(c);
    check!(
        i.frames == 0 && i.written == 0 && i.errors == 0 && i.dropped == 0,
        "stop idle: the stop counts no frame (frames {} written {} errors {} dropped {})",
        i.frames,
        i.written,
        i.errors,
        i.dropped
    );
    stream_close(s);

    let Ok(mut s) = stream_open(c, Some(pool)) else {
        check!(false, "stop idle: stream made again");
        return;
    };
    check!(true, "stop idle: stream made again");
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "stop idle: START again");
    let fs = frame_starts(c);
    check!(
        fs.valid != 0 && fs.frames == 0,
        "stop idle: the next start's count is empty (frames {})",
        fs.frames
    );
    fire(c, TCSI_FS);
    let fs = frame_starts(c);
    check!(
        fs.valid != 0 && fs.frames == 1,
        "stop idle: the first start is counted once (frames {})",
        fs.frames
    );
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f) && f.sequence == 0,
        "stop idle: its sequence is 0 (sequence {})",
        f.sequence
    );
    check!(stop(c) == 0, "stop idle: STOP");
    stream_close(s);
}

/// Closing the node with a frame in flight: a second file may open it but not drive the receiver;
/// the close waits for the DMA; the node reopens and the stream stays open.
fn test_close_node(c: &mut Ctx, node: &str, pool: &OwnedFd) {
    // A second file may open the node; it can't drive the receiver while the first claims it.
    let second = open_rdwr(node);
    check!(second.is_some(), "a second file opens the node");
    check!(
        claim_csi2(second.as_ref().map(|f| f.as_fd())) == -libc::EBUSY,
        "the receiver is claimed by the first file (EBUSY)"
    );
    let Ok(s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    fire(c, TCSI_FS);
    busy(c, 25);
    let mut oi = [0u8; 56];
    let ok = second
        .as_ref()
        .is_some_and(|f| guest::ioctl_bytes(f.as_fd(), RP1_INFO, &mut oi).is_ok());
    check!(
        ok && get32(&oi, 16) != 0,
        "the second file observes the running receiver"
    );
    let refused = match second.as_ref() {
        Some(f) => guest::ioctl_bytes(f.as_fd(), RP1_STOP, &mut [0u8; 0])
            .err()
            .and_then(|e| e.raw_os_error()),
        None => Some(libc::EBADF),
    };
    check!(
        refused == Some(libc::EPERM),
        "the second file cannot stop it (EPERM)"
    );
    drop(second);
    check!(
        info(c).streaming != 0,
        "closing the second file stops nothing"
    );
    drained_by(
        c,
        "closing the node with a frame in flight waits for DMA",
        500,
        |c| {
            c.ctl = None;
        },
    );
    c.ctl = open_rdwr(node);
    check!(c.ctl.is_some(), "the node opens again");
    check!(
        claim_csi2(ctl_fd(c)) == 0,
        "the reopened node claims the receiver again"
    );
    check!(info(c).streaming == 0, "not streaming after the close");
    busy(c, 0);
    check!(
        ring_words(&s)
            .get(SHM_W_END)
            .map_or(0, |w| w.load(Ordering::Acquire))
            == RING_STATE_OPEN,
        "the stream stays open"
    );
    check!(start(c, &a) == 0, "START works again after the node closed");
    check!(stop(c) == 0, "STOP");
    stream_close(s);
}

/// Unbinding with a frame in flight: waits for DMA; the stream ends; the node goes.
fn test_unbind(c: &mut Ctx, node: &str, unbind: &str, dev: &str, pool: &OwnedFd) {
    let Ok(s) = stream_open(c, Some(pool)) else {
        check!(false, "stream made");
        return;
    };
    check!(true, "stream made");
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    fire(c, TCSI_FS);
    drained_by(
        c,
        "unbinding with a frame in flight waits for DMA",
        700,
        |c| {
            let file = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_CLOEXEC)
                .open(unbind);
            check!(file.is_ok(), "unbind file opened");
            let wrote = match file {
                Ok(mut f) => f.write(dev.as_bytes()).is_ok_and(|n| n == dev.len()),
                Err(_) => false,
            };
            check!(wrote, "unbound");
            let _ = c;
        },
    );
    busy(c, 0);
    check!(
        ring_words(&s)
            .get(SHM_W_END)
            .map_or(0, |w| w.load(Ordering::Acquire))
            == RING_STATE_CLOSED,
        "the stream ended"
    );
    check!(std::fs::metadata(node).is_err(), "the node went away");
    check!(
        ioc_on(ctl_fd(c), RP1_STOP, &mut [0u8; 0])
            .err()
            .and_then(|e| e.raw_os_error())
            == Some(libc::ENODEV),
        "an open file now gets ENODEV"
    );
    stream_close(s);
    c.ctl = None;
}

/// How many dma-bufs are attached to `dev`, from the kernel's own list. -1 when the list can't be read.
fn bufinfo_attached(dev: &str) -> i32 {
    let Ok(file) = File::open("/sys/kernel/debug/dma_buf/bufinfo") else {
        return -1;
    };
    let mut buf = Vec::new();
    // The C reads at most one byte short of its 256 KiB buffer, and stops at a NUL.
    if file.take((1u64 << 18) - 1).read_to_end(&mut buf).is_err() {
        return -1;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let pattern = format!("\t{dev}\n");
    let text = &buf[..end];
    let needle = pattern.as_bytes();
    let mut count = 0;
    let mut at = 0;
    while at + needle.len() <= text.len() {
        if &text[at..at + needle.len()] == needle {
            count += 1;
            at += needle.len();
        } else {
            at += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------------------------
// A stream whose buffers are dma-bufs of their own.
// ---------------------------------------------------------------------------------------------

/// One buffer's own dma-buf: its descriptor, the device address the module gave it, and its CPU
/// mapping (the C's `struct own`).
#[derive(Default)]
struct Own {
    fd: Option<OwnedFd>,
    addr: u64,
    cpu: Option<Mapping>,
}

fn own_make(heap: &str) -> Own {
    let Ok(h) = File::open(heap) else {
        return Own::default();
    };
    let Ok(fd) = dma_heap::allocate(h.as_fd(), u64::from(BUFFER_BYTES)) else {
        return Own::default();
    };
    drop(h);
    match Mapping::new(fd.as_fd(), BUFFER_BYTES as usize, 0, true) {
        Ok(mut cpu) => {
            cpu.as_mut_slice().fill(0);
            Own {
                fd: Some(fd),
                addr: 0,
                cpu: Some(cpu),
            }
        }
        Err(_) => Own::default(),
    }
}

fn own_cpu(o: &Own, at: usize) -> u8 {
    o.cpu
        .as_ref()
        .and_then(|m| m.as_slice().get(at).copied())
        .unwrap_or(0)
}

/// Gives buffer `b` the memory `o`, and learns its device address: 0, or -errno.
fn own_set(s: &Stream, b: u32, o: &mut Own) -> i32 {
    let mut a = [0u8; 24];
    put32(&mut a, 0, b);
    put_i32(&mut a, 4, o.fd.as_ref().map_or(-1, |f| f.as_raw_fd()));
    match guest::ioctl_bytes(s.fd.as_fd(), BUFFER, &mut a) {
        Ok(()) => {
            o.addr = get64(&a, 8);
            0
        }
        Err(e) => -errno_of(&e),
    }
}

fn own_fill(o: &mut Own) {
    if let Some(m) = o.cpu.as_mut() {
        m.as_mut_slice().fill(0);
    }
}

fn test_own_buffers(c: &mut Ctx, heap: &str, dev: &str) {
    let before = bufinfo_attached(dev);
    let mut o: [Own; BUFFERS as usize] = std::array::from_fn(|_| own_make(heap));
    let mut fresh = own_make(heap);
    check!(
        o.iter().all(|x| x.fd.is_some()) && fresh.fd.is_some(),
        "own buffers: five dma-bufs"
    );
    let Ok(mut s) = stream_open(c, None) else {
        check!(false, "a stream with no pool on the receiver");
        return;
    };
    check!(s.pool_addr == 0, "a stream with no pool on the receiver");
    let mut f = Frame::default();
    let mut f1 = Frame::default();

    // Started with no buffer having memory: nothing is armed and frames are dropped.
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START with no buffer having memory");
    check!(
        armed_addr(c) == 0,
        "no address reaches the channel ({:x})",
        armed_addr(c)
    );
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    check!(
        info(c).dropped == 1 && !pop_frame(&mut s, &mut f) && armed_addr(c) == 0,
        "a frame with nowhere to go is dropped, and nothing is armed"
    );

    // Memory arrives: it is armed at its own address with no frame start, since a real channel with
    // nothing armed makes none.
    check!(
        own_set(&s, 0, &mut o[0]) == 0 && own_set(&s, 1, &mut o[1]) == 0,
        "buffers 0 and 1 get memory"
    );
    check!(
        o[0].addr != 0 && o[1].addr != 0 && o[0].addr != o[1].addr,
        "each at an address of its own"
    );
    for _ in 0..200 {
        if armed_addr(c) == o[0].addr {
            break;
        }
        sleep_ms(1);
    }
    check!(
        armed_addr(c) == o[0].addr,
        "buffer 0's own dma-buf is armed with no frame start ({:x}, its {:x})",
        armed_addr(c),
        o[0].addr
    );
    fire(c, TCSI_FS);
    check!(
        armed_addr(c) == o[1].addr,
        "then buffer 1's ({:x}, its {:x})",
        armed_addr(c),
        o[1].addr
    );
    // Buffer 0 is being written and buffer 1 is armed: neither may change memory.
    check!(
        own_set(&s, 0, &mut fresh) == -libc::EBUSY,
        "swapping the buffer being written: EBUSY"
    );
    check!(
        own_set(&s, 1, &mut fresh) == -libc::EBUSY,
        "swapping the armed buffer: EBUSY"
    );
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f) && f.buffer == 0 && f.bytes == F_SIZE,
        "the frame is buffer 0's"
    );
    let first = own_cpu(&o[0], 0);
    check!(
        first != 0
            && own_cpu(&o[0], F_SIZE as usize - 1) == first
            && own_cpu(&o[0], F_SIZE as usize) == 0,
        "the channel wrote the frame into buffer 0's own dma-buf (byte {first:x})"
    );
    check!(
        own_cpu(&o[1], 0) == 0 && own_cpu(&o[2], 0) == 0 && own_cpu(&fresh, 0) == 0,
        "and into nothing else"
    );

    // The consumer has buffer 0. It gets new memory, goes back, and is armed there.
    check!(
        own_set(&s, 0, &mut fresh) == 0 && fresh.addr != 0 && fresh.addr != o[0].addr,
        "a buffer the consumer holds is given new memory"
    );
    own_fill(&mut o[0]);
    give_back(&mut s, f.buffer);
    fire(c, TCSI_FS);
    // Buffer 0 holds the newest frame, which the module never takes back (lem_stream_take), so it is
    // not re-armed yet: buffer 1, armed before, stays the one the channel writes next.
    check!(
        armed_addr(c) == o[1].addr,
        "the newest frame's buffer is not re-armed; buffer 1 stays armed ({:x}, its {:x})",
        armed_addr(c),
        o[1].addr
    );
    fire(c, TCSI_FE_ACK);
    check!(
        pop_frame(&mut s, &mut f1) && f1.buffer == 1 && own_cpu(&o[1], 0) != 0,
        "buffer 1's frame in its own memory"
    );
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    let fresh_first = own_cpu(&fresh, 0);
    check!(
        pop_frame(&mut s, &mut f)
            && f.buffer == 0
            && fresh_first != 0
            && own_cpu(&fresh, F_SIZE as usize - 1) == fresh_first,
        "buffer 0's next frame is in its new memory (byte {fresh_first:x})"
    );
    check!(
        own_cpu(&o[0], 0) == 0 && own_cpu(&o[0], F_SIZE as usize - 1) == 0,
        "and the memory it had is not written again"
    );
    check!(
        bufinfo_attached(dev) == before + 2,
        "two buffers pinned, the swapped one's old memory not"
    );

    // Stopping with a frame in flight still waits for the DMA, and gives the buffers back.
    give_back(&mut s, 0);
    give_back(&mut s, 1);
    fire(c, TCSI_FS);
    drained_by(c, "STOP waits for DMA on its own buffers", 800, |c| {
        check!(stop(c) == 0, "STOP");
    });
    busy(c, 0);
    let mut all_free = true;
    for (i, x) in o.iter_mut().enumerate() {
        if own_set(&s, i as u32, x) != 0 {
            all_free = false;
            break;
        }
    }
    check!(all_free, "stopped: every buffer is free to take memory");
    check!(start(c, &a) == 0, "START again");
    {
        let mut seen = 0u32;

        for _ in 0..5 {
            fire(c, TCSI_FS);
            fire(c, TCSI_FE_ACK);
            if pop_frame(&mut s, &mut f)
                && f.buffer < BUFFERS
                && own_cpu(&o[f.buffer as usize], 0) != 0
            {
                seen |= 1u32 << f.buffer;
            }
        }
        check!(
            seen == 0xf,
            "all four fill, each in its own dma-buf ({seen:x})"
        );
    }
    fire(c, TCSI_FS);
    drained_by(
        c,
        "closing it with a frame in flight waits for DMA",
        600,
        move |c| {
            stream_close(s);
            let _ = c;
        },
    );
    busy(c, 0);
    check!(info(c).streaming == 0, "and stops the receiver");
    check!(
        bufinfo_attached(dev) == before,
        "closing released every buffer's pin"
    );
    for x in o.iter_mut() {
        own_drop(x);
    }
    own_drop(&mut fresh);
}

fn own_drop(o: &mut Own) {
    o.cpu = None;
    o.fd = None;
}

/// `make_pool`: a dma-buf for a stream's pool, of `BUFFERS` buffers, or None.
fn make_pool(heap_path: &str) -> Option<OwnedFd> {
    let heap = File::open(heap_path).ok()?;
    dma_heap::allocate(heap.as_fd(), u64::from(BUFFERS) * STRIDE_POOL).ok()
}

/// DMA that never stops: the buffers stay pinned for good and the receiver refuses to start.
fn run_wedge(c: &mut Ctx, node: &str, dev: &str, heap: &str) -> i32 {
    c.ctl = open_rdwr(node);
    c.hw = open_rdwr("/dev/lemnos-test-csi2");
    check!(claim_csi2(ctl_fd(c)) == 0, "the receiver claimed");
    let pool = make_pool(heap);
    let pool2 = make_pool(heap);
    check!(
        c.ctl.is_some() && c.hw.is_some() && pool.is_some() && pool2.is_some(),
        "wedge: set up"
    );
    let (Some(pool), Some(pool2)) = (pool, pool2) else {
        return 1;
    };
    let forever = u32::MAX;

    // A blanking gap while the DMA is busy: the line counter holds still for the blanking lines, and
    // the DMA is still writing. STOP must not return until the frame end after FORCE, so no busy
    // line is left when it returns. A still counter alone, which the old stop read as stopped,
    // returned here with the DMA busy.
    let Ok(s) = stream_open(c, Some(&pool)) else {
        check!(false, "blank: stream made");
        return 1;
    };
    check!(true, "blank: stream made");
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "blank: START");
    fire(c, TCSI_FS);
    blank(c, 400);
    busy(c, 600);
    check!(stop(c) == 0, "blank: STOP returns");
    check!(
        busy_left(c) == 0,
        "STOP did not return while the DMA was busy across a blanking gap ({} lines left)",
        busy_left(c)
    );
    blank(c, 0);
    busy(c, 0);
    stream_close(s);

    let Ok(s) = stream_open(c, Some(&pool)) else {
        check!(false, "stream made");
        return 1;
    };
    check!(true, "stream made");
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    fire(c, TCSI_FS);
    busy(c, forever);
    check!(stop(c) == 0, "STOP returns, DMA never seen to stop");
    // A late FE_ACK: the wedged DMA stops only as the window is released, after the stop gave up. It
    // must count no frame, and the wedge still refuses START. FAKE-ONLY ASSUMPTION (TCSI_IOC_LATE_ACK).
    let before = info(c);
    check!(
        hw_u32(c, TCSI_LATE_ACK, 1).is_ok(),
        "late: one late FE_ACK armed"
    );
    busy(c, 0);
    check!(
        info(c).frames == before.frames
            && info(c).written == before.written
            && info(c).errors == before.errors
            && info(c).dropped == before.dropped,
        "late: the late FE_ACK counts no frame (frames {} written {} errors {})",
        info(c).frames,
        info(c).written,
        info(c).errors
    );
    check!(info(c).flags & INFO_WEDGED != 0, "wedged is reported");
    let Ok(s2) = stream_open(c, Some(&pool2)) else {
        check!(false, "a new stream can be made");
        return 1;
    };
    check!(true, "a new stream can be made");
    let a2 = good_start(s2.fd.as_raw_fd());
    check!(start(c, &a2) == -libc::EIO, "wedged refuses START (EIO)");
    stream_close(s2);
    drop(pool2);

    // Everything the DMA could reach stays pinned, even with nobody holding it.
    stream_close(s);
    drop(pool);
    sleep_ms(5);
    check!(
        bufinfo_attached(dev) == 1,
        "the pool stays attached to the device after every close"
    );

    let fd = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open("/sys/bus/platform/drivers/lemnos-rp1-csi2/unbind");
    let wrote = match fd {
        Ok(mut f) => f.write(dev.as_bytes()).is_ok_and(|n| n == dev.len()),
        Err(_) => false,
    };
    check!(wrote, "unbound while wedged");
    c.ctl = None;
    c.hw = None;
    i32::from(failures() != 0)
}

/// The same on a stream with no pool: each buffer's own dma-buf stays pinned, and none can be swapped.
fn run_wedge_own(c: &mut Ctx, node: &str, dev: &str, heap: &str) -> i32 {
    let before = bufinfo_attached(dev);
    let forever = u32::MAX;

    c.ctl = open_rdwr(node);
    c.hw = open_rdwr("/dev/lemnos-test-csi2");
    check!(claim_csi2(ctl_fd(c)) == 0, "the receiver claimed");
    let mut o = [own_make(heap), own_make(heap)];
    let mut fresh = own_make(heap);
    check!(
        c.ctl.is_some()
            && c.hw.is_some()
            && o[0].fd.is_some()
            && o[1].fd.is_some()
            && fresh.fd.is_some(),
        "wedge, own buffers: set up"
    );
    let Ok(s) = stream_open(c, None) else {
        check!(false, "a stream with no pool, two buffers with memory");
        return 1;
    };
    let mut ok = own_set(&s, 0, &mut o[0]) == 0;
    ok = ok && own_set(&s, 1, &mut o[1]) == 0;
    check!(ok, "a stream with no pool, two buffers with memory");
    let a = good_start(s.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START");
    fire(c, TCSI_FS);
    busy(c, forever);
    check!(stop(c) == 0, "STOP returns, DMA never seen to stop");
    busy(c, 0);
    check!(info(c).flags & INFO_WEDGED != 0, "wedged is reported");
    check!(
        own_set(&s, 0, &mut fresh) == -libc::EIO && own_set(&s, 1, &mut fresh) == -libc::EIO,
        "no buffer's memory can be swapped out from under a block that may be writing: EIO"
    );
    check!(
        own_set(&s, 2, &mut fresh) == -libc::EIO,
        "and none is added: EIO"
    );
    stream_close(s);
    own_drop(&mut o[0]);
    own_drop(&mut o[1]);
    own_drop(&mut fresh);
    sleep_ms(5);
    let now = bufinfo_attached(dev);
    check!(
        now == before + 2,
        "both buffers' dma-bufs stay attached after every close ({} more)",
        now - before
    );
    let fd = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open("/sys/bus/platform/drivers/lemnos-rp1-csi2/unbind");
    let wrote = match fd {
        Ok(mut f) => f.write(dev.as_bytes()).is_ok_and(|n| n == dev.len()),
        Err(_) => false,
    };
    check!(wrote, "unbound while wedged");
    c.ctl = None;
    c.hw = None;
    i32::from(failures() != 0)
}

/// The embedded data stream: asked for before START, it is written by the block's second channel on
/// the image's virtual channel with data type 0x12. Its buffers come from a pool of their own: the
/// frame stream's pool would alias them.
fn test_embedded(c: &mut Ctx, pool: &OwnedFd, heap: &str) {
    let epool = make_pool(heap);
    check!(epool.is_some(), "embedded: a pool of its own allocated");
    let mut ecpu = epool.as_ref().and_then(|p| {
        Mapping::new(p.as_fd(), (BUFFERS as u64 * STRIDE_POOL) as usize, 0, true).ok()
    });
    check!(ecpu.is_some(), "embedded: its pool mapped");
    if let Some(m) = ecpu.as_mut() {
        m.as_mut_slice().fill(0);
    }
    let ecpu_at = |at: usize| -> u8 {
        ecpu.as_ref()
            .and_then(|m| m.as_slice().get(at).copied())
            .unwrap_or(0)
    };

    // Refused: a length the channel can't take, no stream with a length, a reserved word.
    check!(
        ask_embedded(c, -1, 0, 0) == 0,
        "embedded: asking for none is allowed"
    );
    check!(
        ask_embedded(c, -1, 16, 0) == -libc::EINVAL,
        "embedded: a length with no stream is refused"
    );
    let Ok(mut emb) = stream_open(c, epool.as_ref()) else {
        check!(false, "embedded: a stream made for the request");
        return;
    };
    check!(true, "embedded: a stream made for the request");
    check!(
        ask_embedded(c, emb.fd.as_raw_fd(), 24, 0) == -libc::EINVAL,
        "embedded: a length not a multiple of 16"
    );
    check!(
        ask_embedded(c, emb.fd.as_raw_fd(), 0, 0) == -libc::EINVAL,
        "embedded: a zero length"
    );
    check!(
        ask_embedded(c, emb.fd.as_raw_fd(), EMBEDDED_MAX + 16, 0) == -libc::EINVAL,
        "embedded: a length over the channel's limit"
    );
    check!(
        ask_embedded(c, emb.fd.as_raw_fd(), EMB_BYTES, 1) == -libc::EINVAL,
        "embedded: a reserved word is refused"
    );

    // Not asked for: the second channel stays off, and nothing reaches the stream.
    let Ok(main_a) = stream_open(c, Some(pool)) else {
        check!(false, "a frame stream for the run with none asked for");
        return;
    };
    check!(true, "a frame stream for the run with none asked for");
    let mut f = Frame::default();
    let a = good_start(main_a.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START with no embedded stream asked for");
    let r = peek_emb(c);
    check!(
        r.ctrl == 0,
        "no embedded channel without a request (ctrl {:x})",
        r.ctrl
    );
    check!(
        ask_embedded(c, emb.fd.as_raw_fd(), EMB_BYTES, 0) == -libc::EBUSY,
        "embedded: a request is refused while streaming"
    );
    fire(c, TCSI_EMB_FS);
    fire(c, TCSI_EMB_FE_ACK);
    fire(c, TCSI_FE_ACK);
    check!(
        !pop_frame(&mut emb, &mut f),
        "no embedded frame when none is asked for"
    );
    check!(stop(c) == 0, "STOP");
    stream_close(main_a);

    // Asked for: one buffer per frame, carrying that frame's sequence.
    let Ok(main_b) = stream_open(c, Some(pool)) else {
        check!(false, "a frame stream for the embedded run");
        return;
    };
    check!(true, "a frame stream for the embedded run");
    check!(
        ask_embedded(c, emb.fd.as_raw_fd(), EMB_BYTES, 0) == 0,
        "embedded: the request is accepted"
    );
    let a = good_start(main_b.fd.as_raw_fd());
    check!(start(c, &a) == 0, "START with the embedded stream");
    let r = peek_emb(c);
    check!(
        r.ctrl & 1 != 0 && (r.ctrl >> 7 & 63) == EMBEDDED_DT,
        "embedded channel on data type 0x12 (ctrl {:x})",
        r.ctrl
    );
    check!(
        (r.ctrl >> 5 & 3) == get32(&a, 12),
        "embedded channel on the image's virtual channel"
    );
    check!(
        r.frame_size == (1u32 << 16 | EMB_BYTES),
        "embedded frame size {:x}",
        r.frame_size
    );
    check!(
        (u64::from(r.addr1) << 36) | (u64::from(r.addr0) << 4) == buffer_addr(&emb, 0),
        "embedded buffer 0 armed"
    );

    // Frame 0: a line, written and ended within the frame.
    fire(c, TCSI_FS | TCSI_EMB_FS);
    fire(c, TCSI_EMB_FE_ACK);
    fire(c, TCSI_FE_ACK);
    // Frame 1: no embedded line; its buffer goes out empty when frame 2 starts.
    fire(c, TCSI_FS);
    fire(c, TCSI_FE_ACK);
    // Frame 2: a line again.
    fire(c, TCSI_FS | TCSI_EMB_FS);
    fire(c, TCSI_EMB_FE_ACK);
    fire(c, TCSI_FE_ACK);

    // Frame 1 had no line: nothing is published for it, and the gap in sequence shows it.
    check!(
        pop_frame(&mut emb, &mut f) && f.sequence == 0 && f.bytes == EMB_BYTES && f.buffer == 0,
        "embedded frame 0: buffer {} bytes {} sequence {}",
        f.buffer,
        f.bytes,
        f.sequence
    );
    check!(
        pop_frame(&mut emb, &mut f) && f.sequence == 2 && f.bytes == EMB_BYTES && f.buffer == 1,
        "embedded frame 2 (frame 1 has none): buffer {} bytes {} sequence {}",
        f.buffer,
        f.bytes,
        f.sequence
    );
    check!(
        !pop_frame(&mut emb, &mut f),
        "no embedded frame for the frame with no line"
    );

    // Each line's bytes are the block's: one pattern over the whole line, not zero.
    let same0 = (1..EMB_BYTES as usize).all(|i| ecpu_at(i) == ecpu_at(0));
    check!(
        ecpu_at(0) != 0 && same0,
        "frame 0's line is in its buffer (byte {})",
        ecpu_at(0)
    );
    let base = STRIDE_POOL as usize;
    let same1 = (1..EMB_BYTES as usize).all(|i| ecpu_at(base + i) == ecpu_at(base));
    check!(
        ecpu_at(base) != 0 && same1,
        "frame 2's line is in buffer 1 (byte {})",
        ecpu_at(base)
    );
    check!(
        info(c).frames == 3 && info(c).written == 3,
        "image frames counted with the line"
    );
    check!(stop(c) == 0, "STOP after the embedded run");
    check!(info(c).streaming == 0, "not streaming after STOP");
    stream_close(main_b);
    stream_close(emb);
    drop(ecpu);
    drop(epool);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let argc = args.len();
    let mut c = Ctx {
        ctl: None,
        hw: None,
        pool_cpu: None,
    };

    if argc == 5 && args[1] == "wedge" {
        let node = format!("/dev/lemnos-rp1-csi2-{}", args[2]);
        return ExitCode::from(run_wedge(&mut c, &node, &args[2], &args[4]) as u8);
    }
    if argc == 5 && args[1] == "wedge-own" {
        let node = format!("/dev/lemnos-rp1-csi2-{}", args[2]);
        return ExitCode::from(run_wedge_own(&mut c, &node, &args[2], &args[4]) as u8);
    }
    if argc != 6 {
        return ExitCode::from(2);
    }
    let block = args[2].as_str();
    let dev = args[3].as_str();
    let unbind = args[4].as_str();
    let heap = args[5].as_str();
    let node = format!("/dev/lemnos-rp1-csi2-{dev}");

    // The fake's timers run on CPU 0; the driver's reads must share its vCPU (see the fake).
    pin_to_cpu0();
    c.ctl = open_rdwr(&node);
    c.hw = open_rdwr("/dev/lemnos-test-csi2");
    check!(claim_csi2(ctl_fd(&c)) == 0, "the receiver claimed");
    check!(c.ctl.is_some() && c.hw.is_some(), "opened");
    let Some(pool) = make_pool(heap) else {
        check!(false, "pool allocated");
        say("FAILED\n");
        return ExitCode::from(1);
    };
    check!(true, "pool allocated");
    c.pool_cpu = Mapping::new(
        pool.as_fd(),
        (BUFFERS as u64 * STRIDE_POOL) as usize,
        0,
        true,
    )
    .ok();
    check!(c.pool_cpu.is_some(), "pool mapped");

    check!(info(&c).abi == ABI, "ABI");

    test_bad_starts(&c, &pool);
    test_foreign_stream(&c, &pool, block);
    match stream_open(&c, Some(&pool)) {
        Ok(s) => {
            check!(true, "a stream can be made and dropped untouched");
            stream_close(s);
        }
        Err(_) => check!(false, "a stream can be made and dropped untouched"),
    }
    test_frames(&mut c, &pool);
    test_frame_starts(&mut c, &pool);
    test_frame_starts_while_starved(&mut c, &pool);
    test_short_frame(&mut c, &pool);
    test_hostile_hw(&mut c, &pool);
    test_jobs(&mut c, &pool);
    test_own_buffers(&mut c, heap, dev);
    test_embedded(&mut c, &pool, heap);
    test_stop_waits(&mut c, &pool);
    test_stop_nothing_in_flight(&mut c, &pool);
    test_window(&mut c, &pool);
    test_close_node(&mut c, &node, &pool);
    test_unbind(&mut c, &node, unbind, dev, &pool);
    check!(
        bufinfo_attached(dev) == 0,
        "every mapping released after the unbind"
    );

    c.pool_cpu = None;
    drop(pool);
    c.hw = None;
    say(if failures() != 0 {
        "FAILED\n"
    } else {
        "all checks passed\n"
    });
    ExitCode::from(i32::from(failures() != 0) as u8)
}
